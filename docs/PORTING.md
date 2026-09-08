# C# → Rust 변환 설계서

원본: `/home/bhson/00-git/csharp_gameserver` (읽기 전용으로 취급. 해당 저장소 CLAUDE.md가 commit/push를 금지)

## 0. 대원칙

**내부 구조는 Rust답게, 외부 계약은 바이트 단위로 동일하게.**

`Client/World`(Unity, C#)는 그대로 유지되므로 **와이어 호환성은 선택이 아니라 필수 조건**이다.
반대로 C# 쪽 동시성 machinery 상당수는 GC 회피 / `SocketAsyncEventArgs` 대응 코드라 Rust에서는 재현할 이유가 없다.

## 1. 변환 대응표

| C# | Rust | 근거 |
|---|---|---|
| `UserSession` + `Channel<IMessageQueue>` | tokio task 1개 + `mpsc::channel(1000)` | task가 상태를 소유 → 락 불필요. 액터 보장 동일 |
| `TickThreadWorker` 100ms 스캔 + `SessionId % N` 고정 | 세션 task 내부 `tokio::select!` { `rx.recv()`, `interval.tick()` } | 메시지 처리에 최대 100ms 지연이 붙던 문제 제거, O(세션수) 딕셔너리 순회 제거 |
| `UserObjectPoolManager` (세션 10,000개 사전할당) | 삭제 | 순수 GC 압박 회피용 |
| 리플렉션 + `[RemoteMessageHandlerAttribute]` | `match wrapper.payload { .. }` | Rust에 런타임 리플렉션 없음. 대신 컴파일타임 exhaustive 검사를 얻음 (C#은 핸들러 미등록 시 조용히 무시) |
| `SqlWorkerManager` (워커 32 + 채널 15000) | `sqlx::MySqlPool` | 풀 자체가 큐 + 백프레셔. `ISqlRequest`/`DbErrorMessage`/`DbErrorHandler` ~400줄 제거 |
| `CacheWorkerManager` | `redis` `ConnectionManager` | 위와 동일 |
| `TCPAcceptor` + `SocketAsyncEventArgsPool` | `TcpListener::accept` 루프 | |
| `ReceiveParser` 상태머신 | `tokio_util::codec::Framed` + `LengthDelimitedCodec` | |
| `IServerLogger` (지연 델리게이트) | `tracing` | 매크로가 이미 지연 평가 |
| `LobbyThreadManager` / `WorldThreadManager` | 삭제 | 실제로 하는 일이 세션 등록/해제뿐인 잔재 |
| ASP.NET 컨트롤러 + `SessionKeyMiddleware` | axum router + layer (2단계) | |

### 유지하는 것 (동작 관측 가능 → 반드시 보존)
세션 액터 모델, 2바이트 길이 프레이밍, protobuf 스키마, 인증 전 메시지 게이팅, KeepAlive 타임아웃,
중복 로그인 킥, IP 플러드 밴, 송신 큐 상한, 10초 주기 모니터 로그.

### 버리는 것
객체 풀링, 스레드 고정, 리플렉션 디스패치, DB 워커 풀, Lobby/World 스레드 매니저, `ITickable` 추상화.

## 2. 작업 순서

| 단계 | 대상 | 이유 |
|---|---|---|
| **1** | **GameServer** | 두 C# 서버는 Redis(`auth:token:*`)로만 통신 → Rust GameServer가 **살아있는 C# LoginServer와 그대로 붙는다.** 기존 C# `DummyClient`가 수정 없이 인수 테스트가 됨. 표면적도 가장 작음(핸들러 5개, 해시 없음, AdminApi 없음) |
| 2 | LoginServer + AdminApi 7개 컨트롤러 | PBKDF2 검증, axum 필요 |
| 3 | dummy_client (선택) | C# DummyClient로 이미 검증되므로 후순위 |

`Client/`(Unity)는 변환 대상 외 — C#으로 유지.

## 3. 고정된 외부 계약 (변경 불가)

### 3.1 와이어 프레이밍
```
[2바이트 little-endian 본문 길이][protobuf MessageWrapper 본문]
```
- 길이 필드는 **본문 크기만** (헤더 2바이트 미포함). `SenderHandler.TrySerializeToBuffer` 확인.
- 유효 범위: `1 ~ 8190` (`MaxBufferSize 8192 - 2`). 0 또는 8191 이상은 **연결 종료**.
- `MessageWrapper.message_size`(field 1)는 **어디서도 설정하지 않음** → prost에서도 설정하지 말 것 (`grep MessageSize` 결과 사용처 0건).
- `Scripts/message.proto`를 `crates/proto/proto/message.proto`로 복사하고 원본 출처를 주석으로 명시 (저장소 간 빌드 경로 의존 회피).

### 3.2 접속 직후
accept 성공 시 **즉시** `ConnectedResponse { index: 0 }` 송신. (`UserObjectPoolManager.AcceptUser`)

### 3.3 인증 전 게이팅
미인증 세션이 `GameConnectRequest` / `KeepAliveRequest` 외 메시지를 보내면 **즉시 연결 종료**.

### 3.4 인증 (Redis)
- 키: `auth:token:{token}` (prefix는 설정값)
- 원자적 GET+DEL — C#은 Redis 6.2 미만 호환 위해 Lua 스크립트 사용. Rust도 동일 Lua 유지 권장.
- 값 형식: `"{account_id}:{user_id}"` — `:` 기준 **2개로만** split (user_id에 `:` 포함 가능).
- 파싱 실패/미존재 → `GameConnectResponse { success: false, error_code: 1 }`
- 성공 → `OnAuthenticated` 후 `GameConnectResponse { success: true, error_code: 0 }`
- **중복 로그인**: 같은 `user_id`의 기존 세션을 강제 종료.

### 3.5 KeepAlive
- 클라이언트 송신 주기 3초, 서버 타임아웃 10초.
- **인증된 세션에 대해서만** 타임아웃 검사 (`IsAuthenticated` 조건).

### 3.6 핸들러 동작
| 메시지 | 동작 |
|---|---|
| `GameConnectRequest` | Redis 토큰 검증 → `GameConnectResponse`. 이미 인증됨/토큰 빈 문자열이면 `error_code: 1` |
| `KeepAliveRequest` | 타임스탬프 갱신. **응답 없음** |
| `MoveRequest` | 좌표 저장 후 **발신자에게만** `MoveResponse { success, x, y }` 에코. **브로드캐스트 없음** |
| `EnterWorldRequest` | world_id 변경 + `EnterWorldResponse { success: true }` |
| `GameOverReport` | `INSERT INTO scores` 후 `GameOverResponse { success: true }`. 미인증이면 무시 |
| 연결 종료 시 | `UPDATE accounts SET last_login_at = UTC_TIMESTAMP() WHERE account_id = ?` |

### 3.7 DB 스키마
`Scripts/db/create_accounts.sql`, `create_scores.sql` 그대로 사용. Rust 저장소에도 복사본 보관.

### 3.8 접속 제한 (Acceptor)
- IP당 10초 윈도우 내 30회 초과 연결 → **10분 밴**
- allowlist: `127.0.0.1`, `::1`
- IPv4-mapped IPv6는 IPv4로 정규화
- 60초 주기로 만료 엔트리 sweep (IP 로테이션 공격 시 메모리 증식 방지)

### 3.9 기타 상수 (`SessionConstInfo`)
`MaxSendQueueSize 200`(초과 시 세션 종료), `MaxMessageChannelCapacity 1000`,
`MaxMessagesPerTick 50`, `MaxTimerPerSession 100`, `MaxListenerBackLog 4096`

## 4. 원본의 불일치 — 결정 필요

| 항목 | 상태 | 이 포팅의 결정 |
|---|---|---|
| auth token TTL | `appsettings.json`=**60초**, `CLAUDE.md`=30초 | 실제 동작값인 **60초** 채택. LoginServer 변환(2단계)까지는 C# 쪽이 발급하므로 영향 없음 |
| 수신 채널 초과 시 | 주석="세션 강제 종료", 코드=`DropWrite`(조용히 버림) | **강제 종료** 채택. 조용한 패킷 유실은 디버깅 불가능한 버그를 만듦 |
| tick 100ms | 메시지 처리에 최대 100ms 지연 | `select!`로 제거 (지연 감소는 관측 가능한 개선이므로 명시) |

## 5. 워크스페이스 구성

루트를 virtual workspace로 전환 (현재의 루트 `[package]` + `src/main.rs`는 삭제 — 한 커밋에서 처리).

```
Cargo.toml            # [workspace] members
crates/
  proto/              # prost-build (build.rs) + message.proto
  net/                # codec, session id, config, tracing, packet stats(AtomicU64), timer
  db/                 # sqlx MySqlPool, redis ConnectionManager
  game_server/        # 1단계
  login_server/       # 2단계
docs/PORTING.md
sql/                  # 스키마 복사본
```

주요 크레이트: `tokio`, `tokio-util`(codec), `prost` + `prost-build`, `sqlx`(mysql, rustls),
`redis`(tokio, connection-manager), `tracing` + `tracing-subscriber`, `axum`(2단계),
`pbkdf2`/`sha2`(2단계), `serde` + `figment` 또는 `config`.

**sqlx는 `macros`/`query!` 기능을 쓰지 않는다** — 컴파일 시점에 DB 접속을 요구하므로 DB 없는 환경에서 빌드 불가.

## 6. 보안 — 자격증명

C# `appsettings.json`에 MySQL 비밀번호가 **평문으로 커밋되어 있다** (`Password=Crazy1!crazy`).

- Rust 쪽은 **환경변수 또는 gitignore된 `config.toml`**에서 읽고, `config.example.toml`만 커밋한다.
- 원본 저장소에 이미 노출된 자격증명 자체를 어떻게 할지(로테이션 등)는 사용자 판단 사항.

## 7. 검증 계획

자체 인코더로 라운드트립하는 테스트는 와이어 호환성을 증명하지 못한다. 실제로 판별력 있는 항목:

- [ ] 디코더에 **1바이트씩** 흘려넣기 (partial header / partial body)
- [ ] **한 버퍼에 2개 메시지**를 동시에 넣기 (C#의 버퍼 압축 로직이 존재하는 이유)
- [ ] 본문 크기 `0`, `8191` 거부
- [ ] 인증 전 게이팅: 허용 2종 외 → 종료
- [ ] KeepAlive 3초 송신 / 10초 타임아웃
- [ ] **C# DummyClient를 수정 없이 Rust GameServer에 붙여 통과** ← 최종 인수 조건
