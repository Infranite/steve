//! Strongly-typed `sea-query` repository for the dynamic script workflow ledger (WF-3).
//!
//! Operates identically over SQLite and PostgreSQL via [`Read`] / [`W`] handles:
//! - 2 core aggregates: `wf_runs`, `wf_calls`
//! - 3 strict subordinate tables: `wf_script_versions`, `wf_call_attempts`, `wf_version_calls`
//! - Zero cached budget counters (budgets computed via live SQL `COUNT(*)`)
//! - Atomic two-phase delivery settlement (`stage_attempt_closing_in` + `finalize_attempt_in`)
//! - Authoritative 4-rule Hub independent final review verification

use std::str::FromStr;

use sea_query::{Expr, ExprTrait, Func, JoinType, OnConflict, Order, Query};
use serde::{Deserialize, Serialize};
use steve_types::{DomainEvent, WorkEvent};

use super::{Q, Row};
use crate::{Read, Result, StoreError, W, now, uid};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WfRunStatus {
    Running,
    Paused,
    Completed,
    Failed,
    Cancelled,
    Superseded,
}

impl WfRunStatus {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Running => "running",
            Self::Paused => "paused",
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
            Self::Superseded => "superseded",
        }
    }

    pub const fn is_terminal(self) -> bool {
        matches!(
            self,
            Self::Completed | Self::Failed | Self::Cancelled | Self::Superseded
        )
    }
}

impl FromStr for WfRunStatus {
    type Err = StoreError;

    fn from_str(s: &str) -> Result<Self> {
        match s {
            "running" => Ok(Self::Running),
            "paused" => Ok(Self::Paused),
            "completed" => Ok(Self::Completed),
            "failed" => Ok(Self::Failed),
            "cancelled" => Ok(Self::Cancelled),
            "superseded" => Ok(Self::Superseded),
            other => Err(StoreError::Storage(format!(
                "invalid wf_run status: {other}"
            ))),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WfCallStatus {
    Queued,
    Dispatched,
    Completed,
    Failed,
    Cancelled,
}

impl WfCallStatus {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::Dispatched => "dispatched",
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
        }
    }

    pub const fn is_active(self) -> bool {
        matches!(self, Self::Queued | Self::Dispatched)
    }
}

impl FromStr for WfCallStatus {
    type Err = StoreError;

    fn from_str(s: &str) -> Result<Self> {
        match s {
            "queued" => Ok(Self::Queued),
            "dispatched" => Ok(Self::Dispatched),
            "completed" => Ok(Self::Completed),
            "failed" => Ok(Self::Failed),
            "cancelled" => Ok(Self::Cancelled),
            other => Err(StoreError::Storage(format!(
                "invalid wf_call status: {other}"
            ))),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WfAttemptStatus {
    Blocked,
    Dispatched,
    Closing,
    Succeeded,
    Failed,
    Cancelled,
}

impl WfAttemptStatus {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Blocked => "blocked",
            Self::Dispatched => "dispatched",
            Self::Closing => "closing",
            Self::Succeeded => "succeeded",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
        }
    }

    pub const fn is_active(self) -> bool {
        matches!(self, Self::Blocked | Self::Dispatched | Self::Closing)
    }
}

impl FromStr for WfAttemptStatus {
    type Err = StoreError;

    fn from_str(s: &str) -> Result<Self> {
        match s {
            "blocked" => Ok(Self::Blocked),
            "dispatched" => Ok(Self::Dispatched),
            "closing" => Ok(Self::Closing),
            "succeeded" => Ok(Self::Succeeded),
            "failed" => Ok(Self::Failed),
            "cancelled" => Ok(Self::Cancelled),
            other => Err(StoreError::Storage(format!(
                "invalid wf_call_attempt status: {other}"
            ))),
        }
    }
}

/// Strongly-typed machine-readable pause reason stored in `wf_runs.pause_reason_code`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WfPauseReason {
    ScriptFault,
    InfrastructureFault,
    NonDeterministicReplay,
    BudgetExhausted,
    PlacementBlocked,
    ReviewGateBlocked,
    OwnerPaused,
    CoordinatorPaused,
}

impl WfPauseReason {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ScriptFault => "script_fault",
            Self::InfrastructureFault => "infrastructure_fault",
            Self::NonDeterministicReplay => "non_deterministic_replay",
            Self::BudgetExhausted => "budget_exhausted",
            Self::PlacementBlocked => "placement_blocked",
            Self::ReviewGateBlocked => "review_gate_blocked",
            Self::OwnerPaused => "owner_paused",
            Self::CoordinatorPaused => "coordinator_paused",
        }
    }

    /// True when resuming requires an explicit owner decision rather than an autonomous coordinator
    /// `resume` action.
    pub const fn is_owner_gated(self) -> bool {
        matches!(
            self,
            Self::ScriptFault
                | Self::InfrastructureFault
                | Self::NonDeterministicReplay
                | Self::BudgetExhausted
                | Self::PlacementBlocked
                | Self::ReviewGateBlocked
        )
    }
}

impl std::fmt::Display for WfPauseReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for WfPauseReason {
    type Err = StoreError;

    fn from_str(s: &str) -> Result<Self> {
        match s {
            "script_fault" => Ok(Self::ScriptFault),
            "infrastructure_fault" => Ok(Self::InfrastructureFault),
            "non_deterministic_replay" => Ok(Self::NonDeterministicReplay),
            "budget_exhausted" => Ok(Self::BudgetExhausted),
            "placement_blocked" => Ok(Self::PlacementBlocked),
            "review_gate_blocked" => Ok(Self::ReviewGateBlocked),
            "owner_paused" => Ok(Self::OwnerPaused),
            "coordinator_paused" => Ok(Self::CoordinatorPaused),
            other => Err(StoreError::Storage(format!(
                "invalid wf_run pause_reason_code: {other}"
            ))),
        }
    }
}

/// Strongly-typed terminal outcome of a workflow child item when finalizing its active attempt.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WfChildOutcome {
    Completed,
    Failed,
    Cancelled,
}

impl FromStr for WfChildOutcome {
    type Err = StoreError;

    fn from_str(s: &str) -> Result<Self> {
        match s {
            "completed" => Ok(Self::Completed),
            "failed" => Ok(Self::Failed),
            "cancelled" => Ok(Self::Cancelled),
            other => Err(StoreError::Storage(format!(
                "invalid workflow child outcome: {other}"
            ))),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WfRun {
    pub id: String,
    pub workspace_id: String,
    pub parent_item_id: String,
    pub parent_cycle: i64,
    pub goal_version: i64,
    pub status: WfRunStatus,
    pub pause_reason_code: Option<WfPauseReason>,
    pub active_script_version: i64,
    pub run_revision: i64,
    pub single_session_override: bool,
    pub review_verified_call_id: Option<String>,
    pub extra_attempts_budget: i64,
    pub result_json: Option<String>,
    pub created_at: i64,
    pub updated_at: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WfScriptVersion {
    pub run_id: String,
    pub version: i64,
    pub script_source: String,
    pub script_sha256: String,
    pub created_by_session_id: Option<String>,
    pub created_at: i64,
}

/// Verifiable Git tree/commit output fingerprint captured when a workflow child delivers.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WfGitFingerprint {
    pub head: String,
    pub tree: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WfCall {
    pub id: String,
    pub run_id: String,
    pub workspace_id: String,
    pub identity_hash: String,
    pub phase: Option<String>,
    pub title: Option<String>,
    pub prompt: String,
    pub options_canonical_json: String,
    pub schema_json: Option<String>,
    pub occurrence: i64,
    pub is_review: bool,
    pub mutex_key: Option<String>,
    pub status: WfCallStatus,
    pub result_json: Option<String>,
    pub artifact_ids_json: Option<String>,
    pub git_head: Option<String>,
    pub git_tree: Option<String>,
    pub git_cwd: Option<String>,
    pub settled_at_revision: Option<i64>,
    pub created_at: i64,
    pub updated_at: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WfCallAttempt {
    pub id: String,
    pub call_id: String,
    pub run_id: String,
    pub parent_item_id: String,
    pub parent_cycle: i64,
    pub attempt_no: i64,
    pub child_item_id: Option<String>,
    pub bot_id: Option<String>,
    pub placement_id: Option<String>,
    pub harness: Option<String>,
    pub model: Option<String>,
    pub status: WfAttemptStatus,
    pub dispatched_at_revision: i64,
    pub closing_delivery_revision: Option<i64>,
    pub candidate_result_json: Option<String>,
    pub candidate_artifact_ids_json: Option<String>,
    pub candidate_git_head: Option<String>,
    pub candidate_git_tree: Option<String>,
    pub candidate_git_cwd: Option<String>,
    pub error_code: Option<String>,
    pub error_message: Option<String>,
    pub created_at: i64,
    pub settled_at: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WfVersionCall {
    pub run_id: String,
    pub script_version: i64,
    pub seq_no: i64,
    pub call_id: String,
    pub hydrated_from_version: Option<i64>,
}

#[derive(Debug, Clone)]
pub struct NewWfCall<'a> {
    pub run_id: &'a str,
    pub workspace_id: &'a str,
    pub identity_hash: &'a str,
    pub phase: Option<&'a str>,
    pub title: Option<&'a str>,
    pub prompt: &'a str,
    pub options_canonical_json: &'a str,
    pub schema_json: Option<&'a str>,
    pub occurrence: i64,
    pub is_review: bool,
    pub mutex_key: Option<&'a str>,
}

#[derive(Debug, Clone)]
pub struct NewWfAttempt<'a> {
    pub call_id: &'a str,
    pub run_id: &'a str,
    pub parent_item_id: &'a str,
    pub parent_cycle: i64,
    pub attempt_no: i64,
    pub child_item_id: Option<&'a str>,
    pub bot_id: Option<&'a str>,
    pub placement_id: Option<&'a str>,
    pub harness: Option<&'a str>,
    pub model: Option<&'a str>,
    pub status: WfAttemptStatus,
    pub dispatched_at_revision: i64,
    pub error_code: Option<&'a str>,
    pub error_message: Option<&'a str>,
}

/// Authoritative Hub verification of the 4 independent final review rules (§2.5).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum ReviewGateVerdict {
    /// No non-review work calls were executed in this run, or owner explicitly overrode.
    NotRequired,
    /// Passed all 4 Hub-authoritative review rules.
    Verified { review_call_id: String },
    /// Blocked from closing parent delivery until a valid fresh independent review succeeds.
    Blocked { reason: ReviewGateFailure },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "code", rename_all = "snake_case")]
pub enum ReviewGateFailure {
    /// At least one non-review work call ran, but no succeeded review call exists.
    MissingReview,
    /// A review call ran, but none returned `{ approved: true }`.
    NotApproved,
    /// Every approved review was executed by a Bot that also executed non-review work in this cycle.
    SameBotAsWorker { bot_id: String },
    /// Every approved independent review was dispatched at or before the latest non-review settlement revision.
    StaleReview {
        review_dispatched_revision: i64,
        latest_work_settled_revision: i64,
    },
    /// At least one non-review call is still queued/dispatched/closing.
    ActiveWorkRemaining,
}

impl ReviewGateFailure {
    pub const fn code(&self) -> &'static str {
        match self {
            Self::MissingReview => "missing_review",
            Self::NotApproved => "review_not_approved",
            Self::SameBotAsWorker { .. } => "same_bot_as_worker",
            Self::StaleReview { .. } => "stale_review",
            Self::ActiveWorkRemaining => "active_work_remaining",
        }
    }
}

/// Result of atomically finalizing a workflow child item inside `cycle::commit_cycle`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FinalizedAttempt {
    pub run_id: String,
    pub call_id: String,
    pub attempt_id: String,
    pub parent_item_id: String,
    pub new_run_revision: i64,
    pub status: WfAttemptStatus,
}

