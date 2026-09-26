//! LoginServer + AdminApi (2단계). C# `LoginServer` 를 대체한다.
//!
//! 클라이언트 흐름: 접속 → `ConnectedResponse` → `LoginRequest` → `LoginResponse { auth_token }`
//! → 클라이언트가 그 토큰으로 GameServer 에 접속한다. 두 서버는 Redis `auth:token:*` 로만 통신한다.

pub mod admin;
pub mod backend;
pub mod config;
pub mod registry;
pub mod session;

use std::sync::Arc;

use tokio::net::TcpListener;
use tokio_util::sync::CancellationToken;
use tokio_util::task::TaskTracker;

use crate::session::SessionContext;

/// `shutdown` 이 취소될 때까지 수락하고, 취소되면 모든 세션이 끝날 때까지 기다린다.
pub async fn serve(listener: TcpListener, ctx: Arc<SessionContext>, shutdown: CancellationToken) {
    let sessions = TaskTracker::new();

    net::acceptor::run(listener, shutdown.clone(), |stream, addr| {
        tracing::debug!(%addr, "접속");
        let _ = stream.set_nodelay(true);
        sessions.spawn(session::run(stream, ctx.clone(), shutdown.clone()));
    })
    .await;

    sessions.close();
    sessions.wait().await;
}
