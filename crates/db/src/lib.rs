//! MySQL(`sqlx::MySqlPool`)과 Redis(`ConnectionManager`) 접근 계층.
//!
//! C# 은 `AbortOnConnectFail=false` 와 요청마다 새 `MySqlConnection` 을 열었으므로 서버가 DB 없이도
//! 기동된다. 여기서도 풀/매니저를 **지연 연결**로 만들어 같은 동작을 유지한다.

pub mod broadcast;
pub mod password;

use std::time::Duration;

use redis::aio::{ConnectionManager, ConnectionManagerConfig};
use sqlx::mysql::{MySqlPool, MySqlPoolOptions};

pub use redis;
pub use sqlx;

/// 연결을 미리 맺지 않는 MySQL 풀. 첫 쿼리에서 접속한다.
pub fn mysql_pool(url: &str, max_connections: u32) -> sqlx::Result<MySqlPool> {
    MySqlPoolOptions::new()
        .max_connections(max_connections)
        .acquire_timeout(Duration::from_secs(5))
        .connect_lazy(url)
}

/// 연결을 미리 맺지 않는 Redis 커넥션 매니저. 끊기면 자동 재연결한다.
pub fn redis_manager(client: &redis::Client) -> redis::RedisResult<ConnectionManager> {
    let config = ConnectionManagerConfig::new()
        .set_connection_timeout(Some(Duration::from_secs(5)))
        .set_response_timeout(Some(Duration::from_secs(5)));
    ConnectionManager::new_lazy_with_config(client.clone(), config)
}

pub async fn check_mysql(pool: &MySqlPool) -> bool {
    sqlx::query("SELECT 1").execute(pool).await.is_ok()
}

pub async fn check_redis(redis: &ConnectionManager) -> bool {
    let mut conn = redis.clone();
    redis::cmd("PING")
        .query_async::<String>(&mut conn)
        .await
        .is_ok()
}

/// `config.toml` 의 `[database]` 섹션. 모든 바이너리가 공유한다.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct DatabaseConfig {
    /// sqlx URL 형식: `mysql://user:password@host:3306/gamedb` (C# 연결 문자열 형식이 아님)
    pub mysql_url: String,
    pub mysql_max_connections: u32,
    pub redis_url: String,
    pub broadcast_channel: String,
}

impl Default for DatabaseConfig {
    fn default() -> Self {
        Self {
            mysql_url: "mysql://root@127.0.0.1:3306/gamedb".into(),
            mysql_max_connections: 32,
            redis_url: "redis://127.0.0.1:6379".into(),
            broadcast_channel: "server:notice".into(),
        }
    }
}