const RUN_COLS: [&str; 15] = [
    "id",
    "workspace_id",
    "parent_item_id",
    "parent_cycle",
    "goal_version",
    "status",
    "pause_reason_code",
    "active_script_version",
    "run_revision",
    "single_session_override",
    "review_verified_call_id",
    "extra_attempts_budget",
    "result_json",
    "created_at",
    "updated_at",
];

const CALL_COLS: [&str; 21] = [
    "id",
    "run_id",
    "workspace_id",
    "identity_hash",
    "phase",
    "title",
    "prompt",
    "options_canonical_json",
    "schema_json",
    "occurrence",
    "is_review",
    "mutex_key",
    "status",
    "result_json",
    "artifact_ids_json",
    "git_head",
    "git_tree",
    "git_cwd",
    "settled_at_revision",
    "created_at",
    "updated_at",
];

const ATTEMPT_COLS: [&str; 23] = [
    "id",
    "call_id",
    "run_id",
    "parent_item_id",
    "parent_cycle",
    "attempt_no",
    "child_item_id",
    "bot_id",
    "placement_id",
    "harness",
    "model",
    "status",
    "dispatched_at_revision",
    "closing_delivery_revision",
    "candidate_result_json",
    "candidate_artifact_ids_json",
    "candidate_git_head",
    "candidate_git_tree",
    "candidate_git_cwd",
    "error_code",
    "error_message",
    "created_at",
    "settled_at",
];

fn decode_run(r: &Row<'_>) -> Result<WfRun> {
    Ok(WfRun {
        id: r.get(0)?,
        workspace_id: r.get(1)?,
        parent_item_id: r.get(2)?,
        parent_cycle: r.get(3)?,
        goal_version: r.get(4)?,
        status: r.get::<String>(5)?.parse()?,
        pause_reason_code: r.get::<Option<String>>(6)?.map(|s| s.parse()).transpose()?,
        active_script_version: r.get(7)?,
        run_revision: r.get(8)?,
        single_session_override: r.get::<i64>(9)? != 0,
        review_verified_call_id: r.get(10)?,
        extra_attempts_budget: r.get(11)?,
        result_json: r.get(12)?,
        created_at: r.get(13)?,
        updated_at: r.get(14)?,
    })
}

fn decode_call(r: &Row<'_>) -> Result<WfCall> {
    Ok(WfCall {
        id: r.get(0)?,
        run_id: r.get(1)?,
        workspace_id: r.get(2)?,
        identity_hash: r.get(3)?,
        phase: r.get(4)?,
        title: r.get(5)?,
        prompt: r.get(6)?,
        options_canonical_json: r.get(7)?,
        schema_json: r.get(8)?,
        occurrence: r.get(9)?,
        is_review: r.get::<i64>(10)? != 0,
        mutex_key: r.get(11)?,
        status: r.get::<String>(12)?.parse()?,
        result_json: r.get(13)?,
        artifact_ids_json: r.get(14)?,
        git_head: r.get(15)?,
        git_tree: r.get(16)?,
        git_cwd: r.get(17)?,
        settled_at_revision: r.get(18)?,
        created_at: r.get(19)?,
        updated_at: r.get(20)?,
    })
}

fn decode_attempt(r: &Row<'_>) -> Result<WfCallAttempt> {
    Ok(WfCallAttempt {
        id: r.get(0)?,
        call_id: r.get(1)?,
        run_id: r.get(2)?,
        parent_item_id: r.get(3)?,
        parent_cycle: r.get(4)?,
        attempt_no: r.get(5)?,
        child_item_id: r.get(6)?,
        bot_id: r.get(7)?,
        placement_id: r.get(8)?,
        harness: r.get(9)?,
        model: r.get(10)?,
        status: r.get::<String>(11)?.parse()?,
        dispatched_at_revision: r.get(12)?,
        closing_delivery_revision: r.get(13)?,
        candidate_result_json: r.get(14)?,
        candidate_artifact_ids_json: r.get(15)?,
        candidate_git_head: r.get(16)?,
        candidate_git_tree: r.get(17)?,
        candidate_git_cwd: r.get(18)?,
        error_code: r.get(19)?,
        error_message: r.get(20)?,
        created_at: r.get(21)?,
        settled_at: r.get(22)?,
    })
}

fn emit_run_changed(w: &mut W<'_>, ws: &str, run_id: &str, parent_item_id: &str) {
    w.emit(DomainEvent::from(WorkEvent::WorkflowRun {
        id: run_id.to_owned(),
        item_id: parent_item_id.into(),
        ws: ws.into(),
    }));
}

pub struct WorkflowRuns<'a, C>(pub &'a mut C);

