# rust_gameserver

C# MMO 서버([csharp_likeactor](https://github.com/sohnbongho/csharp_likeactor))를 Rust 로 컨버팅하는 프로젝트.
C# ↔ Rust 대응과 고정된 외부 계약은 [`docs/PORTING.md`](docs/PORTING.md) 에 있다.
설계 문서 목차·갱신 규칙·남은 검증 항목은 [`docs/README.md`](docs/README.md) 를 본다.

## 크레이트

| 크레이트 | 종류 | 설명 | 문서 |
|---|---|---|---|
| `login_server` | 바이너리 + lib | LoginServer(TCP 9000) + AdminApi(HTTP 9010). 로그인 후 GameServer 용 1회용 토큰 발급 | [README](crates/login_server/README.md) |
| `game_server` | 바이너리 + lib | GameServer(TCP 9001). 토큰 인증, 이동·월드 입장·점수 보고 | [README](crates/game_server/README.md) |
| `dummy_client` | 바이너리 + lib | 부하·인수 테스트 클라이언트, `--seed` 테스트 계정 생성, 키보드 이동 | [README](crates/dummy_client/README.md) |
| `net` | lib | 와이어 프레이밍 codec, 수락 루프·플러드 밴, 송신 outbox, 세션 레지스트리, 설정·모니터 | [README](crates/net/README.md) |
| `db` | lib | MySQL 풀·Redis 매니저(지연 연결), 비밀번호 해시, 공지 Pub/Sub, `[database]` 설정 | [README](crates/db/README.md) |
| `proto` | lib | `message.proto`(원본에서 복사)와 prost 생성 타입 | [README](crates/proto/README.md) |

```mermaid
flowchart TD
    ls["login_server"] --> net & db & proto
    gs["game_server"] --> net & db & proto
    dc["dummy_client"] --> net & db & proto
    net --> proto
    ls <-.->|"Redis auth:token:*"| gs
```

## 개발 환경

### 설치한 Claude Code 스킬

```bash
claude plugins install mattpocock-skills
```

TypeScript/개발 관련 스킬 모음(mattpocock-skills)을 플러그인으로 설치해서 사용 중이다.

### Redis / MySQL 설치 (WSL Ubuntu 24.04)

로그인서버·게임서버가 쓰는 Redis(인증 토큰, 관리자 세션, 공지 Pub/Sub)와 MySQL(`accounts`, `scores`)을
WSL 에 직접 설치한다. WSL 에 systemd 가 켜져 있어야 한다 (`/etc/wsl.conf` 의 `[boot] systemd=true`).
`sudo` 가 필요한 명령은 직접 실행한다.

#### Redis

```bash
sudo apt update
sudo apt install -y redis-server
sudo systemctl enable --now redis-server   # 서비스 시작 + 부팅 시 자동 시작

redis-cli ping                             # → PONG
```

기본 설치는 `127.0.0.1:6379`, 비밀번호 없음이며 `config.toml` 기본값(`redis://127.0.0.1:6379`)과 같다.

#### MySQL

```bash
sudo apt install -y mysql-server
sudo systemctl enable --now mysql

# DB 와 서버용 계정 (비밀번호는 원하는 값으로 — 특수문자가 없으면 URL 에 그대로 쓸 수 있어 편하다)
sudo mysql <<'SQL'
CREATE DATABASE IF NOT EXISTS gamedb CHARACTER SET utf8mb4;
CREATE USER IF NOT EXISTS 'game'@'localhost' IDENTIFIED BY 'CHANGE_ME';
GRANT ALL PRIVILEGES ON gamedb.* TO 'game'@'localhost';
FLUSH PRIVILEGES;
SQL

# 스키마 적용 (저장소 루트에서)
mysql -u game -p gamedb < sql/create_accounts.sql
mysql -u game -p gamedb < sql/create_scores.sql
mysql -u game -p gamedb -e "SHOW TABLES;"   # accounts, scores
```

#### 서버 설정 연결

```bash
cp config.example.toml config.toml
```

