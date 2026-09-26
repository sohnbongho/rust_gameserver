//! dummy_client 흐름을 실제 Rust LoginServer + Rust GameServer 에 붙여 확인한다.
//!
//! MySQL/Redis 대신 메모리 backend 를 쓰되, 두 서버가 **토큰 저장소 하나를 공유**한다 (Redis `auth:token:*` 대역).
//! LoginServer 가 넣은 `"{account_id}:{user_id}"` 값을 GameServer 가 꺼내 해석하므로 서버 간 계약도 함께 검증된다.
//! C# 서버와의 조합은 여기서 다루지 않는다.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use dummy_client::client::{self, ClientSettings, Counters, Finished, Phase};
use futures::future::BoxFuture;
use login_server::backend::{AccountRow, Backend as LoginBackend, BackendError as LoginError};
use tokio::net::{TcpListener, TcpStream};
use tokio_util::sync::CancellationToken;

const PASSWORD: &str = "Test1234!";
/// 클라이언트 KeepAlive 주기(3초)보다 조금 긴 GameServer 타임아웃 — KeepAlive 가 없으면 이 시간 안에 끊긴다.
const GAME_KEEP_ALIVE_TIMEOUT: Duration = Duration::from_secs(4);

/// Redis `auth:token:*` 대역. token → `"{account_id}:{user_id}"`
type TokenStore = Arc<Mutex<HashMap<String, String>>>;

struct LoginMemory {
    account: AccountRow,
    tokens: TokenStore,
}

impl LoginBackend for LoginMemory {
    fn find_account<'a>(
        &'a self,
        user_id: &'a str,
    ) -> BoxFuture<'a, Result<Option<AccountRow>, LoginError>> {
        Box::pin(async move { Ok((user_id == self.account.user_id).then(|| self.account.clone())) })
    }

    fn touch_last_login(&self, _account_id: u64) -> BoxFuture<'_, Result<(), LoginError>> {
        Box::pin(async { Ok(()) })
    }

    fn issue_auth_token<'a>(
        &'a self,
        account_id: u64,
        user_id: &'a str,
    ) -> BoxFuture<'a, Result<Option<String>, LoginError>> {
        Box::pin(async move {
            let mut tokens = self.tokens.lock().unwrap();
            let token = format!("{:032x}", tokens.len() + 1);
            // MySqlRedisBackend 와 같은 값 형식
            tokens.insert(token.clone(), format!("{account_id}:{user_id}"));
            Ok(Some(token))
        })
    }
}

#[derive(Default)]
struct GameMemory {
    tokens: TokenStore,
    touched: Mutex<Vec<u64>>,
}

impl game_server::backend::Backend for GameMemory {
    fn take_auth_token<'a>(
        &'a self,
        token: &'a str,
    ) -> BoxFuture<'a, Result<Option<String>, game_server::backend::BackendError>> {
        let value = self.tokens.lock().unwrap().remove(token);
        Box::pin(async move { Ok(value) })
    }

    fn save_score(
        &self,
        _account_id: u64,
        _score: i32,
        _kill_count: i32,
        _survive_seconds: i32,
    ) -> BoxFuture<'_, Result<(), game_server::backend::BackendError>> {
        Box::pin(async { Ok(()) })
    }

    fn touch_last_login(
        &self,
        account_id: u64,
    ) -> BoxFuture<'_, Result<(), game_server::backend::BackendError>> {
        self.touched.lock().unwrap().push(account_id);
        Box::pin(async { Ok(()) })
    }
}

struct Servers {
    login_addr: String,
    game_addr: String,
    tokens: TokenStore,
    game: Arc<GameMemory>,
    game_ctx: Arc<game_server::session::SessionContext>,
    game_shutdown: CancellationToken,
    login_shutdown: CancellationToken,
}

impl Drop for Servers {
    fn drop(&mut self) {
        self.login_shutdown.cancel();
        self.game_shutdown.cancel();
    }
}

async fn start_servers() -> Servers {
    let tokens = TokenStore::default();

    let (password_hash, salt) =
        db::password::generate_stored_hash(&db::password::client_hash(PASSWORD));
    let login_backend = Arc::new(LoginMemory {
        account: AccountRow {
            account_id: 7,
            user_id: client::user_id(1),
            password_hash,
            salt,
            status: 0,
        },
        tokens: tokens.clone(),
    });
    let login_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let login_addr = login_listener.local_addr().unwrap().to_string();
    let login_shutdown = CancellationToken::new();
    tokio::spawn(login_server::serve(
        login_listener,
        Arc::new(login_server::session::SessionContext::new(login_backend)),
        login_shutdown.clone(),
    ));

    let game = Arc::new(GameMemory {
        tokens: tokens.clone(),
        ..Default::default()
    });
    let game_ctx = Arc::new(
        game_server::session::SessionContext::new(game.clone())
            .with_keep_alive_timeout(GAME_KEEP_ALIVE_TIMEOUT),
    );
    let game_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let game_addr = game_listener.local_addr().unwrap().to_string();
    let game_shutdown = CancellationToken::new();
    tokio::spawn(game_server::serve(
        game_listener,
        game_ctx.clone(),
        game_shutdown.clone(),
    ));

    Servers {
        login_addr,
        game_addr,
        tokens,
        game,
        game_ctx,
        game_shutdown,
        login_shutdown,
    }
}