impl<C: Read> WorkflowRuns<'_, C> {
    pub fn get_run(&mut self, run_id: &str) -> Result<Option<WfRun>> {
        self.0.one(
            Query::select()
                .columns(RUN_COLS)
                .from("wf_runs")
                .and_where(Expr::col("id").eq(run_id)),
            decode_run,
        )
    }

    pub fn run_for_goal(
        &mut self,
        ws: &str,
        parent_item_id: &str,
        parent_cycle: i64,
        goal_version: i64,
    ) -> Result<Option<WfRun>> {
        self.0.one(
            Query::select()
                .columns(RUN_COLS)
                .from("wf_runs")
                .and_where(Expr::col("workspace_id").eq(ws))
                .and_where(Expr::col("parent_item_id").eq(parent_item_id))
                .and_where(Expr::col("parent_cycle").eq(parent_cycle))
                .and_where(Expr::col("goal_version").eq(goal_version)),
            decode_run,
        )
    }

    /// Returns the newest non-superseded run for `(parent_item_id, parent_cycle)`, if any.
    pub fn latest_run_for_cycle(
        &mut self,
        parent_item_id: &str,
        parent_cycle: i64,
    ) -> Result<Option<WfRun>> {
        self.0.one(
            Query::select()
                .columns(RUN_COLS)
                .from("wf_runs")
                .and_where(Expr::col("parent_item_id").eq(parent_item_id))
                .and_where(Expr::col("parent_cycle").eq(parent_cycle))
                .and_where(Expr::col("status").ne(WfRunStatus::Superseded.as_str()))
                .order_by("goal_version", Order::Desc)
                .limit(1),
            decode_run,
        )
    }

    pub fn list_runs_by_status(&mut self, status: WfRunStatus) -> Result<Vec<WfRun>> {
        self.0.all(
            Query::select()
                .columns(RUN_COLS)
                .from("wf_runs")
                .and_where(Expr::col("status").eq(status.as_str()))
                .order_by("created_at", Order::Asc),
            decode_run,
        )
    }

    pub fn count_active_runs(&mut self) -> Result<i64> {
        self.0
            .one(
                Query::select()
                    .expr(Func::count(Expr::col("id")))
                    .from("wf_runs")
                    .and_where(
                        Expr::col("status")
                            .is_in([WfRunStatus::Running.as_str(), WfRunStatus::Paused.as_str()]),
                    ),
                |r| r.get(0),
            )
            .map(|n| n.unwrap_or(0))
    }

    pub fn get_script_version(
        &mut self,
        run_id: &str,
        version: i64,
    ) -> Result<Option<WfScriptVersion>> {
        self.0.one(
            Query::select()
                .columns([
                    "run_id",
                    "version",
                    "script_source",
                    "script_sha256",
                    "created_by_session_id",
                    "created_at",
                ])
                .from("wf_script_versions")
                .and_where(Expr::col("run_id").eq(run_id))
                .and_where(Expr::col("version").eq(version)),
            |r| {
                Ok(WfScriptVersion {
                    run_id: r.get(0)?,
                    version: r.get(1)?,
                    script_source: r.get(2)?,
                    script_sha256: r.get(3)?,
                    created_by_session_id: r.get(4)?,
                    created_at: r.get(5)?,
                })
            },
        )
    }

    pub fn count_script_versions(&mut self, run_id: &str) -> Result<i64> {
        self.0
            .one(
                Query::select()
                    .expr(Func::count(Expr::col("version")))
                    .from("wf_script_versions")
                    .and_where(Expr::col("run_id").eq(run_id)),
                |r| r.get(0),
            )
            .map(|n| n.unwrap_or(0))
    }

    pub fn get_call(&mut self, call_id: &str) -> Result<Option<WfCall>> {
        self.0.one(
            Query::select()
                .columns(CALL_COLS)
                .from("wf_calls")
                .and_where(Expr::col("id").eq(call_id)),
            decode_call,
        )
    }

    pub fn get_call_by_hash(
        &mut self,
        run_id: &str,
        identity_hash: &str,
    ) -> Result<Option<WfCall>> {
        self.0.one(
            Query::select()
                .columns(CALL_COLS)
                .from("wf_calls")
                .and_where(Expr::col("run_id").eq(run_id))
                .and_where(Expr::col("identity_hash").eq(identity_hash)),
            decode_call,
        )
    }

    pub fn list_calls(&mut self, run_id: &str) -> Result<Vec<WfCall>> {
        self.0.all(
            Query::select()
                .columns(CALL_COLS)
                .from("wf_calls")
                .and_where(Expr::col("run_id").eq(run_id))
                .order_by("created_at", Order::Asc)
                .order_by("id", Order::Asc),
            decode_call,
        )
    }

    pub fn list_version_calls(
        &mut self,
        run_id: &str,
        script_version: i64,
    ) -> Result<Vec<WfVersionCall>> {
        self.0.all(
            Query::select()
                .columns([
                    "run_id",
                    "script_version",
                    "seq_no",
                    "call_id",
                    "hydrated_from_version",
                ])
                .from("wf_version_calls")
                .and_where(Expr::col("run_id").eq(run_id))
                .and_where(Expr::col("script_version").eq(script_version))
                .order_by("seq_no", Order::Asc),
            |r| {
                Ok(WfVersionCall {
                    run_id: r.get(0)?,
                    script_version: r.get(1)?,
                    seq_no: r.get(2)?,
                    call_id: r.get(3)?,
                    hydrated_from_version: r.get(4)?,
                })
            },
        )
    }

    /// Finds the earliest script version that recorded `call_id` (used to populate
    /// `hydrated_from_version` on cross-version lazy hydration).
    pub fn first_version_of_call(&mut self, run_id: &str, call_id: &str) -> Result<Option<i64>> {
        self.0.one(
            Query::select()
                .column("script_version")
                .from("wf_version_calls")
                .and_where(Expr::col("run_id").eq(run_id))
                .and_where(Expr::col("call_id").eq(call_id))
                .order_by("script_version", Order::Asc)
                .limit(1),
            |r| r.get(0),
        )
    }

    pub fn has_active_mutex(
        &mut self,
        ws: &str,
        mutex_key: &str,
        exclude_call_id: &str,
    ) -> Result<bool> {
        let count: i64 = self
            .0
            .one(
                Query::select()
                    .expr(Func::count(Expr::col("id")))
                    .from("wf_calls")
                    .and_where(Expr::col("workspace_id").eq(ws))
                    .and_where(Expr::col("mutex_key").eq(mutex_key))
                    .and_where(Expr::col("status").eq(WfCallStatus::Dispatched.as_str()))
                    .and_where(Expr::col("id").ne(exclude_call_id)),
                |r| r.get(0),
            )?
            .unwrap_or(0);
        Ok(count > 0)
    }

    pub fn get_attempt(&mut self, attempt_id: &str) -> Result<Option<WfCallAttempt>> {
        self.0.one(
            Query::select()
                .columns(ATTEMPT_COLS)
                .from("wf_call_attempts")
                .and_where(Expr::col("id").eq(attempt_id)),
            decode_attempt,
        )
    }

    pub fn attempt_by_child_item(&mut self, child_item_id: &str) -> Result<Option<WfCallAttempt>> {
        self.0.one(
            Query::select()
                .columns(ATTEMPT_COLS)
                .from("wf_call_attempts")
                .and_where(Expr::col("child_item_id").eq(child_item_id)),
            decode_attempt,
        )
    }

    pub fn active_attempt_for_call(&mut self, call_id: &str) -> Result<Option<WfCallAttempt>> {
        self.0.one(
            Query::select()
                .columns(ATTEMPT_COLS)
                .from("wf_call_attempts")
                .and_where(Expr::col("call_id").eq(call_id))
                .and_where(Expr::col("status").is_in([
                    WfAttemptStatus::Blocked.as_str(),
                    WfAttemptStatus::Dispatched.as_str(),
                    WfAttemptStatus::Closing.as_str(),
                ])),
            decode_attempt,
        )
    }

    pub fn list_attempts_for_run(&mut self, run_id: &str) -> Result<Vec<WfCallAttempt>> {
        self.0.all(
            Query::select()
                .columns(ATTEMPT_COLS)
                .from("wf_call_attempts")
                .and_where(Expr::col("run_id").eq(run_id))
                .order_by("created_at", Order::Asc)
                .order_by("attempt_no", Order::Asc),
            decode_attempt,
        )
    }

    pub fn list_active_attempts_for_run(&mut self, run_id: &str) -> Result<Vec<WfCallAttempt>> {
        self.0.all(
            Query::select()
                .columns(ATTEMPT_COLS)
                .from("wf_call_attempts")
                .and_where(Expr::col("run_id").eq(run_id))
                .and_where(Expr::col("status").is_in([
                    WfAttemptStatus::Blocked.as_str(),
                    WfAttemptStatus::Dispatched.as_str(),
                    WfAttemptStatus::Closing.as_str(),
                ]))
                .order_by("created_at", Order::Asc)
                .order_by("attempt_no", Order::Asc),
            decode_attempt,
        )
    }

    pub fn blocked_attempt_for_run(&mut self, run_id: &str) -> Result<Option<WfCallAttempt>> {
        self.0.one(
            Query::select()
                .columns(ATTEMPT_COLS)
                .from("wf_call_attempts")
                .and_where(Expr::col("run_id").eq(run_id))
                .and_where(Expr::col("status").eq(WfAttemptStatus::Blocked.as_str()))
                .order_by("created_at", Order::Asc)
                .limit(1),
            decode_attempt,
        )
    }

    pub fn child_attempt_and_call(
        &mut self,
        child_item_id: &str,
    ) -> Result<Option<(WfCallAttempt, WfCall)>> {
        let Some(attempt) = self.attempt_by_child_item(child_item_id)? else {
            return Ok(None);
        };
        let Some(call) = self.get_call(&attempt.call_id)? else {
            return Ok(None);
        };
        Ok(Some((attempt, call)))
    }

    pub fn list_attempts_for_call(&mut self, call_id: &str) -> Result<Vec<WfCallAttempt>> {
        self.0.all(
            Query::select()
                .columns(ATTEMPT_COLS)
                .from("wf_call_attempts")
                .and_where(Expr::col("call_id").eq(call_id))
                .order_by("attempt_no", Order::Asc),
            decode_attempt,
        )
    }

    /// Real-time SQL `COUNT(*)` of attempts on one call (zero cached count columns).
    pub fn count_call_attempts(&mut self, call_id: &str) -> Result<i64> {
        self.0
            .one(
                Query::select()
                    .expr(Func::count(Expr::col("id")))
                    .from("wf_call_attempts")
                    .and_where(Expr::col("call_id").eq(call_id)),
                |r| r.get(0),
            )
            .map(|n| n.unwrap_or(0))
    }

    /// Real-time SQL `COUNT(*)` of attempts across the entire `(parent_item_id, parent_cycle)`.
    /// Persists across script revisions and goal changes within the same parent cycle.
    pub fn count_cycle_attempts(&mut self, parent_item_id: &str, parent_cycle: i64) -> Result<i64> {
        self.0
            .one(
                Query::select()
                    .expr(Func::count(Expr::col("id")))
                    .from("wf_call_attempts")
                    .and_where(Expr::col("parent_item_id").eq(parent_item_id))
                    .and_where(Expr::col("parent_cycle").eq(parent_cycle)),
                |r| r.get(0),
            )
            .map(|n| n.unwrap_or(0))
    }

    /// Counts currently active child sessions (`dispatched` or `closing`) in a run.
    pub fn count_active_dispatched(&mut self, run_id: &str) -> Result<i64> {
        self.0
            .one(
                Query::select()
                    .expr(Func::count(Expr::col("id")))
                    .from("wf_call_attempts")
                    .and_where(Expr::col("run_id").eq(run_id))
                    .and_where(Expr::col("status").is_in([
                        WfAttemptStatus::Dispatched.as_str(),
                        WfAttemptStatus::Closing.as_str(),
                    ])),
                |r| r.get(0),
            )
            .map(|n| n.unwrap_or(0))
    }

    /// Lists all distinct `bot_id`s that ever executed a non-review (`is_review = 0`) attempt in
    /// `(parent_item_id, parent_cycle)`.
    pub fn non_review_bots_in_cycle(
        &mut self,
        parent_item_id: &str,
        parent_cycle: i64,
    ) -> Result<Vec<String>> {
        self.0.all(
            Query::select()
                .distinct()
                .column(("a", "bot_id"))
                .from_as("wf_call_attempts", "a")
                .join_as(
                    JoinType::InnerJoin,
                    "wf_calls",
                    "c",
                    Expr::col(("c", "id")).eq(Expr::col(("a", "call_id"))),
                )
                .and_where(Expr::col(("a", "parent_item_id")).eq(parent_item_id))
                .and_where(Expr::col(("a", "parent_cycle")).eq(parent_cycle))
                .and_where(Expr::col(("c", "is_review")).eq(0_i64))
                .and_where(Expr::col(("a", "bot_id")).is_not_null())
                .and_where(Expr::col(("a", "status")).ne(WfAttemptStatus::Blocked.as_str())),
            |r| r.get(0),
        )
    }

    /// Authoritative Hub verification of the 4 Independent Final Review hard rules (§2.5 & §6.2):
    /// 0. Coverage: if ANY call actually ran in this run (even if every call was tagged
    ///    `review: true`), an independent review is mandatory unless `single_session_override = 1`.
    /// 1. Identity independence: `review_attempt.bot_id` never ran any non-review attempt in
    ///    `(parent_item_id, parent_cycle)` AND never ran any other call in this run.
    /// 2. Strict freshness: `review_attempt.dispatched_at_revision > MAX(other_calls.settled_at_revision)`.
    /// 3. Structured approval: `review_attempt.status == succeeded` and `result_json.approved === true`.
    /// 4. No dangling work: no call in `queued | dispatched` and no active attempt remains.
    pub fn verify_independent_review(&mut self, run_id: &str) -> Result<ReviewGateVerdict> {
        let Some(run) = self.get_run(run_id)? else {
            return Err(StoreError::NotFound("wf_run"));
        };
        if run.single_session_override {
            return Ok(ReviewGateVerdict::NotRequired);
        }

        let calls = self.list_calls(run_id)?;
        let attempts = self.list_attempts_for_run(run_id)?;
        let calls_with_executed_attempt: std::collections::HashSet<&str> = attempts
            .iter()
            .filter(|a| a.status != WfAttemptStatus::Blocked)
            .map(|a| a.call_id.as_str())
            .collect();

        // Any call that is non-cancelled OR actually executed a non-blocked attempt counts as work
        // that requires independent review (a script cannot bypass §6.2 by tagging all calls
        // `review: true`).
        let ran_calls: Vec<&WfCall> = calls
            .iter()
            .filter(|c| {
                c.status != WfCallStatus::Cancelled
                    || calls_with_executed_attempt.contains(c.id.as_str())
            })
            .collect();
        if ran_calls.is_empty() {
            return Ok(ReviewGateVerdict::NotRequired);
        }

        // Rule 4: no dangling active calls or attempts remain.
        if calls.iter().any(|c| c.status.is_active())
            || attempts.iter().any(|a| a.status.is_active())
        {
            return Ok(ReviewGateVerdict::Blocked {
                reason: ReviewGateFailure::ActiveWorkRemaining,
            });
        }

        let non_review: Vec<&WfCall> = ran_calls.iter().copied().filter(|c| !c.is_review).collect();
        let cycle_worker_bots: std::collections::HashSet<String> = self
            .non_review_bots_in_cycle(&run.parent_item_id, run.parent_cycle)?
            .into_iter()
            .collect();

        let review_calls: Vec<&WfCall> = calls
            .iter()
            .filter(|c| c.is_review && c.status == WfCallStatus::Completed)
            .collect();
        if review_calls.is_empty() {
            return Ok(ReviewGateVerdict::Blocked {
                reason: ReviewGateFailure::MissingReview,
            });
        }

        let mut saw_candidate_with_other_work = false;
        let mut saw_approved = false;
        let mut same_bot_failure: Option<String> = None;
        let mut stale_failure: Option<(i64, i64)> = None;

        for rc in review_calls {
            // A review call cannot review itself when no other call ever ran.
            let other_ran: Vec<&WfCall> = ran_calls
                .iter()
                .copied()
                .filter(|c| c.id != rc.id)
                .collect();
            if other_ran.is_empty() {
                continue;
            }
            saw_candidate_with_other_work = true;

            // Rule 3: structured `approved === true` on succeeded review attempt.
            let approved = rc
                .result_json
                .as_deref()
                .and_then(|raw| serde_json::from_str::<serde_json::Value>(raw).ok())
                .and_then(|v| {
                    v.as_object()
                        .and_then(|o| o.get("approved"))
                        .and_then(|b| b.as_bool())
                })
                == Some(true);
            if !approved {
                continue;
            }
            let Some(attempt) = attempts
                .iter()
                .find(|a| a.call_id == rc.id && a.status == WfAttemptStatus::Succeeded)
            else {
                continue;
            };
            saw_approved = true;

            // Rule 1: identity independence across the entire parent cycle. When the script marked
            // every call `review: true` (`non_review.is_empty()`), all other calls are treated as
            // work so the same bot cannot self-review a disguised work call.
            let Some(bot_id) = attempt.bot_id.as_deref() else {
                same_bot_failure = Some(String::new());
                continue;
            };
            let self_reviewed_disguised_work = non_review.is_empty()
                && attempts.iter().any(|a| {
                    a.call_id != rc.id
                        && a.status != WfAttemptStatus::Blocked
                        && a.bot_id.as_deref() == Some(bot_id)
                });
            if cycle_worker_bots.contains(bot_id) || self_reviewed_disguised_work {
                same_bot_failure = Some(bot_id.to_owned());
                continue;
            }

            // Rule 2: strict freshness by monotonic run_revision against all work being reviewed.
            let work_baseline = if non_review.is_empty() {
                other_ran
                    .iter()
                    .filter_map(|c| c.settled_at_revision)
                    .max()
                    .unwrap_or(0)
            } else {
                non_review
                    .iter()
                    .filter_map(|c| c.settled_at_revision)
                    .max()
                    .unwrap_or(0)
            };
            if attempt.dispatched_at_revision <= work_baseline {
                stale_failure = Some(match stale_failure {
                    Some((prev_disp, _)) if prev_disp >= attempt.dispatched_at_revision => {
                        (prev_disp, work_baseline)
                    }
                    _ => (attempt.dispatched_at_revision, work_baseline),
                });
                continue;
            }

            return Ok(ReviewGateVerdict::Verified {
                review_call_id: rc.id.clone(),
            });
        }

        if !saw_candidate_with_other_work {
            Ok(ReviewGateVerdict::Blocked {
                reason: ReviewGateFailure::MissingReview,
            })
        } else if !saw_approved {
            Ok(ReviewGateVerdict::Blocked {
                reason: ReviewGateFailure::NotApproved,
            })
        } else if let Some(bot_id) = same_bot_failure {
            Ok(ReviewGateVerdict::Blocked {
                reason: ReviewGateFailure::SameBotAsWorker { bot_id },
            })
        } else if let Some((review_dispatched_revision, latest_work_settled_revision)) =
            stale_failure
        {
            Ok(ReviewGateVerdict::Blocked {
                reason: ReviewGateFailure::StaleReview {
                    review_dispatched_revision,
                    latest_work_settled_revision,
                },
            })
        } else {
            Ok(ReviewGateVerdict::Blocked {
                reason: ReviewGateFailure::MissingReview,
            })
        }
    }
}

