//! The paste guard (accounts-vault.md §4.6): finds passwords, tokens, private keys and card numbers in
//! text a person typed (`POST /api/thread`, `POST /api/config/draft`) or a source brought in (knowledge
//! base, event bodies, channel messages), so they are replaced before anything is stored or sent to a
//! model.
//!
//! What counts (a superset of steve-knowledge's interim `TokenGuard`, so it can replace it):
//! - 13–19 digit runs that pass the Luhn check, also written in groups (`4111 1111 1111 1111`,
//!   `4111-1111-…`);
//! - GitHub (`ghp_`/`gho_`/`ghu_`/`ghs_`/`ghr_`, `github_pat_`), OpenAI/Anthropic (`sk-`, `sk-ant-`),
//!   Slack (`xox[abps]-`, `xapp-`), AWS access key ids (`AKIA`/`ASIA` + 16), GitLab (`glpat-`), Google
//!   API keys (`AIza…`), Stripe secret keys (`sk_live_`/`rk_live_`);
//! - PEM / OpenSSH / PGP private key blocks (to the END line, or to the end of the text);
//! - “密码/口令/PIN/验证码/密钥 是 …” and `password: …` / `password=…`: only the value (≥6 non-blank
//!   characters) is the secret.
//!
//! Integration: steve-knowledge's `Guard` trait takes ranges, so its adapter is
//! `guard::scan(t).into_iter().map(|s| s.range()).collect()`.

use std::ops::Range;
use std::sync::LazyLock;

use regex::Regex;

/// What a hit is replaced with before storing or routing the text (§4.6), verbatim.
pub const REPLACEMENT: &str = "［已拦下的机密］";
/// Steve's fixed answer when a message was intercepted (§4.6), verbatim.
pub const REPLY: &str = "看起来你发了一个密码或令牌，我没有记下它。请在保险箱里新建条目保存。";
/// Where [`REPLY`] links to.
pub const REPLY_HREF: &str = "#/vault";

/// One secret-looking piece of text, as byte offsets into the scanned string (always on char
/// boundaries).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Span {
    pub start: usize,
    pub end: usize,
    /// `card` | `token` | `private_key` | `said` (a value after “密码是”). Overlapping hits are merged
    /// and keep the kind of the first.
    pub kind: &'static str,
}

impl Span {
    pub fn range(&self) -> Range<usize> {
        self.start..self.end
    }
}

static TOKENS: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(concat!(
        r"(?-u:\b)(?:",
        r"gh[pousr]_[A-Za-z0-9]{20,}",
        r"|github_pat_[A-Za-z0-9_]{20,}",
        r"|sk-ant-[A-Za-z0-9_\-]{16,}",
        r"|sk-[A-Za-z0-9_\-]{16,}",
        r"|[sr]k_live_[A-Za-z0-9]{16,}",
        r"|xox[abps]-[A-Za-z0-9\-]{10,}",
        r"|xapp-[A-Za-z0-9\-]{10,}",
        r"|(?:AKIA|ASIA)[0-9A-Z]{16}",
        r"|glpat-[A-Za-z0-9_\-]{20,}",
        r"|AIza[0-9A-Za-z_\-]{35}",
        r")",
    ))
    .expect("token pattern")
});

static PRIVATE_KEY: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(concat!(
        r"-----BEGIN [A-Z ]*PRIVATE KEY(?: BLOCK)?-----(?s:.*?)",
        r"(?:-----END [A-Z ]*PRIVATE KEY(?: BLOCK)?-----|\z)",
    ))
    .expect("private key pattern")
});

/// “密码是 …” and friends; group 1 is the value.
static SAID: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(concat!(
        r"(?:密码|口令|PIN|验证码|密钥)\s*(?:是|为|：|:)\s*(\S{6,})",
        r#"|(?i:(?-u:\b)(?:password|passwd|passcode|pwd))\s*(?:[:=：])\s*["']?([^\s"']{6,})"#,
    ))
    .expect("said pattern")
});

/// Digit runs, optionally grouped by single spaces or dashes; checked for length and Luhn after.
static DIGITS: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?-u:\b)[0-9](?:[ \-]?[0-9]){12,18}(?-u:\b)").expect("digit pattern")
});

