//! C# `PasswordHashHelper` 호환 비밀번호 해시.
//!
//! - 클라이언트는 `SHA256(평문)` 32바이트를 보낸다 (`LoginRequest.password_hash`).
//! - DB에는 `PBKDF2-HMAC-SHA256(client_hash, salt, 100000회, 32바이트)` 와 salt 를
//!   각각 **패딩 있는 표준 Base64** 로 저장한다 (`accounts.password_hash`, `accounts.salt`).
//!
//! PBKDF2 10만 회는 수십 ms 의 CPU 작업이므로 async 컨텍스트에서는 `spawn_blocking` 으로 호출한다.

use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;

pub const ITERATIONS: u32 = 100_000;
pub const HASH_SIZE: usize = 32;
pub const SALT_SIZE: usize = 32;

#[derive(Debug, thiserror::Error)]
pub enum PasswordError {
    #[error("저장된 해시/salt 가 Base64 가 아님: {0}")]
    InvalidBase64(#[from] base64::DecodeError),
}

/// 클라이언트 측 해시: `SHA256(UTF-8 평문)`.
pub fn client_hash(plain_password: &str) -> [u8; 32] {
    Sha256::digest(plain_password.as_bytes()).into()
}

/// 새 salt 로 저장용 해시를 만든다. `(hash_base64, salt_base64)`
pub fn generate_stored_hash(client_hash: &[u8]) -> (String, String) {
    let mut salt = [0u8; SALT_SIZE];
    getrandom::fill(&mut salt).expect("OS 난수원을 사용할 수 없다");
    let hash = derive(client_hash, &salt);
    (STANDARD.encode(hash), STANDARD.encode(salt))
}

/// 상수 시간 비교로 검증한다. 저장값이 Base64 가 아니면 오류 (C# 에서는 예외 → DB 오류 처리).
pub fn verify(
    client_hash: &[u8],
    stored_hash_base64: &str,
    stored_salt_base64: &str,
) -> Result<bool, PasswordError> {
    let salt = STANDARD.decode(stored_salt_base64)?;
    let stored = STANDARD.decode(stored_hash_base64)?;
    let expected = derive(client_hash, &salt);
    // 길이가 다르면 subtle 이 false 를 돌려준다 (FixedTimeEquals 와 동일)
    Ok(expected.ct_eq(&stored[..]).into())
}

fn derive(client_hash: &[u8], salt: &[u8]) -> [u8; HASH_SIZE] {
    pbkdf2::pbkdf2_hmac_array::<Sha256, HASH_SIZE>(client_hash, salt, ITERATIONS)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn client_hash_is_sha256() {
        // SHA256("abc") — FIPS 180-2 테스트 벡터
        let expected = "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad";
        let actual: String = client_hash("abc")
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        assert_eq!(actual, expected);
    }

    #[test]
    fn pbkdf2_hmac_sha256_vector() {
        // RFC 7914 §11 PBKDF2-HMAC-SHA256 테스트 벡터 (P="passwd", S="salt", c=1, dkLen=64) 의 앞 32바이트
        let out = pbkdf2::pbkdf2_hmac_array::<Sha256, 32>(b"passwd", b"salt", 1);
        let hex: String = out.iter().map(|b| format!("{b:02x}")).collect();
        assert_eq!(
            hex,
            "55ac046e56e3089fec1691c22544b605f94185216dde0465e68b9d57c20dacbc"
        );
    }

    #[test]
    fn generate_then_verify() {
        let hash = client_hash("Test1234!");
        let (stored_hash, stored_salt) = generate_stored_hash(&hash);

        // 32바이트 → 패딩 포함 Base64 44자 (VARCHAR(88) 에 들어간다)
        assert_eq!(stored_hash.len(), 44);
        assert_eq!(stored_salt.len(), 44);

        assert!(verify(&hash, &stored_hash, &stored_salt).unwrap());
        assert!(!verify(&client_hash("wrong"), &stored_hash, &stored_salt).unwrap());
        assert!(!verify(&[], &stored_hash, &stored_salt).unwrap());
        assert!(verify(&hash, "not base64!", &stored_salt).is_err());
    }
}
