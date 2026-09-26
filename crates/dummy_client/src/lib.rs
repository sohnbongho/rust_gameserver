//! 부하·인수 테스트용 더미 클라이언트 (C# `DummyClient`).
//!
//! 클라이언트 1개 = tokio task 1개: LoginServer 접속 → 로그인 → 토큰 수신 → 연결 종료 →
//! GameServer 접속 → `GameConnectRequest` → 인증 후 3초마다 KeepAlive.

pub mod client;
pub mod config;
pub mod keyboard;
pub mod seeder;
