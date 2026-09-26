//! 접속 1개 = tokio task 1개. task 가 세션 상태를 소유하므로 락이 없다 (C# `UserSession` 액터 대응).
//!
//! 송신은 별도 writer task 로 분리하고 그 사이 큐를 `MAX_SEND_QUEUE_SIZE` 로 제한한다 —
//! 느린 클라이언트가 세션 루프를 막지 않고, 큐가 넘치면 원본처럼 세션을 끊는다.

use std::ops::ControlFlow;
use std::sync::Arc;
use std::time::Duration;

use futures::{SinkExt, StreamExt};
use net::MessageCodec;
use net::consts::{KEEP_ALIVE_TIMEOUT, MAX_MESSAGE_CHANNEL_CAPACITY, MAX_SEND_QUEUE_SIZE};
use proto::message_wrapper::Payload;
use proto::{ConnectedResponse, ErrorResponse, LoginResponse, MessageWrapper};
use tokio::net::TcpStream;
use tokio::net::tcp::OwnedWriteHalf;
use tokio::sync::{Semaphore, mpsc};
use tokio::time::Instant;
use tokio_util::codec::{FramedRead, FramedWrite};
use tokio_util::sync::CancellationToken;

use crate::backend::Backend;
use crate::registry::SessionRegistry;

/// 로그인 시도 제한: 1분에 5회.
const LOGIN_ATTEMPTS_PER_WINDOW: u32 = 5;
const LOGIN_ATTEMPT_WINDOW: Duration = Duration::from_secs(60);
/// 세션 종료 시 남은 송신 큐를 비울 때까지 기다리는 최대 시간.
const WRITER_DRAIN_TIMEOUT: Duration = Duration::from_secs(1);

/// `LoginResponse.error_code` (C# `LoginErrorCode`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(i32)]
pub enum LoginErrorCode {
    Success = 0,
    InvalidCredentials = 1,
    Banned = 2,
    ServerError = 3,
    RateLimited = 4,
}

/// `ErrorResponse.error_code` — DB 오류 (C# `DbErrorHandler`).
const DB_ERROR_CODE: i32 = 3;

/// 모든 세션이 공유하는 서버 자원.
pub struct SessionContext {
    pub backend: Arc<dyn Backend>,
    pub registry: SessionRegistry,
    /// PBKDF2 동시 실행 제한. C# 은 SQL 워커 수(32)가 사실상 상한이었다.
    hash_limiter: Semaphore,
}

impl SessionContext {
    pub fn new(backend: Arc<dyn Backend>) -> Self {
        let permits = std::thread::available_parallelism().map_or(4, |n| n.get());
        Self {
            backend,
            registry: SessionRegistry::default(),
            hash_limiter: Semaphore::new(permits),
        }
    }
}

/// 세션 task 본체. 연결이 끊기거나 `shutdown` 이 취소되면 반환한다.
pub async fn run(stream: TcpStream, ctx: Arc<SessionContext>, shutdown: CancellationToken) {
    let session_id = net::session_id::generate();
    let kick = shutdown.child_token();
    ctx.registry.register(session_id, kick.clone());

    let (read_half, write_half) = stream.into_split();
    let (send_tx, send_rx) = mpsc::channel(MAX_SEND_QUEUE_SIZE);
    let writer = tokio::spawn(write_loop(
        FramedWrite::new(write_half, MessageCodec),
        send_rx,
        kick.clone(),
    ));

    let (inner_tx, inner_rx) = mpsc::channel(MAX_MESSAGE_CHANNEL_CAPACITY);
    let mut session = Session {
        session_id,
        ctx: ctx.clone(),
        send_tx,
        inner_tx,
        account: None,
        last_keep_alive: Instant::now(),
        login_window: None,
        login_attempts: 0,
    };

    session
        .event_loop(FramedRead::new(read_half, MessageCodec), inner_rx, &kick)
        .await;

    ctx.registry.remove(session_id);
    drop(session); // send_tx 를 닫아 writer 가 남은 큐를 비우고 끝나게 한다
    let mut writer = writer;
    if tokio::time::timeout(WRITER_DRAIN_TIMEOUT, &mut writer)
        .await
        .is_err()
    {
        writer.abort();
    }
    tracing::debug!(session_id, "세션 종료");
}

async fn write_loop(
    mut sink: FramedWrite<OwnedWriteHalf, MessageCodec>,
    mut rx: mpsc::Receiver<MessageWrapper>,
    kick: CancellationToken,
) {
    let result: Result<(), net::CodecError> = async {
        while let Some(message) = rx.recv().await {
            sink.feed(message).await?;
            while let Ok(message) = rx.try_recv() {
                sink.feed(message).await?;
            }
            sink.flush().await?;
        }
        Ok(())
    }
    .await;

    if let Err(e) = result {
        tracing::info!(error = %e, "Send Error");
        kick.cancel();
    }
}