impl WorkflowRuns<'_, W<'_>> {
    /// Creates a brand-new `wf_run` + version 1 of `wf_script_versions`.
    #[allow(clippy::too_many_arguments)]
    pub fn create_run(
        &mut self,
        ws: &str,
        parent_item_id: &str,
        parent_cycle: i64,
        goal_version: i64,
        script_source: &str,
        script_sha256: &str,
        created_by_session_id: Option<&str>,
    ) -> Result<WfRun> {
        let id = uid("wfr");
        let ts = now();
        self.0.exec(
            Query::insert()
                .into_table("wf_runs")
                .columns([
                    "id",
                    "workspace_id",
                    "parent_item_id",
                    "parent_cycle",
                    "goal_version",
                    "status",
                    "pause_reason_code",
                    "active_script_version",
                    "run_revision",
                    "single_session_override",
                    "review_verified_call_id",
                    "extra_attempts_budget",
                    "result_json",
                    "created_at",
                    "updated_at",
                ])
                .values_panic([
                    id.clone().into(),
                    ws.into(),
                    parent_item_id.into(),
                    parent_cycle.into(),
                    goal_version.into(),
                    WfRunStatus::Running.as_str().into(),
                    Option::<String>::None.into(),
                    1_i64.into(),
                    1_i64.into(),
                    0_i64.into(),
                    Option::<String>::None.into(),
                    0_i64.into(),
                    Option::<String>::None.into(),
                    ts.into(),
                    ts.into(),
                ]),
        )?;
        self.0.exec(
            Query::insert()
                .into_table("wf_script_versions")
                .columns([
                    "run_id",
                    "version",
                    "script_source",
                    "script_sha256",
                    "created_by_session_id",
                    "created_at",
                ])
                .values_panic([
                    id.clone().into(),
                    1_i64.into(),
                    script_source.into(),
                    script_sha256.into(),
                    created_by_session_id.into(),
                    ts.into(),
                ]),
        )?;
        emit_run_changed(self.0, ws, &id, parent_item_id);
        self.get_run(&id)?.ok_or(StoreError::NotFound("wf_run"))
    }

