//! 세션 송신 경로 (C# `SenderHandler`). LoginServer 와 GameServer 가 같이 쓴다.
//!
//! 송신은 별도 writer task 로 분리하고 그 사이 큐를 `MAX_SEND_QUEUE_SIZE` 로 제한한다 —
//! 느린 클라이언트가 세션 루프를 막지 않고, 큐가 넘치면 원본처럼 세션을 끊는다.

use std::ops::ControlFlow;
use std::time::Duration;

use futures::SinkExt;
use proto::MessageWrapper;
use proto::message_wrapper::Payload;
use tokio::net::tcp::OwnedWriteHalf;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio::time::Instant;
use tokio_util::codec::FramedWrite;
use tokio_util::sync::CancellationToken;

use crate::MessageCodec;
use crate::consts::MAX_SEND_QUEUE_SIZE;

/// 세션 종료 시 남은 송신 큐를 비울 때까지 기다리는 최대 시간.
const WRITER_DRAIN_TIMEOUT: Duration = Duration::from_secs(1);

/// 송신 큐가 넘쳐(또는 writer 가 이미 끝나) 세션을 끊어야 한다.
#[derive(Debug)]
pub struct SendQueueFull;

/// 세션 task 가 소유하는 송신 핸들.
pub struct Outbox {
    session_id: u64,
    tx: mpsc::Sender<MessageWrapper>,
}

/// writer task 를 띄운다. 송신 오류가 나면 `kick` 을 취소해 세션을 끝낸다.
pub fn spawn(
    session_id: u64,
    write_half: OwnedWriteHalf,
    kick: CancellationToken,
) -> (Outbox, Writer) {
    let (tx, rx) = mpsc::channel(MAX_SEND_QUEUE_SIZE);
    let handle = tokio::spawn(write_loop(
        FramedWrite::new(write_half, MessageCodec),
        rx,
        kick,
    ));
    (Outbox { session_id, tx }, Writer(handle))
}

impl Outbox {
    pub fn send(&self, payload: Payload) -> Result<(), SendQueueFull> {
        let message = MessageWrapper {
            message_size: 0,
            payload: Some(payload),
        };
        match self.tx.try_send(message) {
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

    /// 세션 루프용: 큐가 넘치면 `Break`.
    pub fn send_flow(&self, payload: Payload) -> ControlFlow<()> {
        match self.send(payload) {
            Ok(()) => ControlFlow::Continue(()),
            Err(SendQueueFull) => ControlFlow::Break(()),
        }
    }
}

pub struct Writer(JoinHandle<()>);

impl Writer {
    /// `outbox` 를 닫아 writer 가 남은 큐를 비우고 끝나게 한다. 오래 걸리면 중단한다.
    pub async fn close(mut self, outbox: Outbox) {
        drop(outbox);
        if tokio::time::timeout(WRITER_DRAIN_TIMEOUT, &mut self.0)
            .await
            .is_err()
        {
            self.0.abort();
        }
    }
}

async fn write_loop(
    mut sink: FramedWrite<OwnedWriteHalf, MessageCodec>,
    mut rx: mpsc::Receiver<MessageWrapper>,
    kick: CancellationToken,
) {
    let result: Result<(), crate::CodecError> = async {
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

/// `None` 이면 영원히 대기한다 — `select!` 에서 조건부 타이머로 쓴다.
pub async fn sleep_until_opt(deadline: Option<Instant>) {
    match deadline {
        Some(deadline) => tokio::time::sleep_until(deadline).await,
        None => std::future::pending().await,
    }
}
