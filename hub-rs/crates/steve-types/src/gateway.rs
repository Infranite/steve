//! Shared failure vocabulary for a one-shot inference request.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "snake_case")]
pub enum TimeoutPhase {
    Queue,
    FirstByte,
    Execution,
    Unknown,
}

impl TimeoutPhase {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Queue => "queue",
            Self::FirstByte => "first_byte",
            Self::Execution => "execution",
            Self::Unknown => "unknown",
        }
    }
}

impl std::fmt::Display for TimeoutPhase {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Structured Node-side failure evidence. General operation error codes remain unchanged.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum GatewayFailure {
    Timeout { phase: TimeoutPhase },
    NeedsTools,
    Startup,
    OutputLimit,
}
