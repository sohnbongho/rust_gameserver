//! AdminApi 엔드포인트 (C# `Controllers/*`). JSON 은 ASP.NET 기본값과 같은 camelCase.

use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use chrono::NaiveDateTime;
use db::sqlx;
use serde::{Deserialize, Serialize};

use super::session_key::HEADER_NAME;
use super::{AdminState, InternalError};

type Result<T> = std::result::Result<T, InternalError>;

// ASP.NET(System.Text.Json 웹 기본값)은 요청 JSON 속성명을 대소문자 무시로 바인딩한다 — PascalCase 도 받는다.
#[derive(Debug, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct LoginRequestDto {
    #[serde(alias = "UserId")]
    user_id: String,
    #[serde(alias = "Password")]
    password: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LoginResponseDto {
    session_key: String,
    expires_in_minutes: u64,
}

pub async fn login(
    State(state): State<AdminState>,
    Json(req): Json<LoginRequestDto>,
) -> Result<Response> {
    let state = &state.0;
    if req.user_id.trim().is_empty() || req.password.is_empty() {
        return Ok((StatusCode::BAD_REQUEST, "userId and password required").into_response());
    }
    if !state.config.admins.contains(&req.user_id) {
        return Ok((StatusCode::UNAUTHORIZED, "not an admin account").into_response());
    }

    // 원본과 같이 status(밴)는 보지 않는다
    let row: Option<(String, String)> =
        sqlx::query_as("SELECT password_hash, salt FROM accounts WHERE user_id = ?")
            .bind(&req.user_id)
            .fetch_optional(&state.pool)
            .await?;
    let Some((stored_hash, salt)) = row else {
        return Ok((StatusCode::UNAUTHORIZED, "invalid credentials").into_response());
    };

    let client_hash = db::password::client_hash(&req.password);
    let verified = tokio::task::spawn_blocking(move || {
        db::password::verify(&client_hash, &stored_hash, &salt)
    })
    .await??;
    if !verified {
        return Ok((StatusCode::UNAUTHORIZED, "invalid credentials").into_response());
    }

    let session_key = state.keys.issue(&req.user_id).await?;
    Ok(Json(LoginResponseDto {
        session_key,
        expires_in_minutes: state.config.session_key_ttl_minutes,
    })
    .into_response())
}

pub async fn logout(State(state): State<AdminState>, headers: HeaderMap) -> Result<StatusCode> {
    if let Some(key) = headers.get(HEADER_NAME).and_then(|v| v.to_str().ok()) {
        state.0.keys.revoke(key).await?;
    }
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Debug, Serialize)]
pub struct HealthDto {
    status: &'static str,
    db: &'static str,
    redis: &'static str,
}

pub async fn health(State(state): State<AdminState>) -> Json<HealthDto> {
    let (db_ok, redis_ok) = tokio::join!(
        db::check_mysql(&state.0.pool),
        db::check_redis(&state.0.redis)
    );
    let up_down = |ok: bool| if ok { "ok" } else { "down" };
    Json(HealthDto {
        status: if db_ok && redis_ok { "ok" } else { "degraded" },
        db: up_down(db_ok),
        redis: up_down(redis_ok),
    })
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct NoticeRequestDto {
    #[serde(alias = "Message")]
    message: String,
}

pub async fn notice(
    State(state): State<AdminState>,
    Json(req): Json<NoticeRequestDto>,
) -> Result<Response> {
    if req.message.trim().is_empty() {
        return Ok((StatusCode::BAD_REQUEST, "message required").into_response());
    }
    db::broadcast::publish(&state.0.redis, &state.0.broadcast_channel, &req.message).await?;
    Ok(StatusCode::NO_CONTENT.into_response())
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ScoreDto {
    score_id: u64,
    account_id: u64,
    score: u32,
    kill_count: u32,
    survive_seconds: u32,
    /// `2026-05-01T12:34:56` — DB DATETIME 을 타임존 없이 그대로 (ASP.NET 의 Unspecified DateTime 직렬화와 같다)
    played_at: NaiveDateTime,
}

type ScoreRow = (u64, u64, u32, u32, u32, NaiveDateTime);

impl From<ScoreRow> for ScoreDto {
    fn from(
        (score_id, account_id, score, kill_count, survive_seconds, played_at): ScoreRow,
    ) -> Self {
        Self {
            score_id,
            account_id,
            score,
            kill_count,
            survive_seconds,
            played_at,
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ScoresQuery {
    #[serde(default)]
    account_id: u64,
    limit: Option<i64>,
}

pub async fn scores_by_account(
    State(state): State<AdminState>,
    Query(q): Query<ScoresQuery>,
) -> Result<Json<Vec<ScoreDto>>> {
    let limit = q.limit.unwrap_or(50).clamp(1, 200);
    let rows: Vec<ScoreRow> = sqlx::query_as(
        "SELECT score_id, account_id, score, kill_count, survive_seconds, played_at \
         FROM scores WHERE account_id = ? ORDER BY played_at DESC LIMIT ?",
    )
    .bind(q.account_id)
    .bind(limit)
    .fetch_all(&state.0.pool)
    .await?;
    Ok(Json(rows.into_iter().map(ScoreDto::from).collect()))
}

#[derive(Debug, Deserialize)]
pub struct TopQuery {
    limit: Option<i64>,
}

pub async fn scores_top(
    State(state): State<AdminState>,
    Query(q): Query<TopQuery>,
) -> Result<Json<Vec<ScoreDto>>> {
    let limit = q.limit.unwrap_or(10).clamp(1, 100);
    let rows: Vec<ScoreRow> = sqlx::query_as(
        "SELECT score_id, account_id, score, kill_count, survive_seconds, played_at \
         FROM scores ORDER BY score DESC LIMIT ?",
    )
    .bind(limit)
    .fetch_all(&state.0.pool)
    .await?;
    Ok(Json(rows.into_iter().map(ScoreDto::from).collect()))
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionDto {
    session_id: u64,
    user_id: Option<String>,
    /// LoginServer 에는 월드 개념이 없어 원본도 항상 0
    world_id: u64,
    is_authenticated: bool,
}

pub async fn sessions(State(state): State<AdminState>) -> Json<Vec<SessionDto>> {
    let list = state
        .0
        .sessions
        .registry
        .snapshot()
        .into_iter()
        .map(|s| SessionDto {
            session_id: s.session_id,
            is_authenticated: s.is_authenticated(),
            user_id: s.user_id,
            world_id: 0,
        })
        .collect();
    Json(list)
}

pub async fn disconnect(
    State(state): State<AdminState>,
    Path(session_id): Path<u64>,
) -> StatusCode {
    if state.0.sessions.registry.disconnect(session_id) {
        StatusCode::NO_CONTENT
    } else {
        StatusCode::NOT_FOUND
    }
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StatsDto {
    active_sessions: usize,
    packets_received_total: u64,
    packets_sent_total: u64,
    cpu_percent: f64,
    memory_mb: f64,
    uptime_seconds: u64,
}

pub async fn stats(State(state): State<AdminState>) -> Json<StatsDto> {
    let state = &state.0;
    let (cpu, memory_mb) = state.metrics.lock().unwrap().sample();
    let (received, sent) = net::stats::snapshot();
    Json(StatsDto {
        active_sessions: state.sessions.registry.count(),
        packets_received_total: received,
        packets_sent_total: sent,
        cpu_percent: (cpu * 100.0).round() / 100.0,
        memory_mb: (memory_mb * 10.0).round() / 10.0,
        uptime_seconds: state.started_at.elapsed().as_secs(),
    })
}
