# rust_gameserver

## 개발 환경

### 설치한 Claude Code 스킬

```bash
claude plugins install mattpocock-skills
```

TypeScript/개발 관련 스킬 모음(mattpocock-skills)을 플러그인으로 설치해서 사용 중이다.

## 실행

워크스페이스에 실행 파일이 `game_server`, `login_server` 두 개라서 `cargo run` 만으로는 실행 대상이 정해지지 않는다.
패키지를 지정하거나 `.cargo/config.toml` 에 정의된 별칭을 쓴다.

```bash
cargo run -p game_server    # 또는 별칭: cargo gs
cargo run -p login_server   # 또는 별칭: cargo ls
```

별칭 뒤에 붙인 인자는 그대로 전달된다 (예: `cargo gs --release`).

### VS Code

`.vscode/tasks.json` 에 태스크가 등록되어 있다.

- **Ctrl+Shift+B**: 기본 빌드 태스크 `cargo build --workspace` 실행, 에러는 문제(Problems) 패널에 표시
- `cargo run (game_server)`, `cargo clippy`: `Ctrl+Shift+P` → **Tasks: Run Task** 에서 선택
- `cargo test`: **Tasks: Run Test Task**
