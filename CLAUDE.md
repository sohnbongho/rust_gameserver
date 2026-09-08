# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## 이 프로젝트의 목적

이 저장소는 **`/home/bhson/00-git/csharp_gameserver` 의 C# MMO 서버를 Rust로 컨버팅**하는 프로젝트다.

| | |
|---|---|
| 원본 (작업 기준) | `/home/bhson/00-git/csharp_gameserver` |
| 원본 원격 저장소 | https://github.com/sohnbongho/csharp_likeactor (폴더명과 다름에 주의) |
| 변환 대상 | 이 저장소 (https://github.com/sohnbongho/rust_gameserver) |

원본 저장소는 **읽기 전용으로 취급한다.** 해당 저장소의 CLAUDE.md가 `git commit` / `git push` 를
금지하고 있으므로, 원본에는 어떤 변경도 가하지 않는다. 참고할 파일(`message.proto`, 스키마 SQL 등)은
이 저장소로 복사하고 출처를 주석으로 남긴다.

## 변환 설계서

**작업 전 `docs/PORTING.md` 를 먼저 읽는다.** 다음 내용이 확정되어 있다:

- C# 구조 → Rust 구조 대응표 (무엇을 유지하고 무엇을 버리는지)
- 변경 불가능한 외부 계약 (와이어 프레이밍, Redis 키, DB 스키마, 핸들러 동작)
- 원본에서 발견된 불일치와 이 포팅에서 채택한 결정
- 워크스페이스 구성 및 검증 계획

## 대원칙

**내부 구조는 Rust답게, 외부 계약은 바이트 단위로 동일하게.**

원본의 Unity 클라이언트(`Client/World`)는 C#으로 그대로 유지되므로 **와이어 호환성은 필수 조건**이다.
반면 C# 쪽 동시성 machinery(객체 풀, 스레드 고정, 리플렉션 디스패치, DB 워커 풀) 상당수는
GC 회피와 `SocketAsyncEventArgs` 대응 코드라 Rust에서 재현하지 않는다.

## 작업 순서

1. **GameServer** — 두 C# 서버는 Redis(`auth:token:*`)로만 통신하므로, Rust GameServer는
   살아있는 C# LoginServer와 그대로 붙는다. 기존 C# `DummyClient`가 수정 없이 인수 테스트가 된다.
2. LoginServer + AdminApi
3. dummy_client (선택)

`Client/`(Unity)는 변환 대상이 아니다.

## 빌드 및 실행

```bash
cargo build
cargo run
```

Rust 툴체인은 rustup으로 설치되어 있다(`~/.cargo`). 새 셸에서는 `~/.bashrc`가 PATH를 잡아준다.

## 자격증명

설정값은 **환경변수 또는 gitignore된 `config.toml`** 에서 읽는다. 커밋하는 것은 `config.example.toml` 뿐이다.
원본 C# 저장소의 `appsettings.json` 에는 MySQL 비밀번호가 평문으로 들어 있으나, 이 저장소에서는 그 방식을 따르지 않는다.
