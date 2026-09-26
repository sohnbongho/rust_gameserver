//! AdminApi 중 DB/Redis 에 닿기 전에 결정되는 응답(인증 미들웨어, 입력 검증, health degraded)을 확인한다.
//! MySQL/Redis 는 닫힌 포트를 가리키는 지연 연결이라 실제로 접속하지 않거나 즉시 실패한다.

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use futures::future::BoxFuture;
use login_server::admin::{AdminState, router};
use login_server::backend::{AccountRow, Backend, BackendError};
use login_server::config::AdminApiConfig;
use login_server::session::SessionContext;
use tower::ServiceExt;

struct NoBackend;

impl Backend for NoBackend {
    fn find_account<'a>(
        &'a self,
        _: &'a str,
    ) -> BoxFuture<'a, Result<Option<AccountRow>, BackendError>> {
        Box::pin(async { Ok(None) })
    }
    fn touch_last_login(&self, _: u64) -> BoxFuture<'_, Result<(), BackendError>> {
        Box::pin(async { Ok(()) })
    }
    fn issue_auth_token<'a>(
        &'a self,
        _: u64,
        _: &'a str,
    ) -> BoxFuture<'a, Result<Option<String>, BackendError>> {
        Box::pin(async { Ok(None) })
    }
}

fn app() -> axum::Router {
    let pool = db::mysql_pool("mysql://root@127.0.0.1:1/gamedb", 1).unwrap();
    let redis_client = db::redis::Client::open("redis://127.0.0.1:1").unwrap();
    let redis = db::redis_manager(&redis_client).unwrap();
    let config = AdminApiConfig {
        admins: vec!["user_00001".into()],
        ..Default::default()
    };
    let sessions = Arc::new(SessionContext::new(Arc::new(NoBackend)));
    router(AdminState::new(
        config,
        "server:notice".into(),
        pool,
        redis,
        sessions,
    ))
}

async fn call(request: Request<Body>) -> (StatusCode, String) {
    let response = app().oneshot(request).await.unwrap();
    let status = response.status();
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    (status, String::from_utf8(body.to_vec()).unwrap())
}

fn post_json(uri: &str, json: &str) -> Request<Body> {
    Request::post(uri)
        .header("content-type", "application/json")
        .body(Body::from(json.to_owned()))
        .unwrap()
}

#[tokio::test]
async fn protected_routes_require_session_key() {
    let (status, body) = call(Request::get("/api/sessions").body(Body::empty()).unwrap()).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(body, "missing X-Session-Key header");

    let (status, body) = call(
        Request::post("/api/sessions/1/disconnect")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(
        (status, body.as_str()),
        (StatusCode::UNAUTHORIZED, "missing X-Session-Key header")
    );

    let (status, _) = call(
        Request::post("/api/auth/logout")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn admin_login_validation() {
    let (status, body) = call(post_json(
        "/api/auth/login",
        r#"{"userId":" ","password":"x"}"#,
    ))
    .await;
    assert_eq!(
        (status, body.as_str()),
        (StatusCode::BAD_REQUEST, "userId and password required")
    );

    let (status, body) = call(post_json("/api/auth/login", r#"{"userId":"user_00001"}"#)).await;
    assert_eq!(
        (status, body.as_str()),
        (StatusCode::BAD_REQUEST, "userId and password required")
    );

    // PascalCase 도 받는다 (ASP.NET 대소문자 무시 바인딩)
    let (status, body) = call(post_json(
        "/api/auth/login",
        r#"{"UserId":"user_00002","Password":"x"}"#,
    ))
    .await;
    assert_eq!(
        (status, body.as_str()),
        (StatusCode::UNAUTHORIZED, "not an admin account")
    );
}

#[tokio::test]
async fn health_reports_degraded_when_backends_down() {
    let (status, body) = call(Request::get("/api/health").body(Body::empty()).unwrap()).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, r#"{"status":"degraded","db":"down","redis":"down"}"#);
}
