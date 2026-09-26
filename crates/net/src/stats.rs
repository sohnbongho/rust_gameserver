//! 프로세스 전역 패킷 카운터 (C# `PacketStats`).

use std::sync::atomic::{AtomicU64, Ordering};

static RECEIVED: AtomicU64 = AtomicU64::new(0);
static SENT: AtomicU64 = AtomicU64::new(0);

pub fn increment_received() {
    RECEIVED.fetch_add(1, Ordering::Relaxed);
}

pub fn increment_sent() {
    SENT.fetch_add(1, Ordering::Relaxed);
}

/// `(누적 수신, 누적 송신)` 패킷 수.
pub fn snapshot() -> (u64, u64) {
    (
        RECEIVED.load(Ordering::Relaxed),
        SENT.load(Ordering::Relaxed),
    )
}
