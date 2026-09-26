//! 10초 주기 모니터 로그와 프로세스 CPU·메모리 측정.

use std::time::{Duration, Instant};

use sysinfo::{Pid, ProcessRefreshKind, ProcessesToUpdate, System};
use tokio_util::sync::CancellationToken;

use crate::stats;

const MONITOR_INTERVAL: Duration = Duration::from_secs(10);

/// 직전 측정 이후 구간의 CPU 사용률을 계산한다 (C# `TotalProcessorTime` 차분 방식).
pub struct ProcessMetrics {
    system: System,
    pid: Option<Pid>,
    cpu_count: f64,
    prev_cpu_ms: u64,
    prev_at: Instant,
}

impl Default for ProcessMetrics {
    fn default() -> Self {
        let mut metrics = Self {
            system: System::new(),
            pid: sysinfo::get_current_pid().ok(),
            cpu_count: std::thread::available_parallelism().map_or(1, |n| n.get()) as f64,
            prev_cpu_ms: 0,
            prev_at: Instant::now(),
        };
        metrics.prev_cpu_ms = metrics.refresh().map_or(0, |(cpu_ms, _)| cpu_ms);
        metrics
    }
}

impl ProcessMetrics {
    /// `(CPU %, 메모리 MB)` — CPU는 직전 호출 이후 구간, 전체 코어 대비 백분율.
    pub fn sample(&mut self) -> (f64, f64) {
        let now = Instant::now();
        let Some((cpu_ms, memory_bytes)) = self.refresh() else {
            return (0.0, 0.0);
        };

        let elapsed = now.duration_since(self.prev_at).as_secs_f64();
        let cpu_percent = if elapsed > 0.0 {
            (cpu_ms.saturating_sub(self.prev_cpu_ms) as f64 / 1000.0) / (elapsed * self.cpu_count)
                * 100.0
        } else {
            0.0
        };

        self.prev_cpu_ms = cpu_ms;
        self.prev_at = now;
        (cpu_percent, memory_bytes as f64 / 1024.0 / 1024.0)
    }

    fn refresh(&mut self) -> Option<(u64, u64)> {
        let pid = self.pid?;
        self.system.refresh_processes_specifics(
            ProcessesToUpdate::Some(&[pid]),
            false,
            ProcessRefreshKind::nothing().with_cpu().with_memory(),
        );
        let process = self.system.process(pid)?;
        Some((process.accumulated_cpu_time(), process.memory()))
    }
}

/// `[모니터] 동접 | CPU | 메모리 | 수신 | 송신` 로그를 10초마다 남긴다.
pub async fn run(shutdown: CancellationToken, active_sessions: impl Fn() -> usize) {
    let mut metrics = ProcessMetrics::default();
    let (mut prev_recv, mut prev_sent) = stats::snapshot();

    loop {
        tokio::select! {
            _ = shutdown.cancelled() => return,
            _ = tokio::time::sleep(MONITOR_INTERVAL) => {}
        }

        let (cpu, memory_mb) = metrics.sample();
        let (recv, sent) = stats::snapshot();
        tracing::info!(
            "[모니터] 동접: {}명 | CPU: {:.1}% | 메모리: {:.0}MB | 수신: {}패킷/10s | 송신: {}패킷/10s",
            active_sessions(),
            cpu,
            memory_mb,
            recv - prev_recv,
            sent - prev_sent,
        );
        prev_recv = recv;
        prev_sent = sent;
    }
}
