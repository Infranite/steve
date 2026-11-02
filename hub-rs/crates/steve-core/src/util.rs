//! Small helpers shared by the domain modules: ids, hashes, time, and JS-compatible string slicing.

use base64::Engine;
use sha2::{Digest, Sha256};
use std::time::{SystemTime, UNIX_EPOCH};

/// Unix milliseconds, like `Date.now()`.
pub fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// `p + "_" + 16 hex chars`, the TS `uid()`.
pub fn uid(p: &str) -> String {
    let u = uuid::Uuid::new_v4().simple().to_string();
    format!("{p}_{}", &u[..16])
}

/// sha256 hex, used for session and auth tokens.
pub fn sha(s: &str) -> String {
    hex(&Sha256::digest(s.as_bytes()))
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

/// 24 random bytes, base64url without padding (MCP session tokens, release drain tokens).
pub fn token() -> String {
    let b: [u8; 24] = rand::random();
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(b)
}

/// Length in UTF-16 code units, like JS `String.length`.
pub fn js_len(s: &str) -> usize {
    s.encode_utf16().count()
}

/// `s.slice(0, n)` in UTF-16 code units; never splits a character.
pub fn js_head(s: &str, n: usize) -> &str {
    let mut units = 0;
    for (i, c) in s.char_indices() {
        units += c.len_utf16();
        if units > n {
            return &s[..i];
        }
    }
    s
}

/// `s.slice(-n)` in UTF-16 code units; never splits a character.
pub fn js_tail(s: &str, n: usize) -> &str {
    let mut units = 0;
    for (i, c) in s.char_indices().rev() {
        units += c.len_utf16();
        if units > n {
            return &s[i + c.len_utf8()..];
        }
    }
    s
}

/// JS truthiness of a string: non-empty.
pub fn or<'a>(a: &'a str, b: &'a str) -> &'a str {
    if a.is_empty() { b } else { a }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slicing_counts_utf16_units() {
        assert_eq!(js_head("你好世界", 2), "你好");
        assert_eq!(js_head("ab", 5), "ab");
        assert_eq!(js_head("a😀b", 2), "a");
        assert_eq!(js_tail("你好世界", 2), "世界");
        assert_eq!(js_tail("ab", 5), "ab");
        assert_eq!(js_len("a😀"), 3);
    }

    #[test]
    fn ids_and_tokens() {
        let id = uid("dec");
        assert!(id.starts_with("dec_") && id.len() == 20);
        assert_eq!(token().len(), 32);
        assert_eq!(
            sha("x"),
            "2d711642b726b04401627ca9fbac32f5c8530fb1903cc4db02258717921a4881"
        );
    }
}
