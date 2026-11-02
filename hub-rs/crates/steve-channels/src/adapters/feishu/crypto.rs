//! Event-callback crypto for Feishu "send events to developer server" (feishu.md §3.1 steps 2–3).
//!
//! With an Encrypt Key configured, every callback body is `{"encrypt":"<base64>"}` and carries
//! `X-Lark-Request-Timestamp`, `X-Lark-Request-Nonce`, `X-Lark-Signature` headers. The signature
//! covers the raw body bytes, so it must be checked before anything parses the body.

use aes::cipher::{BlockModeDecrypt, KeyIvInit, block_padding::Pkcs7};
use base64::Engine as _;
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;

/// Header carrying the Unix timestamp (seconds, decimal string) the request was signed at.
pub const HEADER_TIMESTAMP: &str = "X-Lark-Request-Timestamp";
/// Header carrying the random nonce mixed into the signature.
pub const HEADER_NONCE: &str = "X-Lark-Request-Nonce";
/// Header carrying `hex(sha256(timestamp + nonce + encrypt_key + body))`.
pub const HEADER_SIGNATURE: &str = "X-Lark-Signature";

/// Requests signed further than this from our clock are refused (feishu.md §3.1 step 2).
pub const MAX_CLOCK_SKEW_SECS: i64 = 10 * 60;

type Aes256CbcDec = cbc::Decryptor<aes::Aes256>;

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum CryptoError {
    #[error("body is not a JSON object with a string `encrypt` field")]
    NotEncrypted,
    #[error("`encrypt` is not valid base64")]
    Base64,
    #[error("ciphertext is shorter than the IV or not a whole number of AES blocks")]
    Length,
    #[error("AES-CBC decryption failed (wrong Encrypt Key or corrupt ciphertext)")]
    Decrypt,
    #[error("decrypted payload is not UTF-8 JSON")]
    Json,
}

/// Expected `X-Lark-Signature` for a request: lowercase hex of
/// `sha256(timestamp + nonce + encrypt_key + body)`, concatenated as bytes.
pub fn signature(timestamp: &str, nonce: &str, encrypt_key: &str, body: &[u8]) -> String {
    let mut h = Sha256::new();
    h.update(timestamp.as_bytes());
    h.update(nonce.as_bytes());
    h.update(encrypt_key.as_bytes());
    h.update(body);
    hex::encode(h.finalize())
}

/// Constant-time check of a received signature. Hex case is ignored; anything that isn't a
/// 64-char hex string is simply a mismatch.
pub fn verify_signature(
    timestamp: &str,
    nonce: &str,
    encrypt_key: &str,
    body: &[u8],
    received: &str,
) -> bool {
    let expected = signature(timestamp, nonce, encrypt_key, body);
    let received = received.trim().to_ascii_lowercase();
    expected.as_bytes().ct_eq(received.as_bytes()).into()
}

/// Whether a `X-Lark-Request-Timestamp` value is within [`MAX_CLOCK_SKEW_SECS`] of `now_secs`.
/// Unparseable timestamps are rejected.
pub fn timestamp_fresh(timestamp: &str, now_secs: i64) -> bool {
    match timestamp.trim().parse::<i64>() {
        Ok(ts) => (now_secs - ts).abs() <= MAX_CLOCK_SKEW_SECS,
        Err(_) => false,
    }
}

/// Decrypts the base64 value of an `encrypt` field: key = sha256(encrypt_key), the first 16
/// bytes of the decoded ciphertext are the IV, the rest is AES-256-CBC with PKCS#7 padding.
pub fn decrypt(encrypt_key: &str, encrypted_b64: &str) -> Result<Vec<u8>, CryptoError> {
    let raw = base64::engine::general_purpose::STANDARD
        .decode(encrypted_b64.trim())
        .map_err(|_| CryptoError::Base64)?;
    if raw.len() < 32 || raw.len() % 16 != 0 {
        return Err(CryptoError::Length);
    }
    let key = Sha256::digest(encrypt_key.as_bytes());
    let (iv, ct) = raw.split_at(16);
    let dec = Aes256CbcDec::new_from_slices(&key, iv).map_err(|_| CryptoError::Length)?;
    dec.decrypt_padded_vec::<Pkcs7>(ct)
        .map_err(|_| CryptoError::Decrypt)
}

/// Decrypts a whole callback body `{"encrypt":"…"}` into the inner JSON document.
pub fn decrypt_body(encrypt_key: &str, body: &[u8]) -> Result<serde_json::Value, CryptoError> {
    let outer: serde_json::Value =
        serde_json::from_slice(body).map_err(|_| CryptoError::NotEncrypted)?;
    let enc = outer
        .get("encrypt")
        .and_then(|v| v.as_str())
        .ok_or(CryptoError::NotEncrypted)?;
    let plain = decrypt(encrypt_key, enc)?;
    serde_json::from_slice(&plain).map_err(|_| CryptoError::Json)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The decryption example in Feishu's "Encrypt Key 加密配置示例" doc (all language samples).
    #[test]
    fn decrypts_doc_vector() {
        let out = decrypt("test key", "P37w+VZImNgPEO1RBhJ6RtKl7n6zymIbEG1pReEzghk=").unwrap();
        assert_eq!(out, b"hello world");
    }

    #[test]
    fn wrong_key_or_garbage_fails() {
        assert!(decrypt("other key", "P37w+VZImNgPEO1RBhJ6RtKl7n6zymIbEG1pReEzghk=").is_err());
        assert_eq!(decrypt("k", "not base64!!"), Err(CryptoError::Base64));
        assert_eq!(decrypt("k", "AAAA"), Err(CryptoError::Length));
        assert_eq!(
            decrypt_body("k", b"{\"x\":1}"),
            Err(CryptoError::NotEncrypted)
        );
        assert_eq!(
            decrypt_body(
                "test key",
                b"{\"encrypt\":\"P37w+VZImNgPEO1RBhJ6RtKl7n6zymIbEG1pReEzghk=\"}"
            ),
            Err(CryptoError::Json)
        );
    }

    /// The docs give the formula but no numeric example; this pins it to an independent
    /// computation (`printf '%s' '1700000000nonce1test key{"encrypt":"x"}' | shasum -a 256`).
    #[test]
    fn signature_formula() {
        let body = br#"{"encrypt":"x"}"#;
        let sig = signature("1700000000", "nonce1", "test key", body);
        assert_eq!(
            sig,
            "0750079ced63fb99ad83cf61c58f8d8134dfc0246555d095fd0b47d6721f2819"
        );
        assert!(verify_signature(
            "1700000000",
            "nonce1",
            "test key",
            body,
            &sig
        ));
        assert!(verify_signature(
            "1700000000",
            "nonce1",
            "test key",
            body,
            &sig.to_uppercase()
        ));
        assert!(!verify_signature(
            "1700000001",
            "nonce1",
            "test key",
            body,
            &sig
        ));
        assert!(!verify_signature(
            "1700000000",
            "nonce1",
            "test key",
            b"{}",
            &sig
        ));
        assert!(!verify_signature(
            "1700000000",
            "nonce1",
            "test key",
            body,
            ""
        ));
    }

    #[test]
    fn timestamp_window() {
        assert!(timestamp_fresh("1700000000", 1_700_000_600));
        assert!(!timestamp_fresh("1700000000", 1_700_000_601));
        assert!(timestamp_fresh("1700000600", 1_700_000_000));
        assert!(!timestamp_fresh("abc", 1_700_000_000));
    }
}
