//! Dynamic JavaScript script-workflow orchestrator (WF-3).
//!
//! Coordinates sandboxed single-step JS replay (`WorkflowStepExecutor`) against the normalized
//! `wf_runs` / `wf_script_versions` / `wf_calls` / `wf_call_attempts` / `wf_version_calls` ledger,
//! W3.1 deterministic placement (`placement::select` / `placement::select_coordinator` + unified
//! `NO_EXECUTOR` coordinator waits), W3.2 two-phase cycle settlement (`cycle.rs`), W3.3 owner
//! decisions (`subject_type = "wf_run"`), live SQL `COUNT(*)` cycle budgets, and Hub-authoritative
//! 4-rule independent final review.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::sync::Arc;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use steve_store::decision_repo::DecisionChange;
use steve_store::workflow_runs::{
    NewWfAttempt, NewWfCall, ReviewGateFailure, ReviewGateVerdict, WfAttemptStatus, WfCall,
    WfCallAttempt, WfCallStatus, WfGitFingerprint, WfPauseReason, WfRun, WfRunStatus, WorkflowRuns,
};
use steve_store::{Decision, DecisionRows, DecisionSource, NewDecision, Store};
use steve_types::{DecisionPrompt, WfBudgetScope};
use steve_workflow_api::{
    CallOptions, CallSpec, JsonSchemaSubset, MAX_RESULT_BYTES, MAX_SCRIPT_SOURCE_BYTES,
    RecordedCall, ResultValidationError, StepFrame, StepInput, ValidationViolationCode,
    WorkflowFaultClass, WorkflowState, sha256_hex, to_canonical_json, validate_structured_result,
};

use crate::cycle::{CloseKind, ClosePolicy};
use crate::error::{CoreError, Result};
use crate::repo::{Item, Session};
use crate::{Core, Output, Tx};

/// Decision `subject_type` for all script-workflow owner decision cards.
pub const WF_RUN_SUBJECT: &str = "wf_run";

/// Workspace settings keys for per-workspace workflow budget caps.
pub mod budget_settings {
    pub const MAX_ATTEMPTS_PER_CALL: &str = "wf.max_attempts_per_call";
    pub const MAX_TOTAL_ATTEMPTS: &str = "wf.max_total_attempts";
    pub const MAX_SCRIPT_VERSIONS: &str = "wf.max_script_versions";
    pub const MAX_PARALLEL_SESSIONS: &str = "wf.max_parallel_sessions";
}

/// Default cycle budget limits (§2.5).
pub const DEFAULT_MAX_ATTEMPTS_PER_CALL: i64 = 3;
pub const DEFAULT_MAX_TOTAL_ATTEMPTS: i64 = 24;
pub const DEFAULT_MAX_SCRIPT_VERSIONS: i64 = 10;
pub const DEFAULT_MAX_PARALLEL_SESSIONS: i64 = 4;

/// Default owner budget top-up increment when resolving `add_budget` without an explicit number.
pub const DEFAULT_BUDGET_ALLOWANCE_DELTA: i64 = 6;
/// Upper bound on a single owner budget top-up decision to prevent accidental runaway allowances.
pub const MAX_BUDGET_ALLOWANCE_DELTA: i64 = 100;

/// Maximum concurrent workflow step executions across the Hub.
/// Bounds blocking SQLite/Git work and `--workflow-step` subprocess fan-out on boot/ticks.
pub const MAX_CONCURRENT_WORKFLOW_STEPS: usize = 4;

/// Maximum CAS re-step attempts when `run_revision` moves concurrently during JS execution.
pub const MAX_STEP_CAS_RETRIES: usize = 3;
/// Maximum automatic retries for transient step infrastructure faults (`Timeout`,
/// `SubprocessFailed`, `ProtocolError`) before pausing as `WfPauseReason::InfrastructureFault`.
pub const MAX_STEP_INFRA_RETRIES: u32 = 2;
/// Base linear backoff delay between automatic step infrastructure retries.
pub const STEP_INFRA_RETRY_BASE_DELAY: Duration = Duration::from_millis(50);

/// Out-of-process JS step executor injected by `steve-hub` (`StepRunner`).
#[async_trait]
pub trait WorkflowStepExecutor: Send + Sync {
    async fn execute(&self, input: &StepInput) -> StepFrame;
}

/// Effective workspace-scoped cycle budget limits (§2.5).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct WfBudget {
    pub max_attempts_per_call: i64,
    pub max_total_attempts: i64,
    pub max_script_versions: i64,
    pub max_parallel_sessions: i64,
}

impl Default for WfBudget {
    fn default() -> Self {
        Self {
            max_attempts_per_call: DEFAULT_MAX_ATTEMPTS_PER_CALL,
            max_total_attempts: DEFAULT_MAX_TOTAL_ATTEMPTS,
            max_script_versions: DEFAULT_MAX_SCRIPT_VERSIONS,
            max_parallel_sessions: DEFAULT_MAX_PARALLEL_SESSIONS,
        }
    }
}

impl WfBudget {
    pub fn load(store: &Store, ws: &str) -> Result<Self> {
        let mut out = Self::default();
        let read_pos =
            |r: &mut steve_store::R<'_>, key: &str| -> steve_store::Result<Option<i64>> {
                Ok(steve_store::WorkspaceSettings(r)
                    .get(ws, key)?
                    .and_then(|v| v.trim().parse::<i64>().ok())
                    .filter(|n| *n > 0))
            };
        store.read(|r| {
            if let Some(n) = read_pos(r, budget_settings::MAX_ATTEMPTS_PER_CALL)? {
                out.max_attempts_per_call = n;
            }
            if let Some(n) = read_pos(r, budget_settings::MAX_TOTAL_ATTEMPTS)? {
                out.max_total_attempts = n;
            }
            if let Some(n) = read_pos(r, budget_settings::MAX_SCRIPT_VERSIONS)? {
                out.max_script_versions = n;
            }
            if let Some(n) = read_pos(r, budget_settings::MAX_PARALLEL_SESSIONS)? {
                out.max_parallel_sessions = n;
            }
            Ok::<_, steve_store::StoreError>(())
        })?;
        Ok(out)
    }
}

/// Strongly-typed parent delivery gate blockers produced by an active/unverified `wf_run`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "code", rename_all = "snake_case")]
pub enum WfDeliveryBlocker {
    RunActive {
        run_id: String,
    },
    RunPaused {
        run_id: String,
        reason: Option<WfPauseReason>,
    },
    RunFailed {
        run_id: String,
    },
    RunCancelled {
        run_id: String,
    },
    ReviewGate {
        run_id: String,
        reason: ReviewGateFailure,
    },
}

impl WfDeliveryBlocker {
    pub fn code(&self) -> &'static str {
        match self {
            Self::RunActive { .. } => "wf_run_active",
            Self::RunPaused { .. } => "wf_run_paused",
            Self::RunFailed { .. } => "wf_run_failed",
            Self::RunCancelled { .. } => "wf_run_cancelled",
            Self::ReviewGate { reason, .. } => reason.code(),
        }
    }
}

/// Strongly-typed error returned by pre-file-receive `steve_deliver.result` validation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChildResultError {
    pub reason: &'static str,
    pub detail: String,
}

impl std::fmt::Display for ChildResultError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.reason, self.detail)
    }
}

impl std::error::Error for ChildResultError {}

/// Pre-file-receive validation of `steve_deliver.result` for a script-workflow child item.
///
/// Pure read-only check: performs zero file receives, zero artifact writes, and zero DB mutations.
/// Returns:
/// - `Ok(None)` when `item_id` is not a script-workflow child item
/// - `Ok(Some(canonical_json))` when `item_id` is an active workflow child and `result` is valid
/// - `Err(ChildResultError)` when `result` is missing, oversized, non-canonical, or violates schema
pub fn validate_child_result(
    store: &Store,
    item_id: &str,
    result: Option<&Value>,
) -> std::result::Result<Option<String>, ChildResultError> {
    validate_child_result_inner(store, item_id, result, true)
}

fn validate_child_result_inner(
    store: &Store,
    item_id: &str,
    result: Option<&Value>,
    record_metric: bool,
) -> std::result::Result<Option<String>, ChildResultError> {
    let record = |reason: &'static str, detail: String| -> ChildResultError {
        if record_metric {
            metrics::counter!("steve_wf_deliver_schema_reject_total", "reason" => reason)
                .increment(1);
        }
        ChildResultError { reason, detail }
    };

    let pair = store
        .read(|r| WorkflowRuns(r).child_attempt_and_call(item_id))
        .map_err(|e| record("storage_error", e.to_string()))?;
    let Some((attempt, call)) = pair else {
        return Ok(None);
    };

    if !matches!(
        attempt.status,
        WfAttemptStatus::Dispatched | WfAttemptStatus::Closing
    ) {
        return Err(record(
            "attempt_not_active",
            format!(
                "workflow child {item_id} attempt is in status {}",
                attempt.status.as_str()
            ),
        ));
    }

    let Some(val) = result else {
        return Err(record(
            "missing_result",
            "steve_deliver requires a structured JSON `result` for script-workflow child items"
                .into(),
        ));
    };

    let schema = match call.schema_json.as_deref() {
        Some(raw) => {
            let v: Value = serde_json::from_str(raw)
                .map_err(|e| record("invalid_schema", format!("stored call schema JSON: {e}")))?;
            Some(
                JsonSchemaSubset::from_json_value(v)
                    .map_err(|e| record("invalid_schema", e.to_string()))?,
            )
        }
        None => None,
    };

    if let Err(err) =
        validate_structured_result(val, schema.as_ref(), call.is_review, MAX_RESULT_BYTES)
    {
        let reason = match &err {
            ResultValidationError::InvalidValue(_) => "canonical_error",
            ResultValidationError::ResultTooLarge { .. } => "too_large",
            ResultValidationError::Schema(_) => "invalid_schema",
            ResultValidationError::Violations(vs)
                if vs
                    .iter()
                    .any(|v| v.code == ValidationViolationCode::ReviewApprovedRequired) =>
            {
                "review_approved_required"
            }
            ResultValidationError::Violations(_) => "schema_violation",
        };
        return Err(record(reason, err.to_string()));
    }

    let canonical = to_canonical_json(val).map_err(|e| record("canonical_error", e.to_string()))?;
    Ok(Some(canonical))
}

/// Strongly-typed MCP arguments for `steve_workflow_run`.
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
pub enum WfRunAction {
    Submit {
        script: String,
    },
    Get,
    Pause {
        #[serde(default)]
        reason: Option<String>,
    },
    Resume,
    Cancel {
        #[serde(default)]
        reason: Option<String>,
    },
    Retry {
        call_id: String,
    },
    Fail {
        reason: String,
    },
}

/// Strongly-typed owner decision choices for `subject_type = "wf_run"` cards.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WfDecisionChoice {
    AddBudget,
    SingleSession,
    Cancel,
    Revise,
    Retry,
}

impl WfDecisionChoice {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::AddBudget => "add_budget",
            Self::SingleSession => "single_session",
            Self::Cancel => "cancel",
            Self::Revise => "revise",
            Self::Retry => "retry",
        }
    }

    pub fn from_choice_str(s: &str) -> Option<Self> {
        match s {
            "add_budget" => Some(Self::AddBudget),
            "single_session" | "yes" => Some(Self::SingleSession),
            "cancel" => Some(Self::Cancel),
            "revise" | "no" => Some(Self::Revise),
            "retry" => Some(Self::Retry),
            _ => None,
        }
    }
}

/// Strongly-typed status marker returned when a workflow child reports failure via MCP.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WfChildFailStatus {
    Failing,
}

/// Strongly-typed output returned by `Core::tool_workflow_run` (`steve_workflow_run` MCP tool).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum WfToolOutput {
    Snapshot(Box<WfRunSnapshot>),
    Empty {
        run: Option<WfRun>,
    },
    ChildFailing {
        status: WfChildFailStatus,
        child_item_id: String,
        reason: String,
        end_turn: bool,
    },
}

impl WfToolOutput {
    pub fn into_snapshot(self) -> Option<WfRunSnapshot> {
        match self {
            Self::Snapshot(s) => Some(*s),
            _ => None,
        }
    }

    pub fn as_snapshot(&self) -> Option<&WfRunSnapshot> {
        match self {
            Self::Snapshot(s) => Some(s),
            _ => None,
        }
    }

    pub fn ends_turn(&self) -> bool {
        matches!(self, Self::ChildFailing { end_turn: true, .. })
    }
}

/// Strongly-typed workflow run summary returned by `steve_workflow_run` and Core APIs.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WfRunSnapshot {
    pub run: WfRun,
    pub calls: Vec<WfCall>,
    pub attempts: Vec<WfCallAttempt>,
    pub cycle_attempts_used: i64,
    pub cycle_attempts_limit: i64,
    pub script_versions_used: i64,
    pub script_versions_limit: i64,
    pub review_gate: ReviewGateVerdict,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub decision_id: Option<String>,
}

struct WorkflowRunLockGuard {
    core: Core,
    run_id: String,
    lock: Arc<tokio::sync::Mutex<()>>,
    _guard: Option<tokio::sync::OwnedMutexGuard<()>>,
}

