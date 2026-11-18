//! How an adapter's errors read: what the Node's driver decides a turn's outcome by.

use std::sync::LazyLock;

use regex::Regex;

/// A Claude Code agent behind claude-agent-acp that is gone (its process died, the session it held
/// crashed): every later prompt on that adapter fails with "Resource not found". The adapter must be
/// dropped so the next turn starts a fresh one that loads the session from the harness's history.
static DEAD_AGENT: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)process exited unexpectedly|start a new session|resource not found|agent process died")
        .expect("static regex")
});

static TRANSIENT: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?i)(?-u:\b)(429|502|503|504|529)(?-u:\b)|temporarily|overloaded|try again|rate.?limit",
    )
    .expect("static regex")
});

static STREAM_INTERRUPTION: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(concat!(
        r"(?i)upstream stream interrupted|stream disconnected before completion|",
        r"stream (?:ended|closed|terminated) (?:unexpectedly|prematurely)|",
        r"connection reset by peer|connection closed unexpectedly|socket hang up|",
        r"unexpected eof|incomplete chunked read|",
        r"peer closed connection without sending complete message body|",
        r"^(?:(?:model|upstream)[ _-])?(?:stream|upstream|provider|transport|network|connection)[ _-](?:error|interrupted|disconnected|reset|failed)$",
    ))
    .expect("static regex")
});

/// A provider/transport failure, from an adapter's error or stop reason. This is deliberately
/// narrower than every transient error: quotas, authentication and agent exits keep their paths.
pub fn is_stream_interruption(message: &str) -> bool {
    STREAM_INTERRUPTION.is_match(message.trim())
}

/// Some adapters put the failure in assistant output and still return `end_turn`. Only a final,
/// standalone diagnostic is evidence there; quoted examples and ordinary reports are not errors.
pub fn stream_interruption_tail(text: &str) -> Option<&str> {
    let line = text.lines().rev().find(|l| !l.trim().is_empty())?.trim();
    let diagnostic = line
        .strip_prefix("Error: ")
        .or_else(|| line.strip_prefix("error: "))
        .or_else(|| line.strip_prefix("API Error: "))
        .unwrap_or(line);
    let lower = diagnostic.to_ascii_lowercase();
    [
        "stream disconnected before completion",
        "upstream stream interrupted",
        "stream ended unexpectedly",
        "stream closed unexpectedly",
        "stream terminated unexpectedly",
    ]
    .iter()
    .any(|prefix| lower.starts_with(prefix))
    .then_some(line)
}

/// A failure that may pass by itself (gateway 5xx, overload, rate limit).
pub fn is_transient(message: &str) -> bool {
    TRANSIENT.is_match(message)
}

/// The agent behind the adapter is gone: the adapter must be dropped.
pub fn is_dead_agent(message: &str) -> bool {
    DEAD_AGENT.is_match(message)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn provider_stream_errors_and_terminal_diagnostics() {
        for error in [
            "stream disconnected before completion: upstream stream interrupted",
            "Internal error: connection reset by peer",
            "peer closed connection without sending complete message body (incomplete chunked read)",
            "socket hang up",
            "unexpected EOF",
            "transport_error",
            "stream_interrupted",
            "model_stream_interrupted",
            "upstream_stream_interrupted",
            "provider_error",
        ] {
            assert!(is_stream_interruption(error), "{error}");
        }
        for other in [
            "end_turn",
            "cancelled",
            "max_tokens",
            "invalid api key",
            "adapter exited (1)",
        ] {
            assert!(!is_stream_interruption(other), "{other}");
        }
        assert_eq!(
            stream_interruption_tail(
                "准备提交\nstream disconnected before completion: upstream stream interrupted\n"
            ),
            Some("stream disconnected before completion: upstream stream interrupted")
        );
        for text in [
            "修复了 `upstream stream interrupted` 的识别。",
            "错误示例：stream disconnected before completion: upstream stream interrupted",
            "`stream disconnected before completion: upstream stream interrupted`",
            "stream disconnected before completion: upstream stream interrupted\n已恢复并完成。",
        ] {
            assert!(stream_interruption_tail(text).is_none(), "{text}");
        }
    }

    #[test]
    fn dead_agent_errors() {
        for m in [
            "Internal error: The Claude Agent process exited unexpectedly. Please start a new session.",
            "Resource not found",
        ] {
            assert!(is_dead_agent(m), "{m}");
        }
        assert!(!is_dead_agent("503 no eligible upstream account"));
    }

    #[test]
    fn transient_errors() {
        for m in [
            "API Error: 503 Service Unavailable",
            "Overloaded",
            "rate limit reached",
            "ratelimit",
            "429",
            "please try again",
            "temporarily down",
            "错误503",
        ] {
            assert!(is_transient(m), "{m}");
        }
        for m in [
            "5030",
            "invalid api key",
            "adapter exited (1)",
            "timeout session/prompt",
        ] {
            assert!(!is_transient(m), "{m}");
        }
    }
}