`config.toml` 의 `[database]` 를 설치한 값으로 채운다. 비밀번호의 특수문자는 URL 인코딩한다 (`!` → `%21`, `@` → `%40`).

```toml
[database]
mysql_url = "mysql://game:CHANGE_ME@127.0.0.1:3306/gamedb"
redis_url = "redis://127.0.0.1:6379"
```

#### 테스트 계정 생성 및 확인

```bash
cargo dc -- --seed    # user_00001 ~ user_10000, 비밀번호 Test1234! (1,000개마다 진행 로그, 약 1~2분)
mysql -u game -p gamedb -e "SELECT COUNT(*) FROM accounts;"

cargo ls              # 다른 터미널에서
curl http://127.0.0.1:9010/api/health
# → {"status":"ok","db":"ok","redis":"ok"}

APP_DUMMY_CLIENT__MAX_CLIENT_COUNT=5 cargo dc   # 로그인 확인 (`cargo gs` 를 안 띄웠으면 이후 "게임서버 접속 실패" 는 정상)
redis-cli --scan --pattern 'auth:token:*'       # 발급된 토큰 (TTL 60초)
```

`--seed` 는 중복 계정을 `INSERT IGNORE` 로 건너뛰므로 중간에 멈췄다가 다시 실행해도 된다.

#### Windows 쪽(C# 서버 등)에서 접속할 때

`localhost:6379` / `localhost:3306` 으로 접속하면 WSL2 가 포트를 전달해 주므로 추가 설정이 필요 없다.
WSL IP(`hostname -I`)로 접속해야 한다면 외부 바인드를 열어야 한다 (로컬 개발용으로만):

```bash
# Redis
sudo sed -i 's/^bind .*/bind 0.0.0.0 -::1/' /etc/redis/redis.conf
sudo sed -i 's/^protected-mode yes/protected-mode no/' /etc/redis/redis.conf
sudo systemctl restart redis-server

# MySQL
sudo sed -i 's/^bind-address.*/bind-address = 0.0.0.0/' /etc/mysql/mysql.conf.d/mysqld.cnf
sudo systemctl restart mysql
sudo mysql -e "CREATE USER 'game'@'%' IDENTIFIED BY 'CHANGE_ME'; GRANT ALL ON gamedb.* TO 'game'@'%'; FLUSH PRIVILEGES;"
```

## 실행

워크스페이스에 실행 파일이 `game_server`, `login_server`, `dummy_client` 여럿이라 `cargo run` 만으로는 실행 대상이 정해지지 않는다.
패키지를 지정하거나 `.cargo/config.toml` 에 정의된 별칭을 쓴다.

```bash
cargo run -p game_server    # 또는 별칭: cargo gs
cargo run -p login_server   # 또는 별칭: cargo ls  (TCP 9000, AdminApi 9010)
cargo run -p dummy_client   # 또는 별칭: cargo dc
cargo dc -- --seed          # 테스트 계정 user_00001.. 생성 후 종료
```

별칭 뒤에 붙인 인자는 그대로 전달된다 (예: `cargo gs --release`).

### 설정

`config.example.toml` 을 `config.toml` 로 복사해 MySQL/Redis 주소를 채운다. `config.toml` 은 gitignore 대상이다.
환경변수로도 덮어쓸 수 있다 (`APP_DATABASE__MYSQL_URL=...`, 중첩은 `__`).

MySQL/Redis 는 지연 연결이라 없어도 서버는 뜬다. 이때 로그인은 `ErrorResponse { error_code: 3 }`,
`/api/health` 는 `degraded` 를 돌려준다.

### LoginServer 실행

```bash
cargo ls                                  # 게임 클라이언트 TCP 9000 + AdminApi HTTP 9010
RUST_LOG=login_server=debug cargo ls      # 세션 접속/종료 등 상세 로그
```

설정은 `config.toml` 의 `[login_server]` / `[login_server.admin_api]` (생략 시 기본값). 환경변수 예: `APP_LOGIN_SERVER__PORT=9100`,
`APP_LOGIN_SERVER__ADMIN_API__ENABLED=false`. `auth_token_prefix` 는 GameServer 와 같아야 한다.

