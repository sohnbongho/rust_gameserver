//! 관리자 세션 키 (C# `SessionKeyStore` + `SessionKeyMiddleware`).

use std::time::Duration;

use axum::extract::{Request, State};
use axum::http::StatusCode;
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use db::redis::{self, AsyncCommands, aio::ConnectionManager};

use super::AdminState;

pub const HEADER_NAME: &str = "X-Session-Key";

/// 세션 키 없이 호출할 수 있는 경로 (대소문자 무시).
const EXEMPT_PATHS: [&str; 2] = ["/api/health", "/api/auth/login"];

pub struct SessionKeyStore {
    redis: ConnectionManager,
    prefix: String,
    ttl: Duration,
}

impl SessionKeyStore {
    pub fn new(redis: ConnectionManager, prefix: String, ttl: Duration) -> Self {
        Self { redis, prefix, ttl }
    }

    pub async fn issue(&self, user_id: &str) -> redis::RedisResult<String> {
        let mut bytes = [0u8; 32];
        getrandom::fill(&mut bytes).expect("OS 난수원을 사용할 수 없다");
        let key = URL_SAFE_NO_PAD.encode(bytes);

        let mut conn = self.redis.clone();
        conn.set_ex::<_, _, ()>(format!("{}{key}", self.prefix), user_id, self.ttl.as_secs())
            .await?;
        Ok(key)
    }

    /// 유효하면 user_id 를 돌려주고 만료 시간을 다시 늘린다 (sliding expiry).
    pub async fn validate(&self, key: &str) -> redis::RedisResult<Option<String>> {
        if key.is_empty() {
            return Ok(None);
        }
        let redis_key = format!("{}{key}", self.prefix);
        let mut conn = self.redis.clone();
        let user_id: Option<String> = conn.get(&redis_key).await?;
        let Some(user_id) = user_id.filter(|u| !u.is_empty()) else {
            return Ok(None);
        };
        conn.expire::<_, ()>(&redis_key, self.ttl.as_secs() as i64)
            .await?;
        Ok(Some(user_id))
    }

    pub async fn revoke(&self, key: &str) -> redis::RedisResult<()> {
        let mut conn = self.redis.clone();
        conn.del(format!("{}{key}", self.prefix)).await
    }
}

pub async fn require_session_key(
    State(state): State<AdminState>,
    request: Request,
    next: Next,
) -> Response {
    let path = request.uri().path();
    if EXEMPT_PATHS.iter().any(|p| p.eq_ignore_ascii_case(path)) {
        return next.run(request).await;
    }

    let Some(key) = request.headers().get(HEADER_NAME) else {
        return (
            StatusCode::UNAUTHORIZED,
            format!("missing {HEADER_NAME} header"),
        )
            .into_response();
    };
    let key = key.to_str().unwrap_or_default().to_owned();

    match state.0.keys.validate(&key).await {
        Ok(Some(_user_id)) => next.run(request).await,
        Ok(None) => (StatusCode::UNAUTHORIZED, "invalid or expired session key").into_response(),
        Err(e) => super::InternalError::from(e).into_response(),
    }
}