fn luhn(digits: &[u8]) -> bool {
    let mut sum = 0;
    for (i, c) in digits.iter().rev().enumerate() {
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

fn is_card(s: &str) -> bool {
    let digits: Vec<u8> = s.bytes().filter(u8::is_ascii_digit).collect();
    (13..=19).contains(&digits.len()) && luhn(&digits)
}

/// Every secret-looking span of `text`, sorted, overlaps merged.
pub fn scan(text: &str) -> Vec<Span> {
    let mut out: Vec<Span> = Vec::new();
    let mut add = |r: Range<usize>, kind| {
        out.push(Span {
            start: r.start,
            end: r.end,
            kind,
        })
    };
    for m in TOKENS.find_iter(text) {
        add(m.range(), "token");
    }
    for m in PRIVATE_KEY.find_iter(text) {
        add(m.range(), "private_key");
    }
    for c in SAID.captures_iter(text) {
        if let Some(m) = c.get(1).or_else(|| c.get(2)) {
            add(m.range(), "said");
        }
    }
    for m in DIGITS.find_iter(text) {
        if is_card(m.as_str()) {
            add(m.range(), "card");
        }
    }
    out.sort_by_key(|s| (s.start, std::cmp::Reverse(s.end)));
    let mut merged: Vec<Span> = Vec::with_capacity(out.len());
    for s in out {
        match merged.last_mut() {
            Some(last) if s.start < last.end => last.end = last.end.max(s.end),
            _ => merged.push(s),
        }
    }
    merged
}

/// `text` with every span replaced by [`REPLACEMENT`]; `None` when nothing was found (store the
/// original then).
pub fn redact(text: &str) -> Option<String> {
    let spans = scan(text);
    if spans.is_empty() {
        return None;
    }
    let mut out = String::with_capacity(text.len());
    let mut pos = 0;
    for s in spans {
        out.push_str(&text[pos..s.start]);
        out.push_str(REPLACEMENT);
        pos = s.end;
    }
    out.push_str(&text[pos..]);
    Some(out)
}

/// What `POST /api/thread` and `POST /api/config/draft` do with a message (§4.6).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Intercepted {
    /// The text to store and route: the original, or with the hits replaced.
    pub text: String,
    /// Something was replaced: store `text`, don't run the normal reply, answer [`REPLY`] (link
    /// [`REPLY_HREF`]).
    pub blocked: bool,
}

/// Runs the guard over a user message.
pub fn intercept(text: &str) -> Intercepted {
    match redact(text) {
        Some(t) => Intercepted {
            text: t,
            blocked: true,
        },
        None => Intercepted {
            text: text.to_string(),
            blocked: false,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn r(s: &str) -> String {
        redact(s).unwrap_or_else(|| s.to_string())
    }

    /// The cases of steve-knowledge's `TokenGuard`, unchanged: this guard must catch all of them.
    #[test]
    fn covers_the_interim_token_guard() {
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
        assert_eq!(r("卡号 4111111111111111"), "卡号 ［已拦下的机密］");
        assert_eq!(r("订单 1234567890123"), "订单 1234567890123");
        assert_eq!(r("ghp_short sk-1 密码是 abc"), "ghp_short sk-1 密码是 abc");
    }

    #[test]
    fn extra_patterns() {
        assert_eq!(r("卡 4111 1111 1111 1111 到期"), "卡 ［已拦下的机密］ 到期");
        assert_eq!(r("4111-1111-1111-1111"), "［已拦下的机密］");
        assert_eq!(r("ASIAABCDEFGHIJKLMNOP"), "［已拦下的机密］");
        assert_eq!(r("glpat-abcdefghijklmnopqrstu"), "［已拦下的机密］");
        assert_eq!(
            r("AIzaSyA1234567890abcdefghijklmnopqrstuv"),
            "［已拦下的机密］"
        );
        assert_eq!(r("password: hunter22"), "password: ［已拦下的机密］");
        assert_eq!(r("PASSWORD=\"s3cr3t!x\""), "PASSWORD=\"［已拦下的机密］\"");
        assert_eq!(r("验证码：382910"), "验证码：［已拦下的机密］");
        assert_eq!(
            r("-----BEGIN PGP PRIVATE KEY BLOCK-----\nxx\n-----END PGP PRIVATE KEY BLOCK-----"),
            "［已拦下的机密］"
        );
        // an unterminated key block is cut to the end
        assert_eq!(
            r("看 -----BEGIN RSA PRIVATE KEY-----\nMIIE"),
            "看 ［已拦下的机密］"
        );
    }

    #[test]
    fn ordinary_text_is_left_alone() {
        for s in [
            "The password is stored in the vault.",
            "电话 138 0013 8000",
            "版本 1.13.1，提交 8c45da2",
            "订单号 2026110100012345",
            "sk-learn 和 ghp 都不是令牌",
            "密码是 abc",
        ] {
            assert_eq!(redact(s), None, "{s}");
        }
    }

    #[test]
    fn spans_merge_and_sit_on_char_boundaries() {
        let t = "密码是 ghp_abcdefghijklmnopqrstuvwxyz0123456789 好";
        let s = scan(t);
        assert_eq!(s.len(), 1);
        assert!(t.is_char_boundary(s[0].start) && t.is_char_boundary(s[0].end));
        assert_eq!(&t[s[0].range()], "ghp_abcdefghijklmnopqrstuvwxyz0123456789");
        let i = intercept(t);
        assert!(i.blocked && !i.text.contains("ghp_"));
        assert!(!intercept("你好").blocked);
    }
}
