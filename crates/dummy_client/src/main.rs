//! 사용법:
//! - `cargo dc`          — LoginServer/GameServer 에 `max_client_count` 개 접속
//! - `cargo dc -- --seed` — 테스트 계정 생성 후 종료

use std::sync::Arc;
use std::time::Duration;

use anyhow::Context;
use tokio::net::TcpStream;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use tokio_util::task::TaskTracker;
use tracing_subscriber::EnvFilter;

use dummy_client::client::{self, ClientSettings, Counters};
use dummy_client::config::Config;
use dummy_client::keyboard;

const STARTUP_DELAY: Duration = Duration::from_secs(2);
const CONNECT_BATCH_SIZE: usize = 1000;
const CONNECT_BATCH_DELAY: Duration = Duration::from_secs(1);
const MONITOR_INTERVAL: Duration = Duration::from_secs(10);

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let seed = std::env::args().any(|a| a == "--seed");
    let use_keyboard = !seed && keyboard::available();
    init_tracing(use_keyboard);

    let config: Config = net::config::load().context("설정 로드 실패")?;
    let cfg = &config.dummy_client;

    if seed {
        let pool = db::mysql_pool(&config.database.mysql_url, 4).context("MySQL URL 형식 오류")?;
        return dummy_client::seeder::seed(&pool, &cfg.password, cfg.seed_account_count).await;
    }

    // 서버가 먼저 뜰 시간을 준다 (원본 동작)
    tokio::time::sleep(STARTUP_DELAY).await;

    let settings = Arc::new(ClientSettings {
        game_server: cfg.game_server.clone(),
        client_hash: db::password::client_hash(&cfg.password),
    });
    let counters = Arc::new(Counters::default());
    let shutdown = CancellationToken::new();
    let clients = TaskTracker::new();

    tokio::spawn(monitor(counters.clone(), shutdown.clone()));
    tokio::spawn({
        let shutdown = shutdown.clone();
        async move {
            let _ = tokio::signal::ctrl_c().await;
            shutdown.cancel();
        }
    });

    tracing::info!("[DummyClient] 접속 시작 (목표: {}명)", cfg.max_client_count);
    let mut player_moves = None;
    let mut connected = 0usize;

    for i in 0..cfg.max_client_count {
        if shutdown.is_cancelled() {
            break;
        }
        let stream = match TcpStream::connect(&cfg.login_server).await {
            Ok(stream) => stream,
            Err(e) => {
                tracing::error!(login_server = %cfg.login_server, error = %e, "[오류] 로그인서버 접속 실패, 접속 중단");
                break;
            }
        };
        let _ = stream.set_nodelay(true);

        // 첫 번째 클라이언트를 키보드 플레이어로 쓴다
        let moves = if i == 0 && use_keyboard {
            let (tx, rx) = mpsc::channel(64);
            player_moves = Some(tx);
            Some(rx)
        } else {
            None
        };
        clients.spawn(client::run(
            stream,
            i + 1,
            settings.clone(),
            counters.clone(),
            moves,
            shutdown.clone(),
        ));
        connected += 1;

        if connected.is_multiple_of(CONNECT_BATCH_SIZE) || connected == cfg.max_client_count {
            tracing::info!("[동접] {connected}/{}", cfg.max_client_count);
            if connected < cfg.max_client_count {
                tokio::time::sleep(CONNECT_BATCH_DELAY).await;
            }
        }
    }

    let _raw_mode = match player_moves {
        Some(tx) => {
            tracing::info!(
                "[키보드] 플레이어: {} | W/A/S/D 또는 방향키=이동, Q=종료",
                client::user_id(1)
            );
            Some(keyboard::spawn(tx, shutdown.clone())?)
        }
        None => None,
    };

    shutdown.cancelled().await;
    tracing::info!("Stop DummyClient");
    clients.close();
    clients.wait().await;
    Ok(())
}

async fn monitor(counters: Arc<Counters>, shutdown: CancellationToken) {
    loop {
        tokio::select! {
            _ = shutdown.cancelled() => return,
            _ = tokio::time::sleep(MONITOR_INTERVAL) => {}
        }
        let (login, game, disconnected) = counters.snapshot();
        tracing::info!(
            "[모니터] 로그인서버: {login}명 | 게임서버: {game}명 | 연결끊김: {disconnected}명"
        );
    }
}

fn init_tracing(raw_mode: bool) {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    let builder = tracing_subscriber::fmt().with_env_filter(filter);
    if raw_mode {
        builder.with_writer(|| keyboard::CrLfStdout).init();
    } else {
        builder.init();
    }
}
