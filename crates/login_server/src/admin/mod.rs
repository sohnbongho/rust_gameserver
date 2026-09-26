//! 운영용 HTTP API (C# `AdminApi`). 경로·JSON 필드명(camelCase)·상태 코드·오류 본문을 원본과 맞춘다.
//!
//! 원본과 다른 점 (docs/PORTING.md 4절):
//! - TLS 는 `tls_cert_path`/`tls_key_path` 가 설정됐을 때만. 없으면 HTTP 로 연다.
//! - Swagger UI 는 제공하지 않는다.

mod handlers;
mod session_key;

use std::net::{Ipv4Addr, SocketAddr};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::Context;
use axum::Router;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use db::redis::aio::ConnectionManager;
use db::sqlx::MySqlPool;
use net::monitor::ProcessMetrics;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use crate::config::AdminApiConfig;
use crate::session::SessionContext;

pub use session_key::SessionKeyStore;

const GRACEFUL_SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Clone)]
pub struct AdminState(Arc<Inner>);

struct Inner {
    config: AdminApiConfig,
    broadcast_channel: String,
    pool: MySqlPool,
    redis: ConnectionManager,
    sessions: Arc<SessionContext>,
    keys: SessionKeyStore,
    metrics: Mutex<ProcessMetrics>,
    started_at: Instant,
}

impl AdminState {
    pub fn new(
        config: AdminApiConfig,
        broadcast_channel: String,
        pool: MySqlPool,
        redis: ConnectionManager,
        sessions: Arc<SessionContext>,
    ) -> Self {
        let keys = SessionKeyStore::new(
            redis.clone(),
            config.redis_key_prefix.clone(),
            Duration::from_secs(config.session_key_ttl_minutes * 60),
        );
        Self(Arc::new(Inner {
            config,
            broadcast_channel,
            pool,
            redis,
            sessions,
            keys,
            metrics: Mutex::new(ProcessMetrics::default()),
            started_at: Instant::now(),
        }))
    }
}

pub fn router(state: AdminState) -> Router {
    Router::new()
        .route("/api/auth/login", post(handlers::login))
        .route("/api/auth/logout", post(handlers::logout))
        .route("/api/health", get(handlers::health))
        .route("/api/notice", post(handlers::notice))
        .route("/api/scores", get(handlers::scores_by_account))
        .route("/api/scores/top", get(handlers::scores_top))
        .route("/api/sessions", get(handlers::sessions))
        .route(
            "/api/sessions/{session_id}/disconnect",
            post(handlers::disconnect),
        )
        .route("/api/stats", get(handlers::stats))
        .layer(axum::middleware::from_fn_with_state(
            state.clone(),
            session_key::require_session_key,
        ))
        .with_state(state)
}

/// 포트 바인드와 TLS 인증서 로드까지 마친 뒤 서버 task 를 띄운다.
/// 포트 충돌·인증서 오류는 여기서 바로 반환되므로 기동 시점에 드러난다.
pub async fn start(
    state: AdminState,
    shutdown: CancellationToken,
) -> anyhow::Result<JoinHandle<std::io::Result<()>>> {
    let config = state.0.config.clone();
    let addr = SocketAddr::from((Ipv4Addr::UNSPECIFIED, config.port));
    let listener = std::net::TcpListener::bind(addr)
        .with_context(|| format!("AdminApi 포트 {} 바인드 실패", config.port))?;
    listener.set_nonblocking(true)?;
    let app = router(state).into_make_service();

    let handle = axum_server::Handle::new();
    tokio::spawn({
        let handle = handle.clone();
        async move {
            shutdown.cancelled().await;
            handle.graceful_shutdown(Some(GRACEFUL_SHUTDOWN_TIMEOUT));
        }
    });

    if config.tls_cert_path.is_empty() || config.tls_key_path.is_empty() {
        tracing::warn!(
            "AdminApi TLS 미설정 — HTTP 로 연다 (tls_cert_path/tls_key_path 설정 시 HTTPS)"
        );
        let server = axum_server::from_tcp(listener)?.handle(handle);
        tracing::info!("AdminApi 시작: http://localhost:{}/api/health", config.port);
        Ok(tokio::spawn(server.serve(app)))
    } else {
        let tls = axum_server::tls_rustls::RustlsConfig::from_pem_file(
            &config.tls_cert_path,
            &config.tls_key_path,
        )
        .await
        .context("AdminApi TLS 인증서/키 로드 실패")?;
        let server = axum_server::from_tcp_rustls(listener, tls)?.handle(handle);
        tracing::info!(
            "AdminApi 시작: https://localhost:{}/api/health",
            config.port
        );
        Ok(tokio::spawn(server.serve(app)))
    }
}

/// DB/Redis 오류 → 500. 원본의 처리되지 않은 예외와 같은 결과.
pub(crate) struct InternalError(anyhow::Error);

impl<E: Into<anyhow::Error>> From<E> for InternalError {
    fn from(e: E) -> Self {
        Self(e.into())
    }
}

impl IntoResponse for InternalError {
    fn into_response(self) -> Response {
        tracing::error!(error = %self.0, "AdminApi 처리 실패");
        StatusCode::INTERNAL_SERVER_ERROR.into_response()
    }
}
