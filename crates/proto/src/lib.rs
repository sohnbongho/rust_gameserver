//! `message.proto` 에서 생성한 protobuf 타입.

/// proto `package Messages;`
pub mod messages {
    include!(concat!(env!("OUT_DIR"), "/messages.rs"));
}

pub use messages::*;
