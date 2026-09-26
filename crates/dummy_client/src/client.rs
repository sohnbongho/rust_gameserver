//! 더미 클라이언트 1개의 전체 흐름 (C# `UserSession` + `Handler/Remote/*`).

use std::sync::Arc;
use std::sync::atomic::{AtomicI64, Ordering};

use futures::{SinkExt, StreamExt};
use net::MessageCodec;
use net::consts::KEEP_ALIVE_INTERVAL;
use proto::message_wrapper::Payload;
use proto::{GameConnectRequest, KeepAliveRequest, LoginRequest, MessageWrapper, MoveRequest};
use tokio::net::TcpStream;
use tokio::sync::mpsc;
use tokio::time::{Instant, Interval};
use tokio_util::codec::Framed;
use tokio_util::sync::CancellationToken;

type Connection = Framed<TcpStream, MessageCodec>;

/// 모든 클라이언트가 공유하는 설정.
#[derive(Debug, Clone)]
pub struct ClientSettings {
    pub game_server: String,
    /// `SHA256(비밀번호)` — `LoginRequest.password_hash`
    pub client_hash: [u8; 32],
}

/// 단계별 접속 수 (모니터 로그용).
#[derive(Debug, Default)]
pub struct Counters {
    login_server: AtomicI64,
    game_server: AtomicI64,
    disconnected: AtomicI64,
}

impl Counters {
    /// `(로그인서버, 게임서버, 연결끊김)`
    pub fn snapshot(&self) -> (i64, i64, i64) {
        (
            self.login_server.load(Ordering::Relaxed),
            self.game_server.load(Ordering::Relaxed),
            self.disconnected.load(Ordering::Relaxed),
        )
    }

    fn on_login_started(&self) {
        self.login_server.fetch_add(1, Ordering::Relaxed);
    }

    fn on_moved_to_game(&self) {
        self.login_server.fetch_sub(1, Ordering::Relaxed);
        self.game_server.fetch_add(1, Ordering::Relaxed);
    }

