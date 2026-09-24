//! prost 생성 타입이 원본 스키마와 맞는지 확인하는 최소 검사.

use prost::Message;
use proto::{ConnectedResponse, MessageWrapper, message_wrapper::Payload};

#[test]
fn connected_response_wire_bytes() {
    let wrapper = MessageWrapper {
        message_size: 0, // 원본에서 설정하지 않는 필드 — 0이면 인코딩되지 않는다
        payload: Some(Payload::ConnectedResponse(ConnectedResponse { index: 0 })),
    };

    // field 10 (connected_response), wire type 2, 길이 0 → 0x52 0x00
    assert_eq!(wrapper.encode_to_vec(), [0x52, 0x00]);
    assert_eq!(
        MessageWrapper::decode(&[0x52u8, 0x00][..]).unwrap(),
        wrapper
    );
}
