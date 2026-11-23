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
