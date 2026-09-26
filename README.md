# rust_gameserver

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
