//! GameServer 가 쓰는 저장소 접근. 테스트에서 MySQL/Redis 없이 대체할 수 있도록 trait 으로 분리한다.

use futures::future::BoxFuture;
use sqlx::MySqlPool;

use db::redis::{self, aio::ConnectionManager};
use db::sqlx;

#[derive(Debug, thiserror::Error)]
pub enum BackendError {
    #[error(transparent)]
    Sql(#[from] sqlx::Error),
    #[error(transparent)]
    Redis(#[from] redis::RedisError),
}

pub trait Backend: Send + Sync + 'static {
    /// 1회용 인증 토큰을 원자적으로 꺼낸다 (GET+DEL). 없으면 `Ok(None)`.
    /// 값(`"{account_id}:{user_id}"`)의 해석은 세션이 한다.
    fn take_auth_token<'a>(
        &'a self,
        token: &'a str,
    ) -> BoxFuture<'a, Result<Option<String>, BackendError>>;

    /// C# `SaveScoreSqlRequest`.
    fn save_score(
        &self,
        account_id: u64,
        score: i32,
        kill_count: i32,
        survive_seconds: i32,
    ) -> BoxFuture<'_, Result<(), BackendError>>;

    /// 연결 종료 시 (C# `LogoutSqlRequest`).
    fn touch_last_login(&self, account_id: u64) -> BoxFuture<'_, Result<(), BackendError>>;
}

pub struct MySqlRedisBackend {
    pool: MySqlPool,
    redis: ConnectionManager,
    auth_token_prefix: String,
    get_del: redis::Script,
}

impl MySqlRedisBackend {
    pub fn new(pool: MySqlPool, redis: ConnectionManager, auth_token_prefix: String) -> Self {
        Self {
            pool,
            redis,
            auth_token_prefix,
            // 원본과 같은 Lua — Redis 6.2 미만은 GETDEL 이 없다
            get_del: redis::Script::new(
                "local v = redis.call('GET', KEYS[1])\n\
                 if v then redis.call('DEL', KEYS[1]) end\n\
                 return v",
            ),
        }
    }
}

impl Backend for MySqlRedisBackend {
    fn take_auth_token<'a>(
        &'a self,
        token: &'a str,
    ) -> BoxFuture<'a, Result<Option<String>, BackendError>> {
        Box::pin(async move {
            let key = format!("{}{token}", self.auth_token_prefix);
            let mut conn = self.redis.clone();
            let value: Option<String> = self.get_del.key(key).invoke_async(&mut conn).await?;
            Ok(value)
        })
    }

    fn save_score(
        &self,
        account_id: u64,
        score: i32,
        kill_count: i32,
        survive_seconds: i32,
    ) -> BoxFuture<'_, Result<(), BackendError>> {
        Box::pin(async move {
            // 컬럼은 INT UNSIGNED 지만 원본처럼 int 그대로 바인드한다 — 음수는 DB 오류로 이어진다
            sqlx::query(
                "INSERT INTO scores (account_id, score, kill_count, survive_seconds) VALUES (?, ?, ?, ?)",
            )
            .bind(account_id)
            .bind(score)
            .bind(kill_count)
            .bind(survive_seconds)
            .execute(&self.pool)
            .await?;
            Ok(())
        })
    }

    fn touch_last_login(&self, account_id: u64) -> BoxFuture<'_, Result<(), BackendError>> {
        Box::pin(async move {
            sqlx::query("UPDATE accounts SET last_login_at = UTC_TIMESTAMP() WHERE account_id = ?")
                .bind(account_id)
                .execute(&self.pool)
                .await?;
            Ok(())
        })
    }
}
