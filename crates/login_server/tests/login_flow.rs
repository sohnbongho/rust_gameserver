//! 실제 TCP 로 LoginServer 를 띄워 C# 과 같은 동작을 하는지 확인한다. MySQL/Redis 대신 메모리 backend 를 쓴다.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures::future::BoxFuture;
use futures::{SinkExt, StreamExt};
use login_server::backend::{AccountRow, Backend, BackendError};
use login_server::session::SessionContext;
use net::MessageCodec;
use proto::message_wrapper::Payload;
use proto::{
    ConnectedResponse, ErrorResponse, KeepAliveRequest, LoginRequest, LoginResponse,
    MessageWrapper, MoveRequest,
};
use tokio::io::AsyncWriteExt;
use tokio::net::{TcpListener, TcpStream};
use tokio_util::codec::Framed;
use tokio_util::sync::CancellationToken;

const PASSWORD: &str = "Test1234!";

#[derive(Default)]
struct FakeBackend {
    accounts: HashMap<String, AccountRow>,
    tokens: Mutex<Vec<(String, String)>>,
    touched: Mutex<Vec<u64>>,
    fail_db: bool,
    fail_token: bool,
}

impl FakeBackend {
    fn with_accounts() -> Self {
        let (hash, salt) = db::password::generate_stored_hash(&db::password::client_hash(PASSWORD));
        let mut accounts = HashMap::new();
        for (account_id, user_id, status) in
            [(1, "user_00001", 0), (2, "user_00002", 0), (3, "banned", 1)]
        {
            accounts.insert(
                user_id.to_owned(),
                AccountRow {
                    account_id,
                    user_id: user_id.to_owned(),
                    password_hash: hash.clone(),
                    salt: salt.clone(),
                    status,
                },
            );
        }
        Self {
            accounts,
            ..Default::default()
        }
    }
}

impl Backend for FakeBackend {
    fn find_account<'a>(
        &'a self,
        user_id: &'a str,
    ) -> BoxFuture<'a, Result<Option<AccountRow>, BackendError>> {
        Box::pin(async move {
            if self.fail_db {
                return Err(BackendError::Sql(db::sqlx::Error::PoolTimedOut));
            }
            Ok(self.accounts.get(user_id).cloned())
        })
    }

    fn touch_last_login(&self, account_id: u64) -> BoxFuture<'_, Result<(), BackendError>> {
        self.touched.lock().unwrap().push(account_id);
        Box::pin(async { Ok(()) })
    }

    fn issue_auth_token<'a>(
        &'a self,
        account_id: u64,
        user_id: &'a str,
    ) -> BoxFuture<'a, Result<Option<String>, BackendError>> {
        Box::pin(async move {
            if self.fail_token {
                return Ok(None);
            }
            let token = format!("{:032x}", self.tokens.lock().unwrap().len() + 1);
            self.tokens
                .lock()
                .unwrap()
                .push((token.clone(), format!("{account_id}:{user_id}")));
            Ok(Some(token))
        })
    }
}

struct TestServer {
    addr: SocketAddr,
    backend: Arc<FakeBackend>,
    ctx: Arc<SessionContext>,
    shutdown: CancellationToken,
}

impl Drop for TestServer {
    fn drop(&mut self) {
        self.shutdown.cancel();
    }
}

async fn start(backend: FakeBackend) -> TestServer {
    start_with(backend, |ctx| ctx).await
}