fn settings(game_server: String, password: &str) -> Arc<ClientSettings> {
    Arc::new(ClientSettings {
        game_server,
        client_hash: db::password::client_hash(password),
    })
}

/// 다른 task 가 반영할 때까지 잠시 기다린다.
async fn eventually(mut condition: impl FnMut() -> bool) {
    for _ in 0..250 {
        if condition() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("조건이 충족되지 않았다");
}

#[tokio::test]
async fn login_then_game_server_authentication_and_keep_alive() {
    let servers = start_servers().await;

    let counters = Arc::new(Counters::default());
    let client_shutdown = CancellationToken::new();
    let stream = TcpStream::connect(&servers.login_addr).await.unwrap();
    let client = tokio::spawn(client::run(
        stream,
        1,
        settings(servers.game_addr.clone(), PASSWORD),
        counters.clone(),
        None,
        client_shutdown.clone(),
    ));

    // LoginServer 토큰으로 GameServer 인증까지 끝난다
    eventually(|| {
        servers
            .game_ctx
            .registry
            .snapshot()
            .iter()
            .any(|s| s.user_id.as_deref() == Some("user_00001"))
    })
    .await;
    assert_eq!(counters.snapshot(), (0, 1, 0));
    assert!(
        servers.tokens.lock().unwrap().is_empty(),
        "토큰은 GameServer 가 1회용으로 소비한다"
    );

    // KeepAlive(3초 주기)가 GameServer 타임아웃(4초)보다 오래 세션을 살려 둔다
    tokio::time::sleep(GAME_KEEP_ALIVE_TIMEOUT + Duration::from_millis(500)).await;
    assert!(!client.is_finished());
    assert_eq!(servers.game_ctx.registry.count(), 1);
    assert_eq!(counters.snapshot(), (0, 1, 0));

    // 클라이언트 종료 → GameServer 가 연결 종료를 보고 로그아웃(last_login_at)을 기록한다
    client_shutdown.cancel();
    assert_eq!(client.await.unwrap(), Finished::Shutdown);
    assert_eq!(counters.snapshot(), (0, 0, 1));
    eventually(|| *servers.game.touched.lock().unwrap() == [7]).await;
}

#[tokio::test]
async fn game_server_shutdown_counts_as_game_phase_disconnect() {
    let servers = start_servers().await;

    let counters = Arc::new(Counters::default());
    let stream = TcpStream::connect(&servers.login_addr).await.unwrap();
    let client = tokio::spawn(client::run(
        stream,
        1,
        settings(servers.game_addr.clone(), PASSWORD),
        counters.clone(),
        None,
        CancellationToken::new(),
    ));
    eventually(|| counters.snapshot() == (0, 1, 0) && servers.game_ctx.registry.count() == 1).await;

    // GameServer 가 끊으면 게임서버 단계의 연결 끊김으로 집계
    servers.game_shutdown.cancel();
    let finished = tokio::time::timeout(Duration::from_secs(5), client)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(finished, Finished::Disconnected(Phase::GameServer));
    assert_eq!(counters.snapshot(), (0, 0, 1));
}

#[tokio::test]
async fn game_server_down_counts_as_disconnect() {
    let servers = start_servers().await;
    // GameServer 를 내려 포트가 닫힌 상태로 만든다 (수락 루프가 끝나면 리스너가 닫힌다)
    servers.game_shutdown.cancel();
    for _ in 0..250 {
        if TcpStream::connect(&servers.game_addr).await.is_err() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }

    let counters = Arc::new(Counters::default());
    let stream = TcpStream::connect(&servers.login_addr).await.unwrap();
    let finished = client::run(
        stream,
        1,
        settings(servers.game_addr.clone(), PASSWORD),
        counters.clone(),
        None,
        CancellationToken::new(),
    )
    .await;
    assert_eq!(finished, Finished::GameConnectFailed);
    assert_eq!(counters.snapshot(), (0, 0, 1));
}

#[tokio::test]
async fn login_failure_stays_connected_until_shutdown() {
    let servers = start_servers().await;

    let counters = Arc::new(Counters::default());
    let client_shutdown = CancellationToken::new();
    let stream = TcpStream::connect(&servers.login_addr).await.unwrap();
    let client = tokio::spawn(client::run(
        stream,
        1,
        settings(servers.game_addr.clone(), "wrong password"),
        counters.clone(),
        None,
        client_shutdown.clone(),
    ));

    // 실패 응답을 받은 뒤에도 로그인 서버에 붙어 있다 (원본 동작: 로그만 남기고 대기)
    tokio::time::sleep(Duration::from_secs(1)).await;
    assert!(!client.is_finished());
    assert_eq!(counters.snapshot(), (1, 0, 0));
    assert!(servers.tokens.lock().unwrap().is_empty());
    assert_eq!(servers.game_ctx.registry.count(), 0);

    client_shutdown.cancel();
    assert_eq!(client.await.unwrap(), Finished::Shutdown);
    assert_eq!(counters.snapshot(), (0, 0, 1));
}
