use serde::{Deserialize, Serialize};

/// `config.toml` 에서 dummy_client 가 읽는 부분.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub database: db::DatabaseConfig,
    pub dummy_client: DummyClientConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct DummyClientConfig {
    /// `host:port`
    pub login_server: String,
    /// `host:port`
    pub game_server: String,
    pub max_client_count: usize,
    /// 시드 계정과 로그인에 공통으로 쓰는 비밀번호 (원본은 로그인 쪽이 `Test1234!` 하드코딩)
    pub password: String,
    /// `--seed` 로 만들 계정 수 (`user_00001` ..)
    pub seed_account_count: usize,
}

impl Default for DummyClientConfig {
    fn default() -> Self {
        Self {
            login_server: "127.0.0.1:9000".into(),
            game_server: "127.0.0.1:9001".into(),
            max_client_count: 10_000,
            password: "Test1234!".into(),
            seed_account_count: 10_000,
        }
    }
}