    /// Appends immutable script version `active_script_version + 1`, bumps `run_revision`,
    /// clears pause reason, and sets status to `running`.
    pub fn revise_script_version(
        &mut self,
        run_id: &str,
        script_source: &str,
        script_sha256: &str,
        created_by_session_id: Option<&str>,
    ) -> Result<WfRun> {
        let run = self
            .get_run(run_id)?
            .ok_or(StoreError::NotFound("wf_run"))?;
        let next_ver = run.active_script_version + 1;
        let next_rev = run.run_revision + 1;
        let ts = now();
        self.0.exec(
            Query::insert()
                .into_table("wf_script_versions")
                .columns([
                    "run_id",
                    "version",
                    "script_source",
                    "script_sha256",
                    "created_by_session_id",
                    "created_at",
                ])
                .values_panic([
                    run_id.into(),
                    next_ver.into(),
                    script_source.into(),
                    script_sha256.into(),
                    created_by_session_id.into(),
                    ts.into(),
                ]),
        )?;
        self.0.exec(
            Query::update()
                .table("wf_runs")
                .values([
                    ("active_script_version", next_ver.into()),
                    ("run_revision", next_rev.into()),
                    ("status", WfRunStatus::Running.as_str().into()),
                    ("pause_reason_code", Option::<String>::None.into()),
                    ("single_session_override", 0_i64.into()),
                    ("review_verified_call_id", Option::<String>::None.into()),
                    ("result_json", Option::<String>::None.into()),
                    ("updated_at", ts.into()),
                ])
                .and_where(Expr::col("id").eq(run_id)),
        )?;
        emit_run_changed(self.0, &run.workspace_id, run_id, &run.parent_item_id);
        self.get_run(run_id)?.ok_or(StoreError::NotFound("wf_run"))
    }

    pub fn pause_run(&mut self, run_id: &str, reason: WfPauseReason) -> Result<WfRun> {
        let run = self
            .get_run(run_id)?
            .ok_or(StoreError::NotFound("wf_run"))?;
        let next_rev = run.run_revision + 1;
        let ts = now();
        self.0.exec(
            Query::update()
                .table("wf_runs")
                .values([
                    ("status", WfRunStatus::Paused.as_str().into()),
                    ("pause_reason_code", Some(reason.as_str().to_owned()).into()),
                    ("run_revision", next_rev.into()),
                    ("updated_at", ts.into()),
                ])
                .and_where(Expr::col("id").eq(run_id)),
        )?;
        emit_run_changed(self.0, &run.workspace_id, run_id, &run.parent_item_id);
        self.get_run(run_id)?.ok_or(StoreError::NotFound("wf_run"))
    }

    pub fn resume_run(&mut self, run_id: &str) -> Result<WfRun> {
        let run = self
            .get_run(run_id)?
            .ok_or(StoreError::NotFound("wf_run"))?;
        let next_rev = run.run_revision + 1;
        let ts = now();
        self.0.exec(
            Query::update()
                .table("wf_runs")
                .values([
                    ("status", WfRunStatus::Running.as_str().into()),
                    ("pause_reason_code", Option::<String>::None.into()),
                    ("run_revision", next_rev.into()),
                    ("updated_at", ts.into()),
                ])
                .and_where(Expr::col("id").eq(run_id)),
        )?;
        emit_run_changed(self.0, &run.workspace_id, run_id, &run.parent_item_id);
        self.get_run(run_id)?.ok_or(StoreError::NotFound("wf_run"))
    }

    pub fn complete_run(
        &mut self,
        run_id: &str,
        result_json: &str,
        review_verified_call_id: Option<&str>,
    ) -> Result<WfRun> {
        let run = self
            .get_run(run_id)?
            .ok_or(StoreError::NotFound("wf_run"))?;
        let next_rev = run.run_revision + 1;
        let ts = now();
        self.0.exec(
            Query::update()
                .table("wf_runs")
                .values([
                    ("status", WfRunStatus::Completed.as_str().into()),
                    ("pause_reason_code", Option::<String>::None.into()),
                    ("result_json", Some(result_json.to_owned()).into()),
                    (
                        "review_verified_call_id",
                        review_verified_call_id.map(str::to_owned).into(),
                    ),
                    ("run_revision", next_rev.into()),
                    ("updated_at", ts.into()),
                ])
                .and_where(Expr::col("id").eq(run_id)),
        )?;
        emit_run_changed(self.0, &run.workspace_id, run_id, &run.parent_item_id);
        self.get_run(run_id)?.ok_or(StoreError::NotFound("wf_run"))
    }

    pub fn set_run_terminal(&mut self, run_id: &str, status: WfRunStatus) -> Result<WfRun> {
        let run = self
            .get_run(run_id)?
            .ok_or(StoreError::NotFound("wf_run"))?;
        let next_rev = run.run_revision + 1;
        let ts = now();
        self.0.exec(
            Query::update()
                .table("wf_runs")
                .values([
                    ("status", status.as_str().into()),
                    ("pause_reason_code", Option::<String>::None.into()),
                    ("run_revision", next_rev.into()),
                    ("updated_at", ts.into()),
                ])
                .and_where(Expr::col("id").eq(run_id)),
        )?;
        emit_run_changed(self.0, &run.workspace_id, run_id, &run.parent_item_id);
        self.get_run(run_id)?.ok_or(StoreError::NotFound("wf_run"))
    }

    pub fn grant_single_session_override(&mut self, run_id: &str) -> Result<WfRun> {
        let run = self
            .get_run(run_id)?
            .ok_or(StoreError::NotFound("wf_run"))?;
        let next_rev = run.run_revision + 1;
        let ts = now();
        self.0.exec(
            Query::update()
                .table("wf_runs")
                .values([
                    ("single_session_override", 1_i64.into()),
                    ("status", WfRunStatus::Cancelled.as_str().into()),
                    ("pause_reason_code", Option::<String>::None.into()),
                    ("run_revision", next_rev.into()),
                    ("updated_at", ts.into()),
                ])
                .and_where(Expr::col("id").eq(run_id)),
        )?;
        emit_run_changed(self.0, &run.workspace_id, run_id, &run.parent_item_id);
        self.get_run(run_id)?.ok_or(StoreError::NotFound("wf_run"))
    }

    pub fn add_budget_allowance(&mut self, run_id: &str, delta: i64) -> Result<WfRun> {
        let run = self
            .get_run(run_id)?
            .ok_or(StoreError::NotFound("wf_run"))?;
        let next_extra = run.extra_attempts_budget + std::cmp::Ord::max(delta, 0);
        let next_rev = run.run_revision + 1;
        let ts = now();
        self.0.exec(
            Query::update()
                .table("wf_runs")
                .values([
                    ("extra_attempts_budget", next_extra.into()),
                    ("status", WfRunStatus::Running.as_str().into()),
                    ("pause_reason_code", Option::<String>::None.into()),
                    ("run_revision", next_rev.into()),
                    ("updated_at", ts.into()),
                ])
                .and_where(Expr::col("id").eq(run_id)),
        )?;
        emit_run_changed(self.0, &run.workspace_id, run_id, &run.parent_item_id);
        self.get_run(run_id)?.ok_or(StoreError::NotFound("wf_run"))
    }

    /// Marks any non-terminal runs for `(parent_item_id, parent_cycle)` with `goal_version < current`
    /// as `superseded` and returns their IDs so the caller can retire active child items.
    pub fn supersede_older_goals(
        &mut self,
        parent_item_id: &str,
        parent_cycle: i64,
        current_goal_version: i64,
    ) -> Result<Vec<WfRun>> {
        let old: Vec<WfRun> = self.0.all(
            Query::select()
                .columns(RUN_COLS)
                .from("wf_runs")
                .and_where(Expr::col("parent_item_id").eq(parent_item_id))
                .and_where(Expr::col("parent_cycle").eq(parent_cycle))
                .and_where(Expr::col("goal_version").lt(current_goal_version))
                .and_where(
                    Expr::col("status")
                        .is_in([WfRunStatus::Running.as_str(), WfRunStatus::Paused.as_str()]),
                ),
            decode_run,
        )?;
        let ts = now();
        for run in &old {
            self.0.exec(
                Query::update()
                    .table("wf_runs")
                    .values([
                        ("status", WfRunStatus::Superseded.as_str().into()),
                        ("run_revision", (run.run_revision + 1).into()),
                        ("updated_at", ts.into()),
                    ])
                    .and_where(Expr::col("id").eq(run.id.as_str())),
            )?;
            emit_run_changed(self.0, &run.workspace_id, &run.id, &run.parent_item_id);
        }
        Ok(old)
    }

