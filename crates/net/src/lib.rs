//! 서버 공통 네트워크 계층: 프레이밍 codec, 세션 id, 설정, tracing 초기화, 패킷 통계, 접속 제한, 모니터.

pub mod acceptor;
pub mod codec;
pub mod config;
pub mod consts;
pub mod monitor;
pub mod session_id;
pub mod stats;

pub use codec::{CodecError, MessageCodec};

/// `RUST_LOG` 가 없으면 `info` 레벨로 tracing 을 초기화한다.
pub fn init_tracing() {
    use tracing_subscriber::EnvFilter;

    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();
}
