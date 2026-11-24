//! Dynamic JavaScript script-workflow orchestrator (WF-3).
//!
//! Coordinates sandboxed single-step JS replay (`WorkflowStepExecutor`) against the normalized
//! `wf_runs` / `wf_script_versions` / `wf_calls` / `wf_call_attempts` / `wf_version_calls` ledger,
//! W3.1 deterministic placement (`placement::select_worker` + PR #544 coordinator policy),
//! W3.2 two-phase cycle settlement (`cycle.rs`), W3.3 owner decisions (`subject_type = "wf_run"`),
//! live SQL `COUNT(*)` cycle budgets, and Hub-authoritative 4-rule independent final review.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::Arc;
use std::time::Instant;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use steve_store::decision_repo::DecisionChange;
use steve_store::workflow_runs::{
    NewWfAttempt, NewWfCall, ReviewGateFailure, ReviewGateVerdict, WfAttemptStatus, WfCall,
    WfCallAttempt, WfCallStatus, WfGitFingerprint, WfRun, WfRunStatus, WorkflowRuns, pause_reason,
};
use steve_store::{Decision, DecisionRows, DecisionSource, NewDecision, NewItem, Store};
use steve_types::{DecisionPrompt, PlacementBlocked};
use steve_workflow_api::{
    CallOptions, CallSpec, JsonSchemaSubset, MAX_RESULT_BYTES, MAX_SCRIPT_SOURCE_BYTES,
    RecordedCall, ResultValidationError, StepFrame, StepInput, ValidationViolationCode,
    WorkflowState, sha256_hex, to_canonical_json, validate_structured_result,
};

use crate::cycle::{CloseKind, ClosePolicy};
use crate::error::{CoreError, Result};
use crate::repo::{Item, Session};
use crate::{Core, Output, Tx};

/// Decision `subject_type` for all script-workflow owner decision cards.
pub const WF_RUN_SUBJECT: &str = "wf_run";

/// Structured `item.source.1` tag identifying a script-workflow child item (distinct from legacy
/// DAG `"workflow"` items until WF-0 removes the legacy graph).
pub const WF_CHILD_SOURCE_KIND: &str = "wf_run";

/// Workspace setting key aligned with PR #544 (`reform/steve-coordinator`): `"ask"` | `"steve"`.
pub const SETTING_NO_EXECUTOR: &str = "noExecutor";

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
        reason: Option<String>,
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
        // Also check whether the item was tagged as a workflow child without an attempt row.
        let tagged = store
            .items()
            .get(item_id)
            .map_err(|e| record("storage_error", e.to_string()))?
            .is_some_and(|i| i.source.1 == WF_CHILD_SOURCE_KIND);
        if tagged {
            return Err(record(
                "attempt_not_active",
                format!("workflow child {item_id} has no active attempt"),
            ));
        }
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

struct PreparedStep {
    run: WfRun,
    input: StepInput,
}

enum StepApplyOutcome {
    Done,
    StaleRetry,
    PausedWithDecision,
}

