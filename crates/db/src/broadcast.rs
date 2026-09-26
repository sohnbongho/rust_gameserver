//! Redis Pub/Sub 공지 채널 (C# `RedisBroadcastManager`).

use std::time::Duration;

use futures::StreamExt;
use redis::AsyncCommands;
use redis::aio::ConnectionManager;
use tokio_util::sync::CancellationToken;

const RESUBSCRIBE_DELAY_MIN: Duration = Duration::from_secs(1);
const RESUBSCRIBE_DELAY_MAX: Duration = Duration::from_secs(30);

pub async fn publish(
    redis: &ConnectionManager,
    channel: &str,
    message: &str,
) -> redis::RedisResult<()> {
    let mut conn = redis.clone();
    conn.publish::<_, _, ()>(channel, message).await
}

/// `shutdown` 전까지 채널을 구독한다. 연결이 끊기면 재구독한다 (StackExchange.Redis 의 자동 재구독 대응).
pub async fn subscribe(
    client: redis::Client,
    channel: String,
    shutdown: CancellationToken,
    mut on_received: impl FnMut(String),
) {
    let mut delay = RESUBSCRIBE_DELAY_MIN;
    loop {
        let session = async {
            let mut pubsub = client.get_async_pubsub().await?;
            pubsub.subscribe(&channel).await?;
            tracing::info!(%channel, "Redis Pub/Sub 구독 시작");
            delay = RESUBSCRIBE_DELAY_MIN;

            let mut messages = pubsub.on_message();
            while let Some(msg) = messages.next().await {
                match msg.get_payload::<String>() {
                    Ok(payload) => on_received(payload),
                    Err(e) => tracing::error!(error = %e, "브로드캐스트 수신 처리 실패"),
                }
            }
            redis::RedisResult::Ok(())
        };

        let result = tokio::select! {
            _ = shutdown.cancelled() => return,
            result = session => result,
        };
        match result {
            Ok(()) => tracing::warn!(%channel, "Redis Pub/Sub 연결 끊김, 재구독"),
            Err(e) => {
                tracing::warn!(error = %e, %channel, retry_in = ?delay, "Redis Pub/Sub 연결 실패, 재시도")
            }
        }

        tokio::select! {
            _ = shutdown.cancelled() => return,
            _ = tokio::time::sleep(delay) => {}
        }
        delay = (delay * 2).min(RESUBSCRIBE_DELAY_MAX);
    }
}