#[derive(Debug, Clone)]
struct Account {
    user_id: String,
}

/// 로그인 task 결과를 세션 task 로 돌려보내는 내부 메시지 (C# `LoginResultMessage` / `DbErrorMessage`).
#[derive(Debug)]
enum LoginOutcome {
    Success { account: Account, token: String },
    Failure(LoginErrorCode),
    DbError,
}

struct Session {
    session_id: u64,
    ctx: Arc<SessionContext>,
    send_tx: mpsc::Sender<MessageWrapper>,
    inner_tx: mpsc::Sender<LoginOutcome>,
    account: Option<Account>,
    last_keep_alive: Instant,
    login_window: Option<Instant>,
    login_attempts: u32,
}

/// 송신 큐가 넘쳐 세션을 끊어야 한다.
struct SendQueueFull;

impl Session {
    async fn event_loop(
        &mut self,
        mut reader: FramedRead<tokio::net::tcp::OwnedReadHalf, MessageCodec>,
        mut inner_rx: mpsc::Receiver<LoginOutcome>,
        kick: &CancellationToken,
    ) {
        if self
            .send(Payload::ConnectedResponse(ConnectedResponse { index: 0 }))
            .is_err()
        {
            return;
        }

        loop {
            // KeepAlive 타임아웃은 인증된 세션만 검사한다
            let keep_alive_deadline = self
                .account
                .as_ref()
                .map(|_| self.last_keep_alive + KEEP_ALIVE_TIMEOUT);

            let flow = tokio::select! {
                biased;
                _ = kick.cancelled() => ControlFlow::Break(()),
                frame = reader.next() => match frame {
                    Some(Ok(message)) => self.on_remote(message),
                    Some(Err(e)) => {
                        tracing::warn!(session_id = self.session_id, error = %e, "수신 오류, 세션 종료");
                        ControlFlow::Break(())
                    }
                    None => ControlFlow::Break(()),
                },
                Some(outcome) = inner_rx.recv() => self.on_login_outcome(outcome),
                _ = sleep_until_opt(keep_alive_deadline) => {
                    tracing::warn!(session_id = self.session_id, "KeepAlive 타임아웃 세션 종료");
                    ControlFlow::Break(())
                }
            };

            if flow.is_break() {
                return;
            }
        }
    }

    fn on_remote(&mut self, message: MessageWrapper) -> ControlFlow<()> {
        let authenticated = self.account.is_some();
        match message.payload {
            Some(Payload::LoginRequest(req)) => {
                self.on_login_request(req.user_id, req.password_hash)
            }
            Some(Payload::KeepAliveRequest(_)) => {
                self.last_keep_alive = Instant::now();
                ControlFlow::Continue(())
            }
            // 인증 전에는 위 두 메시지 외에는 즉시 연결 종료
            _ if !authenticated => {
                tracing::warn!(
                    session_id = self.session_id,
                    "인증 전 허용되지 않은 메시지, 세션 종료"
                );
                ControlFlow::Break(())
            }
            // LoginServer 에 핸들러가 없는 메시지는 원본과 같이 조용히 무시한다
            other => {
                tracing::debug!(session_id = self.session_id, payload = ?other, "처리하지 않는 메시지 무시");
                ControlFlow::Continue(())
            }
        }
    }

    fn on_login_request(&mut self, user_id: String, password_hash: Vec<u8>) -> ControlFlow<()> {
        if self.account.is_some() {
            return self.send_login_failure(LoginErrorCode::InvalidCredentials);
        }
        // 원본 순서 그대로: 빈 요청도 시도 횟수를 소모한다
        if !self.try_consume_login_attempt() {
            return self.send_login_failure(LoginErrorCode::RateLimited);
        }
        if user_id.is_empty() || password_hash.is_empty() {
            return self.send_login_failure(LoginErrorCode::InvalidCredentials);
        }

        let ctx = self.ctx.clone();
        let inner_tx = self.inner_tx.clone();
        tokio::spawn(async move {
            let outcome = authenticate(&ctx, &user_id, password_hash).await;
            // 세션이 이미 끝났으면 수신측이 없다 — 버린다
            let _ = inner_tx.send(outcome).await;
        });
        ControlFlow::Continue(())
    }

    fn on_login_outcome(&mut self, outcome: LoginOutcome) -> ControlFlow<()> {
        let (account, token) = match outcome {
            LoginOutcome::Success { account, token } => (account, token),
            LoginOutcome::Failure(code) => return self.send_login_failure(code),
            LoginOutcome::DbError => {
                return self.send_flow(Payload::ErrorResponse(ErrorResponse {
                    error_code: DB_ERROR_CODE,
                }));
            }
        };

        // OnAuthenticated → 응답 순서 (중복 로그인 킥이 응답보다 먼저)
        self.ctx
            .registry
            .authenticate(self.session_id, &account.user_id);
        self.account = Some(account);
        self.last_keep_alive = Instant::now();

        self.send_flow(Payload::LoginResponse(LoginResponse {
            success: true,
            error_code: LoginErrorCode::Success as i32,
            auth_token: token,
        }))
    }