async fn start_with(
    backend: FakeBackend,
    configure: impl FnOnce(SessionContext) -> SessionContext,
) -> TestServer {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let backend = Arc::new(backend);
    let ctx = Arc::new(configure(SessionContext::new(backend.clone())));
    let shutdown = CancellationToken::new();
    tokio::spawn(login_server::serve(listener, ctx.clone(), shutdown.clone()));
    TestServer {
        addr,
        backend,
        ctx,
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
    let next = tokio::time::timeout(Duration::from_secs(20), client.next())
        .await
        .expect("응답 대기 시간 초과");
    match next {
        Some(Ok(message)) => message.payload,
        Some(Err(_)) | None => None,
    }
}

fn login_request(user_id: &str, password: &str) -> Payload {
    Payload::LoginRequest(LoginRequest {
        user_id: user_id.to_owned(),
        password_hash: db::password::client_hash(password).to_vec(),
    })
}

fn login_failure(error_code: i32) -> Option<Payload> {
    Some(Payload::LoginResponse(LoginResponse {
        success: false,
        error_code,
        auth_token: String::new(),
    }))
}

async fn login_ok(client: &mut Client, user_id: &str) -> String {
    send(client, login_request(user_id, PASSWORD)).await;
    match recv(client).await {
        Some(Payload::LoginResponse(LoginResponse {
            success: true,
            error_code: 0,
            auth_token,
        })) => auth_token,
        other => panic!("로그인 성공 응답이 아님: {other:?}"),
    }
}

#[tokio::test]
async fn login_success_issues_token() {
    let server = start(FakeBackend::with_accounts()).await;
    let mut client = connect(server.addr).await;

    let token = login_ok(&mut client, "user_00001").await;
    assert_eq!(token.len(), 32);
    assert!(
        token
            .chars()
            .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase())
    );
    assert_eq!(
        *server.backend.tokens.lock().unwrap(),
        [(token, "1:user_00001".to_owned())]
    );
    assert_eq!(*server.backend.touched.lock().unwrap(), [1]);

    let sessions = server.ctx.registry.snapshot();
    assert_eq!(sessions.len(), 1);
    assert_eq!(sessions[0].user_id.as_deref(), Some("user_00001"));
}

#[tokio::test]
async fn login_failures_use_original_error_codes() {
    let server = start(FakeBackend::with_accounts()).await;
    let mut client = connect(server.addr).await;

    send(&mut client, login_request("user_00001", "wrong")).await;
    assert_eq!(recv(&mut client).await, login_failure(1));

    send(&mut client, login_request("nobody", PASSWORD)).await;
    assert_eq!(recv(&mut client).await, login_failure(1));

    // 밴 계정: 비밀번호가 맞으면 2, 틀리면 1 (검증이 밴 확인보다 먼저)
    send(&mut client, login_request("banned", PASSWORD)).await;
    assert_eq!(recv(&mut client).await, login_failure(2));
    send(&mut client, login_request("banned", "wrong")).await;
    assert_eq!(recv(&mut client).await, login_failure(1));

    send(
        &mut client,
        Payload::LoginRequest(LoginRequest {
            user_id: String::new(),
            password_hash: vec![1],
        }),
    )
    .await;
    assert_eq!(recv(&mut client).await, login_failure(1));

    // 1분에 5회 — 6번째는 횟수 제한
    send(&mut client, login_request("user_00001", PASSWORD)).await;
    assert_eq!(recv(&mut client).await, login_failure(4));
    assert!(server.backend.touched.lock().unwrap().is_empty());
}

#[tokio::test]
async fn db_error_sends_error_response_and_token_failure_sends_server_error() {
    let server = start(FakeBackend {
        fail_db: true,
        ..FakeBackend::with_accounts()
    })
    .await;
    let mut client = connect(server.addr).await;
    send(&mut client, login_request("user_00001", PASSWORD)).await;
    assert_eq!(
        recv(&mut client).await,
        Some(Payload::ErrorResponse(ErrorResponse { error_code: 3 }))
    );

    let server = start(FakeBackend {
        fail_token: true,
        ..FakeBackend::with_accounts()
    })
    .await;
    let mut client = connect(server.addr).await;
    send(&mut client, login_request("user_00001", PASSWORD)).await;
    assert_eq!(recv(&mut client).await, login_failure(3));
    assert!(!server.ctx.registry.snapshot()[0].is_authenticated());
}

#[tokio::test]
async fn unauthenticated_disallowed_message_closes_connection() {
    let server = start(FakeBackend::with_accounts()).await;
    let mut client = connect(server.addr).await;

    // KeepAlive 는 인증 전에도 허용
    send(&mut client, Payload::KeepAliveRequest(KeepAliveRequest {})).await;
    send(
        &mut client,
        Payload::MoveRequest(MoveRequest { x: 1.0, y: 2.0 }),
    )
    .await;
    assert_eq!(recv(&mut client).await, None);
}

