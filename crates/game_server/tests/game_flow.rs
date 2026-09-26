//! 실제 TCP 로 GameServer 를 띄워 C# 과 같은 동작을 하는지 확인한다. MySQL/Redis 대신 메모리 backend 를 쓴다.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures::future::BoxFuture;
use futures::{SinkExt, StreamExt};
use game_server::backend::{Backend, BackendError};
use game_server::session::SessionContext;
use net::MessageCodec;
use proto::message_wrapper::Payload;
use proto::{
    ConnectedResponse, EnterWorldRequest, EnterWorldResponse, ErrorResponse, GameConnectRequest,
    GameConnectResponse, GameOverReport, GameOverResponse, KeepAliveRequest, MessageWrapper,
    MoveRequest, MoveResponse,
};
use tokio::net::{TcpListener, TcpStream};
use tokio_util::codec::Framed;
use tokio_util::sync::CancellationToken;

#[derive(Default)]
struct FakeBackend {
    /// token → `"{account_id}:{user_id}"`
    tokens: Mutex<HashMap<String, String>>,
    scores: Mutex<Vec<(u64, i32, i32, i32)>>,
    touched: Mutex<Vec<u64>>,
    fail_redis: bool,
    fail_sql: bool,
}

impl FakeBackend {
    fn with_tokens(tokens: &[(&str, &str)]) -> Self {
        Self {
            tokens: Mutex::new(
                tokens
                    .iter()
                    .map(|&(token, value)| (token.to_owned(), value.to_owned()))
                    .collect(),
            ),
            ..Default::default()
        }
    }
}

fn db_error() -> BackendError {
    BackendError::Sql(db::sqlx::Error::PoolTimedOut)
}

impl Backend for FakeBackend {
    fn take_auth_token<'a>(
        &'a self,
        token: &'a str,
    ) -> BoxFuture<'a, Result<Option<String>, BackendError>> {
        let result = if self.fail_redis {
            Err(db_error())
        } else {
            Ok(self.tokens.lock().unwrap().remove(token))
        };
        Box::pin(async move { result })
    }

    fn save_score(
        &self,
        account_id: u64,
        score: i32,
        kill_count: i32,
        survive_seconds: i32,
    ) -> BoxFuture<'_, Result<(), BackendError>> {
        let result = if self.fail_sql {
            Err(db_error())
        } else {
            self.scores
                .lock()
                .unwrap()
                .push((account_id, score, kill_count, survive_seconds));
            Ok(())
        };
        Box::pin(async move { result })
    }

    fn touch_last_login(&self, account_id: u64) -> BoxFuture<'_, Result<(), BackendError>> {
        self.touched.lock().unwrap().push(account_id);
        Box::pin(async { Ok(()) })
    }
}

struct Server {
    addr: SocketAddr,
    ctx: Arc<SessionContext>,
    backend: Arc<FakeBackend>,
    shutdown: CancellationToken,
}

impl Drop for Server {
    fn drop(&mut self) {
        self.shutdown.cancel();
    }
}

async fn start(backend: FakeBackend) -> Server {
    start_with(backend, |ctx| ctx).await
}

async fn start_with(
    backend: FakeBackend,
    configure: impl FnOnce(SessionContext) -> SessionContext,
) -> Server {
    let backend = Arc::new(backend);
    let ctx = Arc::new(configure(SessionContext::new(backend.clone())));
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let shutdown = CancellationToken::new();
    tokio::spawn(game_server::serve(listener, ctx.clone(), shutdown.clone()));
    Server {
        addr,
        ctx,
        backend,
        shutdown,
    }
}

type Client = Framed<TcpStream, MessageCodec>;

/// 접속 후 첫 메시지 `ConnectedResponse { index: 0 }` 까지 확인한다.
async fn connect(addr: SocketAddr) -> Client {
    let mut client = Framed::new(TcpStream::connect(addr).await.unwrap(), MessageCodec);
    assert_eq!(
        recv(&mut client).await,
        Some(Payload::ConnectedResponse(ConnectedResponse { index: 0 }))
    );
    client
}

async fn send(client: &mut Client, payload: Payload) {
    client
        .send(MessageWrapper {
            message_size: 0,
            payload: Some(payload),
        })
        .await
        .unwrap();
}

/// 다음 메시지. 연결이 닫혔으면 None.
async fn recv(client: &mut Client) -> Option<Payload> {
    let next = tokio::time::timeout(Duration::from_secs(5), client.next())
        .await
        .expect("응답 대기 시간 초과");
    match next {
        Some(Ok(message)) => message.payload,
        Some(Err(_)) | None => None,
    }
}

fn game_connect(token: &str) -> Payload {
    Payload::GameConnectRequest(GameConnectRequest {
        auth_token: token.to_owned(),
    })
}

