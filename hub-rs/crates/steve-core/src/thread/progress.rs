//! Bounded, secret-redacted thread progress shaping and threshold constants.

use regex::Regex;
use std::sync::LazyLock;

/// Max characters retained per thought block so live SSE frames and snapshots stay bounded.
pub(crate) const MAX_THOUGHT_CHARS: usize = 4_000;
/// Max thought entries kept in the in-flight snapshot and persisted `meta.steps`.
pub(crate) const MAX_THOUGHTS_IN_SNAPSHOT: usize = 8;
/// Max characters kept for a tool brief or single-line description.
pub(crate) const MAX_TOOL_DESCRIPTION_CHARS: usize = 200;
/// Max characters kept for a tool result excerpt preview.
pub(crate) const MAX_TOOL_EXCERPT_CHARS: usize = 600;
/// Max characters kept for a redacted tool error summary.
pub(crate) const MAX_TOOL_ERROR_CHARS: usize = 300;
/// Max tool entries retained in the `/api/summary` live snapshot.
pub(crate) const MAX_TOOLS_IN_SNAPSHOT: usize = 40;
/// Max plan entries published or persisted per research run.
pub(crate) const MAX_PLAN_ENTRIES: usize = 24;
/// Max characters per plan step label.
pub(crate) const MAX_PLAN_ENTRY_CHARS: usize = 160;
/// Max subtask entries retained in the live snapshot and persisted `meta.steps`.
pub(crate) const MAX_SUBTASKS_IN_SNAPSHOT: usize = 12;
/// Max characters for subtask title, progress, summary, or error lines.
pub(crate) const MAX_SUBTASK_TEXT_CHARS: usize = 240;
/// Minimum interval (ms) between non-terminal SSE emissions for the same streaming thought.
pub(crate) const THOUGHT_THROTTLE_MS: i64 = 120;

const REDACTED: &str = "[REDACTED]";

static PEM_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"-----BEGIN [A-Z0-9 ]*PRIVATE KEY-----[\s\S]*?(?:-----END [A-Z0-9 ]*PRIVATE KEY-----|$)",
    )
    .expect("valid PEM regex")
});

static AUTH_SCHEME_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)\b(Bearer|Basic)\s+[A-Za-z0-9\-._~+/]+=*").expect("valid auth scheme regex")
});

static TOKEN_PREFIX_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\b(?:sk-[A-Za-z0-9_-]{12,}|gh[pousr]_[A-Za-z0-9_]{12,}|github_pat_[A-Za-z0-9_]{12,}|xox[baprs]-[A-Za-z0-9-]{10,}|AIza[0-9A-Za-z_-]{20,})\b")
        .expect("valid token prefix regex")
});

static KV_SECRET_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"(?i)\b(api[_-]?key|access[_-]?token|refresh[_-]?token|auth[_-]?token|secret[_-]?key|client[_-]?secret|token|secret|password|passwd|authorization)\b(\s*[:=]\s*)["']?([^\s"',;\])\}]+)["']?"#)
        .expect("valid kv secret regex")
});

/// Redact credential patterns from `raw`, trim outer whitespace, and clip cleanly to `max_chars`.
pub(crate) fn redact_and_bound(raw: &str, max_chars: usize) -> String {
    let trimmed = raw.trim();
    if trimmed.is_empty() || max_chars == 0 {
        return String::new();
    }
    let stage1 = PEM_RE.replace_all(trimmed, REDACTED);
    let stage2 = AUTH_SCHEME_RE.replace_all(&stage1, "$1 [REDACTED]");
    let stage3 = TOKEN_PREFIX_RE.replace_all(&stage2, REDACTED);
    let stage4 = KV_SECRET_RE.replace_all(&stage3, |caps: &regex::Captures<'_>| {
        let value = &caps[3];
        if value.eq_ignore_ascii_case("bearer")
            || value.eq_ignore_ascii_case("basic")
            || value == REDACTED
        {
            caps[0].to_string()
        } else {
            format!("{}{}{REDACTED}", &caps[1], &caps[2])
        }
    });
    if stage4 != trimmed {
        metrics::counter!("steve_thread_progress_redactions_total").increment(1);
    }

    let mut count = 0usize;
    let mut cut = None;
    for (idx, _) in stage4.char_indices() {
        if count == max_chars {
            cut = Some(idx);
            break;
        }
        count += 1;
    }
    match cut {
        Some(idx) => {
            let prefix = stage4[..idx].trim_end();
            format!("{prefix}…")
        }
        None => stage4.into_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redacts_secrets_and_preserves_context() {
        let raw = "Authorization: Bearer abc.def.ghi\nAuthorization: Basic dXNlcjpwYXNz\ntoken=secret_123\napi_key=my_secret_value_123\nghp_0123456789abcdef\n-----BEGIN RSA PRIVATE KEY-----\nMIIEowIBAAK\n-----END RSA PRIVATE KEY-----\ntoken usage: 42 tokens";
        let out = redact_and_bound(raw, 500);
        assert!(!out.contains("abc.def.ghi"), "{out}");
        assert!(!out.contains("dXNlcjpwYXNz"), "{out}");
        assert!(!out.contains("secret_123"), "{out}");
        assert!(!out.contains("my_secret_value_123"), "{out}");
        assert!(!out.contains("ghp_0123456789abcdef"), "{out}");
        assert!(!out.contains("MIIEowIBAAK"), "{out}");
        assert!(out.contains("Authorization: Bearer [REDACTED]"), "{out}");
        assert!(out.contains("Authorization: Basic [REDACTED]"), "{out}");
        assert!(out.contains("token=[REDACTED]"), "{out}");
        assert!(out.contains("api_key=[REDACTED]"), "{out}");
        assert!(out.contains("token usage: 42 tokens"), "{out}");
    }

    #[test]
    fn bounds_on_utf8_boundaries_for_cjk_and_ascii() {
        let raw = "  正在读取主线代码，验证状态机转换与测试覆盖率  ";
        let out = redact_and_bound(raw, 8);
        assert_eq!(out, "正在读取主线代码…");
        assert_eq!(redact_and_bound("   ", 20), "");
        assert_eq!(redact_and_bound("short", 20), "short");
    }
}
