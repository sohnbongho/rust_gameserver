use serde::{Deserialize, Serialize};

/// `config.toml` 에서 GameServer 가 읽는 부분. 다른 섹션(`[login_server]` 등)은 무시한다.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub database: db::DatabaseConfig,
    pub game_server: GameServerConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct GameServerConfig {
    pub port: u16,
    /// LoginServer 와 공유하는 계약 — `[login_server].auth_token_prefix` 와 같아야 한다.
    pub auth_token_prefix: String,
}

impl Default for GameServerConfig {
    fn default() -> Self {
        Self {
            port: 9001,
            auth_token_prefix: "auth:token:".into(),
        }
    }
}
