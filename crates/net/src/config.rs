//! 설정 로딩: 기본값 ← `config.toml` ← 환경변수 순으로 덮어쓴다.
//!
//! - 파일 경로는 `APP_CONFIG` 환경변수로 바꿀 수 있다 (기본 `config.toml`, 없어도 된다).
//! - 환경변수는 `APP_` 접두사, 중첩은 `__` 로 구분한다. 예: `APP_DATABASE__MYSQL_URL`.
//!
//! 자격증명은 gitignore 된 `config.toml` 또는 환경변수에만 둔다. 커밋 대상은 `config.example.toml` 뿐이다.

use figment::Figment;
use figment::providers::{Env, Format, Serialized, Toml};
use serde::Serialize;
use serde::de::DeserializeOwned;

pub fn load<T>() -> Result<T, Box<figment::Error>>
where
    T: Default + Serialize + DeserializeOwned,
{
    let path = std::env::var("APP_CONFIG").unwrap_or_else(|_| "config.toml".to_owned());
    Figment::from(Serialized::defaults(T::default()))
        .merge(Toml::file(path))
        .merge(Env::prefixed("APP_").ignore(&["config"]).split("__"))
        .extract()
        .map_err(Box::new)
}
