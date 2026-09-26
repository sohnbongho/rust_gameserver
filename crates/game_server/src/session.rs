//! 접속 1개 = tokio task 1개. task 가 세션 상태를 소유하므로 락이 없다 (C# `UserSession` 액터 대응).
//! 송신은 `net::outbox` 의 writer task 가 맡는다.
//!
//! Redis·MySQL 작업은 별도 task 에서 돌리고 결과를 내부 채널로 세션에 돌려보낸다
//! (C# `CacheWorker`/`SqlWorker` → `EnqueueMessageAsync` 대응). 응답 송신은 항상 세션 task 가 한다.

use std::ops::ControlFlow;
use std::sync::Arc;
use std::time::Duration;

use futures::StreamExt;
use net::MessageCodec;
use net::consts::{KEEP_ALIVE_TIMEOUT, MAX_MESSAGE_CHANNEL_CAPACITY};
use net::outbox::{Outbox, sleep_until_opt};
use net::registry::SessionRegistry;
use proto::message_wrapper::Payload;
use proto::{
    ConnectedResponse, EnterWorldResponse, ErrorResponse, GameConnectResponse, GameOverReport,
    GameOverResponse, MessageWrapper, MoveResponse,
};
use tokio::net::TcpStream;
use tokio::net::tcp::OwnedReadHalf;
use tokio::sync::mpsc;
use tokio::time::Instant;
use tokio_util::codec::FramedRead;
use tokio_util::sync::CancellationToken;

use crate::backend::Backend;

/// `ErrorResponse.error_code` — DB/Redis 오류 (C# `DbErrorHandler`).
const DB_ERROR_CODE: i32 = 3;

/// 연결 종료 시 `last_login_at` 갱신 재시도 (C# `SqlWorker` 의 critical 요청 재시도, `RetryBaseDelayMs`/`RetryMaxDelayMs`).
const LOGOUT_RETRY_BASE_DELAY: Duration = Duration::from_millis(500);
const LOGOUT_RETRY_MAX_DELAY: Duration = Duration::from_secs(30);

/// `GameConnectResponse.error_code` (C# `GameConnectErrorCode`).
///
/// `ServerError = 2` 는 원본에서 캐시 워커 채널이 가득 찼을 때만 쓰인다. 이 포팅에는 그 채널이 없으므로
/// 보내지 않는다 — Redis 오류는 원본과 같이 `ErrorResponse { error_code: 3 }` 이다.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(i32)]
pub enum GameConnectErrorCode {
    Success = 0,
    InvalidToken = 1,
}

/// 모든 세션이 공유하는 서버 자원.
pub struct SessionContext {
    pub backend: Arc<dyn Backend>,
    pub registry: SessionRegistry,
    /// 인증 후 KeepAlive 타임아웃. 계약값은 `KEEP_ALIVE_TIMEOUT`(10초) — 테스트만 `with_keep_alive_timeout` 으로 줄인다.
    keep_alive_timeout: Duration,
}

impl SessionContext {
    pub fn new(backend: Arc<dyn Backend>) -> Self {
        Self {
            backend,
            registry: SessionRegistry::default(),
            keep_alive_timeout: KEEP_ALIVE_TIMEOUT,
        }
    }

    pub fn with_keep_alive_timeout(mut self, timeout: Duration) -> Self {
        self.keep_alive_timeout = timeout;
        self
    }
}

/// 세션 task 본체. 연결이 끊기거나 `shutdown` 이 취소되면 (로그아웃 기록까지 마치고) 반환한다.
pub async fn run(stream: TcpStream, ctx: Arc<SessionContext>, shutdown: CancellationToken) {
    let session_id = net::session_id::generate();
    let kick = shutdown.child_token();
    ctx.registry.register(session_id, kick.clone());

    let (read_half, write_half) = stream.into_split();
    let (outbox, writer) = net::outbox::spawn(session_id, write_half, kick.clone());

    let (inner_tx, inner_rx) = mpsc::channel(MAX_MESSAGE_CHANNEL_CAPACITY);
    let mut session = Session {
        session_id,
        ctx: ctx.clone(),
        outbox,
        inner_tx,
        account: None,
        last_keep_alive: Instant::now(),
        world_id: 0,
        position: (0.0, 0.0),
    };

    session
        .event_loop(FramedRead::new(read_half, MessageCodec), inner_rx, &kick)
        .await;

    ctx.registry.remove(session_id);
    let Session {
        outbox,
        account,
        world_id,
        position,
        ..
    } = session;
    writer.close(outbox).await;
    tracing::debug!(session_id, world_id, ?position, "세션 종료");

    if let Some(account) = account {
        record_logout(ctx.backend.as_ref(), account.account_id, &shutdown).await;
    }
}