정상 기동 로그:

```
INFO login_server::admin: AdminApi 시작: http://localhost:9010/api/health
INFO login_server: LoginServer Start Listen Port:9000...
INFO db::broadcast: Redis Pub/Sub 구독 시작 channel=server:notice
INFO net::monitor: [모니터] 동접: 0명 | CPU: 0.0% | ...   # 10초마다
```

TLS 인증서를 지정하지 않으면 `AdminApi TLS 미설정 — HTTP 로 연다` 경고가 함께 나온다 (로컬 개발에서는 정상).

- 클라이언트 흐름: 접속 → `ConnectedResponse` → `LoginRequest { user_id, password_hash }` → `LoginResponse { auth_token }`.
  토큰은 Redis `auth:token:{token}` 에 `"{account_id}:{user_id}"` 로 60초 저장되고, 클라이언트는 이 토큰으로 GameServer 에 붙는다.
- `LoginResponse.error_code`: 0=성공, 1=계정 없음/비밀번호 불일치, 2=밴, 3=서버 오류(토큰 저장 실패), 4=시도 횟수 초과(1분 5회).
  계정 조회 등 DB 오류는 `ErrorResponse { error_code: 3 }` 로 따로 온다.
- 인증 전에는 `LoginRequest`/`KeepAliveRequest` 외 메시지를 받으면 끊고, 인증 후 KeepAlive 가 10초 없으면 끊는다.

#### AdminApi (9010)

`/api/health`, `/api/auth/login` 외에는 로그인으로 받은 키를 `X-Session-Key` 헤더에 넣어야 한다.
관리자 계정은 `[login_server.admin_api].admins` 에 있는 `accounts.user_id` 만 가능하다.

```bash
curl http://127.0.0.1:9010/api/health                    # {"status":"ok","db":"ok","redis":"ok"}

KEY=$(curl -s -X POST http://127.0.0.1:9010/api/auth/login \
  -H 'Content-Type: application/json' \
  -d '{"userId":"user_00001","password":"Test1234!"}' | sed 's/.*"sessionKey":"\([^"]*\)".*/\1/')

curl -H "X-Session-Key: $KEY" http://127.0.0.1:9010/api/sessions          # 접속 세션 목록
curl -H "X-Session-Key: $KEY" http://127.0.0.1:9010/api/stats             # 동접·CPU·메모리
curl -H "X-Session-Key: $KEY" 'http://127.0.0.1:9010/api/scores/top?limit=10'
curl -H "X-Session-Key: $KEY" 'http://127.0.0.1:9010/api/scores?accountId=1&limit=50'
curl -X POST -H "X-Session-Key: $KEY" -H 'Content-Type: application/json' \
  -d '{"message":"점검 공지"}' http://127.0.0.1:9010/api/notice            # 모든 서버 로그에 [공지 수신]
curl -X POST -H "X-Session-Key: $KEY" http://127.0.0.1:9010/api/sessions/1/disconnect
curl -X POST -H "X-Session-Key: $KEY" http://127.0.0.1:9010/api/auth/logout
```

`/api/sessions`, `/disconnect` 는 **LoginServer 에 접속한 세션**만 대상으로 한다 (GameServer 세션은 보이지 않는다).

