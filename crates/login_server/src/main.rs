use std::sync::Arc;

use anyhow::Context;
use tokio_util::sync::CancellationToken;

use login_server::backend::MySqlRedisBackend;
use login_server::config::Config;
use login_server::session::SessionContext;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    net::init_tracing();
    let config: Config = net::config::load().context("설정 로드 실패")?;
    let server_config = &config.login_server;

    // 둘 다 지연 연결 — DB 가 없어도 기동한다 (원본과 동일)
    let pool = db::mysql_pool(
        &config.database.mysql_url,
        config.database.mysql_max_connections,
    )
    .context("MySQL URL 형식 오류")?;
    let redis_client = db::redis::Client::open(config.database.redis_url.as_str())
        .context("Redis URL 형식 오류")?;
    let redis = db::redis_manager(&redis_client)?;

    let backend = Arc::new(MySqlRedisBackend {
        pool: pool.clone(),
        redis: redis.clone(),
        auth_token_prefix: server_config.auth_token_prefix.clone(),
        auth_token_ttl_seconds: server_config.auth_token_ttl_seconds,
    });
    let ctx = Arc::new(SessionContext::new(backend));
    let shutdown = CancellationToken::new();

    let notice = tokio::spawn(db::broadcast::subscribe(
        redis_client,
        config.database.broadcast_channel.clone(),
        shutdown.clone(),
        |message| tracing::info!("[공지 수신] {message}"),
    ));

    let monitor = tokio::spawn({
        let ctx = ctx.clone();
        net::monitor::run(shutdown.clone(), move || ctx.registry.count())
    });

    let admin = if server_config.admin_api.enabled {
        let state = login_server::admin::AdminState::new(
            server_config.admin_api.clone(),
            config.database.broadcast_channel.clone(),
            pool,
            redis,
            ctx.clone(),
        );
        Some(login_server::admin::start(state, shutdown.clone()).await?)
    } else {
        tracing::info!("AdminApi 비활성화됨");
        None
    };

    let listener = net::acceptor::bind(server_config.port)
        .with_context(|| format!("포트 {} 바인드 실패", server_config.port))?;
    tracing::info!("LoginServer Start Listen Port:{}...", server_config.port);
    let server = tokio::spawn(login_server::serve(listener, ctx, shutdown.clone()));

    tokio::signal::ctrl_c().await?;
    tracing::info!("Stop LoginServer");
    shutdown.cancel();

    server.await?;
    if let Some(admin) = admin {
        admin.await??;
    }
    notice.await?;
    monitor.await?;
    Ok(())
}
