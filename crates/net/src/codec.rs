//! `[2바이트 little-endian 본문 길이][protobuf MessageWrapper 본문]` 프레이밍.
//!
//! C# `ReceiveParser` / `SenderHandler.TrySerializeToBuffer` 대응. 길이 필드는 본문 크기만 담는다.

use bytes::{Buf, BufMut, BytesMut};
use prost::Message;
use proto::MessageWrapper;
use tokio_util::codec::{Decoder, Encoder};

use crate::consts::MAX_MESSAGE_BODY_SIZE;
use crate::stats;

const HEADER_SIZE: usize = 2;

#[derive(Debug, thiserror::Error)]
pub enum CodecError {
    #[error("유효하지 않은 메시지 크기: {0} (허용 범위: 1~{MAX_MESSAGE_BODY_SIZE})")]
    InvalidLength(usize),
    #[error("protobuf 디코딩 실패: {0}")]
    Decode(#[from] prost::DecodeError),
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

#[derive(Debug, Default, Clone, Copy)]
pub struct MessageCodec;

impl Decoder for MessageCodec {
    type Item = MessageWrapper;
    type Error = CodecError;

    fn decode(&mut self, src: &mut BytesMut) -> Result<Option<MessageWrapper>, CodecError> {
        if src.len() < HEADER_SIZE {
            return Ok(None);
        }

        let body_size = u16::from_le_bytes([src[0], src[1]]) as usize;
        if body_size == 0 || body_size > MAX_MESSAGE_BODY_SIZE {
            return Err(CodecError::InvalidLength(body_size));
        }

        let frame_size = HEADER_SIZE + body_size;
        if src.len() < frame_size {
            src.reserve(frame_size - src.len());
            return Ok(None);
        }

        src.advance(HEADER_SIZE);
        let body = src.split_to(body_size);
        let message = MessageWrapper::decode(body.freeze())?;
        stats::increment_received();
        Ok(Some(message))
    }
}

impl Encoder<MessageWrapper> for MessageCodec {
    type Error = CodecError;

    /// 본문이 최대 크기를 넘으면 원본과 같이 경고 후 **해당 메시지만 드롭**한다 (연결은 유지).
    fn encode(&mut self, message: MessageWrapper, dst: &mut BytesMut) -> Result<(), CodecError> {
        let body_size = message.encoded_len();
        if body_size > MAX_MESSAGE_BODY_SIZE {
            tracing::warn!(
                body_size,
                max = MAX_MESSAGE_BODY_SIZE,
                "메시지 크기 초과, 해당 메시지 드롭"
            );
            return Ok(());
        }

        dst.reserve(HEADER_SIZE + body_size);
        dst.put_u16_le(body_size as u16);
        message
            .encode(dst)
            .expect("reserve 로 용량을 확보했으므로 인코딩은 실패하지 않는다");
        stats::increment_sent();
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proto::message_wrapper::Payload;
    use proto::{ConnectedResponse, LoginRequest, MoveRequest};

    fn frame(message: &MessageWrapper) -> Vec<u8> {
        let mut dst = BytesMut::new();
        MessageCodec.encode(message.clone(), &mut dst).unwrap();
        dst.to_vec()
    }

    fn login_request() -> MessageWrapper {
        MessageWrapper {
            message_size: 0,
            payload: Some(Payload::LoginRequest(LoginRequest {
                user_id: "user_00001".into(),
                password_hash: vec![0xAB; 32],
            })),
        }
    }

    #[test]
    fn connected_response_wire_bytes() {
        let message = MessageWrapper {
            message_size: 0,
            payload: Some(Payload::ConnectedResponse(ConnectedResponse { index: 0 })),
        };
        // 길이 2 (LE) + field 10, wire type 2, 길이 0
        assert_eq!(frame(&message), [0x02, 0x00, 0x52, 0x00]);
    }

    #[test]
    fn decode_byte_by_byte() {
        let message = login_request();
        let bytes = frame(&message);

        let mut src = BytesMut::new();
        let mut decoded = Vec::new();
        for byte in bytes {
            src.put_u8(byte);
            if let Some(m) = MessageCodec.decode(&mut src).unwrap() {
                decoded.push(m);
            }
        }
        assert_eq!(decoded, [message]);
        assert!(src.is_empty());
    }

    #[test]
    fn decode_two_messages_in_one_buffer() {
        let first = login_request();
        let second = MessageWrapper {
            message_size: 0,
            payload: Some(Payload::MoveRequest(MoveRequest { x: 1.5, y: -2.0 })),
        };
        let mut src = BytesMut::from(&[frame(&first), frame(&second)].concat()[..]);
        // 세 번째 메시지의 헤더 일부만 들어온 상태
        src.put_u8(0x05);

        assert_eq!(MessageCodec.decode(&mut src).unwrap(), Some(first));
        assert_eq!(MessageCodec.decode(&mut src).unwrap(), Some(second));
        assert_eq!(MessageCodec.decode(&mut src).unwrap(), None);
        assert_eq!(&src[..], [0x05]);
    }

    #[test]
    fn reject_zero_and_oversized_length() {
        let mut zero = BytesMut::from(&[0x00u8, 0x00][..]);
        assert!(matches!(
            MessageCodec.decode(&mut zero),
            Err(CodecError::InvalidLength(0))
        ));

        // 8191 = 0x1FFF — 본문이 도착하기 전 헤더만으로 거부해야 한다
        let mut oversized = BytesMut::from(&[0xFFu8, 0x1F][..]);
        assert!(matches!(
            MessageCodec.decode(&mut oversized),
            Err(CodecError::InvalidLength(8191))
        ));

        // 8190 은 유효 — 본문을 기다린다
        let mut max = BytesMut::from(&[0xFEu8, 0x1F][..]);
        assert!(matches!(MessageCodec.decode(&mut max), Ok(None)));
    }

    #[test]
    fn oversized_encode_is_dropped_not_error() {
        let message = MessageWrapper {
            message_size: 0,
            payload: Some(Payload::LoginRequest(LoginRequest {
                user_id: String::new(),
                password_hash: vec![0; MAX_MESSAGE_BODY_SIZE],
            })),
        };
        let mut dst = BytesMut::new();
        MessageCodec.encode(message, &mut dst).unwrap();
        assert!(dst.is_empty());
    }
}