#[tokio::test]
async fn authenticated_unhandled_message_is_ignored() {
    let server = start(FakeBackend::with_accounts()).await;
    let mut client = connect(server.addr).await;
    login_ok(&mut client, "user_00001").await;

    send(
        &mut client,
        Payload::MoveRequest(MoveRequest { x: 1.0, y: 2.0 }),
    )
    .await;
    // 연결이 살아있음을 확인: 이미 인증된 세션의 재로그인은 1
    send(&mut client, login_request("user_00001", PASSWORD)).await;
    assert_eq!(recv(&mut client).await, login_failure(1));
}

#[tokio::test]
async fn invalid_frame_length_closes_connection() {
    let server = start(FakeBackend::with_accounts()).await;
    let mut client = connect(server.addr).await;
    client.get_mut().write_all(&[0x00, 0x00]).await.unwrap();
    assert_eq!(recv(&mut client).await, None);
}

#[tokio::test]
async fn duplicate_login_kicks_previous_session() {
    let server = start(FakeBackend::with_accounts()).await;
    let mut first = connect(server.addr).await;
    login_ok(&mut first, "user_00001").await;

    let mut second = connect(server.addr).await;
    login_ok(&mut second, "user_00001").await;
    assert_eq!(recv(&mut first).await, None);

    // 다른 사용자는 영향 없음
    let mut other = connect(server.addr).await;
    login_ok(&mut other, "user_00002").await;
    send(&mut second, Payload::KeepAliveRequest(KeepAliveRequest {})).await;
    send(&mut second, login_request("user_00001", PASSWORD)).await;
    assert_eq!(recv(&mut second).await, login_failure(1));
}

#[tokio::test]
async fn admin_disconnect_closes_session() {
    let server = start(FakeBackend::with_accounts()).await;
    let mut client = connect(server.addr).await;
    let session_id = server.ctx.registry.snapshot()[0].session_id;

    assert!(server.ctx.registry.disconnect(session_id));
    assert_eq!(recv(&mut client).await, None);
    assert!(!server.ctx.registry.disconnect(u64::MAX));
}

/// 실제 시간으로 돌리고 타임아웃만 줄인다. 정지 시계(`start_paused`)는 실제 소켓 I/O 나 `spawn_blocking`(PBKDF2)을
/// 기다리는 동안에도 다음 타이머로 시간을 건너뛰어, KeepAlive 가 서버에 읽히기 전에 타임아웃이 터진다.
const TEST_KEEP_ALIVE_TIMEOUT: Duration = Duration::from_millis(500);

#[tokio::test]
async fn keep_alive_timeout_only_after_authentication() {
    let server = start_with(FakeBackend::with_accounts(), |ctx| {
        ctx.with_keep_alive_timeout(TEST_KEEP_ALIVE_TIMEOUT)
    })
    .await;
    let mut client = connect(server.addr).await;

    // 인증 전에는 타임아웃보다 오래 조용해도 끊지 않는다
    tokio::time::sleep(TEST_KEEP_ALIVE_TIMEOUT * 3).await;
    login_ok(&mut client, "user_00001").await;

    // 인증 후 KeepAlive 를 보내는 동안은 타임아웃의 몇 배가 지나도 유지
    for _ in 0..15 {
        tokio::time::sleep(TEST_KEEP_ALIVE_TIMEOUT / 5).await;
        send(&mut client, Payload::KeepAliveRequest(KeepAliveRequest {})).await;
    }
    send(&mut client, login_request("user_00001", PASSWORD)).await;
    assert_eq!(recv(&mut client).await, login_failure(1));

    // 조용해지면 마지막 KeepAlive 로부터 타임아웃 뒤에 끊는다
    let started = tokio::time::Instant::now();
    assert_eq!(recv(&mut client).await, None);
    let elapsed = started.elapsed();
    assert!(
        elapsed >= TEST_KEEP_ALIVE_TIMEOUT * 4 / 5 && elapsed < TEST_KEEP_ALIVE_TIMEOUT * 4,
        "{elapsed:?}"
    );
}
