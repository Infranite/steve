//! Evidence from a logical turn's observation window.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StallReason {
    RepeatedFailure,
    NoProgress,
}

impl StallReason {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::RepeatedFailure => "repeated_failure",
            Self::NoProgress => "no_progress",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StallEvidence {
    pub reason: StallReason,
    pub first_at: i64,
    pub detected_at: i64,
    pub repetitions: u8,
    pub event: Option<String>,
}