    /// Inserts a new `wf_calls` row if `(run_id, identity_hash)` does not exist yet, or returns the
    /// existing row (enabling cross-version lazy hydration). Returns `(call, created)`.
    pub fn ensure_call(&mut self, spec: &NewWfCall<'_>) -> Result<(WfCall, bool)> {
        if let Some(existing) = self.get_call_by_hash(spec.run_id, spec.identity_hash)? {
            return Ok((existing, false));
        }
        let id = uid("wfc");
        let ts = now();
        self.0.exec(
            Query::insert()
                .into_table("wf_calls")
                .columns([
                    "id",
                    "run_id",
                    "workspace_id",
                    "identity_hash",
                    "phase",
                    "title",
                    "prompt",
                    "options_canonical_json",
                    "schema_json",
                    "occurrence",
                    "is_review",
                    "mutex_key",
                    "status",
                    "result_json",
                    "artifact_ids_json",
                    "settled_at_revision",
                    "created_at",
                    "updated_at",
                ])
                .values_panic([
                    id.clone().into(),
                    spec.run_id.into(),
                    spec.workspace_id.into(),
                    spec.identity_hash.into(),
                    spec.phase.into(),
                    spec.title.into(),
                    spec.prompt.into(),
                    spec.options_canonical_json.into(),
                    spec.schema_json.into(),
                    spec.occurrence.into(),
                    i64::from(spec.is_review).into(),
                    spec.mutex_key.into(),
                    WfCallStatus::Queued.as_str().into(),
                    Option::<String>::None.into(),
                    Option::<String>::None.into(),
                    Option::<i64>::None.into(),
                    ts.into(),
                    ts.into(),
                ]),
        )?;
        let call = self.get_call(&id)?.ok_or(StoreError::NotFound("wf_call"))?;
        Ok((call, true))
    }

    pub fn record_version_call(
        &mut self,
        run_id: &str,
        script_version: i64,
        seq_no: i64,
        call_id: &str,
        hydrated_from_version: Option<i64>,
    ) -> Result<()> {
        self.0.exec(
            Query::insert()
                .into_table("wf_version_calls")
                .columns([
                    "run_id",
                    "script_version",
                    "seq_no",
                    "call_id",
                    "hydrated_from_version",
                ])
                .values_panic([
                    run_id.into(),
                    script_version.into(),
                    seq_no.into(),
                    call_id.into(),
                    hydrated_from_version.into(),
                ])
                .on_conflict(
                    OnConflict::columns(["run_id", "script_version", "seq_no"])
                        .do_nothing()
                        .to_owned(),
                ),
        )?;
        Ok(())
    }

    pub fn set_call_status(&mut self, call_id: &str, status: WfCallStatus) -> Result<()> {
        self.0.exec(
            Query::update()
                .table("wf_calls")
                .values([
                    ("status", status.as_str().into()),
                    ("updated_at", now().into()),
                ])
                .and_where(Expr::col("id").eq(call_id)),
        )?;
        Ok(())
    }

    /// Resets a previously `failed` or `cancelled` call back to `queued` when a newer script
    /// version reaches the same call identity (§2.3: only `completed` calls hydrate across
    /// versions; prior attempts remain in `wf_call_attempts` for cycle budget accounting).
    pub fn reset_call_for_new_version(&mut self, call_id: &str) -> Result<WfCall> {
        let call = self
            .get_call(call_id)?
            .ok_or(StoreError::NotFound("wf_call"))?;
        if !matches!(call.status, WfCallStatus::Failed | WfCallStatus::Cancelled) {
            return Ok(call);
        }
        let ts = now();
        self.0.exec(
            Query::update()
                .table("wf_calls")
                .values([
                    ("status", WfCallStatus::Queued.as_str().into()),
                    ("result_json", Option::<String>::None.into()),
                    ("artifact_ids_json", Option::<String>::None.into()),
                    ("git_head", Option::<String>::None.into()),
                    ("git_tree", Option::<String>::None.into()),
                    ("git_cwd", Option::<String>::None.into()),
                    ("settled_at_revision", Option::<i64>::None.into()),
                    ("updated_at", ts.into()),
                ])
                .and_where(Expr::col("id").eq(call_id)),
        )?;
        self.get_call(call_id)?
            .ok_or(StoreError::NotFound("wf_call"))
    }

    /// Re-queues a failed call for retry (`attempt_no + 1`), truncates any `wf_version_calls`
    /// recorded strictly after `call_id` in the active script version (e.g. a `catch` branch
    /// executed after the failure), cancels any active downstream calls, and bumps `run_revision`.
    /// Returns child item IDs that must be stopped by the caller.
    pub fn requeue_call_for_retry(&mut self, call_id: &str) -> Result<Vec<String>> {
        let call = self
            .get_call(call_id)?
            .ok_or(StoreError::NotFound("wf_call"))?;
        let run = self
            .get_run(&call.run_id)?
            .ok_or(StoreError::NotFound("wf_run"))?;
        let version_calls = self.list_version_calls(&run.id, run.active_script_version)?;
        let mut children_to_stop = Vec::new();
        if let Some(retried_seq) = version_calls
            .iter()
            .find(|vc| vc.call_id == call_id)
            .map(|vc| vc.seq_no)
        {
            let kept: std::collections::HashSet<&str> = version_calls
                .iter()
                .filter(|vc| vc.seq_no <= retried_seq)
                .map(|vc| vc.call_id.as_str())
                .collect();
            let downstream_ids: Vec<String> = version_calls
                .iter()
                .filter(|vc| vc.seq_no > retried_seq && !kept.contains(vc.call_id.as_str()))
                .map(|vc| vc.call_id.clone())
                .collect();
            self.0.exec(
                Query::delete()
                    .from_table("wf_version_calls")
                    .and_where(Expr::col("run_id").eq(run.id.as_str()))
                    .and_where(Expr::col("script_version").eq(run.active_script_version))
                    .and_where(Expr::col("seq_no").gt(retried_seq)),
            )?;
            for downstream_id in downstream_ids {
                if let Some(dc) = self.get_call(&downstream_id)?
                    && dc.status.is_active()
                    && let Some(cid) = self.cancel_call(&dc.id)?
                {
                    children_to_stop.push(cid);
                }
            }
        }
        let ts = now();
        self.0.exec(
            Query::update()
                .table("wf_calls")
                .values([
                    ("status", WfCallStatus::Queued.as_str().into()),
                    ("settled_at_revision", Option::<i64>::None.into()),
                    ("updated_at", ts.into()),
                ])
                .and_where(Expr::col("id").eq(call_id)),
        )?;
        self.0.exec(
            Query::update()
                .table("wf_runs")
                .values([
                    ("run_revision", (run.run_revision + 1).into()),
                    ("updated_at", ts.into()),
                ])
                .and_where(Expr::col("id").eq(run.id.as_str())),
        )?;
        emit_run_changed(self.0, &run.workspace_id, &run.id, &run.parent_item_id);
        Ok(children_to_stop)
    }

    pub fn bump_run_revision(&mut self, run_id: &str) -> Result<i64> {
        let run = self
            .get_run(run_id)?
            .ok_or(StoreError::NotFound("wf_run"))?;
        let next_rev = run.run_revision + 1;
        let ts = now();
        self.0.exec(
            Query::update()
                .table("wf_runs")
                .values([("run_revision", next_rev.into()), ("updated_at", ts.into())])
                .and_where(Expr::col("id").eq(run_id)),
        )?;
        emit_run_changed(self.0, &run.workspace_id, run_id, &run.parent_item_id);
        Ok(next_rev)
    }

    pub fn create_attempt(&mut self, new: &NewWfAttempt<'_>) -> Result<WfCallAttempt> {
        let id = uid("wfa");
        let ts = now();
        self.0.exec(
            Query::insert()
                .into_table("wf_call_attempts")
                .columns([
                    "id",
                    "call_id",
                    "run_id",
                    "parent_item_id",
                    "parent_cycle",
                    "attempt_no",
                    "child_item_id",
                    "bot_id",
                    "placement_id",
                    "harness",
                    "model",
                    "status",
                    "dispatched_at_revision",
                    "closing_delivery_revision",
                    "candidate_result_json",
                    "candidate_artifact_ids_json",
                    "error_code",
                    "error_message",
                    "created_at",
                    "settled_at",
                ])
                .values_panic([
                    id.clone().into(),
                    new.call_id.into(),
                    new.run_id.into(),
                    new.parent_item_id.into(),
                    new.parent_cycle.into(),
                    new.attempt_no.into(),
                    new.child_item_id.into(),
                    new.bot_id.into(),
                    new.placement_id.into(),
                    new.harness.into(),
                    new.model.into(),
                    new.status.as_str().into(),
                    new.dispatched_at_revision.into(),
                    Option::<i64>::None.into(),
                    Option::<String>::None.into(),
                    Option::<String>::None.into(),
                    new.error_code.into(),
                    new.error_message.into(),
                    ts.into(),
                    Option::<i64>::None.into(),
                ]),
        )?;
        if new.status == WfAttemptStatus::Dispatched {
            self.set_call_status(new.call_id, WfCallStatus::Dispatched)?;
        }
        self.get_attempt(&id)?
            .ok_or(StoreError::NotFound("wf_call_attempt"))
    }

