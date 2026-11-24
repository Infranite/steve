//! A declared pull request and the exact range accepted for delivery.

use serde::{Deserialize, Serialize};

use crate::{CloseKind, ItemId};

/// The item state that an asynchronous delivery receipt can attest to.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeliveryVersion {
    pub cycle: i64,
    pub revision: i64,
    pub scope: i64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
pub struct DeliveryRef {
    pub repository: String,
    pub number: i64,
    pub target: String,
    pub head: String,
    /// Absent on a historical association whose accepted range could not be recovered.
    pub base: Option<String>,
    pub required: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DeliveryBaseline {
    pub item: ItemId,
    pub cycle: i64,
    pub revision: i64,
    pub scope: i64,
    pub refs: Vec<DeliveryRef>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PullProof {
    pub repository: String,
    pub number: i64,
    pub target_head: String,
}

/// Evidence sampled together for the exact required PR ranges of a close request.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DeliveryProof {
    pub baseline: DeliveryBaseline,
    pub pulls: Vec<PullProof>,
}

/// The accepted or pending close request of the current cycle.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct CloseRequest {
    pub kind: CloseKind,
    pub reason: String,
    pub by: String,
    pub at: i64,
    pub frozen_revision: i64,
    pub scope_revision: i64,
    /// A parent acceptance is for exactly this graph. A committed replan followed by a crash
    /// cannot resurrect the previous acceptance after the new work finishes.
    #[serde(default)]
    pub workflow_revision: Option<i64>,
    /// Monotonic `wf_runs.run_revision` of the active script workflow run when delivery froze.
    #[serde(default)]
    pub wf_run_revision: Option<i64>,
    /// The worktree's `HEAD` and changes when the result was frozen; `None` without a git worktree.
    #[serde(default)]
    pub fingerprint: Option<String>,
    /// The worktree is on a Node and its fingerprint was not there yet when the result was frozen:
    /// it is taken when its facts come (`node_git`), and nothing closes before.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub print_pending: bool,
    #[serde(default)]
    pub policy: Option<ClosePolicy>,
    pub accepted: bool,
    /// The acceptance decision (human policy).
    #[serde(default)]
    pub decision: Option<String>,
    /// What the acceptance rests on (checks, a module's receipt, who marked it done).
    #[serde(default)]
    pub evidence: Vec<String>,
    /// The termination rule of a failure decided by a rule.
    #[serde(default)]
    pub rule: Option<String>,
}

/// How an acceptance is decided.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClosePolicy {
    Human,
    Automatic,
    Waived,
    Owner,
}

impl ClosePolicy {
    pub fn as_str(self) -> &'static str {
        match self {
            ClosePolicy::Human => "human",
            ClosePolicy::Automatic => "automatic",
            ClosePolicy::Waived => "waived",
            ClosePolicy::Owner => "owner",
        }
    }
}
