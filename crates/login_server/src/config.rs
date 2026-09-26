use serde::{Deserialize, Serialize};

/// `config.toml` 에서 LoginServer 가 읽는 부분. 다른 섹션(`[game_server]` 등)은 무시한다.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub database: db::DatabaseConfig,
    pub login_server: LoginServerConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct LoginServerConfig {
    pub port: u16,
    /// GameServer 와 공유하는 계약 — 바꾸면 GameServer 설정도 같이 바꿔야 한다.
    pub auth_token_prefix: String,
    /// 원본 `appsettings.json` 의 실제 값 60초 (docs/PORTING.md 4절).
    pub auth_token_ttl_seconds: u64,
    pub admin_api: AdminApiConfig,
}

impl Default for LoginServerConfig {
    fn default() -> Self {
        Self {
            port: 9000,
            auth_token_prefix: "auth:token:".into(),
            auth_token_ttl_seconds: 60,
            admin_api: AdminApiConfig::default(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct AdminApiConfig {
    pub enabled: bool,
    pub port: u16,
    /// 관리자 로그인이 허용되는 `accounts.user_id` 목록.
    pub admins: Vec<String>,
    pub session_key_ttl_minutes: u64,
    pub redis_key_prefix: String,
    /// PEM 인증서/키 경로. 둘 다 있으면 HTTPS, 없으면 HTTP 로 연다 (docs/PORTING.md 4절).
    pub tls_cert_path: String,
    pub tls_key_path: String,
}

impl Default for AdminApiConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            port: 9010,
            admins: Vec::new(),
            session_key_ttl_minutes: 60,
            redis_key_prefix: "admin:session:".into(),
            tls_cert_path: String::new(),
            tls_key_path: String::new(),
        }
    }
}