    /// Promotes a `blocked` attempt to `dispatched` after owner resolves `PlacementBlocked`.
    #[allow(clippy::too_many_arguments)]
    pub fn dispatch_blocked_attempt(
        &mut self,
        attempt_id: &str,
        child_item_id: &str,
        bot_id: &str,
        placement_id: Option<&str>,
        harness: Option<&str>,
        model: Option<&str>,
        dispatched_at_revision: i64,
    ) -> Result<WfCallAttempt> {
        let attempt = self
            .get_attempt(attempt_id)?
            .ok_or(StoreError::NotFound("wf_call_attempt"))?;
        self.0.exec(
            Query::update()
                .table("wf_call_attempts")
                .values([
                    ("child_item_id", Some(child_item_id.to_owned()).into()),
                    ("bot_id", Some(bot_id.to_owned()).into()),
                    ("placement_id", placement_id.map(str::to_owned).into()),
                    ("harness", harness.map(str::to_owned).into()),
                    ("model", model.map(str::to_owned).into()),
                    ("status", WfAttemptStatus::Dispatched.as_str().into()),
                    ("dispatched_at_revision", dispatched_at_revision.into()),
                    ("error_code", Option::<String>::None.into()),
                    ("error_message", Option::<String>::None.into()),
                ])
                .and_where(Expr::col("id").eq(attempt_id)),
        )?;
        self.set_call_status(&attempt.call_id, WfCallStatus::Dispatched)?;
        self.get_attempt(attempt_id)?
            .ok_or(StoreError::NotFound("wf_call_attempt"))
    }

    /// Fills in Bot/harness/model on an already-`dispatched` attempt once async Steve dispatch picks a Bot.
    pub fn settle_dispatching_attempt(
        &mut self,
        attempt_id: &str,
        bot_id: &str,
        harness: Option<&str>,
        model: Option<&str>,
    ) -> Result<WfCallAttempt> {
        self.0.exec(
            Query::update()
                .table("wf_call_attempts")
                .values([
                    ("bot_id", Some(bot_id.to_owned()).into()),
                    ("harness", harness.map(str::to_owned).into()),
                    ("model", model.map(str::to_owned).into()),
                    ("error_code", Option::<String>::None.into()),
                    ("error_message", Option::<String>::None.into()),
                ])
                .and_where(Expr::col("id").eq(attempt_id))
                .and_where(Expr::col("status").eq(WfAttemptStatus::Dispatched.as_str())),
        )?;
        self.get_attempt(attempt_id)?
            .ok_or(StoreError::NotFound("wf_call_attempt"))
    }

    /// Transitions a `dispatched` attempt whose async Steve dispatch failed back to `blocked`.
    pub fn block_dispatching_attempt(
        &mut self,
        attempt_id: &str,
        error_code: &str,
    ) -> Result<WfCallAttempt> {
        let attempt = self
            .get_attempt(attempt_id)?
            .ok_or(StoreError::NotFound("wf_call_attempt"))?;
        self.0.exec(
            Query::update()
                .table("wf_call_attempts")
                .values([
                    ("status", WfAttemptStatus::Blocked.as_str().into()),
                    ("error_code", Some(error_code.to_owned()).into()),
                    ("error_message", Option::<String>::None.into()),
                ])
                .and_where(Expr::col("id").eq(attempt_id))
                .and_where(Expr::col("status").eq(WfAttemptStatus::Dispatched.as_str())),
        )?;
        self.set_call_status(&attempt.call_id, WfCallStatus::Queued)?;
        self.get_attempt(attempt_id)?
            .ok_or(StoreError::NotFound("wf_call_attempt"))
    }

    /// Re-promotes a `blocked` attempt back to `dispatched` while async Steve dispatch retries choosing a Bot.
    pub fn requeue_blocked_attempt_for_dispatch(
        &mut self,
        attempt_id: &str,
        dispatched_at_revision: i64,
    ) -> Result<WfCallAttempt> {
        let attempt = self
            .get_attempt(attempt_id)?
            .ok_or(StoreError::NotFound("wf_call_attempt"))?;
        self.0.exec(
            Query::update()
                .table("wf_call_attempts")
                .values([
                    ("bot_id", Option::<String>::None.into()),
                    ("harness", Option::<String>::None.into()),
                    ("model", Option::<String>::None.into()),
                    ("status", WfAttemptStatus::Dispatched.as_str().into()),
                    ("dispatched_at_revision", dispatched_at_revision.into()),
                    ("error_code", Option::<String>::None.into()),
                    ("error_message", Option::<String>::None.into()),
                ])
                .and_where(Expr::col("id").eq(attempt_id))
                .and_where(Expr::col("status").eq(WfAttemptStatus::Blocked.as_str())),
        )?;
        self.set_call_status(&attempt.call_id, WfCallStatus::Dispatched)?;
        self.get_attempt(attempt_id)?
            .ok_or(StoreError::NotFound("wf_call_attempt"))
    }

    /// Cancels a call and any active attempt (`blocked` / `dispatched` / `closing`) in one write.
    /// Returns the child item ID to stop if one was already dispatched.
    pub fn cancel_call(&mut self, call_id: &str) -> Result<Option<String>> {
        let Some(call) = self.get_call(call_id)? else {
            return Ok(None);
        };
        let ts = now();
        let active = self.active_attempt_for_call(call_id)?;
        let child_to_stop = active.as_ref().and_then(|a| a.child_item_id.clone());
        if let Some(a) = &active {
            self.0.exec(
                Query::update()
                    .table("wf_call_attempts")
                    .values([
                        ("status", WfAttemptStatus::Cancelled.as_str().into()),
                        ("settled_at", ts.into()),
                    ])
                    .and_where(Expr::col("id").eq(a.id.as_str())),
            )?;
        }
        if let Some(run) = self.get_run(&call.run_id)? {
            let next_rev = run.run_revision + 1;
            self.0.exec(
                Query::update()
                    .table("wf_calls")
                    .values([
                        ("status", WfCallStatus::Cancelled.as_str().into()),
                        ("settled_at_revision", Some(next_rev).into()),
                        ("updated_at", ts.into()),
                    ])
                    .and_where(Expr::col("id").eq(call_id)),
            )?;
            self.0.exec(
                Query::update()
                    .table("wf_runs")
                    .values([("run_revision", next_rev.into()), ("updated_at", ts.into())])
                    .and_where(Expr::col("id").eq(run.id.as_str())),
            )?;
            emit_run_changed(self.0, &run.workspace_id, &run.id, &run.parent_item_id);
        }
        Ok(child_to_stop)
    }

    /// Invalidates a previously completed call whose output fingerprint failed verification before
    /// cross-version lazy hydration. Preserves the prior attempt row for cycle budget `COUNT(*)`
    /// while clearing its `succeeded` status so a fresh attempt can succeed under
    /// `wf_call_attempts_one_succeeded`.
    pub fn invalidate_call_for_rerun(
        &mut self,
        call_id: &str,
        reason_code: &str,
        reason_detail: &str,
    ) -> Result<()> {
        let call = self
            .get_call(call_id)?
            .ok_or(StoreError::NotFound("wf_call"))?;
        let run = self
            .get_run(&call.run_id)?
            .ok_or(StoreError::NotFound("wf_run"))?;
        let ts = now();
        self.0.exec(
            Query::update()
                .table("wf_call_attempts")
                .values([
                    ("status", WfAttemptStatus::Cancelled.as_str().into()),
                    ("error_code", Some(reason_code.to_owned()).into()),
                    ("error_message", Some(reason_detail.to_owned()).into()),
                    ("settled_at", Some(ts).into()),
                ])
                .and_where(Expr::col("call_id").eq(call_id))
                .and_where(Expr::col("status").eq(WfAttemptStatus::Succeeded.as_str())),
        )?;
        self.0.exec(
            Query::update()
                .table("wf_calls")
                .values([
                    ("status", WfCallStatus::Queued.as_str().into()),
                    ("result_json", Option::<String>::None.into()),
                    ("artifact_ids_json", Option::<String>::None.into()),
                    ("git_head", Option::<String>::None.into()),
                    ("git_tree", Option::<String>::None.into()),
                    ("git_cwd", Option::<String>::None.into()),
                    ("settled_at_revision", Option::<i64>::None.into()),
                    ("updated_at", ts.into()),
                ])
                .and_where(Expr::col("id").eq(call_id)),
        )?;
        self.0.exec(
            Query::update()
                .table("wf_runs")
                .values([
                    ("run_revision", (run.run_revision + 1).into()),
                    ("updated_at", ts.into()),
                ])
                .and_where(Expr::col("id").eq(run.id.as_str())),
        )?;
        emit_run_changed(self.0, &run.workspace_id, &run.id, &run.parent_item_id);
        Ok(())
    }

    /// Phase 1 of atomic W3.2 delivery settlement (called inside `cycle::request_delivery_close_in`
    /// `extra` closure): binds `candidate_result_json` (and optional Git tree/commit fingerprint)
    /// to `closing_delivery_revision = item.delivery_revision`.
    pub fn stage_attempt_closing(
        &mut self,
        child_item_id: &str,
        closing_delivery_revision: i64,
        candidate_result_json: &str,
        candidate_artifact_ids_json: &str,
    ) -> Result<()> {
        self.stage_attempt_closing_with_git(
            child_item_id,
            closing_delivery_revision,
            candidate_result_json,
            candidate_artifact_ids_json,
            None,
        )
    }