impl Drop for WorkflowRunLockGuard {
    fn drop(&mut self) {
        // Release the per-run lock first so any queued waiter can run, then acquire the map lock.
        // If strong_count == 2 (only `self.lock` and the map entry remain), no other task holds a
        // clone of this lock, so removing it from the map is race-free.
        self._guard.take();
        let mut locks = self
            .core
            .0
            .workflow_run_locks
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        if let Some(entry) = locks.get(&self.run_id)
            && Arc::ptr_eq(entry, &self.lock)
            && Arc::strong_count(entry) == 2
        {
            locks.remove(&self.run_id);
        }
    }
}

struct PreparedStep {
    run: WfRun,
    input: StepInput,
}

enum StepApplyOutcome {
    Done,
    StaleRetry,
    PausedWithDecision,
}

pub(crate) fn update_active_runs_gauge(store: &Store) {
    if let Ok(count) = store.read(|r| WorkflowRuns(r).count_active_runs()) {
        metrics::gauge!("steve_wf_active_runs").set(count as f64);
    }
}

fn build_child_opening_prompt(call: &WfCall) -> String {
    let mut parts = Vec::new();
    parts.push(call.prompt.clone());
    if call.is_review {
        parts.push(
            "Independent review requirement: deliver a structured JSON `result` via `steve_deliver` containing `\"approved\": true` or `\"approved\": false`.".into(),
        );
    }
    if let Some(schema) = &call.schema_json {
        parts.push(format!(
            "Structured delivery schema (`steve_deliver.result`, <= {MAX_RESULT_BYTES} bytes): {schema}"
        ));
    } else if !call.is_review {
        parts.push(format!(
            "Deliver your structured JSON `result` via `steve_deliver` (<= {MAX_RESULT_BYTES} bytes). If you cannot complete the step, call `steve_workflow_run` with `{{\"action\":\"fail\",\"reason\":\"...\"}}`."
        ));
    }
    parts.join("\n\n")
}

fn parent_ref(parent_item_id: &str) -> steve_store::Parent {
    steve_store::Parent {
        item: Some(Some(parent_item_id.to_owned())),
        session: None,
        at: None,
    }
}

/// Extracts a verifiable `(HEAD, tree)` Git fingerprint from a delivering child item.
///
/// Reuses the frozen `CloseRequest.fingerprint` gathered by the child's Node.
fn capture_git_fingerprint(item: &Item) -> Option<WfGitFingerprint> {
    let raw = item.closing.as_ref().and_then(|c| c.fingerprint.clone())?;
    let parsed = crate::upstream::parse(&raw);
    if parsed.head.is_empty() || parsed.tree.is_empty() {
        return None;
    }
    Some(WfGitFingerprint {
        head: parsed.head.to_owned(),
        tree: parsed.tree.to_owned(),
        cwd: item.cwd.clone(),
    })
}

async fn run_blocking<F, R>(f: F) -> Result<R>
where
    F: FnOnce() -> Result<R> + Send + 'static,
    R: Send + 'static,
{
    tokio::task::spawn_blocking(f)
        .await
        .map_err(|e| CoreError::Msg(format!("wf_blocking_task_failed: {e}")))?
}

struct HydrationCandidate {
    call_id: String,
    is_review: bool,
    is_git_backed: bool,
    git_head: Option<String>,
    git_tree: Option<String>,
    git_cwd: Option<String>,
    project_id: Option<String>,
    project_path: Option<String>,
    project_worktree: bool,
    child_item_id: Option<String>,
    artifact_ids: Vec<String>,
}

enum HydrationOutcome {
    Valid {
        call_id: String,
    },
    Invalid {
        call_id: String,
        is_review: bool,
        reason_code: &'static str,
        detail: String,
    },
    TransientUnavailable {
        detail: String,
    },
}

enum LiveFingerprintCheck {
    Match,
    Mismatch { head: String, tree: Option<String> },
    Absent,
    Unavailable(String),
}

enum RepoCommitTreeCheck {
    Match,
    /// Repository answered `Ok(facts)` but `rec_head^{tree} != rec_tree` (e.g. uncommitted delivery).
    TreeDiffers(String),
    /// Repository query returned a non-transient error (missing commit/workspace).
    Error(String),
    Unavailable(String),
}

fn commit_tree_ask(rec_head: &str, rec_tree: &str) -> crate::node_git::Ask {
    use steve_node_api as na;
    crate::node_git::Ask {
        heads_only: true,
        reads: vec![
            na::GitRead::Show {
                rev: rec_head.to_owned(),
                path: None,
                max: 1,
            },
            na::GitRead::ChangedPaths {
                from: rec_head.to_owned(),
                to: rec_tree.to_owned(),
                max: 1,
            },
        ],
        ..Default::default()
    }
}

async fn check_live_fingerprint(
    port: &dyn crate::node_git::NodeGit,
    child_id: &str,
    rec_head: &str,
    rec_tree: &str,
) -> LiveFingerprintCheck {
    use steve_node_api as na;
    match port
        .facts(
            child_id,
            crate::node_git::Ask {
                reads: vec![na::GitRead::Fingerprint],
                ..Default::default()
            },
        )
        .await
    {
        Ok(fp_facts) => {
            let live_tree = fp_facts.reads.iter().find_map(|r| match &r.answer {
                na::GitAnswer::Fingerprint { tree, .. } => Some(tree.clone()),
                _ => None,
            });
            if fp_facts.head == rec_head && live_tree.as_deref() == Some(rec_tree) {
                LiveFingerprintCheck::Match
            } else {
                LiveFingerprintCheck::Mismatch {
                    head: fp_facts.head,
                    tree: live_tree,
                }
            }
        }
        Err(e) if e.code == na::Code::Unavailable => {
            LiveFingerprintCheck::Unavailable(e.safe_detail)
        }
        Err(_) => LiveFingerprintCheck::Absent,
    }
}

fn eval_commit_tree(
    res: steve_node_api::ApiResult<steve_node_api::GitFacts>,
    rec_head: &str,
    rec_tree: &str,
) -> RepoCommitTreeCheck {
    use steve_node_api as na;
    match res {
        Ok(facts) => {
            let has_commit = facts
                .reads
                .iter()
                .any(|r| matches!(r.answer, na::GitAnswer::Commit { .. }));
            let tree_matches = facts.reads.iter().any(|r| {
                matches!(
                    &r.answer,
                    na::GitAnswer::ChangedPaths { paths, truncated }
                        if paths.is_empty() && !truncated
                )
            });
            if has_commit && tree_matches {
                RepoCommitTreeCheck::Match
            } else {
                RepoCommitTreeCheck::TreeDiffers(format!(
                    "Node commit/tree {rec_head}:{rec_tree} did not verify"
                ))
            }
        }
        Err(e) if e.code == na::Code::Unavailable => {
            RepoCommitTreeCheck::Unavailable(e.safe_detail)
        }
        Err(e) => RepoCommitTreeCheck::Error(e.safe_detail),
    }
}

async fn verify_node_git_candidate(
    port: &dyn crate::node_git::NodeGit,
    c: &HydrationCandidate,
) -> HydrationOutcome {
    let (Some(rec_head), Some(rec_tree)) = (c.git_head.as_deref(), c.git_tree.as_deref()) else {
        return HydrationOutcome::Invalid {
            call_id: c.call_id.clone(),
            is_review: c.is_review,
            reason_code: "missing_git_fingerprint",
            detail: format!(
                "git-backed call {} has no recorded (git_head, git_tree) fingerprint",
                c.call_id
            ),
        };
    };
    if rec_head.is_empty() || rec_tree.is_empty() {
        return HydrationOutcome::Invalid {
            call_id: c.call_id.clone(),
            is_review: c.is_review,
            reason_code: "missing_git_fingerprint",
            detail: format!("git-backed call {} has empty git fingerprint", c.call_id),
        };
    }
    let Some(child_id) = c.child_item_id.as_deref() else {
        return HydrationOutcome::Invalid {
            call_id: c.call_id.clone(),
            is_review: c.is_review,
            reason_code: "git_fingerprint_mismatch",
            detail: format!("git-backed call {} missing child_item_id", c.call_id),
        };
    };

    let cwd = c.git_cwd.as_deref().filter(|d| !d.is_empty());
    let project_path = c.project_path.as_deref().filter(|d| !d.is_empty());
    let is_shared_repo = !c.project_worktree
        || match (cwd, project_path) {
            (Some(d), Some(p)) => d == p,
            _ => false,
        };

    if is_shared_repo {
        // Shared repository (`worktree = false`): subsequent calls or commits legitimately advance
        // HEAD in the repository. Verify the recorded commit & tree still exist in the repo first;
        // fall back to the live working-tree fingerprint for uncommitted deliveries.
        let ask = commit_tree_ask(rec_head, rec_tree);
        let repo_res = if let Some(pid) = c.project_id.as_deref() {
            port.project_facts(pid, ask).await
        } else {
            port.facts(child_id, ask).await
        };
        match eval_commit_tree(repo_res, rec_head, rec_tree) {
            RepoCommitTreeCheck::Match => {
                return HydrationOutcome::Valid {
                    call_id: c.call_id.clone(),
                };
            }
            RepoCommitTreeCheck::Unavailable(detail) => {
                return HydrationOutcome::TransientUnavailable { detail };
            }
            RepoCommitTreeCheck::Error(detail) => {
                return HydrationOutcome::Invalid {
                    call_id: c.call_id.clone(),
                    is_review: c.is_review,
                    reason_code: "git_fingerprint_mismatch",
                    detail: format!("Node git verification failed for {child_id}: {detail}"),
                };
            }
            RepoCommitTreeCheck::TreeDiffers(_) => {}
        }
        return match check_live_fingerprint(port, child_id, rec_head, rec_tree).await {
            LiveFingerprintCheck::Match => HydrationOutcome::Valid {
                call_id: c.call_id.clone(),
            },
            LiveFingerprintCheck::Unavailable(detail) => {
                HydrationOutcome::TransientUnavailable { detail }
            }
            LiveFingerprintCheck::Mismatch { head, tree } => HydrationOutcome::Invalid {
                call_id: c.call_id.clone(),
                is_review: c.is_review,
                reason_code: "git_fingerprint_mismatch",
                detail: format!(
                    "shared repo {rec_head}:{rec_tree} != live {head}:{} for {child_id}",
                    tree.as_deref().unwrap_or("<none>")
                ),
            },
            LiveFingerprintCheck::Absent => HydrationOutcome::Invalid {
                call_id: c.call_id.clone(),
                is_review: c.is_review,
                reason_code: "git_fingerprint_mismatch",
                detail: format!(
                    "shared repo commit/tree {rec_head}:{rec_tree} could not be verified for {child_id}"
                ),
            },
        };
    }

    // Dedicated worktree (`worktree = true`): if the worktree still exists on its Node, its live
    // fingerprint MUST match (catching dirty edits or resets); only when the worktree has been
    // tidied/removed do we verify `recorded_head^{tree} == recorded_tree` in the project repo.
    match check_live_fingerprint(port, child_id, rec_head, rec_tree).await {
        LiveFingerprintCheck::Match => {
            return HydrationOutcome::Valid {
                call_id: c.call_id.clone(),
            };
        }
        LiveFingerprintCheck::Mismatch { head, tree } => {
            return HydrationOutcome::Invalid {
                call_id: c.call_id.clone(),
                is_review: c.is_review,
                reason_code: "git_fingerprint_mismatch",
                detail: format!(
                    "recorded {rec_head}:{rec_tree} != live {head}:{} for {child_id}",
                    tree.as_deref().unwrap_or("<none>")
                ),
            };
        }
        LiveFingerprintCheck::Unavailable(detail) => {
            return HydrationOutcome::TransientUnavailable { detail };
        }
        LiveFingerprintCheck::Absent => {}
    }

    if let Some(pid) = c.project_id.as_deref() {
        let repo_res = port
            .project_facts(pid, commit_tree_ask(rec_head, rec_tree))
            .await;
        return match eval_commit_tree(repo_res, rec_head, rec_tree) {
            RepoCommitTreeCheck::Match => HydrationOutcome::Valid {
                call_id: c.call_id.clone(),
            },
            RepoCommitTreeCheck::Unavailable(detail) => {
                HydrationOutcome::TransientUnavailable { detail }
            }
            RepoCommitTreeCheck::TreeDiffers(detail) | RepoCommitTreeCheck::Error(detail) => {
                HydrationOutcome::Invalid {
                    call_id: c.call_id.clone(),
                    is_review: c.is_review,
                    reason_code: "git_fingerprint_mismatch",
                    detail: format!(
                        "tidied worktree commit/tree {rec_head}:{rec_tree} failed for {child_id}: {detail}"
                    ),
                }
            }
        };
    }

    HydrationOutcome::Invalid {
        call_id: c.call_id.clone(),
        is_review: c.is_review,
        reason_code: "git_fingerprint_mismatch",
        detail: format!(
            "worktree absent and no project repository available to verify {rec_head}:{rec_tree} for {child_id}"
        ),
    }
}

impl Core {
    /// Connects the out-of-process `--workflow-step` executor (`StepRunner` in production).
    pub fn set_workflow_executor(&self, executor: Arc<dyn WorkflowStepExecutor>) {
        let _ = self.0.workflow_executor.set(executor);
    }

    pub(crate) fn workflow_executor(&self) -> Option<Arc<dyn WorkflowStepExecutor>> {
        self.0.workflow_executor.get().cloned()
    }

    /// Returns the number of currently tracked per-run step locks (reaches 0 once idle).
    pub fn workflow_run_lock_count(&self) -> usize {
        self.0
            .workflow_run_locks
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .len()
    }

