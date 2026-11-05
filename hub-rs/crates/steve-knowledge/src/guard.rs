//! The knowledge base's secret scanner: `steve_vault::guard::scan` behind
//! the [`Guard`] trait, so folder and file sources never index a token. It used to be an interim
//! copy of the vault's patterns; the vault's set is a superset (grouped card numbers, `xapp-`,
//! `ASIA`, `glpat-`, `AIza`, `sk_live_`, `password=` …), so every builder that does not pass its own
//! guard (`MailBuilder`'s default, tests) gets the full set too.

use std::ops::Range;

use crate::state::Guard;

/// The vault's paste guard as a [`Guard`].
#[derive(Debug, Default, Clone, Copy)]
pub struct TokenGuard;

impl Guard for TokenGuard {
    fn scan(&self, text: &str) -> Vec<Range<usize>> {
        steve_vault::guard::scan(text)
            .into_iter()
            .map(|s| s.range())
            .collect()
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
        // the vault's wider set: grouped card numbers and `password=`
        assert_eq!(r("卡号 4111 1111 1111 1111"), "卡号 ［已拦下的机密］");
        assert_eq!(r("password=hunter22!"), "password=［已拦下的机密］");
    }
}
