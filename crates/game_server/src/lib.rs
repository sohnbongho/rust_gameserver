//! GameServer (3단계). C# `GameServer` 를 대체한다.
//!
//! 클라이언트 흐름: 접속 → `ConnectedResponse` → `GameConnectRequest { auth_token }` (LoginServer 가 발급,
//! Redis `auth:token:*` 에서 1회용으로 꺼냄) → `GameConnectResponse` → KeepAlive·이동·월드 입장·점수 보고.

pub mod backend;
pub mod config;
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
