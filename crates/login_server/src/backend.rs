//! 로그인 흐름이 쓰는 저장소 접근. 테스트에서 MySQL/Redis 없이 대체할 수 있도록 trait 으로 분리한다.

use futures::future::BoxFuture;
use redis::AsyncCommands;
use sqlx::MySqlPool;

use db::redis::{self, aio::ConnectionManager};
use db::sqlx;

#[derive(Debug, Clone)]
pub struct AccountRow {
    pub account_id: u64,
    pub user_id: String,
    pub password_hash: String,
    pub salt: String,
    /// 0=정상, 1=밴
    pub status: u8,
}

#[derive(Debug, thiserror::Error)]
pub enum BackendError {
    #[error(transparent)]
    Sql(#[from] sqlx::Error),
    #[error(transparent)]
    Redis(#[from] redis::RedisError),
}

pub trait Backend: Send + Sync + 'static {
    fn find_account<'a>(
        &'a self,
        user_id: &'a str,
    ) -> BoxFuture<'a, Result<Option<AccountRow>, BackendError>>;

    fn touch_last_login(&self, account_id: u64) -> BoxFuture<'_, Result<(), BackendError>>;

    /// GameServer 용 1회용 인증 토큰을 발급한다. 저장에 실패하면 `Ok(None)`.
    fn issue_auth_token<'a>(
        &'a self,
        account_id: u64,
        user_id: &'a str,
    ) -> BoxFuture<'a, Result<Option<String>, BackendError>>;
}

pub struct MySqlRedisBackend {
    pub pool: MySqlPool,
    pub redis: ConnectionManager,
    pub auth_token_prefix: String,
    pub auth_token_ttl_seconds: u64,
}

impl Backend for MySqlRedisBackend {
    fn find_account<'a>(
        &'a self,
        user_id: &'a str,
    ) -> BoxFuture<'a, Result<Option<AccountRow>, BackendError>> {
        Box::pin(async move {
            let row: Option<(u64, String, String, String, u8)> = sqlx::query_as(
                "SELECT account_id, user_id, password_hash, salt, status FROM accounts WHERE user_id = ?",
            )
            .bind(user_id)
            .fetch_optional(&self.pool)
            .await?;

            Ok(row.map(
                |(account_id, user_id, password_hash, salt, status)| AccountRow {
                    account_id,
                    user_id,
                    password_hash,
                    salt,
                    status,
                },
            ))
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

    fn issue_auth_token<'a>(
        &'a self,
        account_id: u64,
        user_id: &'a str,
    ) -> BoxFuture<'a, Result<Option<String>, BackendError>> {
        Box::pin(async move {
            // C# Guid.ToString("N") 과 같은 32자리 소문자 hex
            let token = uuid::Uuid::new_v4().simple().to_string();
            let key = format!("{}{token}", self.auth_token_prefix);
            // GameServer 는 `:` 기준 2개로만 split 한다 (user_id 에 `:` 포함 가능)
            let value = format!("{account_id}:{user_id}");

            let mut conn = self.redis.clone();
            let reply: redis::Value = conn.set_ex(key, value, self.auth_token_ttl_seconds).await?;
            Ok(matches!(reply, redis::Value::Okay).then_some(token))
        })
    }
}