    /// Phase 1 of atomic W3.2 delivery settlement with optional verifiable Git `HEAD` + `tree`
    /// output fingerprint captured at delivery time.
    pub fn stage_attempt_closing_with_git(
        &mut self,
        child_item_id: &str,
        closing_delivery_revision: i64,
        candidate_result_json: &str,
        candidate_artifact_ids_json: &str,
        git_fp: Option<&WfGitFingerprint>,
    ) -> Result<()> {
        let Some(attempt) = self.attempt_by_child_item(child_item_id)? else {
            return Err(StoreError::NotFound("wf_call_attempt"));
        };
        if !matches!(
            attempt.status,
            WfAttemptStatus::Dispatched | WfAttemptStatus::Closing
        ) {
            return Err(StoreError::Conflict(format!(
                "wf_call_attempt {} is in status {}, cannot stage closing",
                attempt.id,
                attempt.status.as_str()
            )));
        }
        self.0.exec(
            Query::update()
                .table("wf_call_attempts")
                .values([
                    ("status", WfAttemptStatus::Closing.as_str().into()),
                    (
                        "closing_delivery_revision",
                        Some(closing_delivery_revision).into(),
                    ),
                    (
                        "candidate_result_json",
                        Some(candidate_result_json.to_owned()).into(),
                    ),
                    (
                        "candidate_artifact_ids_json",
                        Some(candidate_artifact_ids_json.to_owned()).into(),
                    ),
                    (
                        "candidate_git_head",
                        git_fp.map(|fp| fp.head.clone()).into(),
                    ),
                    (
                        "candidate_git_tree",
                        git_fp.map(|fp| fp.tree.clone()).into(),
                    ),
                    (
                        "candidate_git_cwd",
                        git_fp.and_then(|fp| fp.cwd.clone()).into(),
                    ),
                ])
                .and_where(Expr::col("id").eq(attempt.id.as_str())),
        )?;
        Ok(())
    }

    /// Phase 2 of atomic W3.2 delivery settlement (called inside `cycle::commit_cycle` BEFORE
    /// appending `ITEM_LIFECYCLE` / `ITEM_DONE` outbox events).
    ///
    /// - For `outcome == "completed"`: verifies `closing_delivery_revision == item_delivery_revision`
    ///   (never `cas_revision`), marks attempt `succeeded`, promotes `candidate_result_json` into
    ///   `wf_calls(completed, result_json, settled_at_revision = run_revision + 1)`, bumps
    ///   `wf_runs.run_revision`, and emits `WorkEvent::WorkflowRun`.
    /// - For `outcome == "failed"` or `"cancelled"`: settles active attempt and `wf_calls`
    ///   accordingly and bumps `wf_runs.run_revision`.
    /// - Returns `Ok(None)` when `child_item_id` is not an active workflow child attempt.
    pub fn finalize_attempt(
        &mut self,
        child_item_id: &str,
        item_delivery_revision: i64,
        outcome: WfChildOutcome,
        error_code: Option<&str>,
        error_message: Option<&str>,
    ) -> Result<Option<FinalizedAttempt>> {
        let Some(attempt) = self.attempt_by_child_item(child_item_id)? else {
            return Ok(None);
        };
        if !attempt.status.is_active() {
            return Ok(None);
        }
        let Some(run) = self.get_run(&attempt.run_id)? else {
            return Ok(None);
        };
        let next_rev = run.run_revision + 1;
        let ts = now();

        match outcome {
            WfChildOutcome::Completed => {
                if attempt.closing_delivery_revision != Some(item_delivery_revision) {
                    return Err(StoreError::Conflict(format!(
                        "wf_delivery_revision_mismatch: attempt closing_delivery_revision={:?} != item delivery_revision={item_delivery_revision}",
                        attempt.closing_delivery_revision
                    )));
                }
                let result_json = attempt.candidate_result_json.clone().ok_or_else(|| {
                    StoreError::Conflict(
                        "wf_missing_candidate_result: cannot finalize completed attempt without staged structured result".into(),
                    )
                })?;
                let artifact_ids_json = attempt
                    .candidate_artifact_ids_json
                    .clone()
                    .unwrap_or_else(|| "[]".into());

                self.0.exec(
                    Query::update()
                        .table("wf_call_attempts")
                        .values([
                            ("status", WfAttemptStatus::Succeeded.as_str().into()),
                            ("settled_at", Some(ts).into()),
                        ])
                        .and_where(Expr::col("id").eq(attempt.id.as_str())),
                )?;
                self.0.exec(
                    Query::update()
                        .table("wf_calls")
                        .values([
                            ("status", WfCallStatus::Completed.as_str().into()),
                            ("result_json", Some(result_json).into()),
                            ("artifact_ids_json", Some(artifact_ids_json).into()),
                            ("git_head", attempt.candidate_git_head.clone().into()),
                            ("git_tree", attempt.candidate_git_tree.clone().into()),
                            ("git_cwd", attempt.candidate_git_cwd.clone().into()),
                            ("settled_at_revision", Some(next_rev).into()),
                            ("updated_at", ts.into()),
                        ])
                        .and_where(Expr::col("id").eq(attempt.call_id.as_str())),
                )?;
                self.0.exec(
                    Query::update()
                        .table("wf_runs")
                        .values([("run_revision", next_rev.into()), ("updated_at", ts.into())])
                        .and_where(Expr::col("id").eq(run.id.as_str())),
                )?;
                emit_run_changed(self.0, &run.workspace_id, &run.id, &run.parent_item_id);
                Ok(Some(FinalizedAttempt {
                    run_id: run.id,
                    call_id: attempt.call_id,
                    attempt_id: attempt.id,
                    parent_item_id: run.parent_item_id,
                    new_run_revision: next_rev,
                    status: WfAttemptStatus::Succeeded,
                }))
            }
            WfChildOutcome::Failed => {
                self.0.exec(
                    Query::update()
                        .table("wf_call_attempts")
                        .values([
                            ("status", WfAttemptStatus::Failed.as_str().into()),
                            (
                                "error_code",
                                Some(error_code.unwrap_or("child_failed").to_owned()).into(),
                            ),
                            ("error_message", error_message.map(str::to_owned).into()),
                            ("settled_at", Some(ts).into()),
                        ])
                        .and_where(Expr::col("id").eq(attempt.id.as_str())),
                )?;
                self.0.exec(
                    Query::update()
                        .table("wf_calls")
                        .values([
                            ("status", WfCallStatus::Failed.as_str().into()),
                            ("settled_at_revision", Some(next_rev).into()),
                            ("updated_at", ts.into()),
                        ])
                        .and_where(Expr::col("id").eq(attempt.call_id.as_str())),
                )?;
                self.0.exec(
                    Query::update()
                        .table("wf_runs")
                        .values([("run_revision", next_rev.into()), ("updated_at", ts.into())])
                        .and_where(Expr::col("id").eq(run.id.as_str())),
                )?;
                emit_run_changed(self.0, &run.workspace_id, &run.id, &run.parent_item_id);
                Ok(Some(FinalizedAttempt {
                    run_id: run.id,
                    call_id: attempt.call_id,
                    attempt_id: attempt.id,
                    parent_item_id: run.parent_item_id,
                    new_run_revision: next_rev,
                    status: WfAttemptStatus::Failed,
                }))
            }
            WfChildOutcome::Cancelled => {
                self.0.exec(
                    Query::update()
                        .table("wf_call_attempts")
                        .values([
                            ("status", WfAttemptStatus::Cancelled.as_str().into()),
                            ("settled_at", Some(ts).into()),
                        ])
                        .and_where(Expr::col("id").eq(attempt.id.as_str())),
                )?;
                self.0.exec(
                    Query::update()
                        .table("wf_calls")
                        .values([
                            ("status", WfCallStatus::Cancelled.as_str().into()),
                            ("settled_at_revision", Some(next_rev).into()),
                            ("updated_at", ts.into()),
                        ])
                        .and_where(Expr::col("id").eq(attempt.call_id.as_str())),
                )?;
                self.0.exec(
                    Query::update()
                        .table("wf_runs")
                        .values([("run_revision", next_rev.into()), ("updated_at", ts.into())])
                        .and_where(Expr::col("id").eq(run.id.as_str())),
                )?;
                emit_run_changed(self.0, &run.workspace_id, &run.id, &run.parent_item_id);
                Ok(Some(FinalizedAttempt {
                    run_id: run.id,
                    call_id: attempt.call_id,
                    attempt_id: attempt.id,
                    parent_item_id: run.parent_item_id,
                    new_run_revision: next_rev,
                    status: WfAttemptStatus::Cancelled,
                }))
            }
        }
    }

    /// Cancels all non-terminal runs of `(parent_item_id, parent_cycle)` when the parent closes.
    pub fn cancel_runs_for_parent_cycle(
        &mut self,
        parent_item_id: &str,
        parent_cycle: i64,
    ) -> Result<Vec<String>> {
        let runs: Vec<WfRun> = self.0.all(
            Query::select()
                .columns(RUN_COLS)
                .from("wf_runs")
                .and_where(Expr::col("parent_item_id").eq(parent_item_id))
                .and_where(Expr::col("parent_cycle").eq(parent_cycle))
                .and_where(
                    Expr::col("status")
                        .is_in([WfRunStatus::Running.as_str(), WfRunStatus::Paused.as_str()]),
                ),
            decode_run,
        )?;
        let ts = now();
        let mut ids = Vec::with_capacity(runs.len());
        for run in runs {
            self.0.exec(
                Query::update()
                    .table("wf_runs")
                    .values([
                        ("status", WfRunStatus::Cancelled.as_str().into()),
                        ("run_revision", (run.run_revision + 1).into()),
                        ("updated_at", ts.into()),
                    ])
                    .and_where(Expr::col("id").eq(run.id.as_str())),
            )?;
            emit_run_changed(self.0, &run.workspace_id, &run.id, &run.parent_item_id);
            ids.push(run.id);
        }
        Ok(ids)
    }
}