fn update_active_runs_gauge(store: &Store) {
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

fn no_executor_allows_steve(store: &Store, ws: &str) -> Result<bool> {
    Ok(store
        .read(|r| steve_store::WorkspaceSettings(r).get(ws, SETTING_NO_EXECUTOR))?
        .is_some_and(|v| v.trim() == "steve"))
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
/// Reuses the frozen `CloseRequest.fingerprint` when present (including Node-gathered facts),
/// falling back to `cycle::fingerprint(cwd)` for local Git worktrees.
fn capture_git_fingerprint(item: &Item) -> Option<WfGitFingerprint> {
    let raw = item
        .closing
        .as_ref()
        .and_then(|c| c.fingerprint.clone())
        .or_else(|| crate::cycle::fingerprint(item.cwd.as_deref()))?;
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

/// Checks whether `recorded_head^{tree}` in `repo_dir` matches `recorded_tree` when a child's
/// worktree directory has already been tidied after committing.
fn git_commit_tree_matches(repo_dir: &str, recorded_head: &str, recorded_tree: &str) -> bool {
    let path = std::path::Path::new(repo_dir);
    if !path.exists() {
        return false;
    }
    let mut cmd = crate::git::command();
    cmd.arg("-C")
        .arg(repo_dir)
        .args([
            "rev-parse",
            "--verify",
            &format!("{recorded_head}^{{tree}}"),
        ])
        .env("GIT_TERMINAL_PROMPT", "0");
    match crate::util::run_deadline(cmd, crate::cycle::PRINT_DEADLINE) {
        Ok((true, out, _)) => out.trim() == recorded_tree,
        _ => false,
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

    async fn workflow_run_lock(&self, run_id: &str) -> Arc<tokio::sync::Mutex<()>> {
        let mut locks = self
            .0
            .workflow_run_locks
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        locks
            .entry(run_id.to_owned())
            .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(())))
            .clone()
    }

    /// MCP entrypoint for `steve_workflow_run`.
    pub async fn tool_workflow_run(&self, session: &Session, args: Value) -> Result<Value> {
        let action: WfRunAction = serde_json::from_value(args)
            .map_err(|e| CoreError::Status(400, format!("wf_invalid_args: {e}")))?;
        let Some(item_id) = session.item_id.as_deref() else {
            return Err(CoreError::Status(
                400,
                "wf_session_missing_item: session is not bound to an item".into(),
            ));
        };
        let is_child = self.tx().is_wf_run_child(item_id)?;
        if is_child {
            return match action {
                WfRunAction::Get => {
                    let run_id = self
                        .store()
                        .store()
                        .read(|r| WorkflowRuns(r).attempt_by_child_item(item_id))?
                        .map(|a| a.run_id)
                        .ok_or_else(|| CoreError::Status(404, "wf_run_not_found".into()))?;
                    let snap = self.tx().wf_run_snapshot(&run_id)?;
                    Ok(serde_json::to_value(snap)?)
                }
                WfRunAction::Fail { reason } => {
                    let reason = reason.trim();
                    if reason.is_empty() {
                        return Err(CoreError::Status(400, "wf_fail_reason_required".into()));
                    }
                    self.tx().wf_child_report_failure(session, reason)?;
                    Ok(json!({
                        "status": "failing",
                        "child_item_id": item_id,
                        "reason": reason,
                        "end_turn": true,
                    }))
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
                    .submit_workflow_script(item_id, &script, Some(&session.id))
                    .await?;
                Ok(serde_json::to_value(snap)?)
            }
            WfRunAction::Get => {
                let item = self.tx().item(item_id)?;
                let run = self
                    .store()
                    .store()
                    .read(|r| WorkflowRuns(r).latest_run_for_cycle(item_id, item.cycle))?;
                match run {
                    Some(r) => Ok(serde_json::to_value(self.tx().wf_run_snapshot(&r.id)?)?),
                    None => Ok(json!({ "run": null })),
                }
            }
            WfRunAction::Pause { reason: _ } => {
                let run_id = self
                    .tx()
                    .wf_pause_parent_run(item_id, pause_reason::COORDINATOR_PAUSED)?;
                let snap = self.tx().wf_run_snapshot(&run_id)?;
                Ok(serde_json::to_value(snap)?)
            }
            WfRunAction::Resume => {
                let run_id = self.tx().wf_resume_parent_run(item_id, false)?;
                let snap = self.step_workflow_run(&run_id).await?;
                Ok(serde_json::to_value(snap)?)
            }
            WfRunAction::Cancel { reason } => {
                let run_id = self.tx().wf_cancel_parent_run(
                    item_id,
                    reason.as_deref().unwrap_or("coordinator_cancelled"),
                )?;
                let snap = self.tx().wf_run_snapshot(&run_id)?;
                Ok(serde_json::to_value(snap)?)
            }
            WfRunAction::Retry { call_id } => {
                let (run_id, should_step) = self.tx().wf_retry_call(item_id, &call_id)?;
                let snap = if should_step {
                    self.step_workflow_run(&run_id).await?
                } else {
                    self.tx().wf_run_snapshot(&run_id)?
                };
                Ok(serde_json::to_value(snap)?)
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
        let (run_id, should_step) =
            self.tx()
                .wf_submit_script(parent_item_id, script_source, session_id)?;
        if should_step {
            self.step_workflow_run(&run_id).await
        } else {
            self.tx().wf_run_snapshot(&run_id)
        }
    }

    /// Owner structured takeover for a script-workflow child item (§2.4 #2):
    /// validates `result` against the call's schema + <= 32 KiB + review rules, stages closing
    /// via `request_delivery_close_in`, and commits atomically through `commit_cycle`.
    pub fn wf_child_owner_deliver(
        &self,
        child_item_id: &str,
        result: &Value,
        summary: &str,
        actor: &str,
    ) -> Result<Item> {
        let canonical = validate_child_result(self.store().store(), child_item_id, Some(result))
            .map_err(|e| CoreError::Status(409, e.to_string()))?
            .ok_or_else(|| {
                CoreError::Status(
                    404,
                    format!("wf_not_child_item: {child_item_id} is not a workflow child item"),
                )
            })?;
        let mut t = self.tx();
        let item = t.item(child_item_id)?;
        if item.phase == crate::cycle::CLOSED {
            return Err(crate::cycle::conflict("wf_child_already_closed"));
        }
        let summary_text = if summary.trim().is_empty() {
            "owner_structured_delivery"
        } else {
            summary.trim()
        };
        t.db()
            .timeline_add(child_item_id, actor, "me", summary_text, None)?;
        let item = t.request_delivery_close_in(
            child_item_id,
            actor,
            ClosePolicy::Owner,
            true,
            summary_text,
            None,
            vec![],
            |w, committed_item| {
                let git_fp = capture_git_fingerprint(committed_item);
                WorkflowRuns(w).stage_attempt_closing_with_git(
                    child_item_id,
                    committed_item.delivery_revision,
                    &canonical,
                    "[]",
                    git_fp.as_ref(),
                )?;
                Ok(())
            },
        )?;
        Ok(item)
    }

    /// Executes one replay & dispatch step for `run_id`, serialized by a per-run async lock.
    ///
    /// Never holds Core's state mutex across the `--workflow-step` subprocess `.await`.
    pub async fn step_workflow_run(&self, run_id: &str) -> Result<WfRunSnapshot> {
        let lock = self.workflow_run_lock(run_id).await;
        let _guard = lock.lock().await;

        let Some(executor) = self.workflow_executor() else {
            return Err(CoreError::Msg(
                "wf_executor_not_wired: workflow step executor is not configured".into(),
            ));
        };

        // Loop handles at most 2 retries if run_revision moved concurrently while executing JS.
        for _ in 0..3 {
            let Some(prepared) = self.tx().wf_prepare_step(run_id)? else {
                return self.tx().wf_run_snapshot(run_id);
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
                StepFrame::Failed { .. } => "failed",
            };
            metrics::histogram!("steve_wf_step_duration_seconds", "outcome" => outcome_label)
                .record(elapsed.as_secs_f64());

            let outcome = self.tx().wf_apply_step_frame(&prepared.run, frame)?;
            update_active_runs_gauge(self.store().store());
            match outcome {
                StepApplyOutcome::Done | StepApplyOutcome::PausedWithDecision => break,
                StepApplyOutcome::StaleRetry => continue,
            }
        }
        self.tx().wf_run_snapshot(run_id)
    }

    /// Crash/restart continuation: scans all `running` `wf_runs` and idempotently steps each one.
    /// Already-dispatched calls are never duplicated; settled calls advance the script immediately.
    pub async fn resume_running_workflows(&self) -> Result<usize> {
        let runs = self
            .store()
            .store()
            .read(|r| WorkflowRuns(r).list_runs_by_status(WfRunStatus::Running))?;
        update_active_runs_gauge(self.store().store());
        let mut stepped = 0usize;
        for run in runs {
            if self.workflow_executor().is_some() {
                if let Err(e) = self.step_workflow_run(&run.id).await {
                    tracing::warn!(run_id = %run.id, "workflow recovery step failed: {e}");
                } else {
                    stepped += 1;
                }
            }
        }
        Ok(stepped)
    }
}

impl Tx<'_> {
    /// Schedules an asynchronous workflow step wakeup to be spawned once the Core lock drops.
    pub(crate) fn schedule_wf_step(&mut self, run_id: &str) {
        if !self.st.wf_step_wakeups.iter().any(|r| r == run_id) {
            self.st.wf_step_wakeups.push(run_id.to_owned());
        }
    }

    /// Schedules all `Running` workflow runs on startup (`Core::ready`).
    pub(crate) fn schedule_running_wf_runs(&mut self) -> Result<usize> {
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
    /// (zero active attempts, or a `Queued` call with an available parallel slot and free mutex).
    pub(crate) fn wf_run_tick(&mut self) -> Result<()> {
        let store = self.db().store().clone();
        let runs = store.read(|r| WorkflowRuns(r).list_runs_by_status(WfRunStatus::Running))?;
        for run in runs {
            let budget = WfBudget::load(&store, &run.workspace_id)?;
            let needs_step = store.read(|r| {
                let mut repo = WorkflowRuns(r);
                let active_attempts = repo.list_active_attempts_for_run(&run.id)?;
                if active_attempts.is_empty() {
                    return Ok::<_, steve_store::StoreError>(true);
                }
                let dispatched = repo.count_active_dispatched(&run.id)?;
                if dispatched >= budget.max_parallel_sessions {
                    return Ok(false);
                }
                for call in repo.list_calls(&run.id)? {
                    if call.status == WfCallStatus::Queued {
                        let mutex_free = match call.mutex_key.as_deref() {
                            Some(mk) => !repo.has_active_mutex(&run.workspace_id, mk, &call.id)?,
                            None => true,
                        };
                        if mutex_free {
                            return Ok(true);
                        }
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

    /// Returns true if `item_id` is a script-workflow child item.
    pub(crate) fn is_wf_run_child(&self, item_id: &str) -> Result<bool> {
        if let Some(item) = self.db().item(item_id)?
            && item.source.1 == WF_CHILD_SOURCE_KIND
        {
            return Ok(true);
        }
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
        let budget = WfBudget::load(store, &run.workspace_id)?;
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
            .map(|d| d.id);
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
                if matches!(
                    run.status,
                    WfRunStatus::Completed | WfRunStatus::Cancelled | WfRunStatus::Superseded
                ) {
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
                        pause_reason::BUDGET_EXHAUSTED,
                        "ask",
                        DecisionPrompt::WorkflowBudgetExhausted {
                            scope: "script_versions".into(),
                            used: versions_used,
                            limit: budget.max_script_versions,
                        },
                        format!(
                            "script_versions budget exhausted: {versions_used}/{}",
                            budget.max_script_versions
                        ),
                        vec!["cancel".into(), "single_session".into()],
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

    fn wf_pause_parent_run(&mut self, parent_item_id: &str, reason: &str) -> Result<String> {
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
        let owner_gated = matches!(
            run.pause_reason_code.as_deref(),
            Some(
                pause_reason::NON_DETERMINISTIC_REPLAY
                    | pause_reason::BUDGET_EXHAUSTED
                    | pause_reason::PLACEMENT_BLOCKED
                    | pause_reason::REVIEW_GATE_BLOCKED
                    | pause_reason::SCRIPT_FAULT
            )
        );
        if owner_gated && !by_owner {
            return Err(CoreError::Status(
                409,
                format!(
                    "wf_owner_decision_required: run is paused for {}",
                    run.pause_reason_code.as_deref().unwrap_or("unknown")
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
        update_active_runs_gauge(self.db().store());
        Ok(())
    }

    fn wf_retry_call(&mut self, parent_item_id: &str, call_id: &str) -> Result<(String, bool)> {
        let parent = self.item(parent_item_id)?;
        let store = self.db().store();
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
        let budget = WfBudget::load(store, &run.workspace_id)?;
        let per_call_limit = budget.max_attempts_per_call + run.extra_attempts_budget;
        let total_limit = budget.max_total_attempts + run.extra_attempts_budget;

        // Requeue the failed call so once budget is available (now or after owner top-up) it runs.
        store.write(|w| WorkflowRuns(w).requeue_call_for_retry(call_id))?;

        if call_attempts >= per_call_limit {
            self.pause_and_open_decision(
                &run.id,
                pause_reason::BUDGET_EXHAUSTED,
                "ask",
                DecisionPrompt::WorkflowBudgetExhausted {
                    scope: "per_call".into(),
                    used: call_attempts,
                    limit: per_call_limit,
                },
                format!(
                    "call_id={call_id} exhausted per_call budget {call_attempts}/{per_call_limit}"
                ),
                vec![
                    "add_budget".into(),
                    "cancel".into(),
                    "single_session".into(),
                ],
            )?;
            return Ok((run.id, false));
        }
        if cycle_attempts >= total_limit {
            self.pause_and_open_decision(
                &run.id,
                pause_reason::BUDGET_EXHAUSTED,
                "ask",
                DecisionPrompt::WorkflowBudgetExhausted {
                    scope: "cycle_total".into(),
                    used: cycle_attempts,
                    limit: total_limit,
                },
                format!("cycle budget exhausted {cycle_attempts}/{total_limit}"),
                vec![
                    "add_budget".into(),
                    "cancel".into(),
                    "single_session".into(),
                ],
            )?;
            return Ok((run.id, false));
        }

        if run.status == WfRunStatus::Paused {
            store.write(|w| WorkflowRuns(w).resume_run(&run.id))?;
        }
        update_active_runs_gauge(store);
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
        let mut succeeded_attempt_by_call: HashMap<&str, &WfCallAttempt> = HashMap::new();
        for a in &attempts {
            if a.status == WfAttemptStatus::Failed {
                latest_failed.insert(a.call_id.as_str(), a);
            } else if a.status == WfAttemptStatus::Succeeded {
                succeeded_attempt_by_call.insert(a.call_id.as_str(), a);
            }
        }

        let mut invalidated_any = false;
        let mut recorded_calls = BTreeMap::new();
        for c in &calls {
            match c.status {
                WfCallStatus::Completed => {
                    // Cross-version lazy hydration verification: before reusing a completed call
                    // produced by an earlier script version, verify its Git tree/commit output
                    // fingerprint (or Hub artifact existence for non-Git calls). Never hydrate an
                    // unverified or mutated Git output.
                    if run.active_script_version > 1
                        && !recorded_on_active_ver.contains(c.id.as_str())
                        && let Err((reason_code, detail)) = self.verify_output_for_hydration(
                            &parent,
                            c,
                            succeeded_attempt_by_call.get(c.id.as_str()).copied(),
                        )?
                    {
                        tracing::info!(
                            run_id = %run.id,
                            call_id = %c.id,
                            reason = reason_code,
                            detail = %detail,
                            "cross-version hydration rejected: output fingerprint verification failed; invalidating for rerun"
                        );
                        store.write(|w| {
                            WorkflowRuns(w).invalidate_call_for_rerun(&c.id, reason_code, &detail)
                        })?;
                        metrics::counter!(
                            "steve_wf_calls_total",
                            "outcome" => "invalidated",
                            "review" => if c.is_review { "true" } else { "false" },
                            "hydrated" => "false"
                        )
                        .increment(1);
                        invalidated_any = true;
                        continue;
                    }
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
                        .unwrap_or_else(|| ("child_failed".into(), "workflow child failed".into()));
                    recorded_calls.insert(
                        c.identity_hash.clone(),
                        RecordedCall::Failed {
                            error_code,
                            message,
                            settled_at_revision: c.settled_at_revision,
                        },
                    );
                }
                WfCallStatus::Cancelled => {
                    recorded_calls.insert(
                        c.identity_hash.clone(),
                        RecordedCall::Cancelled {
                            settled_at_revision: c.settled_at_revision,
                        },
                    );
                }
                WfCallStatus::Queued | WfCallStatus::Dispatched => {}
            }
        }

        // Reload run if invalidation bumped run_revision so StepInput and CAS revision match.
        let run = if invalidated_any {
            store
                .read(|r| WorkflowRuns(r).get_run(&run.id))?
                .ok_or(steve_store::StoreError::NotFound("wf_run"))?
        } else {
            run
        };

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
            limits: None,
        };
        Ok(Some(PreparedStep { run, input }))
    }

    /// Verifies a completed call's verifiable output identity before cross-version lazy hydration:
    /// - For Git-backed calls (recorded Git fingerprint OR bound to a `kind == "git"` project):
    ///   requires both `git_head` and `git_tree` to be present and still match either the live
    ///   worktree's `(HEAD, tree)` or (if the worktree was tidied) the repository's `HEAD^{tree}`.
    /// - For non-Git calls: verifies every recorded Hub artifact ID still exists.
    fn verify_output_for_hydration(
        &self,
        parent: &Item,
        call: &WfCall,
        succeeded_attempt: Option<&WfCallAttempt>,
    ) -> Result<std::result::Result<(), (&'static str, String)>> {
        let child_item = match succeeded_attempt.and_then(|a| a.child_item_id.as_deref()) {
            Some(cid) => self.db().item(cid)?,
            None => None,
        };
        let effective_project_id = child_item
            .as_ref()
            .and_then(|c| c.project_id.as_deref())
            .or(parent.project_id.as_deref());
        let project = self.db().project(effective_project_id)?;
        let is_git_project = project.as_ref().is_some_and(|p| p.kind == "git");
        let is_git_backed = is_git_project
            || call.git_head.is_some()
            || call.git_tree.is_some()
            || call.git_cwd.is_some();

        if is_git_backed {
            let (Some(rec_head), Some(rec_tree)) =
                (call.git_head.as_deref(), call.git_tree.as_deref())
            else {
                return Ok(Err((
                    "missing_git_fingerprint",
                    format!(
                        "git-backed call {} has no recorded (git_head, git_tree) fingerprint",
                        call.id
                    ),
                )));
            };
            if rec_head.is_empty() || rec_tree.is_empty() {
                return Ok(Err((
                    "missing_git_fingerprint",
                    format!("git-backed call {} has empty git fingerprint", call.id),
                )));
            }
            let cwd = call
                .git_cwd
                .as_deref()
                .or_else(|| child_item.as_ref().and_then(|c| c.cwd.as_deref()));
            if let Some(dir) = cwd.filter(|d| !d.is_empty() && std::path::Path::new(d).exists()) {
                let Some(live_raw) = crate::cycle::fingerprint(Some(dir)) else {
                    return Ok(Err((
                        "git_fingerprint_unreadable",
                        format!("unable to compute live git fingerprint for {dir}"),
                    )));
                };
                let live = crate::upstream::parse(&live_raw);
                if live.head == rec_head && live.tree == rec_tree {
                    return Ok(Ok(()));
                }
                return Ok(Err((
                    "git_fingerprint_mismatch",
                    format!(
                        "recorded {rec_head}:{rec_tree} != live {}:{} in {dir}",
                        live.head, live.tree
                    ),
                )));
            }
            // Worktree directory no longer exists (e.g. tidied after commit): verify the exact
            // commit and its tree object still match in the project's main Git repository.
            if let Some(repo_path) = project.as_ref().map(|p| p.path.as_str())
                && git_commit_tree_matches(repo_path, rec_head, rec_tree)
            {
                return Ok(Ok(()));
            }
            return Ok(Err((
                "git_fingerprint_mismatch",
                format!(
                    "worktree missing and commit tree {rec_head}^{{tree}} != {rec_tree} could not be verified"
                ),
            )));
        }

        // Non-Git call: verify any recorded Hub artifacts still exist.
        if let Some(raw_ids) = call.artifact_ids_json.as_deref()
            && let Ok(ids) = serde_json::from_str::<Vec<String>>(raw_ids)
            && !ids.is_empty()
            && let Some(cid) = child_item.as_ref().map(|c| c.id.as_str())
        {
            let present: HashSet<String> = self
                .db()
                .store()
                .artifacts()
                .list(cid)?
                .into_iter()
                .map(|a| a.id)
                .collect();
            for aid in &ids {
                if !present.contains(aid) {
                    return Ok(Err((
                        "artifact_missing",
                        format!("recorded artifact {aid} is no longer present on {cid}"),
                    )));
                }
            }
        }
        Ok(Ok(()))
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
                let code = serde_json::to_value(fault.code)
                    .ok()
                    .and_then(|v| v.as_str().map(str::to_owned))
                    .unwrap_or_else(|| "runtime_error".into());
                let _dec_id = self.pause_and_open_decision(
                    &current.id,
                    pause_reason::SCRIPT_FAULT,
                    "fail",
                    DecisionPrompt::WorkflowScriptFault {
                        fault_code: code.clone(),
                    },
                    format!("fault_code={code}: {}", fault.detail),
                    vec!["revise".into(), "cancel".into(), "single_session".into()],
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
                        Ok(StepApplyOutcome::Done)
                    }
                    ReviewGateVerdict::Blocked { reason } => {
                        let code = reason.code();
                        metrics::counter!("steve_wf_review_gate_total", "result" => code)
                            .increment(1);
                        let _dec_id = self.pause_and_open_decision(
                            &current.id,
                            pause_reason::REVIEW_GATE_BLOCKED,
                            "ask",
                            DecisionPrompt::WorkflowReviewBlocked {
                                reason: code.into(),
                            },
                            serde_json::to_string(&reason).unwrap_or_else(|_| code.into()),
                            vec!["revise".into(), "single_session".into(), "cancel".into()],
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
        // 1. Authoritative CallIdentity re-verification (never trust subprocess hashes blindly).
        for (idx, spec) in emitted_calls.iter().enumerate() {
            if spec.seq_no as usize != idx {
                let dec_id = self.pause_and_open_decision(
                    &run.id,
                    pause_reason::SCRIPT_FAULT,
                    "fail",
                    DecisionPrompt::WorkflowScriptFault {
                        fault_code: "protocol_error".into(),
                    },
                    format!(
                        "emitted_calls seq_no mismatch at index {idx}: got {}",
                        spec.seq_no
                    ),
                    vec!["revise".into(), "cancel".into(), "single_session".into()],
                )?;
                return Ok(Some(dec_id));
            }
            if let Err(err) = spec.identity.verify(&spec.prompt, &spec.options) {
                let dec_id = self.pause_and_open_decision(
                    &run.id,
                    pause_reason::SCRIPT_FAULT,
                    "fail",
                    DecisionPrompt::WorkflowScriptFault {
                        fault_code: "protocol_error".into(),
                    },
                    format!("authoritative CallIdentity verification failed at seq {idx}: {err}"),
                    vec!["revise".into(), "cancel".into(), "single_session".into()],
                )?;
                return Ok(Some(dec_id));
            }
        }

        let store = self.db().store().clone();
        // 2. Same-version strict superset check (§2.3).
        let (prev_version_calls, existing_calls) = store.read(|r| {
            let mut repo = WorkflowRuns(r);
            Ok::<_, steve_store::StoreError>((
                repo.list_version_calls(&run.id, run.active_script_version)?,
                repo.list_calls(&run.id)?,
            ))
        })?;
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
                pause_reason::NON_DETERMINISTIC_REPLAY,
                "fail",
                DecisionPrompt::WorkflowNonDeterministicReplay,
                detail,
                vec!["revise".into(), "cancel".into(), "single_session".into()],
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
                let schema_json = spec
                    .options
                    .schema
                    .as_ref()
                    .map(serde_json::to_string)
                    .transpose()?;
                let (call, created) = repo.ensure_call(&NewWfCall {
                    run_id: &run.id,
                    workspace_id: &run.workspace_id,
                    identity_hash: &spec.identity.identity_hash,
                    phase: spec.identity.phase.as_deref(),
                    title: spec.options.title.as_deref(),
                    prompt: &spec.prompt,
                    options_canonical_json: &spec.identity.options_canonical_json,
                    schema_json: schema_json.as_deref(),
                    occurrence: i64::from(spec.identity.occurrence),
                    is_review: spec.options.is_review(),
                    mutex_key: spec.options.mutex_key.as_deref(),
                })?;
                if idx >= prev_len {
                    let hydrated_from = if !created {
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

        for hash in pending_identities {
            let Some(call) = store.read(|r| WorkflowRuns(r).get_call_by_hash(&run.id, hash))?
            else {
                continue;
            };
            // Never dispatch a call that is already dispatched or already has an active attempt.
            if call.status != WfCallStatus::Queued {
                continue;
            }
            if store
                .read(|r| WorkflowRuns(r).active_attempt_for_call(&call.id))?
                .is_some()
            {
                continue;
            }

            // Enforce per-run parallel session limit (queue excess calls instead of rejecting).
            let active_dispatched =
                store.read(|r| WorkflowRuns(r).count_active_dispatched(&run.id))?;
            if active_dispatched >= budget.max_parallel_sessions {
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
                    pause_reason::BUDGET_EXHAUSTED,
                    "ask",
                    DecisionPrompt::WorkflowBudgetExhausted {
                        scope: "per_call".into(),
                        used: call_attempts,
                        limit: per_call_limit,
                    },
                    format!(
                        "call_id={} per_call budget {call_attempts}/{per_call_limit}",
                        call.id
                    ),
                    vec![
                        "add_budget".into(),
                        "cancel".into(),
                        "single_session".into(),
                    ],
                )?;
                return Ok(StepApplyOutcome::PausedWithDecision);
            }
            if cycle_attempts >= total_limit {
                let _dec_id = self.pause_and_open_decision(
                    &run.id,
                    pause_reason::BUDGET_EXHAUSTED,
                    "ask",
                    DecisionPrompt::WorkflowBudgetExhausted {
                        scope: "cycle_total".into(),
                        used: cycle_attempts,
                        limit: total_limit,
                    },
                    format!("cycle_total budget {cycle_attempts}/{total_limit}"),
                    vec![
                        "add_budget".into(),
                        "cancel".into(),
                        "single_session".into(),
                    ],
                )?;
                return Ok(StepApplyOutcome::PausedWithDecision);
            }

            let options: CallOptions =
                serde_json::from_str(&call.options_canonical_json).unwrap_or_default();
            let attempt_no = call_attempts + 1;

            match self.select_and_dispatch_call(run, &parent, &call, &options, attempt_no)? {
                DispatchCallResult::Dispatched => {}
                DispatchCallResult::Blocked(blocked) => {
                    let call_title = call
                        .title
                        .clone()
                        .unwrap_or_else(|| call.phase.clone().unwrap_or_else(|| call.id.clone()));
                    let reason_code = blocked.reason.as_str().to_owned();
                    metrics::counter!(
                        "steve_wf_calls_total",
                        "outcome" => "blocked",
                        "review" => if call.is_review { "true" } else { "false" },
                        "hydrated" => "false"
                    )
                    .increment(1);
                    let _dec_id = self.pause_and_open_decision(
                        &run.id,
                        pause_reason::PLACEMENT_BLOCKED,
                        "ask",
                        DecisionPrompt::WorkflowPlacementBlocked {
                            call_title,
                            reason: reason_code.clone(),
                        },
                        serde_json::to_string(&json!({
                            "call_id": call.id,
                            "reason": reason_code,
                            "rejected": blocked.rejected,
                        }))
                        .unwrap_or_default(),
                        vec!["assign_bot".into(), "steve".into(), "later".into()],
                    )?;
                    return Ok(StepApplyOutcome::PausedWithDecision);
                }
            }
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
        let team =
            store.read(|r| steve_store::bot_dispatch::selected(r, ws, options.team.as_deref()))?;
        let project_opt: Option<Option<String>> = match options.project.as_deref() {
            Some(p) if !p.is_empty() => Some(Some(p.to_owned())),
            _ => Some(parent.project_id.clone()),
        };
        let tags = options.tags.clone().unwrap_or_default();
        let specialties = options.specialties.clone().unwrap_or_default();
        let explicit_bot = options.bot.as_deref().filter(|b| !b.is_empty());
        let req = crate::placement::DispatchRequest {
            workspace: ws,
            bot: explicit_bot,
            team: team.as_ref(),
            project: &project_opt,
            goal: &call.prompt,
            tags: &tags,
            required_specialties: &specialties,
            node: options.node.as_deref(),
            required_nodes: None,
        };
        let model_fn = |bot: &steve_store::Bot| {
            bot.model
                .clone()
                .or_else(|| self.c.models().agent_model(ws, &bot.harness))
        };

        let selected = match crate::placement::select_worker(self.db(), self.rt(), req, model_fn) {
            Ok(pair) => Ok(pair),
            Err(CoreError::Placement(blocked))
                if explicit_bot.is_none() && no_executor_allows_steve(&store, ws)? =>
            {
                let fallback_req = crate::placement::DispatchRequest {
                    workspace: ws,
                    bot: None,
                    team: team.as_ref(),
                    project: &project_opt,
                    goal: &call.prompt,
                    tags: &tags,
                    required_specialties: &specialties,
                    node: options.node.as_deref(),
                    required_nodes: None,
                };
                crate::placement::select_coordinator_fallback(
                    self.db(),
                    self.rt(),
                    fallback_req,
                    model_fn,
                    blocked,
                )
            }
            Err(e) => Err(e),
        };

        let (placement, project) = match selected {
            Ok(pair) => pair,
            Err(CoreError::Placement(blocked)) => {
                // Record a blocked attempt row so owner settlement can promote it directly.
                store.write(|w| {
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
                        child_item_id: None,
                        bot_id: None,
                        placement_id: None,
                        harness: None,
                        model: None,
                        status: WfAttemptStatus::Blocked,
                        dispatched_at_revision: current_rev,
                        error_code: Some(blocked.reason.as_str()),
                        error_message: None,
                    })?;
                    Ok::<_, steve_store::StoreError>(())
                })?;
                return Ok(DispatchCallResult::Blocked(blocked));
            }
            Err(e) => return Err(e),
        };

        let (child_item, dispatch_rev) = self.create_child_and_attempt(
            run,
            parent,
            call,
            attempt_no,
            team.as_ref(),
            &placement,
            project.as_ref(),
            None,
        )?;
        let opening = build_child_opening_prompt(call);
        let bot_id = placement.explanation.bot.as_str();
        self.start_or_fail(&child_item.id, Some(&opening), Some(bot_id))?;

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

    #[allow(clippy::too_many_arguments)]
    fn create_child_and_attempt(
        &mut self,
        run: &WfRun,
        parent: &Item,
        call: &WfCall,
        attempt_no: i64,
        team: Option<&steve_store::bot_dispatch::Team>,
        placement: &steve_types::BotPlacement,
        project: Option<&steve_store::Project>,
        promote_blocked_attempt_id: Option<&str>,
    ) -> Result<(Item, i64)> {
        let ws = run.workspace_id.as_str();
        let title = call
            .title
            .clone()
            .or_else(|| call.phase.clone())
            .unwrap_or_else(|| format!("Workflow step {}", call.occurrence + 1));
        let bot_id = placement.explanation.bot.as_str().to_owned();
        let db = self.db().clone();
        let (child, dispatch_rev) = db.store().write::<_, CoreError>(|w| {
            let (tags, _) = crate::kinds::settle(&db, ws, &[], &title, &call.prompt)?;
            let parent_requester = steve_store::item_repo::requested_by(w, &parent.id)?;
            let child = db.item_create(
                ws,
                &NewItem {
                    title: title.clone(),
                    goal: call.prompt.clone(),
                    bot_id: bot_id.clone(),
                    project_id: project.map(|p| p.id.clone()),
                    kind: None,
                    phase: None,
                    source: Some(("workflow".into(), WF_CHILD_SOURCE_KIND.into())),
                    parent: Some(parent_ref(&parent.id)),
                    tags,
                    requested_by: parent_requester,
                },
            )?;
            if let Some(team) = team {
                steve_store::bot_dispatch::snapshot(w, &child.id, team, "inherited")?;
            }
            steve_store::bot_dispatch::record(w, ws, &child.id, None, Ok(placement))?;
            let dispatch_rev = WorkflowRuns(w).bump_run_revision(&run.id)?;
            if let Some(blocked_id) = promote_blocked_attempt_id {
                WorkflowRuns(w).dispatch_blocked_attempt(
                    blocked_id,
                    &child.id,
                    &bot_id,
                    None,
                    Some(&placement.harness),
                    placement.model.as_deref(),
                    dispatch_rev,
                )?;
            } else {
                WorkflowRuns(w).create_attempt(&NewWfAttempt {
                    call_id: &call.id,
                    run_id: &run.id,
                    parent_item_id: &run.parent_item_id,
                    parent_cycle: run.parent_cycle,
                    attempt_no,
                    child_item_id: Some(&child.id),
                    bot_id: Some(&bot_id),
                    placement_id: None,
                    harness: Some(&placement.harness),
                    model: placement.model.as_deref(),
                    status: WfAttemptStatus::Dispatched,
                    dispatched_at_revision: dispatch_rev,
                    error_code: None,
                    error_message: None,
                })?;
            }
            Ok((child, dispatch_rev))
        })?;
        Ok((child, dispatch_rev))
    }

    fn pause_and_open_decision(
        &mut self,
        run_id: &str,
        reason_code: &str,
        kind: &str,
        template: DecisionPrompt,
        draft: String,
        options: Vec<String>,
    ) -> Result<String> {
        let store = self.db().store().clone();
        let (dec_id, _) = store.write::<_, CoreError>(|w| {
            let run = WorkflowRuns(w).pause_run(run_id, reason_code)?;
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
                options: Some(options),
                requested_by: "steve".into(),
                question_template: Some(template),
                source: Some(source),
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
        let run = store
            .read(|r| WorkflowRuns(r).get_run(&run_id))?
            .ok_or_else(|| CoreError::Status(404, "wf_run_not_found".into()))?;
        let choice = cx.choice;
        let text = cx.text.trim();

        match choice {
            "add_budget" => {
                let delta = text
                    .parse::<i64>()
                    .ok()
                    .filter(|n| *n > 0)
                    .unwrap_or(DEFAULT_BUDGET_ALLOWANCE_DELTA)
                    .min(100);
                store.write::<_, CoreError>(|w| {
                    settle_fn(
                        w,
                        json!({"choice": "add_budget", "delta": delta}),
                        DecisionChange::Resolve,
                    )?;
                    WorkflowRuns(w).add_budget_allowance(&run_id, delta)?;
                    Ok(())
                })?;
                self.schedule_wf_step(&run_id);
            }
            "single_session" | "yes" => {
                let children = store.write::<_, CoreError>(|w| {
                    settle_fn(
                        w,
                        json!({"choice": choice, "single_session_override": true}),
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
            "cancel" => {
                let children = store.write::<_, CoreError>(|w| {
                    settle_fn(w, json!({"choice": "cancel"}), DecisionChange::Resolve)?;
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
            "revise" | "later" | "no" => {
                store.write::<_, CoreError>(|w| {
                    settle_fn(
                        w,
                        json!({"choice": choice, "text": text}),
                        DecisionChange::Resolve,
                    )
                })?;
            }
            "assign_bot" | "steve" => {
                let blocked_attempt = store
                    .read(|r| WorkflowRuns(r).blocked_attempt_for_run(&run_id))?
                    .ok_or_else(|| crate::cycle::conflict("wf_no_blocked_attempt"))?;
                let call = store
                    .read(|r| WorkflowRuns(r).get_call(&blocked_attempt.call_id))?
                    .ok_or_else(|| CoreError::Status(404, "wf_call_not_found".into()))?;
                let parent = self.item(&run.parent_item_id)?;
                let options: CallOptions =
                    serde_json::from_str(&call.options_canonical_json).unwrap_or_default();
                let ws = run.workspace_id.as_str();
                let team = store.read(|r| {
                    steve_store::bot_dispatch::selected(r, ws, options.team.as_deref())
                })?;
                let project_opt = match options.project.as_deref() {
                    Some(p) if !p.is_empty() => Some(Some(p.to_owned())),
                    _ => Some(parent.project_id.clone()),
                };
                let tags = options.tags.clone().unwrap_or_default();
                let model_fn = |bot: &steve_store::Bot| {
                    bot.model
                        .clone()
                        .or_else(|| self.c.models().agent_model(ws, &bot.harness))
                };

                let (placement, project) = if choice == "steve" {
                    let req = crate::placement::DispatchRequest {
                        workspace: ws,
                        bot: None,
                        team: team.as_ref(),
                        project: &project_opt,
                        goal: &call.prompt,
                        tags: &tags,
                        required_specialties: &[],
                        node: options.node.as_deref(),
                        required_nodes: None,
                    };
                    crate::placement::select_coordinator_fallback(
                        self.db(),
                        self.rt(),
                        req,
                        model_fn,
                        PlacementBlocked {
                            reason: steve_types::PlacementRejection::NoCandidates,
                            rejected: vec![],
                        },
                    )?
                } else {
                    if text.is_empty() {
                        return Err(CoreError::Status(
                            400,
                            "wf_assign_bot_id_required: provide target bot_id in text".into(),
                        ));
                    }
                    let req = crate::placement::DispatchRequest {
                        workspace: ws,
                        bot: Some(text),
                        team: None,
                        project: &project_opt,
                        goal: &call.prompt,
                        tags: &tags,
                        required_specialties: &[],
                        node: options.node.as_deref(),
                        required_nodes: None,
                    };
                    crate::placement::select(self.db(), self.rt(), req, model_fn)?
                };

                // Atomically create child item, promote blocked attempt, settle decision, and resume run.
                let title = call
                    .title
                    .clone()
                    .or_else(|| call.phase.clone())
                    .unwrap_or_else(|| format!("Workflow step {}", call.occurrence + 1));
                let bot_id = placement.explanation.bot.as_str().to_owned();
                let db = self.db().clone();
                let child = store.write::<_, CoreError>(|w| {
                    let (tags, _) = crate::kinds::settle(&db, ws, &[], &title, &call.prompt)?;
                    let parent_requester = steve_store::item_repo::requested_by(w, &parent.id)?;
                    let child = db.item_create(
                        ws,
                        &NewItem {
                            title: title.clone(),
                            goal: call.prompt.clone(),
                            bot_id: bot_id.clone(),
                            project_id: project.as_ref().map(|p| p.id.clone()),
                            kind: None,
                            phase: None,
                            source: Some(("workflow".into(), WF_CHILD_SOURCE_KIND.into())),
                            parent: Some(parent_ref(&parent.id)),
                            tags,
                            requested_by: parent_requester,
                        },
                    )?;
                    if let Some(team) = team.as_ref() {
                        steve_store::bot_dispatch::snapshot(w, &child.id, team, "inherited")?;
                    }
                    steve_store::bot_dispatch::record(w, ws, &child.id, None, Ok(&placement))?;
                    let dispatch_rev = WorkflowRuns(w).bump_run_revision(&run.id)?;
                    WorkflowRuns(w).dispatch_blocked_attempt(
                        &blocked_attempt.id,
                        &child.id,
                        &bot_id,
                        None,
                        Some(&placement.harness),
                        placement.model.as_deref(),
                        dispatch_rev,
                    )?;
                    settle_fn(
                        w,
                        json!({"choice": choice, "bot_id": bot_id, "child_item_id": child.id}),
                        DecisionChange::Resolve,
                    )?;
                    WorkflowRuns(w).resume_run(&run_id)?;
                    Ok(child)
                })?;
                let opening = build_child_opening_prompt(&call);
                self.start_or_fail(
                    &child.id,
                    Some(&opening),
                    Some(placement.explanation.bot.as_str()),
                )?;
                self.schedule_wf_step(&run_id);
            }
            _ => {
                return Err(CoreError::Status(
                    400,
                    format!("wf_invalid_decision_choice: {choice}"),
                ));
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
    Blocked(PlacementBlocked),
}