| 로그 | 의미 |
|---|---|
| `포트 9000 바인드 실패` / `AdminApi 포트 9010 바인드 실패` | 이미 LoginServer(또는 C# LoginServer)가 떠 있다 |
| `유효하지 않은 메시지 크기: 17735` | 9000 에 HTTP 요청이 들어왔다 — 아래 "자동 포트 포워딩 끄기" 참고 |
| `계정 조회 실패` / `last_login_at 갱신 실패` | MySQL 연결 문제 → 클라이언트는 `ErrorResponse { error_code: 3 }` |
| `인증 토큰 발급 실패` | Redis 연결 문제 → `LoginResponse { error_code: 3 }` |
| `중복 로그인, 기존 세션 종료` | 같은 계정이 다시 로그인해 이전 세션을 끊었다 |

### GameServer 실행

```bash
cargo gs                                  # TCP 9001
RUST_LOG=game_server=debug cargo gs       # 세션 접속/종료 등 상세 로그
```

`config.toml` 의 `[game_server]` 섹션은 생략해도 된다 (기본값 `port = 9001`, `auth_token_prefix = "auth:token:"`).
환경변수로는 `APP_GAME_SERVER__PORT=9101` 처럼 바꾼다. `auth_token_prefix` 는 LoginServer 와 같아야 한다.

정상 기동 로그:

```
INFO game_server: GameServer Start Listen Port:9001...
INFO db::broadcast: Redis Pub/Sub 구독 시작 channel=server:notice
INFO net::monitor: [모니터] 동접: 5명 | CPU: 0.0% | 메모리: 14MB | 수신: 20패킷/10s | 송신: 0패킷/10s   # 10초마다
```

- 클라이언트 흐름: 접속 → `ConnectedResponse` → `GameConnectRequest { auth_token }` → `GameConnectResponse`.
  토큰은 LoginServer 가 Redis `auth:token:*` 에 넣은 1회용 값이며, GameServer 가 인증하면서 지운다 (TTL 60초).
- 인증 후 KeepAlive 가 10초 없으면 `KeepAlive 타임아웃 세션 종료`, 같은 계정이 다시 들어오면 기존 세션을 끊는다.
- 연결 종료 시 `accounts.last_login_at` 을 갱신하고, `GameOverReport` 는 `scores` 에 저장한다.
- `Ctrl+C` 로 종료하면 `Stop GameServer` 후 모든 세션을 닫는다.

| 로그 | 의미 |
|---|---|
| `포트 9001 바인드 실패` | 이미 GameServer(또는 C# GameServer)가 떠 있다 |
| `인증 전 허용되지 않은 메시지, 세션 종료` | 인증 전 `GameConnectRequest`/`KeepAliveRequest` 외 메시지 — 원본과 같은 차단 |
| `인증 토큰 조회 실패` | Redis 연결 문제 → 클라이언트는 `ErrorResponse { error_code: 3 }` |
| `로그아웃 기록 실패` | MySQL 연결 문제 — 500ms 부터 최대 30초 간격으로 재시도 |

### dummy_client 로 접속 확인

`cargo ls`(9000)와 `cargo gs`(9001)를 각각 띄워 둔 채 다른 터미널에서 실행한다. 테스트 계정이 없으면 먼저 `cargo dc -- --seed`.

```bash
cargo ls                                        # 터미널 1
cargo gs                                        # 터미널 2
APP_DUMMY_CLIENT__MAX_CLIENT_COUNT=5 cargo dc   # 터미널 3 — 기본 10000명이라 처음엔 줄여서
```

- 각 클라이언트는 LoginServer(9000) 로그인 → 받은 토큰으로 GameServer(9001) 접속 순서로 진행한다.
- 첫 번째 클라이언트(`user_00001`)는 키보드로 조종한다: **W/A/S/D·방향키** 이동, **Q** 또는 **Ctrl+C** 종료.
- 접속 대상은 `[dummy_client]` 의 `login_server` / `game_server`, 또는 `APP_DUMMY_CLIENT__LOGIN_SERVER` / `APP_DUMMY_CLIENT__GAME_SERVER` 로 바꾼다.

로그 판단 (로그인 성공 자체는 로그를 남기지 않는다):

| 로그 | 의미 |
|---|---|
| 경고 없이 `[모니터] 로그인서버: 0명 \| 게임서버: 5명 \| 연결끊김: 0명` | **정상**. 로그인·게임서버 인증 모두 성공, KeepAlive 유지 중 (`cargo gs` 쪽은 `[모니터] 동접: 5명`) |
| `게임서버 접속 실패 ... Connection refused (os error 111)` | **로그인은 성공**. 9001 에 GameServer 가 떠 있지 않다 |
| `[GameConnectResponse] 게임서버 연결 실패 error_code=1` | 토큰 무효 — 60초 TTL 만료이거나 두 서버의 `auth_token_prefix`/Redis 가 다름 |
| `[LoginResponse] 로그인 실패 error_code=…` | 계정 없음/비밀번호 불일치 → `--seed` 여부, `dummy_client.password` 확인 |
| `[오류] 로그인서버 접속 실패, 접속 중단` | `cargo ls` 미실행 또는 `login_server` 주소 오류 |

서버 쪽 흔적: 토큰은 GameServer 가 인증하면서 지우므로 `redis-cli --scan --pattern 'auth:token:*'` 가 비어 있으면
정상이다 (GameServer 없이 돌렸다면 TTL 60초 동안 남는다). `accounts.last_login_at` 은 로그인 시와 GameServer 연결 종료 시 갱신된다.

#### Windows 의 C# GameServer 에 붙일 때

Rust GameServer 와 비교하려면 Windows 에서 C# GameServer 를 띄워 붙인다 (이때는 `cargo gs` 를 띄우지 않는다).
C# GameServer → WSL 의 Redis/MySQL 은 `localhost` 로 되지만, **WSL → Windows 는 WSL2 기본(NAT) 모드에서
`127.0.0.1` 로 닿지 않는다.** 둘 중 하나로 해결한다.

1. **미러 네트워킹 (권장)** — Windows `%UserProfile%\.wslconfig` 에 아래를 넣고 PowerShell 에서 `wsl --shutdown` 후 재시작.
   이후 `127.0.0.1:9001` 이 그대로 동작한다.
   ```ini
   [wsl2]
   networkingMode=mirrored
   ```
2. **Windows 호스트 IP 지정** — Windows 방화벽에서 9001 인바운드 허용이 필요할 수 있다.
   ```bash
   APP_DUMMY_CLIENT__GAME_SERVER=$(ip route show default | awk '{print $3}'):9001 \
   APP_DUMMY_CLIENT__MAX_CLIENT_COUNT=5 cargo dc
   ```

GameServer 에 붙으면 `게임서버 접속 실패` 경고가 사라지고 `user_00001` 을 키보드로 움직일 수 있다.

### VS Code

`.vscode/tasks.json` 에 태스크가 등록되어 있다.

- **Ctrl+Shift+B**: 기본 빌드 태스크 `cargo build --workspace` 실행, 에러는 문제(Problems) 패널에 표시
- `cargo run (game_server)`, `cargo clippy`: `Ctrl+Shift+P` → **Tasks: Run Task** 에서 선택
- `cargo test`: **Tasks: Run Test Task**

#### 자동 포트 포워딩 끄기

WSL 에서 VS Code 를 쓰면 새로 열린 포트를 자동 감지해 포워딩하면서 HTTP 로 한 번 찔러본다.
그러면 `cargo ls` 직후 LoginServer(9000) 에 다음 경고가 찍힌다.

```
WARN login_server::session: 수신 오류, 세션 종료 session_id=1 error=유효하지 않은 메시지 크기: 17735 (허용 범위: 1~8190)
```

`17735` = `0x4547` 이 `"GET"` 의 앞 두 바이트(`G`,`E`)를 길이 헤더(u16 LE)로 읽은 값이다. 서버 버그는 아니지만 로그가 섞이므로 끈다.

- **전부 끄기**: `Ctrl+,` → `autoForwardPorts` 검색 → **Remote: Auto Forward Ports** 해제
  (WSL 창에서는 **Remote [WSL]** 탭에서 바꾸면 WSL 에만 적용). JSON 으로는
  ```json
  { "remote.autoForwardPorts": false }
  ```
- **게임 TCP 포트만 제외**: `.vscode/settings.json` 에
  ```json
  {
    "remote.portsAttributes": {
      "9000": { "onAutoForward": "ignore" },
      "9001": { "onAutoForward": "ignore" }
    }
  }
  ```

이미 포워딩된 포트는 하단 **PORTS** 패널에서 우클릭 → **Stop Forwarding** 후 서버를 다시 띄운다.
WSL2 는 `localhost` 를 Windows 로 전달하므로 꺼도 Windows 쪽 C# 클라이언트 접속에는 영향이 없다.

## 테스트

모든 테스트는 **MySQL/Redis 없이** 돌아간다. 서버는 메모리 backend 를 주입해 실제 TCP 소켓으로 띄운다.

```bash
cargo test --workspace                    # 전체 (약 10초)
cargo test -p login_server                # 크레이트 하나만
cargo test -p game_server --test game_flow                        # 통합 테스트 파일 하나만
cargo test -p game_server keep_alive                              # 이름에 keep_alive 가 들어간 테스트만
cargo test -p login_server --test login_flow -- --nocapture       # println!/eprintln! 출력 보기 (서버 tracing 로그는 테스트에서 초기화하지 않아 나오지 않는다)
cargo clippy --workspace --all-targets    # 린트 (경고 0 유지)
cargo fmt --all                           # 포맷
```

| 크레이트 | 테스트 | 검증 내용 | 소요 |
|---|---|---|---|
| `net` | 단위 (`src/`) | codec 경계(1바이트씩, 한 버퍼 2개, 길이 0·8191 거부), 플러드 밴, 세션 레지스트리 | 즉시 |
| `db` | 단위 (`src/password.rs`) | SHA256·PBKDF2 테스트 벡터, 해시 생성·검증 | 즉시 |
| `proto` | `tests/generated.rs` | 생성 타입의 실제 와이어 바이트 | 즉시 |
| `login_server` | `tests/login_flow.rs` | 로그인 코드 0~4, 인증 전 게이팅, 중복 로그인 킥, DB/Redis 오류 구분, KeepAlive 타임아웃 | 약 4초 |
| `login_server` | `tests/admin_api.rs` | 세션 키 요구, 관리자 로그인, DB/Redis 없을 때 `degraded` | 약 10초 (연결 타임아웃 대기) |
| `game_server` | 단위 + `tests/game_flow.rs` | 토큰 1회용·`:` 포함 user_id, Redis 오류 → `ErrorResponse 3`, 이동 에코, 점수 저장, 로그아웃 재시도, KeepAlive | 약 4초 |
| `dummy_client` | `tests/end_to_end.rs` | 실제 LoginServer + GameServer(토큰 저장소 공유)로 로그인 → 인증 → KeepAlive → 종료 집계 | 약 5초 |

크레이트별 테스트 설명은 각 `crates/*/README.md` 의 "테스트" 절에 있다.

### 실제 MySQL/Redis 로 확인

자동 테스트는 메모리 backend 만 쓰므로, 실제 저장소와 C# 호환성은 서버를 띄워서 확인한다.

```bash
cargo ls                                        # 터미널 1
cargo gs                                        # 터미널 2
APP_DUMMY_CLIENT__MAX_CLIENT_COUNT=5 cargo dc   # 터미널 3 → [모니터] 게임서버: 5명 | 연결끊김: 0명 이면 정상
```

아직 남은 인수 검증(C# DummyClient → Rust GameServer 등)은 [`docs/README.md`](docs/README.md) 를 본다.

### 테스트 작성 시 주의

- **KeepAlive 처럼 시간이 걸린 동작은 실제 시간 + 짧은 타임아웃**(`SessionContext::with_keep_alive_timeout`)으로 검사한다.
  `#[tokio::test(start_paused = true)]` 정지 시계는 실제 소켓 I/O·`spawn_blocking` 을 기다리는 동안에도 시간을 건너뛰어
  타임아웃이 먼저 터진다. 소켓이 없는 순수 로직(예: 로그아웃 재시도 백오프)에만 쓴다.
- 자체 인코더로 왕복하는 테스트는 C# 호환성을 증명하지 못한다. 와이어 바이트는 직접 만든 바이트로 검사한다.
