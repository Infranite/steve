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
