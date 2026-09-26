# C# → Rust 변환 설계서

원본: `/home/bhson/00-git/csharp` (읽기 전용으로 취급. 해당 저장소 CLAUDE.md가 commit/push를 금지)

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

> **순서 변경 (2026-09-26, 사용자 결정)**: LoginServer + dummy_client 를 먼저 진행하고 GameServer 를 그 다음에 한다.
> 아래 표는 원래 계획이며, 완료 표시는 현재 상태다.

| 단계 | 대상 | 상태 |
|---|---|---|
| 1 | **LoginServer + AdminApi 7개 컨트롤러** | 구현 완료. 메모리 backend 로 TCP 통합 테스트. 실제 MySQL/Redis·C# 클라이언트와는 미검증 |
| 2 | **dummy_client** | 구현 완료. 실제 Rust LoginServer + Rust GameServer(메모리 backend, 토큰 저장소 공유)로 흐름 테스트. 실제 Redis/MySQL 로도 5명 접속 확인. **C# 서버와의 조합은 미검증** |
| 3 | **GameServer** | 구현 완료. 메모리 backend 로 TCP 통합 테스트(`crates/game_server/tests/game_flow.rs`). 실제 Redis/MySQL + Rust LoginServer + Rust dummy_client 로 인증·KeepAlive·종료 기록 확인. **C# DummyClient, `scores` INSERT 는 실환경 미검증** |

현재 가능한 인수 테스트: Rust LoginServer + Rust GameServer + Rust/C# DummyClient. 두 서버는 Redis(`auth:token:*`)로만
통신하므로 C#/Rust 서버를 섞어 붙여도 된다.

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
| LoginServer 인증 세션 해제 | `UnregisterAuthenticatedSession` 을 LoginServer 에서 아무도 호출하지 않음 → 종료된 세션이 매핑에 남아, 같은 user_id 재로그인 시 **풀에 반납되어 다른 사용자에게 재사용 중인 세션을 끊을 수 있음** | 세션 종료 시 매핑이 그 세션을 가리킬 때만 해제. 계약이 아니라 버그로 판단 |
| LoginServer `LogoutSqlRequest` | 클래스는 있으나 LoginServer 에서 호출 안 함 (GameServer 만 호출) | 추가하지 않음. 로그인 성공 시 `last_login_at` 갱신만 유지 |
| 로그인 DB 오류 응답 | 쿼리 예외 → `DbErrorMessage` → `ErrorResponse { error_code: 3 }` (LoginResponse 아님). Redis 토큰 저장 실패 → `LoginResponse { error_code: 3 }` | 둘을 구분해 그대로 유지. 저장된 해시/salt 가 Base64 가 아닌 경우도 C# 은 예외 → 전자로 처리 |
| AdminApi TLS | Kestrel `UseHttps()` (개발 인증서) | `tls_cert_path`/`tls_key_path` 가 둘 다 있으면 HTTPS, 없으면 HTTP + 경고 로그 |
| AdminApi Swagger | Swashbuckle UI | 제공하지 않음 (개발 편의 기능이지 계약이 아님) |
| AdminApi 라우팅 | ASP.NET 은 경로 대소문자 무시, 미들웨어가 라우팅 전에 실행(없는 경로도 키 없으면 401) | axum 은 경로 대소문자 구분, 없는 경로는 404. JSON 속성명은 camelCase 출력 + PascalCase 입력 허용 |
| DummyClient 비밀번호 | 시드는 `Seeder:Password`, 로그인은 `"Test1234!"` 하드코딩 | 둘 다 `dummy_client.password` 하나로 |
| DummyClient 접속 실패 | 로그인서버 접속 예외가 루프 밖으로 나가 이후 접속을 중단 | 동일하게 중단하되 에러 로그 후 이미 접속한 클라이언트는 계속 유지 |
| GameServer `ServerError`(2) | 캐시 워커 채널이 가득 찼을 때만 발생 | 채널이 없으므로 보내지 않음. Redis 오류는 원본과 같이 `ErrorResponse { error_code: 3 }` |
| GameServer 로그아웃 기록 | `LogoutSqlRequest` 는 critical — 실패 시 500ms×2ⁿ(최대 30초) 재시도, 서버 종료 시 중단 | 동일. 세션 task 안에서 재시도하므로 종료 시 첫 시도까지는 기다린다 |
| GameServer 인증 결과 중복 | `GameConnectRequest` 를 응답 전에 두 번 보내면 결과 핸들러가 인증 여부를 다시 보지 않는다 | 원본대로 둔다 (정상 클라이언트는 한 번만 보냄) |
| 로그인 PBKDF2 실행 위치 | SQL 워커 스레드(32개)에서 실행 → 사실상 동시 32개 상한 | `spawn_blocking` + 코어 수 크기의 `Semaphore` |

## 5. 워크스페이스 구성

루트를 virtual workspace로 전환 (현재의 루트 `[package]` + `src/main.rs`는 삭제 — 한 커밋에서 처리).

```
Cargo.toml            # [workspace] members
crates/
  proto/              # prost-build (build.rs) + message.proto
  net/                # codec, session id, config, tracing, packet stats(AtomicU64), 송신 outbox, 세션 레지스트리
  db/                 # sqlx MySqlPool, redis ConnectionManager
  game_server/        # GameServer (lib + bin, tests/ 에 통합 테스트)
  login_server/       # LoginServer + AdminApi (lib + bin, tests/ 에 통합 테스트)
  dummy_client/       # 부하·인수 테스트 클라이언트 (lib + bin)
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

- [x] 디코더에 **1바이트씩** 흘려넣기 (partial header / partial body) — `net::codec` 단위 테스트
- [x] **한 버퍼에 2개 메시지**를 동시에 넣기 (C#의 버퍼 압축 로직이 존재하는 이유)
- [x] 본문 크기 `0`, `8191` 거부
- [x] 인증 전 게이팅: 허용 2종 외 → 종료 (LoginServer: `LoginRequest`/`KeepAliveRequest`)
- [x] KeepAlive 3초 송신(dummy_client) / 10초 타임아웃(LoginServer·GameServer, 인증 후만). 테스트는 실제 시간 + 타임아웃 주입
      (`SessionContext::with_keep_alive_timeout`) — 정지 시계는 소켓 I/O 대기 중에도 시간을 건너뛰어 쓸 수 없다
- [x] GameServer: 토큰 1회용, `:` 포함 user_id, Redis 오류 → `ErrorResponse{3}`, 인증 전 게이팅, 이동 에코(발신자만),
      점수 저장, 종료 시 `last_login_at`, 중복 로그인 킥, 로그아웃 재시도 백오프 — `game_server` 테스트
- [x] PBKDF2-HMAC-SHA256 을 RFC 7914 테스트 벡터로 확인 (C# 가 만든 해시로 교차 검증은 아직)
- [ ] 실제 MySQL 의 C# 시드 계정으로 Rust LoginServer 로그인 (PBKDF2 호환성 최종 확인)
- [ ] Rust LoginServer + C# GameServer + C# DummyClient
- [ ] Rust dummy_client + C# LoginServer/GameServer
- [ ] **C# DummyClient를 수정 없이 Rust GameServer에 붙여 통과** ← 최종 인수 조건