    async fn acquire_workflow_run_lock(&self, run_id: &str) -> WorkflowRunLockGuard {
        let lock = {
            let mut locks = self
                .0
                .workflow_run_locks
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            locks
                .entry(run_id.to_owned())
                .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(())))
                .clone()
        };
        let guard = lock.clone().lock_owned().await;
        WorkflowRunLockGuard {
            core: self.clone(),
            run_id: run_id.to_owned(),
            lock,
            _guard: Some(guard),
        }
    }

    /// MCP entrypoint for `steve_workflow_run`.
    ///
    /// Offloads all synchronous `Core::tx()` / SQLite operations onto Tokio's blocking pool so
    /// contended state locks or disk I/O never stall async runtime worker threads.
    pub async fn tool_workflow_run(&self, session: &Session, args: Value) -> Result<WfToolOutput> {
        let action: WfRunAction = serde_json::from_value(args)
            .map_err(|e| CoreError::Status(400, format!("wf_invalid_args: {e}")))?;
        let Some(item_id) = session.item_id.clone() else {
            return Err(CoreError::Status(
                400,
                "wf_session_missing_item: session is not bound to an item".into(),
            ));
        };
        let core_c = self.clone();
        let iid_c = item_id.clone();
        let is_child = run_blocking(move || core_c.tx().is_wf_run_child(&iid_c)).await?;
        if is_child {
            return match action {
                WfRunAction::Get => {
                    let core = self.clone();
                    run_blocking(move || {
                        let run_id = core
                            .store()
                            .store()
                            .read(|r| WorkflowRuns(r).attempt_by_child_item(&item_id))?
                            .map(|a| a.run_id)
                            .ok_or_else(|| CoreError::Status(404, "wf_run_not_found".into()))?;
                        let snap = core.tx().wf_run_snapshot(&run_id)?;
                        Ok(WfToolOutput::Snapshot(Box::new(snap)))
                    })
                    .await
                }
                WfRunAction::Fail { reason } => {
                    let reason = reason.trim().to_owned();
                    if reason.is_empty() {
                        return Err(CoreError::Status(400, "wf_fail_reason_required".into()));
                    }
                    let core = self.clone();
                    let sess = session.clone();
                    let reason_c = reason.clone();
                    run_blocking(move || core.tx().wf_child_report_failure(&sess, &reason_c))
                        .await?;
                    Ok(WfToolOutput::ChildFailing {
                        status: WfChildFailStatus::Failing,
                        child_item_id: item_id,
                        reason,
                        end_turn: true,
                    })
                }
                _ => Err(CoreError::Status(
                    403,
                    "wf_child_forbidden: child items may only invoke get or fail".into(),
                )),
            };
        }

        match action {
            WfRunAction::Submit { script } => {
                let snap = self
                    .submit_workflow_script(&item_id, &script, Some(&session.id))
                    .await?;
                Ok(WfToolOutput::Snapshot(Box::new(snap)))
            }
            WfRunAction::Get => {
                let core = self.clone();
                run_blocking(move || {
                    let item = core.tx().item(&item_id)?;
                    let run = core
                        .store()
                        .store()
                        .read(|r| WorkflowRuns(r).latest_run_for_cycle(&item_id, item.cycle))?;
                    match run {
                        Some(r) => Ok(WfToolOutput::Snapshot(Box::new(
                            core.tx().wf_run_snapshot(&r.id)?,
                        ))),
                        None => Ok(WfToolOutput::Empty { run: None }),
                    }
                })
                .await
            }
            WfRunAction::Pause { reason: _ } => {
                let core = self.clone();
                run_blocking(move || {
                    let mut t = core.tx();
                    let run_id =
                        t.wf_pause_parent_run(&item_id, WfPauseReason::CoordinatorPaused)?;
                    let snap = t.wf_run_snapshot(&run_id)?;
                    Ok(WfToolOutput::Snapshot(Box::new(snap)))
                })
                .await
            }
            WfRunAction::Resume => {
                let core = self.clone();
                let run_id =
                    run_blocking(move || core.tx().wf_resume_parent_run(&item_id, false)).await?;
                let snap = self.step_workflow_run(&run_id).await?;
                Ok(WfToolOutput::Snapshot(Box::new(snap)))
            }
            WfRunAction::Cancel { reason } => {
                let core = self.clone();
                run_blocking(move || {
                    let mut t = core.tx();
                    let run_id = t.wf_cancel_parent_run(
                        &item_id,
                        reason.as_deref().unwrap_or("coordinator_cancelled"),
                    )?;
                    let snap = t.wf_run_snapshot(&run_id)?;
                    Ok(WfToolOutput::Snapshot(Box::new(snap)))
                })
                .await
            }
            WfRunAction::Retry { call_id } => {
                let core = self.clone();
                let (run_id, should_step) =
                    run_blocking(move || core.tx().wf_retry_call(&item_id, &call_id)).await?;
                let snap = if should_step {
                    self.step_workflow_run(&run_id).await?
                } else {
                    let core = self.clone();
                    run_blocking(move || core.tx().wf_run_snapshot(&run_id)).await?
                };
                Ok(WfToolOutput::Snapshot(Box::new(snap)))
            }
            WfRunAction::Fail { .. } => Err(CoreError::Status(
                400,
                "wf_fail_child_only: action=fail is only valid on a workflow child item".into(),
            )),
        }
    }

    /// Submits an initial script (v1) or appends a revised script version (v+1) for the parent
    /// item's current `(cycle, goal_version)`, then executes one replay step immediately.
    pub async fn submit_workflow_script(
        &self,
        parent_item_id: &str,
        script_source: &str,
        session_id: Option<&str>,
    ) -> Result<WfRunSnapshot> {
        if script_source.trim().is_empty() {
            return Err(CoreError::Status(400, "wf_script_empty".into()));
        }
        if script_source.len() > MAX_SCRIPT_SOURCE_BYTES {
            return Err(CoreError::Status(
                400,
                format!(
                    "wf_script_too_large: {} bytes exceeds limit of {MAX_SCRIPT_SOURCE_BYTES}",
                    script_source.len()
                ),
            ));
        }
        let core = self.clone();
        let pid = parent_item_id.to_owned();
        let src = script_source.to_owned();
        let sid = session_id.map(str::to_owned);
        let (run_id, should_step) =
            run_blocking(move || core.tx().wf_submit_script(&pid, &src, sid.as_deref())).await?;
        if should_step {
            self.step_workflow_run(&run_id).await
        } else {
            let core = self.clone();
            run_blocking(move || core.tx().wf_run_snapshot(&run_id)).await
        }
    }

    async fn verify_hydration_candidates(
        &self,
        candidates: Vec<HydrationCandidate>,
    ) -> Vec<HydrationOutcome> {
        let node_git = self.node_git();
        let store = self.store().store().clone();
        let mut results = Vec::with_capacity(candidates.len());
        for cand in candidates {
            if !cand.is_git_backed {
                let store_c = store.clone();
                let outcome = tokio::task::spawn_blocking(move || {
                    if !cand.artifact_ids.is_empty()
                        && let Some(cid) = cand.child_item_id.as_deref()
                    {
                        let present: HashSet<String> = match store_c.artifacts().list(cid) {
                            Ok(list) => list.into_iter().map(|a| a.id).collect(),
                            Err(e) => {
                                return HydrationOutcome::Invalid {
                                    call_id: cand.call_id,
                                    is_review: cand.is_review,
                                    reason_code: "artifact_missing",
                                    detail: format!("failed to list artifacts on {cid}: {e}"),
                                };
                            }
                        };
                        for aid in &cand.artifact_ids {
                            if !present.contains(aid) {
                                return HydrationOutcome::Invalid {
                                    call_id: cand.call_id,
                                    is_review: cand.is_review,
                                    reason_code: "artifact_missing",
                                    detail: format!(
                                        "recorded artifact {aid} is no longer present on {cid}"
                                    ),
                                };
                            }
                        }
                    }
                    HydrationOutcome::Valid {
                        call_id: cand.call_id,
                    }
                })
                .await
                .unwrap_or_else(|e| HydrationOutcome::TransientUnavailable {
                    detail: format!("artifact verification task panicked: {e}"),
                });
                results.push(outcome);
                continue;
            }
            if let Some(port) = node_git.as_deref() {
                results.push(verify_node_git_candidate(port, &cand).await);
            } else {
                results.push(HydrationOutcome::TransientUnavailable {
                    detail: "no node git port available for git fingerprint verification".into(),
                });
            }
        }
        results
    }

    pub(crate) fn clear_hydration_cache(&self, run_id: &str) {
        self.0
            .workflow_hydration_verified
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(run_id);
    }

    /// Executes one replay & dispatch step for `run_id`, serialized by a per-run async lock and
    /// bounded globally by [`MAX_CONCURRENT_WORKFLOW_STEPS`].
    ///
    /// Never holds Core's state mutex across Git commands, Node RPCs, or `--workflow-step` I/O,
    /// and offloads all synchronous `Core::tx()` / SQLite operations onto Tokio's blocking pool.
    pub async fn step_workflow_run(&self, run_id: &str) -> Result<WfRunSnapshot> {
        let _run_guard = self.acquire_workflow_run_lock(run_id).await;
        let _step_permit = self
            .0
            .workflow_step_permits
            .clone()
            .acquire_owned()
            .await
            .map_err(|_| CoreError::Msg("wf_step_semaphore_closed".into()))?;

        let Some(executor) = self.workflow_executor() else {
            return Err(CoreError::Msg(
                "wf_executor_not_wired: workflow step executor is not configured".into(),
            ));
        };

        let mut cas_attempts = 0usize;
        let mut infra_retries = 0u32;
        while cas_attempts < MAX_STEP_CAS_RETRIES {
            let core_c = self.clone();
            let rid_c = run_id.to_owned();
            let candidates =
                run_blocking(move || core_c.tx().wf_collect_hydration_candidates(&rid_c)).await?;

            if let Some((script_version, list)) = candidates
                && !list.is_empty()
            {
                let verifications = self.verify_hydration_candidates(list).await;
                if let Some(detail) = verifications.iter().find_map(|v| match v {
                    HydrationOutcome::TransientUnavailable { detail } => Some(detail.clone()),
                    _ => None,
                }) {
                    if infra_retries < MAX_STEP_INFRA_RETRIES {
                        infra_retries += 1;
                        metrics::counter!(
                            "steve_wf_step_infra_retry_total",
                            "code" => "node_git_unavailable"
                        )
                        .increment(1);
                        tracing::warn!(
                            run_id = %run_id,
                            attempt = infra_retries,
                            max = MAX_STEP_INFRA_RETRIES,
                            detail = %detail,
                            "retrying workflow step after transient remote Node git error"
                        );
                        tokio::time::sleep(STEP_INFRA_RETRY_BASE_DELAY * infra_retries).await;
                        continue;
                    }
                    let core_p = self.clone();
                    let rid_p = run_id.to_owned();
                    run_blocking(move || {
                        core_p.tx().pause_and_open_decision(
                            &rid_p,
                            WfPauseReason::InfrastructureFault,
                            "fail",
                            DecisionPrompt::WorkflowScriptFault {
                                fault_code: "node_git_unavailable".into(),
                            },
                            format!(
                                "remote Node git unavailable during hydration verification: {detail}"
                            ),
                            &[
                                WfDecisionChoice::Retry,
                                WfDecisionChoice::Revise,
                                WfDecisionChoice::Cancel,
                                WfDecisionChoice::SingleSession,
                            ],
                        )?;
                        Ok(())
                    })
                    .await?;
                    break;
                }
                let core_a = self.clone();
                let rid_a = run_id.to_owned();
                run_blocking(move || {
                    core_a.tx().wf_apply_hydration_verifications(
                        &rid_a,
                        script_version,
                        &verifications,
                    )
                })
                .await?;
            }

            let core_p = self.clone();
            let rid_p = run_id.to_owned();
            let Some(prepared) = run_blocking(move || core_p.tx().wf_prepare_step(&rid_p)).await?
            else {
                let core_s = self.clone();
                let rid_s = run_id.to_owned();
                return run_blocking(move || core_s.tx().wf_run_snapshot(&rid_s)).await;
            };
            let run_rev = prepared.run.run_revision;
            let started = Instant::now();
            let span = tracing::info_span!(
                "wf.step",
                run_id = %run_id,
                run_revision = run_rev,
                duration_ms = tracing::field::Empty,
            );
            let frame = {
                let _enter = span.enter();
                drop(_enter);
                executor.execute(&prepared.input).await
            };
            let elapsed = started.elapsed();
            span.record("duration_ms", elapsed.as_millis() as u64);
            let outcome_label = match &frame {
                StepFrame::PendingCalls { .. } => "pending_calls",
                StepFrame::Completed { .. } => "completed",
                StepFrame::Failed { fault, .. }
                    if fault.code.class() == WorkflowFaultClass::Infrastructure =>
                {
                    "infra_failed"
                }
                StepFrame::Failed { .. } => "failed",
            };
            metrics::histogram!("steve_wf_step_duration_seconds", "outcome" => outcome_label)
                .record(elapsed.as_secs_f64());

            if let StepFrame::Failed { ref fault, .. } = frame
                && fault.code.class() == WorkflowFaultClass::Infrastructure
                && infra_retries < MAX_STEP_INFRA_RETRIES
            {
                infra_retries += 1;
                metrics::counter!(
                    "steve_wf_step_infra_retry_total",
                    "code" => fault.code.as_str()
                )
                .increment(1);
                tracing::warn!(
                    run_id = %run_id,
                    code = fault.code.as_str(),
                    attempt = infra_retries,
                    max = MAX_STEP_INFRA_RETRIES,
                    detail = %fault.detail,
                    "retrying workflow step after transient infrastructure fault"
                );
                tokio::time::sleep(STEP_INFRA_RETRY_BASE_DELAY * infra_retries).await;
                continue;
            }

            let core_f = self.clone();
            let expected_run = prepared.run;
            let outcome = run_blocking(move || {
                let out = core_f.tx().wf_apply_step_frame(&expected_run, frame)?;
                update_active_runs_gauge(core_f.store().store());
                Ok(out)
            })
            .await?;
            match outcome {
                StepApplyOutcome::Done | StepApplyOutcome::PausedWithDecision => break,
                StepApplyOutcome::StaleRetry => {
                    cas_attempts += 1;
                    continue;
                }
            }
        }
        let core_s = self.clone();
        let rid_s = run_id.to_owned();
        run_blocking(move || core_s.tx().wf_run_snapshot(&rid_s)).await
    }
}

