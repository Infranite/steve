//! Provider capacity errors and the recovery hints adapters forward in their diagnostics.
//! No provider name or local clock zone is needed to interpret an absolute recovery time.

use std::sync::LazyLock;

use chrono::DateTime;
use regex::Regex;

static CAPACITY: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)\b(?:503|529)\b|no eligible upstream account|service unavailable|overloaded")
        .unwrap()
});
static RECOVERY: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(concat!(
        r"(?i)expected to be available at\s*<?",
        r"(\d{4}-\d{2}-\d{2}[Tt]\d{2}:\d{2}:\d{2}(?:\.\d+)?(?:[Zz]|[+-]\d{2}:\d{2}))",
    ))
    .unwrap()
});
static RETRY_AFTER: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"(?i)\bretry[- ]after["']?\s*[:=]\s*["']?([^\r\n"'<>;]+)"#).unwrap()
});

/// This is narrower than the general gateway retry: authentication and 429 quota failures keep
/// their existing policies. A capacity failure may also happen after tools have run.
pub fn is_capacity_error(error: &str) -> bool {
    CAPACITY.is_match(error)
}

/// Only a terminal standalone diagnostic in the final assistant message can turn a successful
/// adapter completion into a failure. Quotes, reports, and text followed by success are excluded.
pub fn capacity_error_tail(text: &str) -> Option<&str> {
    let text = text.trim_end();
    let mut start = text.len();
    for line in text.split_inclusive('\n').rev() {
        start = start.checked_sub(line.len())?;
        let diagnostic = line.trim();
        let diagnostic = diagnostic
            .strip_prefix("API Error: ")
            .or_else(|| diagnostic.strip_prefix("Error: "))
            .or_else(|| diagnostic.strip_prefix("error: "))
            .unwrap_or(diagnostic);
        let lower = diagnostic.to_ascii_lowercase();
        if [
            "503",
            "529",
            "unexpected status 503",
            "unexpected status 529",
            "http 503",
            "http 529",
            "http/1.1 503",
            "http/2 503",
            "no eligible upstream account",
            "service unavailable",
            "overloaded",
        ]
        .iter()
        .any(|prefix| lower.starts_with(prefix))
            && is_capacity_error(diagnostic)
        {
            return Some(&text[start..]);
        }
        if !lower.starts_with("retry-after:") && !lower.starts_with("expected to be available at") {
            return None;
        }
    }
    None
}

/// The latest valid advertised instant, in milliseconds. RFC 3339 has an explicit offset;
/// Retry-After accepts both delta-seconds and HTTP-date, including obsolete HTTP date formats.
/// Invalid/overflowing hints are ignored, never interpreted as an immediate retry.
pub fn capacity_recovery_at(error: &str, now: i64) -> Option<i64> {
    let absolute = RECOVERY.captures_iter(error).filter_map(|c| {
        DateTime::parse_from_rfc3339(&c[1])
            .ok()
            .map(|d| d.timestamp_millis())
    });
    let after = RETRY_AFTER.captures_iter(error).filter_map(|c| {
        let value = c[1].trim().trim_end_matches(['}', ',', '.']);
        if value.bytes().all(|b| b.is_ascii_digit()) {
            let seconds = value.parse::<i64>().ok()?;
            now.checked_add(seconds.checked_mul(1000)?)
        } else {
            let date = httpdate::parse_http_date(value).ok()?;
            i64::try_from(date.duration_since(std::time::UNIX_EPOCH).ok()?.as_millis()).ok()
        }
    });
    absolute.chain(after).max()
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: i64 = 1_794_988_800_000; // 2026-11-18T08:00:00Z

    #[test]
    fn recovery_hints_include_offsets_seconds_and_all_http_dates() {
        for hint in [
            "expected to be available at 2026-11-18T08:02:00Z",
            "expected to be available at <2026-11-18T16:02:00+08:00>",
            "Retry-After: 120",
            "Retry-After: Wed, 18 Nov 2026 08:02:00 GMT",
            "Retry-After: Wednesday, 18-Nov-26 08:02:00 GMT",
            "Retry-After: Wed Nov 18 08:02:00 2026",
            r#"{"Retry-After":"120"}"#,
        ] {
            assert_eq!(
                capacity_recovery_at(hint, NOW),
                Some(NOW + 120_000),
                "{hint}"
            );
        }
        assert_eq!(
            capacity_recovery_at(
                "expected to be available at 2026-11-18T08:01:00Z; Retry-After: 120",
                NOW
            ),
            Some(NOW + 120_000)
        );
    }

    #[test]
    fn invalid_past_and_extreme_hints_are_safe() {
        for hint in [
            "Retry-After: -1",
            "Retry-After: NaN",
            "Retry-After: 9223372036854775807",
            "Retry-After: 999999999999999999999999999",
            "expected to be available at 2026-99-99T08:02:00Z",
            "request created 2026-11-18T08:02:00Z",
        ] {
            assert_eq!(capacity_recovery_at(hint, NOW), None, "{hint}");
        }
        assert_eq!(capacity_recovery_at("Retry-After: 0", NOW), Some(NOW));
        assert_eq!(
            capacity_recovery_at("expected to be available at 2026-11-18T07:59:00Z", NOW),
            Some(NOW - 60_000)
        );
    }

    #[test]
    fn only_final_standalone_capacity_diagnostics_supply_evidence() {
        for error in [
            "503 Service Unavailable: no eligible upstream account is currently available",
            "API Error: 503 Service Unavailable",
            "Error: no eligible upstream account",
            "Error: unexpected status 503 Service Unavailable",
            "HTTP/1.1 503 Service Unavailable",
            "503 Service Unavailable\r\nRetry-After: 120",
            "503 Service Unavailable\nRetry-After: 120",
        ] {
            assert_eq!(
                capacity_error_tail(&format!("finished a tool\n{error}\n")),
                Some(error),
                "{error}"
            );
        }
        for text in [
            "example: 503 Service Unavailable",
            "`503 Service Unavailable`",
            "> 503 Service Unavailable",
            "503 Service Unavailable\n已恢复完成",
            "修好了 503 Service Unavailable",
            "5030",
            "429 rate limit",
            "invalid api key",
        ] {
            assert_eq!(capacity_error_tail(text), None, "{text}");
        }
    }
}
