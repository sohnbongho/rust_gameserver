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

APP_DUMMY_CLIENT__MAX_CLIENT_COUNT=5 cargo dc   # 로그인 확인 (GameServer 가 없으면 이후 "게임서버 접속 실패" 는 정상)
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

### VS Code

`.vscode/tasks.json` 에 태스크가 등록되어 있다.

- **Ctrl+Shift+B**: 기본 빌드 태스크 `cargo build --workspace` 실행, 에러는 문제(Problems) 패널에 표시
- `cargo run (game_server)`, `cargo clippy`: `Ctrl+Shift+P` → **Tasks: Run Task** 에서 선택
- `cargo test`: **Tasks: Run Test Task**
