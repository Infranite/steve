//! A minimal stand-in for `steve_vault::guard::scan` (accounts-vault.md §4, slice V4), so folder
//! and file sources never index a token while the real scanner is not wired yet. Same patterns as
//! the vault spec: Luhn-valid 13–19 digit runs, `ghp_`, `github_pat_`, `sk-`/`sk-ant-`,
//! `xox[abps]-`, `AKIA…`, PEM private keys, and “密码/口令/PIN 是 …”.
//!
//! The integrator should replace it with the vault's guard once V4 lands (`Knowledge::new(guard, …)`).

use std::ops::Range;
use std::sync::LazyLock;

use regex::Regex;

use crate::state::Guard;

static TOKENS: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(concat!(
        r"(?-u:\b)(?:",
        r"gh[pousr]_[A-Za-z0-9]{20,}",
        r"|github_pat_[A-Za-z0-9_]{20,}",
        r"|sk-ant-[A-Za-z0-9_\-]{16,}",
        r"|sk-[A-Za-z0-9_\-]{16,}",
        r"|xox[abps]-[A-Za-z0-9\-]{10,}",
        r"|AKIA[0-9A-Z]{16}",
        r")",
        r"|-----BEGIN [A-Z ]*PRIVATE KEY-----(?s:.*?)(?:-----END [A-Z ]*PRIVATE KEY-----|\z)",
    ))
    .expect("token pattern")
});

/// “密码是 …” and friends: only the value (≥6 non-blank characters) is a secret.
static SAID: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?:密码|口令|PIN)\s*(?:是|为|：|:)\s*(\S{6,})").expect("said pattern")
});

static DIGITS: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?-u:\b)[0-9]{13,19}(?-u:\b)").expect("digit pattern"));

fn luhn(digits: &str) -> bool {
    let mut sum = 0;
    for (i, c) in digits.bytes().rev().enumerate() {
        let mut d = u32::from(c - b'0');
        if i % 2 == 1 {
            d *= 2;
            if d > 9 {
                d -= 9;
            }
        }
        sum += d;
    }
    sum % 10 == 0
}

/// Pattern-based secret scanner (see the module docs).
#[derive(Debug, Default, Clone, Copy)]
pub struct TokenGuard;

impl Guard for TokenGuard {
    fn scan(&self, text: &str) -> Vec<Range<usize>> {
        let mut out: Vec<Range<usize>> = TOKENS.find_iter(text).map(|m| m.range()).collect();
        out.extend(
            SAID.captures_iter(text)
                .filter_map(|c| c.get(1))
                .map(|m| m.range()),
        );
        out.extend(
            DIGITS
                .find_iter(text)
                .filter(|m| luhn(m.as_str()))
                .map(|m| m.range()),
        );
        out.sort_by_key(|r| r.start);
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::redact;

    #[test]
    fn finds_the_vault_patterns() {
        let g = TokenGuard;
        let r = |s: &str| redact(&g, s);
        assert_eq!(
            r("令牌 ghp_abcdefghijklmnopqrstuvwxyz0123456789 别外传"),
            "令牌 ［已拦下的机密］ 别外传"
        );
        assert_eq!(
            r("key=sk-ant-api03-AAAAAAAAAAAAAAAAAAAA end"),
            "key=［已拦下的机密］ end"
        );
        assert_eq!(r("AKIAABCDEFGHIJKLMNOP"), "［已拦下的机密］");
        assert_eq!(r("xoxb-1234567890-abc"), "［已拦下的机密］");
        assert_eq!(
            r("github_pat_11ABCDEFG0123456789_abcdefghijklmnop"),
            "［已拦下的机密］"
        );
        assert_eq!(
            r("a\n-----BEGIN OPENSSH PRIVATE KEY-----\nAAAA\n-----END OPENSSH PRIVATE KEY-----\nb"),
            "a\n［已拦下的机密］\nb"
        );
        assert_eq!(r("wifi 密码是 hunter22!"), "wifi 密码是 ［已拦下的机密］");
        assert_eq!(
            r("令牌ghp_abcdefghijklmnopqrstuvwxyz0123456789。"),
            "令牌［已拦下的机密］。"
        );
        // A Luhn-valid card number is a secret; an order number that fails Luhn is not.
        assert_eq!(r("卡号 4111111111111111"), "卡号 ［已拦下的机密］");
        assert_eq!(r("订单 1234567890123"), "订单 1234567890123");
        // Too short to be a token, or ordinary prose.
        assert_eq!(r("ghp_short sk-1 密码是 abc"), "ghp_short sk-1 密码是 abc");
    }
}
