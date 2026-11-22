//! Read-only projection of what the PR follower observed. None of these fields authorizes a merge.
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
pub struct PullChecks {
    pub passed: usize,
    pub pending: usize,
    pub failed: usize,
    pub other: usize,
    pub failing: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
pub struct PullSnapshot {
    pub number: u64,
    pub url: String,
    pub head: String,
    pub state: String,
    pub checks: Option<PullChecks>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
pub struct Closeout {
    pub status: &'static str,
    pub next_owner: &'static str,
    pub pr: Option<PullSnapshot>,
    pub last_success_at: Option<i64>,
    pub sync_error: Option<String>,
    pub merge_error: Option<String>,
}
