//! C# `SessionConstInfo` 대응 상수.

use std::time::Duration;

/// 수신 버퍼 크기. 본문 최대 크기는 여기서 2바이트 길이 헤더를 뺀 값이다.
pub const MAX_BUFFER_SIZE: usize = 8192;
/// 2바이트 길이 헤더를 제외한 메시지 본문 최대 크기 (`1 ..= 8190` 만 유효).
pub const MAX_MESSAGE_BODY_SIZE: usize = MAX_BUFFER_SIZE - 2;

/// 서버가 동시에 처리하지 못한 연결 요청을 쌓아두는 listen backlog.
pub const MAX_LISTENER_BACKLOG: u32 = 4096;
/// 세션당 최대 송신 큐 크기 (초과 시 해당 세션 강제 종료).
pub const MAX_SEND_QUEUE_SIZE: usize = 200;
/// 세션당 내부 메시지 채널 용량 (초과 시 세션 강제 종료).
pub const MAX_MESSAGE_CHANNEL_CAPACITY: usize = 1000;

/// 플러드 감지 윈도우.
pub const FLOOD_WINDOW: Duration = Duration::from_secs(10);
/// 윈도우 내 최대 허용 연결 수 (초과 시 즉시 밴).
pub const MAX_CONNECTIONS_PER_WINDOW: usize = 30;
/// 밴 지속 시간.
pub const BAN_DURATION: Duration = Duration::from_secs(10 * 60);

/// 클라이언트 KeepAlive 전송 주기.
pub const KEEP_ALIVE_INTERVAL: Duration = Duration::from_secs(3);
/// 서버 KeepAlive 타임아웃 (초과 시 세션 종료, 인증된 세션만 검사).
pub const KEEP_ALIVE_TIMEOUT: Duration = Duration::from_secs(10);
