//! TCP 수락 루프 + IP 플러드 밴 (C# `TCPAcceptor`).

use std::collections::{HashMap, VecDeque};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::time::{Duration, Instant};

use tokio::net::{TcpListener, TcpSocket, TcpStream};
use tokio_util::sync::CancellationToken;

use crate::consts::{BAN_DURATION, FLOOD_WINDOW, MAX_CONNECTIONS_PER_WINDOW, MAX_LISTENER_BACKLOG};

/// 만료된 IP 엔트리 정리 주기. IP 로테이션 공격으로 맵이 무한 증식하는 것을 방지.
const SWEEP_INTERVAL: Duration = Duration::from_secs(60);

/// 원본과 같이 IPv4 `0.0.0.0:port` 에 backlog 4096 으로 바인드한다.
pub fn bind(port: u16) -> std::io::Result<TcpListener> {
    let socket = TcpSocket::new_v4()?;
    #[cfg(not(windows))]
    socket.set_reuseaddr(true)?;
    socket.bind(SocketAddr::from((Ipv4Addr::UNSPECIFIED, port)))?;
    socket.listen(MAX_LISTENER_BACKLOG)
}

/// `shutdown` 이 취소될 때까지 연결을 수락한다. 차단된 IP의 연결은 즉시 닫는다.
pub async fn run(
    listener: TcpListener,
    shutdown: CancellationToken,
    mut on_accepted: impl FnMut(TcpStream, SocketAddr),
) {
    let mut guard = ConnectionGuard::default();
    loop {
        let accepted = tokio::select! {
            _ = shutdown.cancelled() => return,
            accepted = listener.accept() => accepted,
        };

        match accepted {
            Ok((stream, addr)) => {
                if !guard.should_block(addr.ip(), Instant::now()) {
                    on_accepted(stream, addr);
                }
            }
            Err(e) => {
                // EMFILE 등 일시적 오류에서 바쁜 루프를 돌지 않도록 잠시 쉰다
                tracing::warn!(error = %e, "Fail Accept");
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        }
    }
}

/// IP별 버스트 연결 감지 및 밴. 수락 루프 한 task가 소유하므로 락이 없다.
#[derive(Debug, Default)]
pub struct ConnectionGuard {
    timestamps: HashMap<IpAddr, VecDeque<Instant>>,
    banned_until: HashMap<IpAddr, Instant>,
    last_sweep: Option<Instant>,
}

impl ConnectionGuard {
    pub fn should_block(&mut self, ip: IpAddr, now: Instant) -> bool {
        let ip = ip.to_canonical(); // IPv4-mapped IPv6 → IPv4
        if is_allowlisted(ip) {
            return false;
        }

        if let Some(&until) = self.banned_until.get(&ip) {
            if now < until {
                tracing::warn!(%ip, "밴된 IP 접속 시도");
                return true;
            }
            self.banned_until.remove(&ip);
        }

        let flooded = self.is_flood_detected(ip, now);
        self.sweep_if_due(now);
        if flooded {
            self.banned_until.insert(ip, now + BAN_DURATION);
            tracing::warn!(%ip, minutes = BAN_DURATION.as_secs() / 60, "플러드 감지, 밴");
        }
        flooded
    }

    fn is_flood_detected(&mut self, ip: IpAddr, now: Instant) -> bool {
        let queue = self.timestamps.entry(ip).or_default();
        prune(queue, now);
        if queue.len() >= MAX_CONNECTIONS_PER_WINDOW {
            return true;
        }
        queue.push_back(now);
        false
    }

    fn sweep_if_due(&mut self, now: Instant) {
        // 원본은 마지막 sweep 시각 0에서 출발하므로 첫 호출에서 바로 sweep 한다
        if self
            .last_sweep
            .is_some_and(|last| now.duration_since(last) < SWEEP_INTERVAL)
        {
            return;
        }
        self.last_sweep = Some(now);

        self.timestamps.retain(|_, queue| {
            prune(queue, now);
            !queue.is_empty()
        });
        self.banned_until.retain(|_, until| now < *until);
    }

    #[cfg(test)]
    fn tracked_ips(&self) -> usize {
        self.timestamps.len()
    }
}

fn prune(queue: &mut VecDeque<Instant>, now: Instant) {
    while queue
        .front()
        .is_some_and(|&t| now.duration_since(t) > FLOOD_WINDOW)
    {
        queue.pop_front();
    }
}

fn is_allowlisted(ip: IpAddr) -> bool {
    ip == IpAddr::V4(Ipv4Addr::LOCALHOST) || ip == IpAddr::V6(Ipv6Addr::LOCALHOST)
}

#[cfg(test)]
mod tests {
    use super::*;

    const REMOTE: IpAddr = IpAddr::V4(Ipv4Addr::new(10, 0, 0, 1));

    #[test]
    fn ban_after_threshold_within_window() {
        let mut guard = ConnectionGuard::default();
        let now = Instant::now();
        for i in 0..MAX_CONNECTIONS_PER_WINDOW {
            assert!(!guard.should_block(REMOTE, now), "{i}번째 연결은 허용");
        }
        assert!(guard.should_block(REMOTE, now));
        // 윈도우가 지나도 밴 기간 동안은 차단
        assert!(guard.should_block(REMOTE, now + FLOOD_WINDOW * 2));
        // 밴 만료 후 허용
        assert!(!guard.should_block(REMOTE, now + BAN_DURATION + Duration::from_secs(1)));
    }

    #[test]
    fn window_slides() {
        let mut guard = ConnectionGuard::default();
        let start = Instant::now();
        for _ in 0..MAX_CONNECTIONS_PER_WINDOW {
            assert!(!guard.should_block(REMOTE, start));
        }
        assert!(!guard.should_block(REMOTE, start + FLOOD_WINDOW + Duration::from_millis(1)));
    }

    #[test]
    fn allowlist_and_ipv4_mapped() {
        let mut guard = ConnectionGuard::default();
        let now = Instant::now();
        let mapped_localhost = IpAddr::V6(Ipv4Addr::LOCALHOST.to_ipv6_mapped());
        for _ in 0..100 {
            assert!(!guard.should_block(mapped_localhost, now));
            assert!(!guard.should_block(IpAddr::V6(Ipv6Addr::LOCALHOST), now));
        }

        // IPv4-mapped 주소는 IPv4 주소와 같은 카운터를 쓴다
        let mapped_remote = IpAddr::V6(Ipv4Addr::new(10, 0, 0, 1).to_ipv6_mapped());
        for _ in 0..MAX_CONNECTIONS_PER_WINDOW {
            assert!(!guard.should_block(mapped_remote, now));
        }
        assert!(guard.should_block(REMOTE, now));
    }

    #[test]
    fn sweep_removes_expired_entries() {
        let mut guard = ConnectionGuard::default();
        let start = Instant::now();
        for i in 0..50u8 {
            guard.should_block(IpAddr::V4(Ipv4Addr::new(10, 0, 1, i)), start);
        }
        assert_eq!(guard.tracked_ips(), 50);

        guard.should_block(REMOTE, start + SWEEP_INTERVAL + FLOOD_WINDOW);
        assert_eq!(guard.tracked_ips(), 1);
    }
}
