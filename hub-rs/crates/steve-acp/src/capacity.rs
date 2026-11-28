//! Provider capacity errors and terminal diagnostics adapters forward in their replies.

use std::sync::LazyLock;

use regex::Regex;

static CAPACITY: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)\b(?:503|529)\b|no eligible upstream account|service unavailable|overloaded")
        .unwrap()
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

#[cfg(test)]
mod tests {
    use super::*;

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