fn game_connect_response(error_code: i32) -> Option<Payload> {
    Some(Payload::GameConnectResponse(GameConnectResponse {
        success: error_code == 0,
        error_code,
    }))
}

async fn authenticate(client: &mut Client, token: &str) {
    send(client, game_connect(token)).await;
    assert_eq!(recv(client).await, game_connect_response(0));
}

/// 다른 task 가 반영할 때까지 잠시 기다린다.
async fn eventually(mut condition: impl FnMut() -> bool) {
    for _ in 0..100 {
        if condition() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("조건이 충족되지 않았다");
}

#[tokio::test]
async fn token_is_single_use() {
    let server = start(FakeBackend::with_tokens(&[("tok", "1:user_00001")])).await;

    let mut first = connect(server.addr).await;
    authenticate(&mut first, "tok").await;

    let mut second = connect(server.addr).await;
    send(&mut second, game_connect("tok")).await;
    assert_eq!(recv(&mut second).await, game_connect_response(1));
}

#[tokio::test]
async fn user_id_may_contain_colons() {
    let server = start(FakeBackend::with_tokens(&[("tok", "7:user:with:colons")])).await;
    let mut client = connect(server.addr).await;
    authenticate(&mut client, "tok").await;

    let sessions = server.ctx.registry.snapshot();
    assert_eq!(sessions[0].user_id.as_deref(), Some("user:with:colons"));
}

#[tokio::test]
async fn invalid_token_requests() {
    let server = start(FakeBackend::with_tokens(&[
        ("no_colon", "12345"),
        ("bad_id", "abc:user"),
        ("ok", "1:user_00001"),
    ]))
    .await;
    let mut client = connect(server.addr).await;

    for token in ["", "missing", "no_colon", "bad_id"] {
        send(&mut client, game_connect(token)).await;
        assert_eq!(
            recv(&mut client).await,
            game_connect_response(1),
            "{token:?}"
        );
    }

    // 실패한 뒤에도 인증할 수 있고, 인증 후 재요청은 1
    authenticate(&mut client, "ok").await;
    send(&mut client, game_connect("ok")).await;
    assert_eq!(recv(&mut client).await, game_connect_response(1));
}

#[tokio::test]
async fn redis_error_is_error_response_and_keeps_connection() {
    let server = start(FakeBackend {
        fail_redis: true,
        ..Default::default()
    })
    .await;
    let mut client = connect(server.addr).await;

    for _ in 0..2 {
        send(&mut client, game_connect("tok")).await;
        assert_eq!(
            recv(&mut client).await,
            Some(Payload::ErrorResponse(ErrorResponse { error_code: 3 }))
        );
    }
}

#[tokio::test]
async fn gating_before_authentication() {
    let server = start(FakeBackend::default()).await;

    // KeepAlive 는 허용
    let mut client = connect(server.addr).await;
    send(&mut client, Payload::KeepAliveRequest(KeepAliveRequest {})).await;
    send(&mut client, game_connect("missing")).await;
    assert_eq!(recv(&mut client).await, game_connect_response(1));

    // 그 외는 즉시 종료
    for payload in [
        Payload::MoveRequest(MoveRequest { x: 1.0, y: 2.0 }),
        Payload::EnterWorldRequest(EnterWorldRequest { world_id: 1 }),
        Payload::GameOverReport(GameOverReport::default()),
    ] {
        let mut client = connect(server.addr).await;
        send(&mut client, payload).await;
        assert_eq!(recv(&mut client).await, None);
    }
    assert!(server.backend.scores.lock().unwrap().is_empty());
}

#[tokio::test]
async fn move_echoes_to_sender_only() {
    let server = start(FakeBackend::with_tokens(&[
        ("a", "1:user_00001"),
        ("b", "2:user_00002"),
    ]))
    .await;
    let mut a = connect(server.addr).await;
    let mut b = connect(server.addr).await;
    authenticate(&mut a, "a").await;
    authenticate(&mut b, "b").await;

    send(
        &mut a,
        Payload::MoveRequest(MoveRequest { x: 1.5, y: -2.0 }),
    )
    .await;
    assert_eq!(
        recv(&mut a).await,
        Some(Payload::MoveResponse(MoveResponse {
            success: true,
            x: 1.5,
            y: -2.0
        }))
    );

    // b 가 받는 다음 메시지는 자기 요청의 응답이어야 한다 (a 의 이동은 브로드캐스트되지 않는다)
    send(
        &mut b,
        Payload::EnterWorldRequest(EnterWorldRequest { world_id: 3 }),
    )
    .await;
    assert_eq!(
        recv(&mut b).await,
        Some(Payload::EnterWorldResponse(EnterWorldResponse {
            success: true
        }))
    );
}

#[tokio::test]
async fn game_over_saves_score() {
    let server = start(FakeBackend::with_tokens(&[("tok", "5:user_00005")])).await;
    let mut client = connect(server.addr).await;
    authenticate(&mut client, "tok").await;

    let report = GameOverReport {
        score: 1200,
        kill_count: 7,
        survive_seconds: 95,
    };
    send(&mut client, Payload::GameOverReport(report)).await;
    assert_eq!(
        recv(&mut client).await,
        Some(Payload::GameOverResponse(GameOverResponse {
            success: true
        }))
    );
    assert_eq!(*server.backend.scores.lock().unwrap(), [(5, 1200, 7, 95)]);
}

#[tokio::test]
async fn game_over_db_error() {
    let server = start(FakeBackend {
        fail_sql: true,
        ..FakeBackend::with_tokens(&[("tok", "5:user_00005")])
    })
    .await;
    let mut client = connect(server.addr).await;
    authenticate(&mut client, "tok").await;

    send(
        &mut client,
        Payload::GameOverReport(GameOverReport::default()),
    )
    .await;
    assert_eq!(
        recv(&mut client).await,
        Some(Payload::ErrorResponse(ErrorResponse { error_code: 3 }))
    );
}

#[tokio::test]
async fn disconnect_records_logout_only_when_authenticated() {
    let server = start(FakeBackend::with_tokens(&[("tok", "9:user_00009")])).await;

    drop(connect(server.addr).await);
    let mut client = connect(server.addr).await;
    authenticate(&mut client, "tok").await;
    drop(client);

    eventually(|| server.ctx.registry.count() == 0).await;
    eventually(|| !server.backend.touched.lock().unwrap().is_empty()).await;
    assert_eq!(*server.backend.touched.lock().unwrap(), [9]);
}

#[tokio::test]
async fn duplicate_login_kicks_previous_session() {
    let server = start(FakeBackend::with_tokens(&[
        ("first", "1:user_00001"),
        ("second", "1:user_00001"),
    ]))
    .await;
    let mut first = connect(server.addr).await;
    authenticate(&mut first, "first").await;

    let mut second = connect(server.addr).await;
    authenticate(&mut second, "second").await;

    assert_eq!(recv(&mut first).await, None);
    send(
        &mut second,
        Payload::MoveRequest(MoveRequest { x: 0.0, y: 0.0 }),
    )
    .await;
    assert!(matches!(
        recv(&mut second).await,
        Some(Payload::MoveResponse(_))
    ));
}

#[tokio::test]
async fn shutdown_closes_sessions_and_records_logout() {
    let server = start(FakeBackend::with_tokens(&[("tok", "4:user_00004")])).await;
    let mut client = connect(server.addr).await;
    authenticate(&mut client, "tok").await;

    server.shutdown.cancel();
    assert_eq!(recv(&mut client).await, None);
    eventually(|| *server.backend.touched.lock().unwrap() == [4]).await;
}

/// 실제 시간으로 돌리고 타임아웃만 줄인다 (정지 시계는 실제 소켓 I/O 를 기다리는 동안 시간을 건너뛴다).
const TEST_KEEP_ALIVE_TIMEOUT: Duration = Duration::from_millis(500);

#[tokio::test]
async fn keep_alive_timeout_only_after_authentication() {
    let server = start_with(
        FakeBackend::with_tokens(&[("tok", "1:user_00001")]),
        |ctx| ctx.with_keep_alive_timeout(TEST_KEEP_ALIVE_TIMEOUT),
    )
    .await;
    let mut client = connect(server.addr).await;

    // 인증 전에는 타임아웃보다 오래 조용해도 끊지 않는다
    tokio::time::sleep(TEST_KEEP_ALIVE_TIMEOUT * 3).await;
    authenticate(&mut client, "tok").await;

    // 인증 후 KeepAlive 를 보내는 동안은 타임아웃의 몇 배가 지나도 유지
    for _ in 0..15 {
        tokio::time::sleep(TEST_KEEP_ALIVE_TIMEOUT / 5).await;
        send(&mut client, Payload::KeepAliveRequest(KeepAliveRequest {})).await;
    }
    send(
        &mut client,
        Payload::MoveRequest(MoveRequest { x: 0.0, y: 0.0 }),
    )
    .await;
    assert!(matches!(
        recv(&mut client).await,
        Some(Payload::MoveResponse(_))
    ));

    // 조용해지면 마지막 KeepAlive 로부터 타임아웃 뒤에 끊고, 로그아웃을 기록한다
    let started = tokio::time::Instant::now();
    assert_eq!(recv(&mut client).await, None);
    let elapsed = started.elapsed();
    assert!(
        elapsed >= TEST_KEEP_ALIVE_TIMEOUT * 4 / 5 && elapsed < TEST_KEEP_ALIVE_TIMEOUT * 4,
        "{elapsed:?}"
    );
    eventually(|| *server.backend.touched.lock().unwrap() == [1]).await;
}
