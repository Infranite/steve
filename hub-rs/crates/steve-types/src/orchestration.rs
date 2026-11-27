//! Evidence from a logical turn's observation window.

use serde::{Deserialize, Serialize};

pub const TURN_STALL: &str = "turn_stall";
/// Typed `stop_reason` recorded when Steve cancels a turn due to repeated identical tool failures.
pub const STOP_REASON_STALL_REPEATED_FAILURE: &str = "turn_stall_repeated_failure";
/// Typed `stop_reason` recorded when Steve cancels a turn due to lack of verified progress.
pub const STOP_REASON_STALL_NO_PROGRESS: &str = "turn_stall_no_progress";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(test, derive(ts_rs::TS))]
pub enum StallReason {
    RepeatedFailure,
    NoProgress,
}

impl StallReason {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::RepeatedFailure => "repeated_failure",
            Self::NoProgress => "no_progress",
        }
    }

    pub const fn stop_reason(self) -> &'static str {
        match self {
            Self::RepeatedFailure => STOP_REASON_STALL_REPEATED_FAILURE,
            Self::NoProgress => STOP_REASON_STALL_NO_PROGRESS,
        }
    }

    pub fn from_stop_reason(stop_reason: &str) -> Option<Self> {
        match stop_reason {
            STOP_REASON_STALL_REPEATED_FAILURE => Some(Self::RepeatedFailure),
            STOP_REASON_STALL_NO_PROGRESS => Some(Self::NoProgress),
            _ => None,
        }
    }
}

/// Kind of active in-flight work still running when a turn stalled.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(test, derive(ts_rs::TS))]
pub enum StallBlockerKind {
    Tool,
    Subagent,
    BackgroundTask,
    NativeWorkflow,
}

impl StallBlockerKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Tool => "tool",
            Self::Subagent => "subagent",
            Self::BackgroundTask => "background_task",
            Self::NativeWorkflow => "native_workflow",
        }
    }
}

/// Bounded summary of the active tool or subtask still running when a turn stalled.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
pub struct StallBlocker {
    pub kind: StallBlockerKind,
    pub label: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StallEvidence {
    pub reason: StallReason,
    pub first_at: i64,
    pub detected_at: i64,
    pub repetitions: u8,
    pub event: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub observed_ms: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub blocker: Option<StallBlocker>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReviewedUnit {
    pub unit: String,
    pub attempt: i64,
    pub input_digest: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReviewScope {
    pub cycle: i64,
    pub graph_revision: i64,
    pub review_unit: String,
    pub review_attempt: i64,
    pub units: Vec<ReviewedUnit>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Finding {
    pub affected_unit: String,
    pub criterion: String,
    pub evidence_ref: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReworkReason {
    ReviewFinding,
    DependencyOutputChanged,
    EvidenceUnverifiable,
    ExplicitRetry,
}

impl ReworkReason {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ReviewFinding => "review_finding",
            Self::DependencyOutputChanged => "dependency_output_changed",
            Self::EvidenceUnverifiable => "evidence_unverifiable",
            Self::ExplicitRetry => "explicit_retry",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReviewReceipt {
    Applied,
    Replay,
    Unverified,
}