impl Tx<'_> {
    /// Owner structured takeover for a script-workflow child item via `POST /items/:id/lifecycle`
    /// (`command = "finish"` with a structured JSON `result`). Enforces cycle/CAS revision checks,
    /// rejects unstructured `finish` (`result = None`) with `409 wf_child_structured_delivery_required`,
    /// validates schema + <= 32 KiB + review rules, stages closing via `request_delivery_close_in`,
    /// and commits atomically through `commit_cycle`.
    pub(crate) fn wf_child_finish_by_owner(
        &mut self,
        item: &Item,
        expected_cycle: i64,
        expected_revision: i64,
        result: Option<&Value>,
        reason: Option<&str>,
        actor: &str,
    ) -> Result<Item> {
        if item.cycle == expected_cycle
            && item.phase == crate::cycle::CLOSED
            && self.db().store().items().owner_finish_recorded(
                &item.id,
                expected_cycle,
                expected_revision,
            )?
        {
            return Ok(item.clone());
        }
        if item.cycle != expected_cycle || item.cas_revision != expected_revision {
            return Err(crate::cycle::conflict(steve_i18n::t!(
                "core.item.stale",
                cycle = item.cycle,
                revision = item.cas_revision
            )));
        }
        if item.phase == crate::cycle::CLOSED {
            return Err(crate::cycle::conflict("wf_child_already_closed"));
        }
        let Some(result) = result else {
            return Err(crate::cycle::conflict(
                "wf_child_structured_delivery_required",
            ));
        };
        let canonical = validate_child_result(self.db().store(), &item.id, Some(result))
            .map_err(|e| CoreError::Status(409, e.to_string()))?
            .ok_or_else(|| {
                CoreError::Status(
                    404,
                    format!(
                        "wf_not_child_item: {} is not a workflow child item",
                        item.id
                    ),
                )
            })?;
        let summary_text = reason
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .unwrap_or("owner_structured_delivery");
        self.db().audit(
            actor,
            "item.lifecycle.finish",
            &item.id,
            &json!({"cycle": item.cycle, "revision": item.cas_revision, "reason": reason}),
            &item.workspace_id,
        )?;
        self.db()
            .timeline_add(&item.id, actor, "me", summary_text, None)?;
        let item_id = item.id.clone();
        self.request_delivery_close_in(
            &item_id,
            actor,
            ClosePolicy::Owner,
            true,
            summary_text,
            None,
            vec![],
            |w, committed_item| {
                let git_fp = capture_git_fingerprint(committed_item);
                WorkflowRuns(w).stage_attempt_closing_with_git(
                    &item_id,
                    committed_item.delivery_revision,
                    &canonical,
                    "[]",
                    git_fp.as_ref(),
                )?;
                Ok(())
            },
        )
    }

    /// Schedules an asynchronous workflow step wakeup to be spawned once the Core lock drops.
    pub(crate) fn schedule_wf_step(&mut self, run_id: &str) {
        if !self.st.wf_step_wakeups.iter().any(|r| r == run_id) {
            self.st.wf_step_wakeups.push(run_id.to_owned());
        }
    }

    /// Schedules all `Running` workflow runs on startup (`Core::ready`).
    pub(crate) fn schedule_running_wf_runs(&mut self) -> Result<usize> {
        if self.cfg().no_resume || self.holding() || self.db().store().maintenance() {
            return Ok(0);
        }
        let runs = self
            .db()
            .store()
            .read(|r| WorkflowRuns(r).list_runs_by_status(WfRunStatus::Running))?;
        update_active_runs_gauge(self.db().store());
        let count = runs.len();
        for run in runs {
            self.schedule_wf_step(&run.id);
        }
        Ok(count)
    }

    /// Periodic tick wakeup: only schedules `Running` runs that can actually make progress now
    /// (zero active attempts and no mutex-blocked queued calls, or a `Queued` call with an
    /// available parallel slot and free mutex). Respects `STEVE_NO_RESUME`, maintenance mode, and
    /// active occupancy holds.
    pub(crate) fn wf_run_tick(&mut self) -> Result<()> {
        if self.cfg().no_resume || self.holding() || self.db().store().maintenance() {
            return Ok(());
        }
        let store = self.db().store().clone();
        let runs = store.read(|r| WorkflowRuns(r).list_runs_by_status(WfRunStatus::Running))?;
        for run in runs {
            let budget = WfBudget::load(&store, &run.workspace_id)?;
            let needs_step = store.read(|r| {
                let mut repo = WorkflowRuns(r);
                let active_attempts = repo.list_active_attempts_for_run(&run.id)?;
                let calls = repo.list_calls(&run.id)?;
                let queued_calls: Vec<&WfCall> = calls
                    .iter()
                    .filter(|c| c.status == WfCallStatus::Queued)
                    .collect();
                if active_attempts.is_empty() {
                    if queued_calls.is_empty() {
                        return Ok::<_, steve_store::StoreError>(true);
                    }
                    for call in queued_calls {
                        let mutex_free = match call.mutex_key.as_deref() {
                            Some(mk) => !repo.has_active_mutex(&run.workspace_id, mk, &call.id)?,
                            None => true,
                        };
                        if mutex_free {
                            return Ok(true);
                        }
                    }
                    return Ok(false);
                }
                let dispatched = repo.count_active_dispatched(&run.id)?;
                if dispatched >= budget.max_parallel_sessions {
                    return Ok(false);
                }
                for call in queued_calls {
                    let mutex_free = match call.mutex_key.as_deref() {
                        Some(mk) => !repo.has_active_mutex(&run.workspace_id, mk, &call.id)?,
                        None => true,
                    };
                    if mutex_free {
                        return Ok(true);
                    }
                }
                Ok(false)
            })?;
            if needs_step {
                self.schedule_wf_step(&run.id);
            }
        }
        Ok(())
    }

    /// Returns true if `item_id` is a script-workflow child item (authoritative ledger check).
    pub(crate) fn is_wf_run_child(&self, item_id: &str) -> Result<bool> {
        Ok(self
            .db()
            .store()
            .read(|r| WorkflowRuns(r).attempt_by_child_item(item_id))?
            .is_some())
    }

    /// True when the parent item currently has an active (`running` or `paused`) script workflow
    /// run for its current cycle, so an idle coordinator turn should wait rather than stall-fail.
    pub(crate) fn wf_run_parent_waiting(&self, item: &Item) -> Result<bool> {
        let Some(run) = self
            .db()
            .store()
            .read(|r| WorkflowRuns(r).latest_run_for_cycle(&item.id, item.cycle))?
        else {
            return Ok(false);
        };
        Ok(!run.single_session_override
            && matches!(run.status, WfRunStatus::Running | WfRunStatus::Paused))
    }

    /// Computes strongly-typed delivery gate blockers for a parent item's script workflow run.
    pub(crate) fn wf_run_delivery_blockers(&self, item: &Item) -> Result<Vec<WfDeliveryBlocker>> {
        let Some(run) = self
            .db()
            .store()
            .read(|r| WorkflowRuns(r).latest_run_for_cycle(&item.id, item.cycle))?
        else {
            return Ok(vec![]);
        };
        if run.single_session_override {
            return Ok(vec![]);
        }
        match run.status {
            WfRunStatus::Running => Ok(vec![WfDeliveryBlocker::RunActive { run_id: run.id }]),
            WfRunStatus::Paused => Ok(vec![WfDeliveryBlocker::RunPaused {
                run_id: run.id,
                reason: run.pause_reason_code,
            }]),
            WfRunStatus::Failed => Ok(vec![WfDeliveryBlocker::RunFailed { run_id: run.id }]),
            WfRunStatus::Cancelled => Ok(vec![WfDeliveryBlocker::RunCancelled { run_id: run.id }]),
            WfRunStatus::Superseded => Ok(vec![]),
            WfRunStatus::Completed => {
                match self
                    .db()
                    .store()
                    .read(|r| WorkflowRuns(r).verify_independent_review(&run.id))?
                {
                    ReviewGateVerdict::NotRequired | ReviewGateVerdict::Verified { .. } => {
                        Ok(vec![])
                    }
                    ReviewGateVerdict::Blocked { reason } => {
                        Ok(vec![WfDeliveryBlocker::ReviewGate {
                            run_id: run.id,
                            reason,
                        }])
                    }
                }
            }
        }
    }

    /// Supersedes any active workflow runs from older goal versions when parent goal changes (§1.1 #8).
    pub(crate) fn wf_supersede_on_goal_change(
        &mut self,
        parent_item_id: &str,
        new_goal_version: i64,
    ) -> Result<()> {
        let item = self.item(parent_item_id)?;
        let (superseded, children_to_stop) = self.db().store().write(|w| {
            let runs = WorkflowRuns(w).supersede_older_goals(
                parent_item_id,
                item.cycle,
                new_goal_version,
            )?;
            let mut children = Vec::new();
            for run in &runs {
                for call in WorkflowRuns(w).list_calls(&run.id)? {
                    if call.status.is_active()
                        && let Some(cid) = WorkflowRuns(w).cancel_call(&call.id)?
                    {
                        children.push(cid);
                    }
                }
            }
            Ok::<_, steve_store::StoreError>((runs, children))
        })?;
        for child_id in children_to_stop {
            self.stop_wf_child_best_effort(&child_id, "goal_revised", "wf_goal_superseded");
        }
        if !superseded.is_empty() {
            for run in &superseded {
                self.c.clear_hydration_cache(&run.id);
            }
            update_active_runs_gauge(self.db().store());
        }
        Ok(())
    }

    pub(crate) fn wf_run_snapshot(&self, run_id: &str) -> Result<WfRunSnapshot> {
        let store = self.db().store();
        let (run, calls, attempts, cycle_used, versions_used, review_gate) = store.read(|r| {
            let mut repo = WorkflowRuns(r);
            let run = repo
                .get_run(run_id)?
                .ok_or(steve_store::StoreError::NotFound("wf_run"))?;
            let calls = repo.list_calls(run_id)?;
            let attempts = repo.list_attempts_for_run(run_id)?;
            let cycle_used = repo.count_cycle_attempts(&run.parent_item_id, run.parent_cycle)?;
            let versions_used = repo.count_script_versions(run_id)?;
            let review_gate = repo.verify_independent_review(run_id)?;
            Ok::<_, steve_store::StoreError>((
                run,
                calls,
                attempts,
                cycle_used,
                versions_used,
                review_gate,
            ))
        })?;
        let budget = WfBudget::load(&store, &run.workspace_id)?;
        let open_decision = self
            .db()
            .decisions_of_item(&run.parent_item_id)?
            .into_iter()
            .rev()
            .find(|d| {
                d.subject_type == WF_RUN_SUBJECT
                    && d.subject_id == run.id
                    && d.status == steve_types::DecisionStatus::Open
            })
            .map(|d| d.id)
            .or_else(|| {
                (run.status == WfRunStatus::Paused
                    && run.pause_reason_code == Some(WfPauseReason::PlacementBlocked))
                .then(|| {
                    store
                        .read(|r| {
                            DecisionRows(r).open_for(
                                &run.workspace_id,
                                steve_types::NO_EXECUTOR,
                                &run.workspace_id,
                            )
                        })
                        .ok()
                        .flatten()
                        .map(|d| d.id)
                })
                .flatten()
            });
        Ok(WfRunSnapshot {
            cycle_attempts_limit: budget.max_total_attempts + run.extra_attempts_budget,
            script_versions_limit: budget.max_script_versions,
            run,
            calls,
            attempts,
            cycle_attempts_used: cycle_used,
            script_versions_used: versions_used,
            review_gate,
            decision_id: open_decision,
        })
    }

    fn current_goal_version(&self, item_id: &str) -> Result<i64> {
        Ok(self
            .db()
            .goals(item_id)?
            .into_iter()
            .map(|g| g.version)
            .max()
            .unwrap_or(1))
    }

    fn wf_submit_script(
        &mut self,
        parent_item_id: &str,
        script_source: &str,
        session_id: Option<&str>,
    ) -> Result<(String, bool)> {
        self.require_open_item(parent_item_id)?;
        let parent = self.item(parent_item_id)?;
        let goal_version = self.current_goal_version(parent_item_id)?;
        let budget = WfBudget::load(self.db().store(), &parent.workspace_id)?;
        let sha = sha256_hex(script_source.as_bytes());

        // Supersede any active runs from an earlier goal version in this cycle first.
        self.wf_supersede_on_goal_change(parent_item_id, goal_version)?;

        let existing = self.db().store().read(|r| {
            WorkflowRuns(r).run_for_goal(
                &parent.workspace_id,
                parent_item_id,
                parent.cycle,
                goal_version,
            )
        })?;

        match existing {
            None => {
                let run = self.db().store().write(|w| {
                    WorkflowRuns(w).create_run(
                        &parent.workspace_id,
                        parent_item_id,
                        parent.cycle,
                        goal_version,
                        script_source,
                        &sha,
                        session_id,
                    )
                })?;
                update_active_runs_gauge(self.db().store());
                Ok((run.id, true))
            }
            Some(run) => {
                if matches!(run.status, WfRunStatus::Completed | WfRunStatus::Superseded) {
                    return Err(crate::cycle::conflict(format!(
                        "wf_run_terminal: cannot revise run in status {}",
                        run.status.as_str()
                    )));
                }
                let versions_used = self
                    .db()
                    .store()
                    .read(|r| WorkflowRuns(r).count_script_versions(&run.id))?;
                if versions_used >= budget.max_script_versions {
                    let paused = self.pause_and_open_decision(
                        &run.id,
                        WfPauseReason::BudgetExhausted,
                        "ask",
                        DecisionPrompt::WorkflowBudgetExhausted {
                            scope: WfBudgetScope::ScriptVersions,
                            used: versions_used,
                            limit: budget.max_script_versions,
                        },
                        format!(
                            "script_versions budget exhausted: {versions_used}/{}",
                            budget.max_script_versions
                        ),
                        &[WfDecisionChoice::Cancel, WfDecisionChoice::SingleSession],
                    )?;
                    let _ = paused;
                    return Ok((run.id, false));
                }
                let revised = self.db().store().write(|w| {
                    WorkflowRuns(w).revise_script_version(&run.id, script_source, &sha, session_id)
                })?;
                // Expire any stale workflow fault/review/replay decision cards from the previous version.
                crate::lifecycle::supersede_where(
                    self.db(),
                    parent_item_id,
                    |d| d.subject_type == WF_RUN_SUBJECT && d.subject_id == revised.id,
                    "wf_script_revised",
                )?;
                update_active_runs_gauge(self.db().store());
                Ok((revised.id, true))
            }
        }
    }

    fn wf_pause_parent_run(
        &mut self,
        parent_item_id: &str,
        reason: WfPauseReason,
    ) -> Result<String> {
        let parent = self.item(parent_item_id)?;
        let run = self
            .db()
            .store()
            .read(|r| WorkflowRuns(r).latest_run_for_cycle(parent_item_id, parent.cycle))?
            .ok_or_else(|| CoreError::Status(404, "wf_run_not_found".into()))?;
        if run.status.is_terminal() {
            return Err(crate::cycle::conflict(format!(
                "wf_run_terminal: cannot pause run in status {}",
                run.status.as_str()
            )));
        }
        self.db()
            .store()
            .write(|w| WorkflowRuns(w).pause_run(&run.id, reason))?;
        update_active_runs_gauge(self.db().store());
        Ok(run.id)
    }

    fn wf_resume_parent_run(&mut self, parent_item_id: &str, by_owner: bool) -> Result<String> {
        let parent = self.item(parent_item_id)?;
        let run = self
            .db()
            .store()
            .read(|r| WorkflowRuns(r).latest_run_for_cycle(parent_item_id, parent.cycle))?
            .ok_or_else(|| CoreError::Status(404, "wf_run_not_found".into()))?;
        if run.status != WfRunStatus::Paused {
            return Err(crate::cycle::conflict(format!(
                "wf_run_not_paused: run status is {}",
                run.status.as_str()
            )));
        }
        let owner_gated = run
            .pause_reason_code
            .is_some_and(WfPauseReason::is_owner_gated);
        if owner_gated && !by_owner {
            return Err(CoreError::Status(
                409,
                format!(
                    "wf_owner_decision_required: run is paused for {}",
                    run.pause_reason_code
                        .map(WfPauseReason::as_str)
                        .unwrap_or("unknown")
                ),
            ));
        }
        self.db()
            .store()
            .write(|w| WorkflowRuns(w).resume_run(&run.id))?;
        update_active_runs_gauge(self.db().store());
        Ok(run.id)
    }

    fn wf_cancel_parent_run(&mut self, parent_item_id: &str, reason: &str) -> Result<String> {
        let parent = self.item(parent_item_id)?;
        let run = self
            .db()
            .store()
            .read(|r| WorkflowRuns(r).latest_run_for_cycle(parent_item_id, parent.cycle))?
            .ok_or_else(|| CoreError::Status(404, "wf_run_not_found".into()))?;
        self.cancel_wf_run_and_children(&run.id, "workflow_cancel", reason)?;
        Ok(run.id)
    }

    pub(crate) fn cancel_wf_run_and_children(
        &mut self,
        run_id: &str,
        by: &str,
        reason: &str,
    ) -> Result<()> {
        let (parent_item_id, children_to_stop) = self.db().store().write(|w| {
            let run = WorkflowRuns(w)
                .get_run(run_id)?
                .ok_or(steve_store::StoreError::NotFound("wf_run"))?;
            if !run.status.is_terminal() {
                WorkflowRuns(w).set_run_terminal(run_id, WfRunStatus::Cancelled)?;
            }
            let mut children = Vec::new();
            for call in WorkflowRuns(w).list_calls(run_id)? {
                if call.status.is_active()
                    && let Some(cid) = WorkflowRuns(w).cancel_call(&call.id)?
                {
                    children.push(cid);
                }
            }
            Ok::<_, steve_store::StoreError>((run.parent_item_id, children))
        })?;
        for child_id in children_to_stop {
            self.stop_wf_child_best_effort(&child_id, by, reason);
        }
        let _ = crate::lifecycle::supersede_where(
            self.db(),
            &parent_item_id,
            |d| d.subject_type == WF_RUN_SUBJECT && d.subject_id == run_id,
            reason,
        );
        self.c.clear_hydration_cache(run_id);
        update_active_runs_gauge(self.db().store());
        Ok(())
    }

    fn wf_retry_call(&mut self, parent_item_id: &str, call_id: &str) -> Result<(String, bool)> {
        let parent = self.item(parent_item_id)?;
        let store = self.db().store().clone();
        let (run, call, call_attempts, cycle_attempts) = store.read(|r| {
            let mut repo = WorkflowRuns(r);
            let call = repo
                .get_call(call_id)?
                .ok_or(steve_store::StoreError::NotFound("wf_call"))?;
            let run = repo
                .get_run(&call.run_id)?
                .ok_or(steve_store::StoreError::NotFound("wf_run"))?;
            let ca = repo.count_call_attempts(call_id)?;
            let ta = repo.count_cycle_attempts(&run.parent_item_id, run.parent_cycle)?;
            Ok::<_, steve_store::StoreError>((run, call, ca, ta))
        })?;
        if run.parent_item_id != parent_item_id || run.parent_cycle != parent.cycle {
            return Err(CoreError::Status(403, "wf_call_wrong_parent".into()));
        }
        if call.status != WfCallStatus::Failed {
            return Err(crate::cycle::conflict(format!(
                "wf_call_not_failed: call is in status {}",
                call.status.as_str()
            )));
        }
        let budget = WfBudget::load(&store, &run.workspace_id)?;
        let per_call_limit = budget.max_attempts_per_call + run.extra_attempts_budget;
        let total_limit = budget.max_total_attempts + run.extra_attempts_budget;

        // Requeue the failed call so once budget is available (now or after owner top-up) it runs,
        // and stop any downstream child items that were only reached after this failure (e.g. a
        // catch branch) so replaying up to this call does not trigger NonDeterministicReplay.
        let children_to_stop = store.write(|w| WorkflowRuns(w).requeue_call_for_retry(call_id))?;
        for child_id in children_to_stop {
            self.stop_wf_child_best_effort(&child_id, "workflow", "wf_retry_invalidated_branch");
        }

        if call_attempts >= per_call_limit {
            self.pause_and_open_decision(
                &run.id,
                WfPauseReason::BudgetExhausted,
                "ask",
                DecisionPrompt::WorkflowBudgetExhausted {
                    scope: WfBudgetScope::PerCall,
                    used: call_attempts,
                    limit: per_call_limit,
                },
                format!(
                    "call_id={call_id} exhausted per_call budget {call_attempts}/{per_call_limit}"
                ),
                &[
                    WfDecisionChoice::AddBudget,
                    WfDecisionChoice::Cancel,
                    WfDecisionChoice::SingleSession,
                ],
            )?;
            return Ok((run.id, false));
        }
        if cycle_attempts >= total_limit {
            self.pause_and_open_decision(
                &run.id,
                WfPauseReason::BudgetExhausted,
                "ask",
                DecisionPrompt::WorkflowBudgetExhausted {
                    scope: WfBudgetScope::CycleTotal,
                    used: cycle_attempts,
                    limit: total_limit,
                },
                format!("cycle budget exhausted {cycle_attempts}/{total_limit}"),
                &[
                    WfDecisionChoice::AddBudget,
                    WfDecisionChoice::Cancel,
                    WfDecisionChoice::SingleSession,
                ],
            )?;
            return Ok((run.id, false));
        }

        if run.status == WfRunStatus::Paused {
            store.write(|w| WorkflowRuns(w).resume_run(&run.id))?;
        }
        update_active_runs_gauge(&store);
        Ok((run.id, true))
    }

    fn wf_child_report_failure(&mut self, s: &Session, reason: &str) -> Result<()> {
        let Some(item_id) = s.item_id.as_deref() else {
            return Err(CoreError::Status(400, "wf_session_missing_item".into()));
        };
        let by = s.bot_id.as_deref().unwrap_or("agent").to_owned();
        self.db()
            .timeline_add(item_id, &by, "sys", reason, Some(&s.id))?;
        self.request_cycle_stop(
            item_id,
            CloseKind::Fail,
            &by,
            reason,
            Some("child_reported_failure"),
        )?;
        Ok(())
    }

    /// Structured delivery handler for a script-workflow child item (called from `Tx::tool_deliver_with_result`).
    pub(crate) fn wf_child_tool_deliver(
        &mut self,
        s: &Session,
        item: &Item,
        summary: &str,
        outputs: &[Output],
        checks: &[crate::Check],
        unfinished: &[String],
        result: Option<&Value>,
        already_validated: bool,
    ) -> Result<()> {
        let canonical_result =
            validate_child_result_inner(self.db().store(), &item.id, result, !already_validated)
                .map_err(|e| crate::cycle::conflict(e.to_string()))?
                .ok_or_else(|| crate::cycle::conflict("wf_not_child_item"))?;

        if !unfinished.is_empty() || item.plan.iter().any(|p| !p.done) {
            return Err(crate::cycle::conflict("wf_child_unfinished_steps"));
        }
        if checks.iter().any(|c| !c.valid_scope()) {
            return Err(crate::cycle::conflict("wf_invalid_check_scope"));
        }

        let item_id = item.id.clone();
        let by = s.bot_id.as_deref().unwrap_or("agent").to_owned();
        let evidence: Vec<String> = checks.iter().map(crate::proof::check_line).collect();
        let reason = if summary.trim().is_empty() {
            "wf_child_delivered"
        } else {
            summary.trim()
        };

        self.with_turn_receipt(s, |tx| {
            let before_ids: HashSet<String> = tx
                .db()
                .store()
                .artifacts()
                .list(&item_id)?
                .into_iter()
                .map(|a| a.id)
                .collect();
            for o in outputs {
                tx.db().artifact_add(
                    &item_id,
                    &o.kind,
                    &o.title,
                    o.location.as_deref(),
                    o.role.as_deref(),
                )?;
            }
            let artifact_ids: Vec<String> = tx
                .db()
                .store()
                .artifacts()
                .list(&item_id)?
                .into_iter()
                .filter(|a| !before_ids.contains(&a.id))
                .map(|a| a.id)
                .collect();
            let artifact_ids_json = serde_json::to_string(&artifact_ids)?;

            tx.db().timeline_add(
                &item_id,
                &by,
                "sys",
                crate::takeover::DELIVERED,
                Some(&s.id),
            )?;
            tx.mark(&s.id, crate::Mark::Delivered);
            tx.request_delivery_close_in(
                &item_id,
                &by,
                ClosePolicy::Automatic,
                true,
                reason,
                None,
                evidence.clone(),
                |w, committed_item| {
                    let git_fp = capture_git_fingerprint(committed_item);
                    WorkflowRuns(w).stage_attempt_closing_with_git(
                        &item_id,
                        committed_item.delivery_revision,
                        &canonical_result,
                        &artifact_ids_json,
                        git_fp.as_ref(),
                    )?;
                    Ok(())
                },
            )?;
            Ok(())
        })
    }

    /// Collects unverified completed calls from earlier script versions so `step_workflow_run`
    /// can verify their Git / remote-Node / Hub-artifact outputs completely OFF `Core::tx()`.
    fn wf_collect_hydration_candidates(
        &self,
        run_id: &str,
    ) -> Result<Option<(i64, Vec<HydrationCandidate>)>> {
        let store = self.db().store();
        let Some(run) = store.read(|r| WorkflowRuns(r).get_run(run_id))? else {
            return Ok(None);
        };
        if run.status != WfRunStatus::Running || run.active_script_version <= 1 {
            return Ok(None);
        }
        let Some(parent) = self.db().item(&run.parent_item_id)? else {
            return Ok(None);
        };
        if parent.phase == crate::cycle::CLOSED
            || parent.cycle != run.parent_cycle
            || parent.is_paused()
        {
            return Ok(None);
        }
        let (calls, attempts, active_version_calls) = store.read(|r| {
            let mut repo = WorkflowRuns(r);
            Ok::<_, steve_store::StoreError>((
                repo.list_calls(&run.id)?,
                repo.list_attempts_for_run(&run.id)?,
                repo.list_version_calls(&run.id, run.active_script_version)?,
            ))
        })?;
        let recorded_on_active_ver: HashSet<&str> = active_version_calls
            .iter()
            .map(|vc| vc.call_id.as_str())
            .collect();
        let already_verified: HashSet<String> = self
            .c
            .0
            .workflow_hydration_verified
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(&run.id)
            .map(|set| {
                set.iter()
                    .filter(|(ver, _)| *ver == run.active_script_version)
                    .map(|(_, cid)| cid.clone())
                    .collect()
            })
            .unwrap_or_default();

        let mut succeeded_attempt_by_call: HashMap<&str, &WfCallAttempt> = HashMap::new();
        for a in &attempts {
            if a.status == WfAttemptStatus::Succeeded {
                succeeded_attempt_by_call.insert(a.call_id.as_str(), a);
            }
        }
        let mut candidates = Vec::new();
        for c in &calls {
            if c.status != WfCallStatus::Completed
                || recorded_on_active_ver.contains(c.id.as_str())
                || already_verified.contains(&c.id)
            {
                continue;
            }
            let child_item = match succeeded_attempt_by_call
                .get(c.id.as_str())
                .and_then(|a| a.child_item_id.as_deref())
            {
                Some(cid) => self.db().item(cid)?,
                None => None,
            };
            let effective_project_id = child_item
                .as_ref()
                .and_then(|ch| ch.project_id.as_deref())
                .or(parent.project_id.as_deref());
            let project = self.db().project(effective_project_id)?;
            let is_git_project = project.as_ref().is_some_and(|p| p.kind == "git");
            let is_git_backed = is_git_project
                || c.git_head.is_some()
                || c.git_tree.is_some()
                || c.git_cwd.is_some();
            let child_item_id = child_item.as_ref().map(|ch| ch.id.clone());
            let artifact_ids = c
                .artifact_ids_json
                .as_deref()
                .and_then(|raw| serde_json::from_str::<Vec<String>>(raw).ok())
                .unwrap_or_default();
            candidates.push(HydrationCandidate {
                call_id: c.id.clone(),
                is_review: c.is_review,
                is_git_backed,
                git_head: c.git_head.clone(),
                git_tree: c.git_tree.clone(),
                git_cwd: c
                    .git_cwd
                    .clone()
                    .or_else(|| child_item.as_ref().and_then(|ch| ch.cwd.clone())),
                project_id: effective_project_id.map(str::to_owned),
                project_path: project.as_ref().map(|p| p.path.clone()),
                project_worktree: project.as_ref().is_some_and(|p| p.worktree),
                child_item_id,
                artifact_ids,
            });
        }
        Ok(Some((run.active_script_version, candidates)))
    }

    /// Commits off-lock hydration verification outcomes (invalidating mismatched calls and caching
    /// verified `(script_version, call_id)` entries for the active script version).
    fn wf_apply_hydration_verifications(
        &mut self,
        run_id: &str,
        script_version: i64,
        outcomes: &[HydrationOutcome],
    ) -> Result<()> {
        let store = self.db().store();
        let mut verified_ids = Vec::new();
        for outcome in outcomes {
            match outcome {
                HydrationOutcome::Valid { call_id } => {
                    verified_ids.push(call_id.clone());
                }
                HydrationOutcome::Invalid {
                    call_id,
                    is_review,
                    reason_code,
                    detail,
                } => {
                    tracing::info!(
                        run_id = %run_id,
                        call_id = %call_id,
                        reason = *reason_code,
                        detail = %detail,
                        "cross-version hydration rejected: output fingerprint verification failed; invalidating for rerun"
                    );
                    store.write(|w| {
                        WorkflowRuns(w).invalidate_call_for_rerun(call_id, reason_code, detail)
                    })?;
                    metrics::counter!(
                        "steve_wf_calls_total",
                        "outcome" => "invalidated",
                        "review" => if *is_review { "true" } else { "false" },
                        "hydrated" => "false"
                    )
                    .increment(1);
                }
                HydrationOutcome::TransientUnavailable { .. } => {}
            }
        }
        if !verified_ids.is_empty() {
            let mut cache = self
                .c
                .0
                .workflow_hydration_verified
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            let entry = cache.entry(run_id.to_owned()).or_default();
            // Drop stale version keys so cache remains strictly bounded to the active version.
            entry.retain(|(v, _)| *v == script_version);
            for cid in verified_ids {
                entry.insert((script_version, cid));
            }
        }
        Ok(())
    }

    fn wf_prepare_step(&mut self, run_id: &str) -> Result<Option<PreparedStep>> {
        let store = self.db().store();
        let Some(run) = store.read(|r| WorkflowRuns(r).get_run(run_id))? else {
            return Ok(None);
        };
        if run.status != WfRunStatus::Running {
            return Ok(None);
        }
        let Some(parent) = self.db().item(&run.parent_item_id)? else {
            self.cancel_wf_run_and_children(&run.id, "system", "wf_parent_missing")?;
            return Ok(None);
        };
        if parent.phase == crate::cycle::CLOSED || parent.cycle != run.parent_cycle {
            self.cancel_wf_run_and_children(&run.id, "parent", "wf_parent_closed")?;
            return Ok(None);
        }
        let current_goal_ver = self.current_goal_version(&parent.id)?;
        if current_goal_ver > run.goal_version {
            self.wf_supersede_on_goal_change(&parent.id, current_goal_ver)?;
            return Ok(None);
        }
        if parent.is_paused() {
            return Ok(None);
        }

        let (script, calls, attempts, active_version_calls) = store.read(|r| {
            let mut repo = WorkflowRuns(r);
            let script = repo
                .get_script_version(&run.id, run.active_script_version)?
                .ok_or(steve_store::StoreError::NotFound("wf_script_version"))?;
            let calls = repo.list_calls(&run.id)?;
            let attempts = repo.list_attempts_for_run(&run.id)?;
            let vc = repo.list_version_calls(&run.id, run.active_script_version)?;
            Ok::<_, steve_store::StoreError>((script, calls, attempts, vc))
        })?;

        let recorded_on_active_ver: HashSet<&str> = active_version_calls
            .iter()
            .map(|vc| vc.call_id.as_str())
            .collect();

        let mut latest_failed: HashMap<&str, &WfCallAttempt> = HashMap::new();
        for a in &attempts {
            if a.status == WfAttemptStatus::Failed {
                latest_failed.insert(a.call_id.as_str(), a);
            }
        }

        let mut recorded_calls = BTreeMap::new();
        for c in &calls {
            match c.status {
                WfCallStatus::Completed => {
                    let result: Value = c
                        .result_json
                        .as_deref()
                        .map(serde_json::from_str)
                        .transpose()?
                        .unwrap_or(Value::Null);
                    recorded_calls.insert(
                        c.identity_hash.clone(),
                        RecordedCall::Succeeded {
                            result,
                            settled_at_revision: c.settled_at_revision,
                        },
                    );
                }
                WfCallStatus::Failed => {
                    // Only hydrate failed outcomes within the same script version (§2.3: cross-version
                    // hydration only reuses `completed` calls, never poisoning newer versions).
                    if recorded_on_active_ver.contains(c.id.as_str()) {
                        let (error_code, message) = latest_failed
                            .get(c.id.as_str())
                            .map(|a| {
                                (
                                    a.error_code
                                        .clone()
                                        .unwrap_or_else(|| "child_failed".into()),
                                    a.error_message
                                        .clone()
                                        .unwrap_or_else(|| "workflow child failed".into()),
                                )
                            })
                            .unwrap_or_else(|| {
                                ("child_failed".into(), "workflow child failed".into())
                            });
                        recorded_calls.insert(
                            c.identity_hash.clone(),
                            RecordedCall::Failed {
                                error_code,
                                message,
                                settled_at_revision: c.settled_at_revision,
                            },
                        );
                    }
                }
                WfCallStatus::Cancelled => {
                    // Only preserve cancelled outcomes within the same script version (§2.3).
                    if recorded_on_active_ver.contains(c.id.as_str()) {
                        recorded_calls.insert(
                            c.identity_hash.clone(),
                            RecordedCall::Cancelled {
                                settled_at_revision: c.settled_at_revision,
                            },
                        );
                    }
                }
                WfCallStatus::Queued | WfCallStatus::Dispatched => {}
            }
        }

        let known_call_hashes: BTreeSet<String> =
            calls.iter().map(|c| c.identity_hash.clone()).collect();
        let input = StepInput {
            script_source: script.script_source,
            state: WorkflowState {
                parent_item_id: run.parent_item_id.clone(),
                cycle: run.parent_cycle,
                goal_version: run.goal_version,
                run_revision: run.run_revision,
                script_version: run.active_script_version,
            },
            recorded_calls,
            known_call_hashes,
            limits: None,
        };
        Ok(Some(PreparedStep { run, input }))
    }

    fn wf_apply_step_frame(
        &mut self,
        expected: &WfRun,
        frame: StepFrame,
    ) -> Result<StepApplyOutcome> {
        let store = self.db().store().clone();
        let Some(current) = store.read(|r| WorkflowRuns(r).get_run(&expected.id))? else {
            return Ok(StepApplyOutcome::Done);
        };
        if current.status != WfRunStatus::Running {
            return Ok(StepApplyOutcome::Done);
        }
        if current.run_revision != expected.run_revision
            || current.active_script_version != expected.active_script_version
        {
            return Ok(StepApplyOutcome::StaleRetry);
        }

        match frame {
            StepFrame::Failed { fault, .. } => {
                let code = fault.code.as_str().to_owned();
                let (reason, options): (WfPauseReason, &[WfDecisionChoice]) =
                    match fault.code.class() {
                        WorkflowFaultClass::Infrastructure => (
                            WfPauseReason::InfrastructureFault,
                            &[
                                WfDecisionChoice::Retry,
                                WfDecisionChoice::Revise,
                                WfDecisionChoice::Cancel,
                                WfDecisionChoice::SingleSession,
                            ],
                        ),
                        WorkflowFaultClass::Script => (
                            WfPauseReason::ScriptFault,
                            &[
                                WfDecisionChoice::Revise,
                                WfDecisionChoice::Cancel,
                                WfDecisionChoice::SingleSession,
                            ],
                        ),
                    };
                let _dec_id = self.pause_and_open_decision(
                    &current.id,
                    reason,
                    "fail",
                    DecisionPrompt::WorkflowScriptFault {
                        fault_code: code.clone(),
                    },
                    format!("fault_code={code}: {}", fault.detail),
                    options,
                )?;
                return Ok(StepApplyOutcome::PausedWithDecision);
            }
            StepFrame::PendingCalls {
                emitted_calls,
                pending_identities,
                cancel_identities,
                ..
            } => {
                if self
                    .verify_and_record_emitted(&current, &emitted_calls, &cancel_identities)?
                    .is_some()
                {
                    return Ok(StepApplyOutcome::PausedWithDecision);
                }
                self.dispatch_pending_calls(&current, &pending_identities)
            }
            StepFrame::Completed {
                result,
                emitted_calls,
                cancel_identities,
                ..
            } => {
                if self
                    .verify_and_record_emitted(&current, &emitted_calls, &cancel_identities)?
                    .is_some()
                {
                    return Ok(StepApplyOutcome::PausedWithDecision);
                }
                // Cancel any remaining active calls/attempts when the script completes (e.g. an
                // un-awaited call or sibling in `try { await all([a, b]) } catch { return x }`).
                let remaining_children = store.write(|w| {
                    let mut repo = WorkflowRuns(w);
                    let mut to_stop = Vec::new();
                    for call in repo.list_calls(&current.id)? {
                        if call.status.is_active()
                            && let Some(cid) = repo.cancel_call(&call.id)?
                        {
                            to_stop.push(cid);
                        }
                    }
                    Ok::<_, steve_store::StoreError>(to_stop)
                })?;
                for child_id in remaining_children {
                    self.stop_wf_child_best_effort(
                        &child_id,
                        "workflow",
                        "wf_run_completed_cleanup",
                    );
                }
                let canonical = to_canonical_json(&result)
                    .map_err(|e| CoreError::Status(400, format!("wf_invalid_final_result: {e}")))?;
                let verdict =
                    store.read(|r| WorkflowRuns(r).verify_independent_review(&current.id))?;
                match verdict {
                    ReviewGateVerdict::NotRequired => {
                        metrics::counter!("steve_wf_review_gate_total", "result" => "not_required")
                            .increment(1);
                        store.write(|w| {
                            WorkflowRuns(w).complete_run(&current.id, &canonical, None)
                        })?;
                        self.c.clear_hydration_cache(&current.id);
                        Ok(StepApplyOutcome::Done)
                    }
                    ReviewGateVerdict::Verified { review_call_id } => {
                        metrics::counter!("steve_wf_review_gate_total", "result" => "verified")
                            .increment(1);
                        store.write(|w| {
                            WorkflowRuns(w).complete_run(
                                &current.id,
                                &canonical,
                                Some(&review_call_id),
                            )
                        })?;
                        self.c.clear_hydration_cache(&current.id);
                        Ok(StepApplyOutcome::Done)
                    }
                    ReviewGateVerdict::Blocked { reason } => {
                        let code = reason.code();
                        metrics::counter!("steve_wf_review_gate_total", "result" => code)
                            .increment(1);
                        let _dec_id = self.pause_and_open_decision(
                            &current.id,
                            WfPauseReason::ReviewGateBlocked,
                            "ask",
                            DecisionPrompt::WorkflowReviewBlocked {
                                reason: code.into(),
                            },
                            serde_json::to_string(&reason).unwrap_or_else(|_| code.into()),
                            &[
                                WfDecisionChoice::Revise,
                                WfDecisionChoice::SingleSession,
                                WfDecisionChoice::Cancel,
                            ],
                        )?;
                        Ok(StepApplyOutcome::PausedWithDecision)
                    }
                }
            }
        }
    }

    /// Re-verifies authoritative `CallIdentity`, enforces same-version strict superset prefix,
    /// records cross-version lazy hydration, and cancels `race()` losers and orphaned calls from
    /// previous script versions. Returns `Some(decision_id)` if verification paused the run.
    fn verify_and_record_emitted(
        &mut self,
        run: &WfRun,
        emitted_calls: &[CallSpec],
        cancel_identities: &[String],
    ) -> Result<Option<String>> {
        let store = self.db().store().clone();
        let (prev_version_calls, existing_calls) = store.read(|r| {
            let mut repo = WorkflowRuns(r);
            Ok::<_, steve_store::StoreError>((
                repo.list_version_calls(&run.id, run.active_script_version)?,
                repo.list_calls(&run.id)?,
            ))
        })?;
        let existing_by_hash: HashMap<&str, &WfCall> = existing_calls
            .iter()
            .map(|c| (c.identity_hash.as_str(), c))
            .collect();

        // 1. Authoritative CallIdentity re-verification (never trust subprocess hashes blindly).
        //    Compact references omit prompt/options only for calls already persisted in wf_calls.
        for (idx, spec) in emitted_calls.iter().enumerate() {
            if spec.seq_no as usize != idx {
                let dec_id = self.pause_and_open_decision(
                    &run.id,
                    WfPauseReason::InfrastructureFault,
                    "fail",
                    DecisionPrompt::WorkflowScriptFault {
                        fault_code: "protocol_error".into(),
                    },
                    format!(
                        "emitted_calls seq_no mismatch at index {idx}: got {}",
                        spec.seq_no
                    ),
                    &[
                        WfDecisionChoice::Retry,
                        WfDecisionChoice::Revise,
                        WfDecisionChoice::Cancel,
                        WfDecisionChoice::SingleSession,
                    ],
                )?;
                return Ok(Some(dec_id));
            }
            if let (Some(prompt), Some(options)) = (spec.prompt.as_deref(), spec.options.as_ref()) {
                if let Err(err) = spec.identity.verify(prompt, options) {
                    let dec_id = self.pause_and_open_decision(
                        &run.id,
                        WfPauseReason::InfrastructureFault,
                        "fail",
                        DecisionPrompt::WorkflowScriptFault {
                            fault_code: "protocol_error".into(),
                        },
                        format!(
                            "authoritative CallIdentity verification failed at seq {idx}: {err}"
                        ),
                        &[
                            WfDecisionChoice::Retry,
                            WfDecisionChoice::Revise,
                            WfDecisionChoice::Cancel,
                            WfDecisionChoice::SingleSession,
                        ],
                    )?;
                    return Ok(Some(dec_id));
                }
            } else if spec.is_compact_known() {
                let valid = existing_by_hash
                    .get(spec.identity.identity_hash.as_str())
                    .is_some_and(|existing| {
                        sha256_hex(existing.prompt.as_bytes()) == spec.identity.prompt_sha256
                            && existing.phase == spec.identity.phase
                            && existing.occurrence == i64::from(spec.identity.occurrence)
                    });
                if !valid {
                    let dec_id = self.pause_and_open_decision(
                        &run.id,
                        WfPauseReason::InfrastructureFault,
                        "fail",
                        DecisionPrompt::WorkflowScriptFault {
                            fault_code: "protocol_error".into(),
                        },
                        format!(
                            "compact CallSpec at seq {idx} does not match any known persisted call ({})",
                            spec.identity.identity_hash
                        ),
                        &[
                            WfDecisionChoice::Retry,
                            WfDecisionChoice::Revise,
                            WfDecisionChoice::Cancel,
                            WfDecisionChoice::SingleSession,
                        ],
                    )?;
                    return Ok(Some(dec_id));
                }
            } else {
                let dec_id = self.pause_and_open_decision(
                    &run.id,
                    WfPauseReason::InfrastructureFault,
                    "fail",
                    DecisionPrompt::WorkflowScriptFault {
                        fault_code: "protocol_error".into(),
                    },
                    format!("malformed partial CallSpec at seq {idx}"),
                    &[
                        WfDecisionChoice::Retry,
                        WfDecisionChoice::Revise,
                        WfDecisionChoice::Cancel,
                        WfDecisionChoice::SingleSession,
                    ],
                )?;
                return Ok(Some(dec_id));
            }
        }

        // 2. Same-version strict superset check (§2.3).
        let call_hash_by_id: HashMap<&str, &str> = existing_calls
            .iter()
            .map(|c| (c.id.as_str(), c.identity_hash.as_str()))
            .collect();

        let drift = if emitted_calls.len() < prev_version_calls.len() {
            Some(format!(
                "replay emitted {} calls, fewer than recorded prefix length {}",
                emitted_calls.len(),
                prev_version_calls.len()
            ))
        } else {
            prev_version_calls.iter().enumerate().find_map(|(i, vc)| {
                let expected_hash = call_hash_by_id.get(vc.call_id.as_str()).copied();
                let actual_hash = emitted_calls[i].identity.identity_hash.as_str();
                if expected_hash != Some(actual_hash) {
                    Some(format!(
                        "replay drift at seq {i}: expected {}, got {actual_hash}",
                        expected_hash.unwrap_or("<missing>")
                    ))
                } else {
                    None
                }
            })
        };
        if let Some(detail) = drift {
            let dec_id = self.pause_and_open_decision(
                &run.id,
                WfPauseReason::NonDeterministicReplay,
                "fail",
                DecisionPrompt::WorkflowNonDeterministicReplay,
                detail,
                &[
                    WfDecisionChoice::Revise,
                    WfDecisionChoice::Cancel,
                    WfDecisionChoice::SingleSession,
                ],
            )?;
            return Ok(Some(dec_id));
        }

        // 3. Persist new calls, record version call rows with lazy hydration provenance, and
        //    collect race losers + orphaned active calls from older script versions to cancel.
        let emitted_hashes: HashSet<&str> = emitted_calls
            .iter()
            .map(|s| s.identity.identity_hash.as_str())
            .collect();
        let prev_len = prev_version_calls.len();

        let (children_to_cancel, hydrated_metrics) = store.write(|w| {
            let mut repo = WorkflowRuns(w);
            let mut hydrated_metrics: Vec<bool> = Vec::new();
            for (idx, spec) in emitted_calls.iter().enumerate() {
                let (mut call, created) = if let (Some(prompt), Some(options)) =
                    (spec.prompt.as_deref(), spec.options.as_ref())
                {
                    let schema_json = options
                        .schema
                        .as_ref()
                        .map(serde_json::to_string)
                        .transpose()?;
                    repo.ensure_call(&NewWfCall {
                        run_id: &run.id,
                        workspace_id: &run.workspace_id,
                        identity_hash: &spec.identity.identity_hash,
                        phase: spec.identity.phase.as_deref(),
                        title: options.title.as_deref(),
                        prompt,
                        options_canonical_json: &spec.identity.options_canonical_json,
                        schema_json: schema_json.as_deref(),
                        occurrence: i64::from(spec.identity.occurrence),
                        is_review: options.is_review(),
                        mutex_key: options.mutex_key.as_deref(),
                    })?
                } else {
                    let existing = repo
                        .get_call_by_hash(&run.id, &spec.identity.identity_hash)?
                        .ok_or(steve_store::StoreError::NotFound("wf_call"))?;
                    (existing, false)
                };
                if idx >= prev_len {
                    // Only `completed` calls hydrate across versions (§2.3). When a new script
                    // version (or post-retry replay) reaches a previously `failed` or `cancelled`
                    // call not yet bound to this version, re-queue it fresh while keeping prior
                    // attempts in `wf_call_attempts` for cycle budget accounting.
                    if !created
                        && matches!(call.status, WfCallStatus::Failed | WfCallStatus::Cancelled)
                    {
                        call = repo.reset_call_for_new_version(&call.id)?;
                    }
                    let hydrated_from = if !created && call.status == WfCallStatus::Completed {
                        repo.first_version_of_call(&run.id, &call.id)?
                            .filter(|&v| v < run.active_script_version)
                    } else {
                        None
                    };
                    repo.record_version_call(
                        &run.id,
                        run.active_script_version,
                        idx as i64,
                        &call.id,
                        hydrated_from,
                    )?;
                    if hydrated_from.is_some() && call.status == WfCallStatus::Completed {
                        hydrated_metrics.push(call.is_review);
                    }
                }
            }

            let mut to_cancel_ids: Vec<(String, &'static str)> = Vec::new();
            for hash in cancel_identities {
                if let Some(call) = repo.get_call_by_hash(&run.id, hash)?
                    && call.status.is_active()
                    && let Some(cid) = repo.cancel_call(&call.id)?
                {
                    to_cancel_ids.push((cid, "wf_race_loser"));
                }
            }
            if run.active_script_version > 1 {
                for call in repo.list_calls(&run.id)? {
                    if call.status.is_active()
                        && !emitted_hashes.contains(call.identity_hash.as_str())
                        && let Some(cid) = repo.cancel_call(&call.id)?
                    {
                        to_cancel_ids.push((cid, "wf_script_orphan"));
                    }
                }
            }
            Ok::<_, steve_store::StoreError>((to_cancel_ids, hydrated_metrics))
        })?;

        for is_review in hydrated_metrics {
            metrics::counter!(
                "steve_wf_calls_total",
                "outcome" => "completed",
                "review" => if is_review { "true" } else { "false" },
                "hydrated" => "true"
            )
            .increment(1);
        }
        for (child_id, reason) in children_to_cancel {
            self.stop_wf_child_best_effort(&child_id, "workflow", reason);
        }
        Ok(None)
    }

    fn dispatch_pending_calls(
        &mut self,
        run: &WfRun,
        pending_identities: &[String],
    ) -> Result<StepApplyOutcome> {
        let store = self.db().store().clone();
        let budget = WfBudget::load(&store, &run.workspace_id)?;
        let parent = self.item(&run.parent_item_id)?;
        let per_call_limit = budget.max_attempts_per_call + run.extra_attempts_budget;
        let total_limit = budget.max_total_attempts + run.extra_attempts_budget;

        let mut any_blocked = false;
        let mut blocked_in_batch = 0_i64;

        for hash in pending_identities {
            let Some(call) = store.read(|r| WorkflowRuns(r).get_call_by_hash(&run.id, hash))?
            else {
                continue;
            };
            // Never dispatch a call that is already dispatched or already has an active attempt.
            if call.status != WfCallStatus::Queued {
                continue;
            }
            if let Some(active) =
                store.read(|r| WorkflowRuns(r).active_attempt_for_call(&call.id))?
            {
                if active.status == WfAttemptStatus::Blocked {
                    any_blocked = true;
                    blocked_in_batch += 1;
                }
                continue;
            }

            // Enforce per-run parallel session limit (queue excess calls instead of rejecting).
            let active_dispatched =
                store.read(|r| WorkflowRuns(r).count_active_dispatched(&run.id))?;
            if active_dispatched + blocked_in_batch >= budget.max_parallel_sessions {
                break;
            }

            // Enforce optional workspace-scoped mutex_key serialization (§1.2).
            if let Some(mk) = call.mutex_key.as_deref()
                && store
                    .read(|r| WorkflowRuns(r).has_active_mutex(&run.workspace_id, mk, &call.id))?
            {
                continue;
            }

            // Enforce live SQL COUNT(*) cycle budgets (§2.5).
            let (call_attempts, cycle_attempts) = store.read(|r| {
                let mut repo = WorkflowRuns(r);
                Ok::<_, steve_store::StoreError>((
                    repo.count_call_attempts(&call.id)?,
                    repo.count_cycle_attempts(&run.parent_item_id, run.parent_cycle)?,
                ))
            })?;
            if call_attempts >= per_call_limit {
                let _dec_id = self.pause_and_open_decision(
                    &run.id,
                    WfPauseReason::BudgetExhausted,
                    "ask",
                    DecisionPrompt::WorkflowBudgetExhausted {
                        scope: WfBudgetScope::PerCall,
                        used: call_attempts,
                        limit: per_call_limit,
                    },
                    format!(
                        "call_id={} per_call budget {call_attempts}/{per_call_limit}",
                        call.id
                    ),
                    &[
                        WfDecisionChoice::AddBudget,
                        WfDecisionChoice::Cancel,
                        WfDecisionChoice::SingleSession,
                    ],
                )?;
                return Ok(StepApplyOutcome::PausedWithDecision);
            }
            if cycle_attempts >= total_limit {
                let _dec_id = self.pause_and_open_decision(
                    &run.id,
                    WfPauseReason::BudgetExhausted,
                    "ask",
                    DecisionPrompt::WorkflowBudgetExhausted {
                        scope: WfBudgetScope::CycleTotal,
                        used: cycle_attempts,
                        limit: total_limit,
                    },
                    format!("cycle_total budget {cycle_attempts}/{total_limit}"),
                    &[
                        WfDecisionChoice::AddBudget,
                        WfDecisionChoice::Cancel,
                        WfDecisionChoice::SingleSession,
                    ],
                )?;
                return Ok(StepApplyOutcome::PausedWithDecision);
            }

            let options: CallOptions =
                serde_json::from_str(&call.options_canonical_json).map_err(|e| {
                    CoreError::Msg(format!(
                        "wf_corrupt_call_options: call {} has invalid options_canonical_json: {e}",
                        call.id
                    ))
                })?;
            let attempt_no = call_attempts + 1;

            match self.select_and_dispatch_call(run, &parent, &call, &options, attempt_no)? {
                DispatchCallResult::Dispatched => {}
                DispatchCallResult::Blocked => {
                    any_blocked = true;
                    blocked_in_batch += 1;
                }
            }
        }
        if any_blocked {
            store.write(|w| WorkflowRuns(w).pause_run(&run.id, WfPauseReason::PlacementBlocked))?;
            self.open_or_update_no_executor_card(&run.workspace_id)?;
            update_active_runs_gauge(&store);
            return Ok(StepApplyOutcome::PausedWithDecision);
        }
        Ok(StepApplyOutcome::Done)
    }

    fn select_and_dispatch_call(
        &mut self,
        run: &WfRun,
        parent: &Item,
        call: &WfCall,
        options: &CallOptions,
        attempt_no: i64,
    ) -> Result<DispatchCallResult> {
        let started = Instant::now();
        let ws = run.workspace_id.as_str();
        let store = self.db().store().clone();
        let project_opt: Option<Option<String>> = match options.project.as_deref() {
            Some(p) if !p.is_empty() => Some(Some(p.to_owned())),
            _ => Some(parent.project_id.clone()),
        };
        let title = call
            .title
            .clone()
            .or_else(|| call.phase.clone())
            .unwrap_or_else(|| format!("Workflow step {}", call.occurrence + 1));
        let opening = build_child_opening_prompt(call);
        let parent_requester =
            store.read(|r| steve_store::item_repo::requested_by(r, &parent.id))?;
        let tag_ids = if let Some(requested) = options.tags.as_deref() {
            let ws_tags = self.db().store().tags().list(ws)?;
            let mut resolved = Vec::new();
            for raw in requested {
                let needle = raw.trim();
                if needle.is_empty() {
                    continue;
                }
                if let Some(found) = ws_tags
                    .iter()
                    .find(|t| t.id == needle || t.name.eq_ignore_ascii_case(needle))
                    && !resolved.contains(&found.id)
                {
                    resolved.push(found.id.clone());
                }
            }
            resolved
        } else {
            vec![]
        };

        let create_req = crate::CreateItem {
            ws: ws.to_owned(),
            title,
            goal: call.prompt.clone(),
            bot: options
                .bot
                .as_deref()
                .filter(|b| !b.is_empty())
                .map(str::to_owned),
            team: options.team.clone(),
            node: options.node.clone(),
            node_tag: options.node_tag.clone(),
            required_specialties: options.specialties.clone().unwrap_or_default(),
            project: project_opt,
            source: Some(("workflow".into(), "script".into())),
            parent: Some(parent_ref(&parent.id)),
            tag_ids,
            opening: Some(opening),
            actor: Some("steve".into()),
            requester: parent_requester,
            ..Default::default()
        };

        let mut dispatch_rev = run.run_revision;
        let created = self.create_item_with(create_req, |w, child, placement, blocked| {
            if let Some(placement) = placement {
                dispatch_rev = WorkflowRuns(w).bump_run_revision(&run.id)?;
                WorkflowRuns(w).create_attempt(&NewWfAttempt {
                    call_id: &call.id,
                    run_id: &run.id,
                    parent_item_id: &run.parent_item_id,
                    parent_cycle: run.parent_cycle,
                    attempt_no,
                    child_item_id: Some(&child.id),
                    bot_id: Some(placement.explanation.bot.as_str()),
                    placement_id: None,
                    harness: Some(&placement.harness),
                    model: placement.model.as_deref(),
                    status: WfAttemptStatus::Dispatched,
                    dispatched_at_revision: dispatch_rev,
                    error_code: None,
                    error_message: None,
                })?;
            } else if let Some(blocked) = blocked {
                let current_rev = WorkflowRuns(w)
                    .get_run(&run.id)?
                    .map(|r| r.run_revision)
                    .unwrap_or(run.run_revision);
                WorkflowRuns(w).create_attempt(&NewWfAttempt {
                    call_id: &call.id,
                    run_id: &run.id,
                    parent_item_id: &run.parent_item_id,
                    parent_cycle: run.parent_cycle,
                    attempt_no,
                    child_item_id: Some(&child.id),
                    bot_id: None,
                    placement_id: None,
                    harness: None,
                    model: None,
                    status: WfAttemptStatus::Blocked,
                    dispatched_at_revision: current_rev,
                    error_code: Some(blocked.reason.as_str()),
                    error_message: None,
                })?;
            }
            Ok(())
        })?;

        if created.blocked.is_some() {
            metrics::counter!(
                "steve_wf_calls_total",
                "outcome" => "blocked",
                "review" => if call.is_review { "true" } else { "false" },
                "hydrated" => "false"
            )
            .increment(1);
            return Ok(DispatchCallResult::Blocked);
        }

        let duration_ms = started.elapsed().as_millis() as u64;
        tracing::info_span!(
            "wf.dispatch",
            run_id = %run.id,
            call_id = %call.id,
            attempt_no = attempt_no,
            run_revision = dispatch_rev,
            duration_ms = duration_ms,
        )
        .in_scope(|| {});
        metrics::counter!(
            "steve_wf_calls_total",
            "outcome" => "dispatched",
            "review" => if call.is_review { "true" } else { "false" },
            "hydrated" => "false"
        )
        .increment(1);
        Ok(DispatchCallResult::Dispatched)
    }

    /// Promotes a `Blocked` workflow child attempt to `Dispatched` inside the same transaction
    /// that settles `NO_EXECUTOR` or reassigns an unassigned item. Resumes the parent `wf_run`
    /// once no other blocked attempts remain and returns `Some(run_id)` to wake the workflow.
    pub(crate) fn promote_wf_blocked_child_in(
        w: &mut steve_store::W<'_>,
        item_id: &str,
        placement: &steve_types::BotPlacement,
    ) -> Result<Option<String>> {
        let Some(attempt) = WorkflowRuns(w).attempt_by_child_item(item_id)? else {
            return Ok(None);
        };
        if attempt.status != WfAttemptStatus::Blocked {
            return Ok(None);
        }
        let dispatch_rev = WorkflowRuns(w).bump_run_revision(&attempt.run_id)?;
        WorkflowRuns(w).dispatch_blocked_attempt(
            &attempt.id,
            item_id,
            placement.explanation.bot.as_str(),
            None,
            Some(&placement.harness),
            placement.model.as_deref(),
            dispatch_rev,
        )?;
        if let Some(run) = WorkflowRuns(w).get_run(&attempt.run_id)?
            && run.status == WfRunStatus::Paused
            && run.pause_reason_code == Some(WfPauseReason::PlacementBlocked)
            && WorkflowRuns(w)
                .blocked_attempt_for_run(&attempt.run_id)?
                .is_none()
        {
            WorkflowRuns(w).resume_run(&attempt.run_id)?;
            return Ok(Some(attempt.run_id));
        }
        Ok(None)
    }

    fn pause_and_open_decision(
        &mut self,
        run_id: &str,
        reason: WfPauseReason,
        kind: &str,
        template: DecisionPrompt,
        draft: String,
        options: &[WfDecisionChoice],
    ) -> Result<String> {
        let store = self.db().store().clone();
        let reason_code = reason.as_str();
        let option_strings: Vec<String> = options.iter().map(|c| c.as_str().to_owned()).collect();
        let (dec_id, _) = store.write::<_, CoreError>(|w| {
            let run = WorkflowRuns(w).pause_run(run_id, reason)?;
            let parent_session = self.active_session(&run.parent_item_id)?.map(|s| s.id);
            let source = DecisionSource::Revision {
                revision: format!("{reason_code}:{}", run.run_revision),
            };
            let (dec, _) = DecisionRows(w).open_in(&NewDecision {
                workspace_id: run.workspace_id.clone(),
                kind: kind.into(),
                subject_type: WF_RUN_SUBJECT.into(),
                subject_id: run.id.clone(),
                item_id: Some(run.parent_item_id.clone()),
                session_id: parent_session,
                question: String::new(),
                draft: Some(draft),
                options: Some(option_strings),
                requested_by: "steve".into(),
                question_template: Some(template),
                source: Some(source),
                wait: None,
            })?;
            Ok((dec.id, run))
        })?;
        update_active_runs_gauge(&store);
        Ok(dec_id)
    }

    /// Resolves a W3.3 decision card with `subject_type == "wf_run"`.
    pub(crate) fn resolve_wf_run_decision(
        &mut self,
        d: &Decision,
        cx: &steve_modules::decisions::DecisionCx<'_>,
        settle_fn: impl FnOnce(&mut steve_store::W<'_>, Value, DecisionChange) -> Result<()>,
    ) -> Result<Decision> {
        let run_id = d.subject_id.clone();
        let store = self.db().store().clone();
        let _run = store
            .read(|r| WorkflowRuns(r).get_run(&run_id))?
            .ok_or_else(|| CoreError::Status(404, "wf_run_not_found".into()))?;
        let Some(choice) = WfDecisionChoice::from_choice_str(cx.choice) else {
            return Err(CoreError::Status(
                400,
                format!("wf_invalid_decision_choice: {}", cx.choice),
            ));
        };
        let text = cx.text.trim();

        match choice {
            WfDecisionChoice::AddBudget => {
                let delta = text
                    .parse::<i64>()
                    .ok()
                    .filter(|n| *n > 0)
                    .unwrap_or(DEFAULT_BUDGET_ALLOWANCE_DELTA)
                    .min(MAX_BUDGET_ALLOWANCE_DELTA);
                store.write::<_, CoreError>(|w| {
                    settle_fn(
                        w,
                        json!({"choice": choice.as_str(), "delta": delta}),
                        DecisionChange::Resolve,
                    )?;
                    WorkflowRuns(w).add_budget_allowance(&run_id, delta)?;
                    Ok(())
                })?;
                self.schedule_wf_step(&run_id);
            }
            WfDecisionChoice::Retry => {
                store.write::<_, CoreError>(|w| {
                    settle_fn(
                        w,
                        json!({"choice": choice.as_str()}),
                        DecisionChange::Resolve,
                    )?;
                    WorkflowRuns(w).resume_run(&run_id)?;
                    Ok(())
                })?;
                self.schedule_wf_step(&run_id);
            }
            WfDecisionChoice::SingleSession => {
                let children = store.write::<_, CoreError>(|w| {
                    settle_fn(
                        w,
                        json!({"choice": choice.as_str(), "single_session_override": true}),
                        DecisionChange::Resolve,
                    )?;
                    WorkflowRuns(w).grant_single_session_override(&run_id)?;
                    let mut to_stop = Vec::new();
                    for call in WorkflowRuns(w).list_calls(&run_id)? {
                        if call.status.is_active()
                            && let Some(cid) = WorkflowRuns(w).cancel_call(&call.id)?
                        {
                            to_stop.push(cid);
                        }
                    }
                    Ok(to_stop)
                })?;
                for cid in children {
                    self.stop_wf_child_best_effort(&cid, cx.actor, "wf_single_session_override");
                }
            }
            WfDecisionChoice::Cancel => {
                let children = store.write::<_, CoreError>(|w| {
                    settle_fn(
                        w,
                        json!({"choice": choice.as_str()}),
                        DecisionChange::Resolve,
                    )?;
                    WorkflowRuns(w).set_run_terminal(&run_id, WfRunStatus::Cancelled)?;
                    let mut to_stop = Vec::new();
                    for call in WorkflowRuns(w).list_calls(&run_id)? {
                        if call.status.is_active()
                            && let Some(cid) = WorkflowRuns(w).cancel_call(&call.id)?
                        {
                            to_stop.push(cid);
                        }
                    }
                    Ok(to_stop)
                })?;
                for cid in children {
                    self.stop_wf_child_best_effort(&cid, cx.actor, "wf_owner_cancelled");
                }
            }
            WfDecisionChoice::Revise => {
                store.write::<_, CoreError>(|w| {
                    settle_fn(
                        w,
                        json!({"choice": choice.as_str(), "text": text}),
                        DecisionChange::Resolve,
                    )
                })?;
            }
        }
        update_active_runs_gauge(&store);
        self.db()
            .decision(&d.id)?
            .ok_or_else(|| CoreError::Status(404, "decision_not_found".into()))
    }

    fn stop_wf_child_best_effort(&mut self, child_id: &str, by: &str, reason: &str) {
        if let Ok(Some(child)) = self.db().item(child_id)
            && child.phase != crate::cycle::CLOSED
            && let Err(e) = self.request_cycle_stop(child_id, CloseKind::Cancel, by, reason, None)
        {
            tracing::warn!(child_id, reason, "stop workflow child failed: {e}");
        }
    }
}

enum DispatchCallResult {
    Dispatched,
    Blocked,
}