    fn on_disconnected(&self, phase: Phase) {
        let counter = match phase {
            Phase::LoginServer => &self.login_server,
            Phase::GameServer => &self.game_server,
        };
        counter.fetch_sub(1, Ordering::Relaxed);
        self.disconnected.fetch_add(1, Ordering::Relaxed);
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    LoginServer,
    GameServer,
}

/// 클라이언트가 끝난 이유. 테스트에서 흐름을 확인하는 데 쓴다.
#[derive(Debug, PartialEq, Eq)]
pub enum Finished {
    /// 서버가 끊었거나 수신 오류 — 어느 단계였는지
    Disconnected(Phase),
    /// GameServer 접속 실패
    GameConnectFailed,
    Shutdown,
}

pub fn user_id(account_index: usize) -> String {
    format!("user_{account_index:05}")
}

/// `account_index` 계정으로 전체 흐름을 수행한다. `login` 은 이미 LoginServer 에 연결된 소켓.
///
/// `moves` 가 있으면 인증 후 `(dx, dy)` 를 받아 누적 좌표로 `MoveRequest` 를 보낸다 (키보드 플레이어).
pub async fn run(
    login: TcpStream,
    account_index: usize,
    settings: Arc<ClientSettings>,
    counters: Arc<Counters>,
    moves: Option<mpsc::Receiver<(f32, f32)>>,
    shutdown: CancellationToken,
) -> Finished {
    let user_id = user_id(account_index);
    counters.on_login_started();

    let token = match login_phase(
        Framed::new(login, MessageCodec),
        &user_id,
        &settings,
        &shutdown,
    )
    .await
    {
        Ok(token) => token,
        Err(finished) => {
            counters.on_disconnected(Phase::LoginServer);
            return finished;
        }
    };

    counters.on_moved_to_game();
    let finished = game_phase(&user_id, token, &settings, moves, &shutdown).await;
    counters.on_disconnected(Phase::GameServer);
    finished
}

/// 로그인 성공 시 인증 토큰. 로그인 실패 시에는 원본과 같이 로그만 남기고 연결을 유지한 채 대기한다.
async fn login_phase(
    mut conn: Connection,
    user_id: &str,
    settings: &ClientSettings,
    shutdown: &CancellationToken,
) -> Result<String, Finished> {
    loop {
        let message = tokio::select! {
            _ = shutdown.cancelled() => return Err(Finished::Shutdown),
            message = conn.next() => message,
        };
        let Some(Ok(message)) = message else {
            return Err(Finished::Disconnected(Phase::LoginServer));
        };

        match message.payload {
            Some(Payload::ConnectedResponse(_)) => {
                let request = Payload::LoginRequest(LoginRequest {
                    user_id: user_id.to_owned(),
                    password_hash: settings.client_hash.to_vec(),
                });
                if send(&mut conn, request).await.is_err() {
                    return Err(Finished::Disconnected(Phase::LoginServer));
                }
            }
            Some(Payload::LoginResponse(resp)) if resp.success => {
                // LoginServer 연결은 여기서 닫는다 (conn drop)
                return Ok(resp.auth_token);
            }
            Some(Payload::LoginResponse(resp)) => {
                tracing::warn!(
                    user_id,
                    error_code = resp.error_code,
                    "[LoginResponse] 로그인 실패"
                );
            }
            _ => {}
        }
    }
}

async fn game_phase(
    user_id: &str,
    auth_token: String,
    settings: &ClientSettings,
    mut moves: Option<mpsc::Receiver<(f32, f32)>>,
    shutdown: &CancellationToken,
) -> Finished {
    let stream = match TcpStream::connect(&settings.game_server).await {
        Ok(stream) => stream,
        Err(e) => {
            tracing::warn!(user_id, game_server = %settings.game_server, error = %e, "게임서버 접속 실패");
            return Finished::GameConnectFailed;
        }
    };
    let _ = stream.set_nodelay(true);
    let mut conn = Framed::new(stream, MessageCodec);

    // 인증 전에는 None — KeepAlive 도 이동도 보내지 않는다
    let mut keep_alive: Option<Interval> = None;
    let (mut x, mut y) = (0.0f32, 0.0f32);

    loop {
        let sent = tokio::select! {
            _ = shutdown.cancelled() => return Finished::Shutdown,
            message = conn.next() => {
                let Some(Ok(message)) = message else {
                    return Finished::Disconnected(Phase::GameServer);
                };
                match message.payload {
                    Some(Payload::ConnectedResponse(_)) => {
                        let request = GameConnectRequest { auth_token: auth_token.clone() };
                        send(&mut conn, Payload::GameConnectRequest(request)).await
                    }
                    Some(Payload::GameConnectResponse(resp)) => {
                        if resp.success {
                            let start = Instant::now() + KEEP_ALIVE_INTERVAL;
                            keep_alive = Some(tokio::time::interval_at(start, KEEP_ALIVE_INTERVAL));
                        } else {
                            tracing::warn!(user_id, error_code = resp.error_code, "[GameConnectResponse] 게임서버 연결 실패");
                        }
                        Ok(())
                    }
                    Some(Payload::MoveResponse(resp)) => {
                        tracing::debug!(user_id, x = resp.x, y = resp.y, success = resp.success, "[MoveResponse]");
                        Ok(())
                    }
                    _ => Ok(()),
                }
            }
            _ = tick(keep_alive.as_mut()) => send(&mut conn, Payload::KeepAliveRequest(KeepAliveRequest {})).await,
            Some((dx, dy)) = recv_move(moves.as_mut()) => {
                if keep_alive.is_none() {
                    Ok(())
                } else {
                    x += dx;
                    y += dy;
                    tracing::debug!(user_id, x, y, "[이동]");
                    send(&mut conn, Payload::MoveRequest(MoveRequest { x, y })).await
                }
            }
        };

        if sent.is_err() {
            return Finished::Disconnected(Phase::GameServer);
        }
    }
}

async fn send(conn: &mut Connection, payload: Payload) -> Result<(), net::CodecError> {
    conn.send(MessageWrapper {
        message_size: 0,
        payload: Some(payload),
    })
    .await
}

async fn tick(interval: Option<&mut Interval>) {
    match interval {
        Some(interval) => {
            interval.tick().await;
        }
        None => std::future::pending().await,
    }
}

async fn recv_move(moves: Option<&mut mpsc::Receiver<(f32, f32)>>) -> Option<(f32, f32)> {
    match moves {
        Some(rx) => rx.recv().await,
        None => std::future::pending().await,
    }
}
