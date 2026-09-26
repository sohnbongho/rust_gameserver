//! 첫 번째 클라이언트를 키보드로 조종한다: W/A/S/D 또는 방향키 = 이동, Q = 종료.
//!
//! 키를 Enter 없이 받으려면 터미널 raw 모드가 필요하다. raw 모드에서는 `\n` 이 줄 처음으로
//! 돌아가지 않으므로 로그 출력은 [`CrLfStdout`] 로 `\r\n` 을 쓴다.

use std::io::{self, IsTerminal, Write};
use std::time::Duration;

use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use crossterm::terminal;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

const POLL_INTERVAL: Duration = Duration::from_millis(100);

/// stdin 이 터미널일 때만 키보드 입력을 쓴다 (파이프·CI 에서는 끈다).
pub fn available() -> bool {
    io::stdin().is_terminal() && io::stdout().is_terminal()
}

/// raw 모드를 켜고 키 입력 스레드를 시작한다. Q 또는 Ctrl+C 를 누르면 `shutdown` 을 취소한다.
/// 반환된 guard 가 drop 될 때 raw 모드를 끈다.
pub fn spawn(
    moves: mpsc::Sender<(f32, f32)>,
    shutdown: CancellationToken,
) -> io::Result<RawModeGuard> {
    terminal::enable_raw_mode()?;
    let guard = RawModeGuard;

    std::thread::spawn(move || {
        while !shutdown.is_cancelled() {
            match event::poll(POLL_INTERVAL) {
                Ok(false) => continue,
                Ok(true) => {}
                Err(e) => {
                    tracing::error!(error = %e, "[키보드] 입력 오류");
                    return;
                }
            }
            let Ok(Event::Key(key)) = event::read() else {
                continue;
            };
            if key.kind != KeyEventKind::Press {
                continue;
            }

            let delta = match key.code {
                KeyCode::Char('w' | 'W') | KeyCode::Up => (0.0, -1.0),
                KeyCode::Char('s' | 'S') | KeyCode::Down => (0.0, 1.0),
                KeyCode::Char('a' | 'A') | KeyCode::Left => (-1.0, 0.0),
                KeyCode::Char('d' | 'D') | KeyCode::Right => (1.0, 0.0),
                KeyCode::Char('q' | 'Q') => {
                    shutdown.cancel();
                    return;
                }
                // raw 모드에서는 Ctrl+C 가 시그널이 아니라 키 입력으로 들어온다
                KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                    shutdown.cancel();
                    return;
                }
                _ => continue,
            };
            if moves.blocking_send(delta).is_err() {
                return; // 플레이어 세션이 끝났다
            }
        }
    });

    Ok(guard)
}

pub struct RawModeGuard;

impl Drop for RawModeGuard {
    fn drop(&mut self) {
        let _ = terminal::disable_raw_mode();
    }
}

/// `\n` 을 `\r\n` 으로 바꿔 쓰는 stdout (raw 모드용 tracing writer).
pub struct CrLfStdout;

impl Write for CrLfStdout {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let mut out = io::stdout().lock();
        for chunk in buf.split_inclusive(|&b| b == b'\n') {
            match chunk.strip_suffix(b"\n") {
                Some(line) => {
                    out.write_all(line)?;
                    out.write_all(b"\r\n")?;
                }
                None => out.write_all(chunk)?,
            }
        }
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        io::stdout().flush()
    }
}
