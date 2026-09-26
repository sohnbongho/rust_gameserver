//! dummy_client 흐름을 Rust LoginServer(메모리 backend) + 가짜 GameServer 에 붙여 확인한다.
//!
//! 가짜 GameServer 는 C# GameServer 의 핸드셰이크(ConnectedResponse → GameConnectRequest →
//! GameConnectResponse)만 흉내 낸다. 실제 GameServer 와의 인수 테스트를 대신하지는 않는다.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use dummy_client::client::{self, ClientSettings, Counters, Finished, Phase};
use futures::future::BoxFuture;
use futures::{SinkExt, StreamExt};
use login_server::backend::{AccountRow, Backend, BackendError};
use login_server::session::SessionContext;
use net::MessageCodec;
use proto::message_wrapper::Payload;
use proto::{ConnectedResponse, GameConnectResponse, MessageWrapper};
use tokio::net::{TcpListener, TcpStream};
use tokio_util::codec::Framed;
use tokio_util::sync::CancellationToken;

const PASSWORD: &str = "Test1234!";

struct MemoryBackend {
    account: AccountRow,
    tokens: Mutex<Vec<String>>,
}

impl Backend for MemoryBackend {
    fn find_account<'a>(
        &'a self,
        user_id: &'a str,
    ) -> BoxFuture<'a, Result<Option<AccountRow>, BackendError>> {
        Box::pin(async move { Ok((user_id == self.account.user_id).then(|| self.account.clone())) })
    }

    fn touch_last_login(&self, _account_id: u64) -> BoxFuture<'_, Result<(), BackendError>> {
        Box::pin(async { Ok(()) })
    }

    fn issue_auth_token<'a>(
        &'a self,
        _account_id: u64,
        _user_id: &'a str,
    ) -> BoxFuture<'a, Result<Option<String>, BackendError>> {
        Box::pin(async move {
            let token = "0123456789abcdef0123456789abcdef".to_owned();
            self.tokens.lock().unwrap().push(token.clone());
            Ok(Some(token))
        })
    }
}

async fn start_login_server(shutdown: &CancellationToken) -> (String, Arc<MemoryBackend>) {
    let (password_hash, salt) =
        db::password::generate_stored_hash(&db::password::client_hash(PASSWORD));
    let backend = Arc::new(MemoryBackend {
        account: AccountRow {
            account_id: 1,
            user_id: client::user_id(1),
            password_hash,
            salt,
            status: 0,
        },
        tokens: Mutex::default(),
    });
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap().to_string();
    let ctx = Arc::new(SessionContext::new(backend.clone()));
    tokio::spawn(login_server::serve(listener, ctx, shutdown.clone()));
    (addr, backend)
}

fn settings(game_server: String, password: &str) -> Arc<ClientSettings> {
    Arc::new(ClientSettings {
        game_server,
        client_hash: db::password::client_hash(password),
    })
}

async fn send(conn: &mut Framed<TcpStream, MessageCodec>, payload: Payload) {
    conn.send(MessageWrapper {
        message_size: 0,
        payload: Some(payload),
    })
    .await
    .unwrap();
}

async fn recv(conn: &mut Framed<TcpStream, MessageCodec>) -> Payload {
    let message = tokio::time::timeout(Duration::from_secs(10), conn.next())
        .await
        .expect("대기 시간 초과");
    message.unwrap().unwrap().payload.unwrap()
}

#[tokio::test]
async fn login_then_game_server_handshake_and_keep_alive() {
    let shutdown = CancellationToken::new();
    let (login_addr, backend) = start_login_server(&shutdown).await;

    let game = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let game_addr = game.local_addr().unwrap().to_string();

    let counters = Arc::new(Counters::default());
    let stream = TcpStream::connect(&login_addr).await.unwrap();
    let client = tokio::spawn(client::run(
        stream,
        1,
        settings(game_addr, PASSWORD),
        counters.clone(),
        None,
        shutdown.clone(),
    ));

    // 가짜 GameServer
    let (socket, _) = tokio::time::timeout(Duration::from_secs(10), game.accept())
        .await
        .unwrap()
        .unwrap();
    let mut conn = Framed::new(socket, MessageCodec);
    assert_eq!(counters.snapshot(), (0, 1, 0));

    send(
        &mut conn,
        Payload::ConnectedResponse(ConnectedResponse { index: 0 }),
    )
    .await;
    let Payload::GameConnectRequest(request) = recv(&mut conn).await else {
        panic!("GameConnectRequest 가 와야 한다");
    };
    assert_eq!(*backend.tokens.lock().unwrap(), [request.auth_token]);

    send(
        &mut conn,
        Payload::GameConnectResponse(GameConnectResponse {
            success: true,
            error_code: 0,
        }),
    )
    .await;
    // 인증 후 3초 주기 KeepAlive
    let started = tokio::time::Instant::now();
    assert!(matches!(
        recv(&mut conn).await,
        Payload::KeepAliveRequest(_)
    ));
    let elapsed = started.elapsed();
    assert!(
        elapsed >= Duration::from_millis(2900) && elapsed < Duration::from_secs(4),
        "{elapsed:?}"
    );

    // GameServer 가 끊으면 게임서버 단계의 연결 끊김으로 집계
    drop(conn);
    let finished = tokio::time::timeout(Duration::from_secs(5), client)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(finished, Finished::Disconnected(Phase::GameServer));
    assert_eq!(counters.snapshot(), (0, 0, 1));
    shutdown.cancel();
}

#[tokio::test]
async fn login_failure_stays_connected_until_shutdown() {
    let shutdown = CancellationToken::new();
    let (login_addr, backend) = start_login_server(&shutdown).await;

    let counters = Arc::new(Counters::default());
    let client_shutdown = CancellationToken::new();
    let stream = TcpStream::connect(&login_addr).await.unwrap();
    let client = tokio::spawn(client::run(
        stream,
        1,
        settings("127.0.0.1:1".into(), "wrong password"),
        counters.clone(),
        None,
        client_shutdown.clone(),
    ));

    // 실패 응답을 받은 뒤에도 로그인 서버에 붙어 있다 (원본 동작: 로그만 남기고 대기)
    tokio::time::sleep(Duration::from_secs(1)).await;
    assert!(!client.is_finished());
    assert_eq!(counters.snapshot(), (1, 0, 0));
    assert!(backend.tokens.lock().unwrap().is_empty());

    client_shutdown.cancel();
    assert_eq!(client.await.unwrap(), Finished::Shutdown);
    assert_eq!(counters.snapshot(), (0, 0, 1));
    shutdown.cancel();
}
