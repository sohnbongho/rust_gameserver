//! 프로세스 전역 세션 id 발급기 (C# `SessionIdGenerator`). 1부터 시작한다.

use std::sync::atomic::{AtomicU64, Ordering};

static NEXT_ID: AtomicU64 = AtomicU64::new(0);

pub fn generate() -> u64 {
    NEXT_ID.fetch_add(1, Ordering::Relaxed) + 1
}