    fn try_consume_login_attempt(&mut self) -> bool {
        let now = Instant::now();
        if self
            .login_window
            .is_none_or(|start| now.duration_since(start) >= LOGIN_ATTEMPT_WINDOW)
        {
            self.login_window = Some(now);
            self.login_attempts = 0;
        }
        self.login_attempts += 1;
        self.login_attempts <= LOGIN_ATTEMPTS_PER_WINDOW
    }

    fn send_login_failure(&self, code: LoginErrorCode) -> ControlFlow<()> {
        self.send_flow(Payload::LoginResponse(LoginResponse {
            success: false,
            error_code: code as i32,
            auth_token: String::new(),
        }))
    }

    fn send_flow(&self, payload: Payload) -> ControlFlow<()> {
        match self.send(payload) {
            Ok(()) => ControlFlow::Continue(()),
            Err(SendQueueFull) => ControlFlow::Break(()),
        }
    }

    fn send(&self, payload: Payload) -> Result<(), SendQueueFull> {
        let message = MessageWrapper {
            message_size: 0,
            payload: Some(payload),
        };
        match self.send_tx.try_send(message) {
            Ok(()) => Ok(()),
            Err(mpsc::error::TrySendError::Full(_)) => {
                tracing::warn!(
                    session_id = self.session_id,
                    max = MAX_SEND_QUEUE_SIZE,
                    "송신 큐 초과, 세션 강제 종료"
                );
                Err(SendQueueFull)
            }
            // writer 가 이미 오류로 끝났다 — kick 이 취소되어 곧 루프가 끝난다
            Err(mpsc::error::TrySendError::Closed(_)) => Err(SendQueueFull),
        }
    }
}

/// 계정 조회 → 비밀번호 검증 → 밴 확인 → `last_login_at` 갱신 (C# `LoginSqlRequest`)
/// → 인증 토큰 발급 (C# `LoginResultHandler.TryIssueAuthToken`).
async fn authenticate(ctx: &SessionContext, user_id: &str, client_hash: Vec<u8>) -> LoginOutcome {
    let row = match ctx.backend.find_account(user_id).await {
        Ok(Some(row)) => row,
        Ok(None) => return LoginOutcome::Failure(LoginErrorCode::InvalidCredentials),
        Err(e) => {
            tracing::error!(user_id, error = %e, "계정 조회 실패");
            return LoginOutcome::DbError;
        }
    };

    let verified = {
        let _permit = ctx
            .hash_limiter
            .acquire()
            .await
            .expect("semaphore 는 닫지 않는다");
        let (hash, salt) = (row.password_hash.clone(), row.salt.clone());
        tokio::task::spawn_blocking(move || db::password::verify(&client_hash, &hash, &salt)).await
    };
    match verified {
        Ok(Ok(true)) => {}
        Ok(Ok(false)) => return LoginOutcome::Failure(LoginErrorCode::InvalidCredentials),
        Ok(Err(e)) => {
            tracing::error!(user_id, error = %e, "저장된 비밀번호 해시 형식 오류");
            return LoginOutcome::DbError;
        }
        Err(e) => {
            tracing::error!(user_id, error = %e, "비밀번호 검증 task 실패");
            return LoginOutcome::DbError;
        }
    }

    // 비밀번호 확인 후에 밴을 본다 — 밴 계정이라도 비밀번호가 틀리면 1
    if row.status == 1 {
        return LoginOutcome::Failure(LoginErrorCode::Banned);
    }

    if let Err(e) = ctx.backend.touch_last_login(row.account_id).await {
        tracing::error!(user_id, error = %e, "last_login_at 갱신 실패");
        return LoginOutcome::DbError;
    }

    // 토큰 발급(Redis)도 세션 루프 밖에서 한다 — Redis 가 느리거나 죽어도 세션은 KeepAlive·킥에 계속 반응한다
    let token = match ctx
        .backend
        .issue_auth_token(row.account_id, &row.user_id)
        .await
    {
        Ok(Some(token)) => token,
        Ok(None) => return LoginOutcome::Failure(LoginErrorCode::ServerError),
        Err(e) => {
            tracing::error!(user_id, error = %e, "인증 토큰 발급 실패");
            return LoginOutcome::Failure(LoginErrorCode::ServerError);
        }
    };

    LoginOutcome::Success {
        account: Account {
            user_id: row.user_id,
        },
        token,
    }
}

async fn sleep_until_opt(deadline: Option<Instant>) {
    match deadline {
        Some(deadline) => tokio::time::sleep_until(deadline).await,
        None => std::future::pending().await,
    }
}