/// `UPDATE accounts SET last_login_at` 을 성공할 때까지 지수 백오프로 재시도한다.
/// 서버 종료 중이면 첫 시도만 하고 그만둔다 (원본과 같이 종료를 막지 않는다).
pub(crate) async fn record_logout(
    backend: &dyn Backend,
    account_id: u64,
    shutdown: &CancellationToken,
) {
    let mut attempt = 0u32;
    loop {
        let Err(e) = backend.touch_last_login(account_id).await else {
            return;
        };
        tracing::error!(account_id, attempt, error = %e, "로그아웃 기록 실패");

        let delay = (LOGOUT_RETRY_BASE_DELAY * (1 << attempt.min(10))).min(LOGOUT_RETRY_MAX_DELAY);
        tokio::select! {
            _ = shutdown.cancelled() => {
                tracing::warn!(account_id, "서버 종료로 중요 DB 요청 재시도 중단");
                return;
            }
            _ = tokio::time::sleep(delay) => {}
        }
        attempt += 1;
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Account {
    account_id: u64,
    user_id: String,
}

/// Redis `auth:token:*` 값 `"{account_id}:{user_id}"` 를 해석한다. `:` 기준 2개로만 나눈다 (user_id 에 `:` 허용).
fn parse_token_value(value: &str) -> Option<Account> {
    let (account_id, user_id) = value.split_once(':')?;
    Some(Account {
        account_id: account_id.parse().ok()?,
        user_id: user_id.to_owned(),
    })
}

/// DB/Redis task 결과를 세션 task 로 돌려보내는 내부 메시지
/// (C# `GameConnectResultMessage` / `RemoteSendMessage` / `DbErrorMessage`).
#[derive(Debug)]
enum InnerMessage {
    /// 토큰 조회 결과. 없으면 `None`.
    TokenTaken(Option<String>),
    ScoreSaved,
    DbError,
}

struct Session {
    session_id: u64,
    ctx: Arc<SessionContext>,
    outbox: Outbox,
    inner_tx: mpsc::Sender<InnerMessage>,
    account: Option<Account>,
    last_keep_alive: Instant,
    /// 0 = 로비. 원본도 값만 기록한다 (월드별 동작 없음).
    world_id: u64,
    position: (f32, f32),
}

impl Session {
    async fn event_loop(
        &mut self,
        mut reader: FramedRead<OwnedReadHalf, MessageCodec>,
        mut inner_rx: mpsc::Receiver<InnerMessage>,
        kick: &CancellationToken,
    ) {
        if self
            .outbox
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
                .map(|_| self.last_keep_alive + self.ctx.keep_alive_timeout);

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
                Some(message) = inner_rx.recv() => self.on_inner(message),
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
            Some(Payload::GameConnectRequest(req)) => self.on_game_connect_request(req.auth_token),
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
            // 이동은 발신자에게만 에코한다 (브로드캐스트 없음)
            Some(Payload::MoveRequest(req)) => {
                self.position = (req.x, req.y);
                self.outbox.send_flow(Payload::MoveResponse(MoveResponse {
                    success: true,
                    x: req.x,
                    y: req.y,
                }))
            }
            Some(Payload::EnterWorldRequest(req)) => {
                self.world_id = req.world_id;
                self.outbox
                    .send_flow(Payload::EnterWorldResponse(EnterWorldResponse {
                        success: true,
                    }))
            }
            Some(Payload::GameOverReport(report)) => self.on_game_over_report(report),
            // GameServer 에 핸들러가 없는 메시지는 원본과 같이 조용히 무시한다
            other => {
                tracing::debug!(session_id = self.session_id, payload = ?other, "처리하지 않는 메시지 무시");
                ControlFlow::Continue(())
            }
        }
    }

    fn on_game_connect_request(&mut self, token: String) -> ControlFlow<()> {
        if self.account.is_some() || token.is_empty() {
            return self.send_game_connect(GameConnectErrorCode::InvalidToken);
        }

        let ctx = self.ctx.clone();
        let session_id = self.session_id;
        self.spawn_inner(async move {
            match ctx.backend.take_auth_token(&token).await {
                Ok(value) => InnerMessage::TokenTaken(value),
                Err(e) => {
                    tracing::error!(session_id, error = %e, "인증 토큰 조회 실패");
                    InnerMessage::DbError
                }
            }
        });
        ControlFlow::Continue(())
    }

    fn on_game_over_report(&mut self, report: GameOverReport) -> ControlFlow<()> {
        let Some(account_id) = self.account.as_ref().map(|a| a.account_id) else {
            return ControlFlow::Continue(());
        };

        let ctx = self.ctx.clone();
        self.spawn_inner(async move {
            let saved = ctx
                .backend
                .save_score(
                    account_id,
                    report.score,
                    report.kill_count,
                    report.survive_seconds,
                )
                .await;
            match saved {
                Ok(()) => InnerMessage::ScoreSaved,
                Err(e) => {
                    tracing::error!(account_id, error = %e, "점수 저장 실패");
                    InnerMessage::DbError
                }
            }
        });
        ControlFlow::Continue(())
    }

    fn on_inner(&mut self, message: InnerMessage) -> ControlFlow<()> {
        match message {
            InnerMessage::TokenTaken(value) => match value.as_deref().and_then(parse_token_value) {
                Some(account) => self.on_authenticated(account),
                None => self.send_game_connect(GameConnectErrorCode::InvalidToken),
            },
            InnerMessage::ScoreSaved => {
                self.outbox
                    .send_flow(Payload::GameOverResponse(GameOverResponse {
                        success: true,
                    }))
            }
            InnerMessage::DbError => self.outbox.send_flow(Payload::ErrorResponse(ErrorResponse {
                error_code: DB_ERROR_CODE,
            })),
        }
    }

    fn on_authenticated(&mut self, account: Account) -> ControlFlow<()> {
        // OnAuthenticated → 응답 순서 (중복 로그인 킥이 응답보다 먼저)
        self.ctx
            .registry
            .authenticate(self.session_id, &account.user_id);
        self.account = Some(account);
        self.last_keep_alive = Instant::now();
        self.send_game_connect(GameConnectErrorCode::Success)
    }

    fn send_game_connect(&self, code: GameConnectErrorCode) -> ControlFlow<()> {
        self.outbox
            .send_flow(Payload::GameConnectResponse(GameConnectResponse {
                success: code == GameConnectErrorCode::Success,
                error_code: code as i32,
            }))
    }

    /// 세션 루프 밖에서 `work` 를 돌리고 결과를 내부 채널로 받는다.
    /// 세션이 먼저 끝났으면 결과는 버려진다 (DB 작업 자체는 끝까지 수행된다).
    fn spawn_inner(&self, work: impl Future<Output = InnerMessage> + Send + 'static) {
        let inner_tx = self.inner_tx.clone();
        tokio::spawn(async move {
            let _ = inner_tx.send(work.await).await;
        });
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use futures::future::BoxFuture;

    use super::*;
    use crate::backend::BackendError;

    #[test]
    fn token_value_splits_on_first_colon_only() {
        assert_eq!(
            parse_token_value("7:user:with:colons"),
            Some(Account {
                account_id: 7,
                user_id: "user:with:colons".into()
            })
        );
        assert_eq!(
            parse_token_value("1:"),
            Some(Account {
                account_id: 1,
                user_id: String::new()
            })
        );
        assert_eq!(parse_token_value("no_colon"), None);
        assert_eq!(parse_token_value("abc:user"), None);
        assert_eq!(parse_token_value("-1:user"), None);
    }

    /// `touch_last_login` 이 처음 `failures` 번 실패한다.
    struct FlakyBackend {
        failures: Mutex<u32>,
        calls: Mutex<Vec<Instant>>,
    }

    impl Backend for FlakyBackend {
        fn take_auth_token<'a>(
            &'a self,
            _: &'a str,
        ) -> BoxFuture<'a, Result<Option<String>, BackendError>> {
            unreachable!()
        }

        fn save_score(
            &self,
            _: u64,
            _: i32,
            _: i32,
            _: i32,
        ) -> BoxFuture<'_, Result<(), BackendError>> {
            unreachable!()
        }

        fn touch_last_login(&self, _: u64) -> BoxFuture<'_, Result<(), BackendError>> {
            self.calls.lock().unwrap().push(Instant::now());
            let mut failures = self.failures.lock().unwrap();
            let result = if *failures > 0 {
                *failures -= 1;
                Err(BackendError::Sql(db::sqlx::Error::PoolTimedOut))
            } else {
                Ok(())
            };
            Box::pin(async move { result })
        }
    }

    #[tokio::test(start_paused = true)]
    async fn logout_retries_with_exponential_backoff() {
        let backend = FlakyBackend {
            failures: Mutex::new(3),
            calls: Mutex::new(Vec::new()),
        };
        record_logout(&backend, 1, &CancellationToken::new()).await;

        let calls = backend.calls.lock().unwrap();
        let gaps: Vec<_> = calls.windows(2).map(|w| w[1] - w[0]).collect();
        assert_eq!(
            gaps,
            [500, 1000, 2000].map(Duration::from_millis),
            "4번째 시도에서 성공"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn logout_retry_stops_on_shutdown() {
        let backend = FlakyBackend {
            failures: Mutex::new(u32::MAX),
            calls: Mutex::new(Vec::new()),
        };
        let shutdown = CancellationToken::new();
        shutdown.cancel();
        record_logout(&backend, 1, &shutdown).await;
        assert_eq!(backend.calls.lock().unwrap().len(), 1, "첫 시도는 한다");
    }
}
