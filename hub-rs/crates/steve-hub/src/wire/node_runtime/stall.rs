//! Deterministic progress observation for the active logical turn.

use super::*;
use serde::{Deserialize, Serialize};
use std::time::Duration;
use steve_store::node_write::stall::{self, Active};
use steve_types::orchestration::{
    StallBlocker, StallBlockerKind, StallEvidence, StallReason, TURN_STALL,
};

/// Repeated identical tool failures before holding the turn.
const REPEAT_LIMIT: u8 = 5;
/// Observable silence/spin budget when no non-daemon tool or subtask is actively running.
const NO_PROGRESS_MS: i64 = 15 * 60_000;
/// Extended observable budget while a genuinely in-flight tool or non-daemon subtask is running.
const ACTIVE_WORK_NO_PROGRESS_MS: i64 = 60 * 60_000;
/// Polling cadence for turn progress observation in the upkeep worker.
pub(super) const POLL_INTERVAL: Duration = Duration::from_secs(1);
/// Deduplication ring capacity for distinct verified progress signatures.
const PROGRESS_SAMPLES: usize = 64;
/// Deduplication ring capacity for distinct failure signatures.
const FAILURE_SAMPLES: usize = 32;
/// Upper bound on concurrently tracked active tools or subtasks per turn.
const ACTIVE_WORK_LIMIT: usize = 64;
/// Character cap for human-facing blocker labels embedded in stall evidence.
const BLOCKER_LABEL_MAX: usize = 120;

#[derive(Serialize, Deserialize)]
struct Failure {
    digest: Digest,
    first: i64,
    count: u8,
}

#[derive(Serialize, Deserialize)]
struct Observation {
    at: i64,
    owner: String,
    attempt: String,
    generation: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct ActiveWork {
    id: String,
    kind: StallBlockerKind,
    label: String,
    started_at: i64,
    #[serde(default = "default_true")]
    extends_deadline: bool,
    #[serde(default)]
    needs_user: bool,
}

fn default_true() -> bool {
    true
}

#[derive(Default, Serialize, Deserialize)]
struct Progress {
    observation: Option<Observation>,
    first_observed: Option<i64>,
    observed_ms: i64,
    #[serde(default)]
    workspace_epoch: u64,
    progress: Vec<Digest>,
    failures: Vec<Failure>,
    #[serde(default)]
    active_tools: Vec<ActiveWork>,
    #[serde(default)]
    active_subtasks: Vec<ActiveWork>,
    #[serde(default)]
    deferred_reported: bool,
    evidence: Option<StallEvidence>,
}

enum ObserveOutcome {
    Continue,
    Deferred(StallBlockerKind),
    Stalled(StallEvidence),
}

impl Progress {
    fn has_subtask_needs_user(&self) -> bool {
        self.active_subtasks.iter().any(|w| w.needs_user)
    }

    fn primary_active_work(&self) -> Option<&ActiveWork> {
        self.active_subtasks
            .iter()
            .chain(self.active_tools.iter())
            .filter(|w| w.extends_deadline && !w.needs_user)
            .min_by_key(|w| w.started_at)
    }

    fn observe(&mut self, at: i64, active: Option<(&str, &str, u64)>) -> ObserveOutcome {
        if self.evidence.is_some() {
            return ObserveOutcome::Continue;
        }
        self.observation = match (active, self.observation.take()) {
            (Some((owner, attempt, generation)), Some(mut prior))
                if prior.owner == owner
                    && prior.attempt == attempt
                    && prior.generation == generation =>
            {
                self.first_observed.get_or_insert(prior.at);
                self.observed_ms = self
                    .observed_ms
                    .saturating_add(at.saturating_sub(prior.at).max(0));
                prior.at = prior.at.max(at);
                Some(prior)
            }
            (Some((owner, attempt, generation)), _) => Some(Observation {
                at,
                owner: owner.to_owned(),
                attempt: attempt.to_owned(),
                generation,
            }),
            (None, _) => None,
        };
        let blocker = self.primary_active_work().cloned();
        let threshold = if blocker.is_some() {
            ACTIVE_WORK_NO_PROGRESS_MS
        } else {
            NO_PROGRESS_MS
        };
        if self.observed_ms >= threshold {
            return ObserveOutcome::Stalled(StallEvidence {
                reason: StallReason::NoProgress,
                first_at: self.first_observed.unwrap_or(at),
                detected_at: at,
                repetitions: 0,
                event: None,
                observed_ms: Some(self.observed_ms),
                blocker: blocker.map(|w| StallBlocker {
                    kind: w.kind,
                    label: w.label,
                }),
            });
        }
        if let Some(w) = blocker
            && self.observed_ms >= NO_PROGRESS_MS
            && !self.deferred_reported
        {
            self.deferred_reported = true;
            return ObserveOutcome::Deferred(w.kind);
        }
        ObserveOutcome::Continue
    }

    /// Record a verified progress signature. Returns `true` when it was new (and reset the stall
    /// clock), `false` when it duplicated a recently seen signature.
    fn advance(&mut self, digest: Digest, at: i64, bumps_workspace_epoch: bool) -> bool {
        if let Some(index) = self.progress.iter().position(|seen| *seen == digest) {
            self.progress.remove(index);
            self.progress.push(digest);
            return false;
        }
        if self.progress.len() == PROGRESS_SAMPLES {
            self.progress.remove(0);
        }
        self.progress.push(digest);
        if bumps_workspace_epoch {
            self.workspace_epoch = self.workspace_epoch.saturating_add(1);
        }
        self.observed_ms = 0;
        self.first_observed = None;
        self.deferred_reported = false;
        if let Some(observation) = &mut self.observation {
            observation.at = observation.at.max(at);
        }
        self.failures.clear();
        true
    }

    fn has_progress(&self, digest: &Digest) -> bool {
        self.progress.iter().any(|seen| seen == digest)
    }

    fn upsert_active_tool(&mut self, work: ActiveWork) -> bool {
        upsert_active(&mut self.active_tools, work)
    }

    fn remove_active_tool(&mut self, id: &str) -> bool {
        remove_active(&mut self.active_tools, id)
    }

    fn upsert_active_subtask(&mut self, work: ActiveWork) -> bool {
        upsert_active(&mut self.active_subtasks, work)
    }

    fn remove_active_subtask(&mut self, id: &str) -> bool {
        remove_active(&mut self.active_subtasks, id)
    }

    fn failed(&mut self, digest: Digest, at: i64, event: &EventId) -> Option<StallEvidence> {
        if self.evidence.is_some() {
            return None;
        }
        let mut failure = match self.failures.iter().position(|f| f.digest == digest) {
            Some(index) => self.failures.remove(index),
            None => {
                if self.failures.len() == FAILURE_SAMPLES {
                    self.failures.remove(0);
                }
                Failure {
                    digest,
                    first: at,
                    count: 0,
                }
            }
        };
        failure.count += 1;
        let evidence = (failure.count == REPEAT_LIMIT).then(|| StallEvidence {
            reason: StallReason::RepeatedFailure,
            first_at: failure.first,
            detected_at: at,
            repetitions: failure.count,
            event: Some(event.to_string()),
            observed_ms: Some(self.observed_ms),
            blocker: None,
        });
        self.failures.push(failure);
        evidence
    }
}

fn upsert_active(list: &mut Vec<ActiveWork>, work: ActiveWork) -> bool {
    if let Some(existing) = list.iter_mut().find(|w| w.id == work.id) {
        let changed = existing.kind != work.kind
            || existing.label != work.label
            || existing.extends_deadline != work.extends_deadline
            || existing.needs_user != work.needs_user;
        if changed {
            existing.kind = work.kind;
            existing.label = work.label;
            existing.extends_deadline = work.extends_deadline;
            existing.needs_user = work.needs_user;
        }
        return changed;
    }
    if list.len() >= ACTIVE_WORK_LIMIT {
        list.remove(0);
    }
    list.push(work);
    true
}

fn remove_active(list: &mut Vec<ActiveWork>, id: &str) -> bool {
    let before = list.len();
    list.retain(|w| w.id != id);
    list.len() != before
}

fn bound_label(raw: &str) -> String {
    let trimmed = raw.trim();
    if trimmed.chars().count() <= BLOCKER_LABEL_MAX {
        return trimmed.to_owned();
    }
    let mut out: String = trimmed.chars().take(BLOCKER_LABEL_MAX).collect();
    out.push('…');
    out
}

fn tool_label(tool: &ToolView) -> String {
    let name = tool
        .name
        .strip_prefix("mcp__steve__")
        .unwrap_or(&tool.name)
        .trim();
    let detail = tool
        .target
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .or_else(|| {
            let t = tool.title.trim();
            (!t.is_empty() && t != name).then_some(t)
        });
    match (name.is_empty(), detail) {
        (false, Some(d)) => bound_label(&format!("{name}: {d}")),
        (false, None) => bound_label(name),
        (true, Some(d)) => bound_label(d),
        (true, None) => "tool".into(),
    }
}

fn tool_in_flight_extends_deadline(tool: &ToolView, summary: &Progress) -> bool {
    // Only tools capable of producing verified progress extend the deadline while running.
    // Generic thinking/polling tools (`Think`, harness `Other`) do not extend on their own.
    let prospective = match tool.category {
        ToolCategory::Edit | ToolCategory::Delete | ToolCategory::Move => return true,
        ToolCategory::Read => digest_of(&(
            "read_in_flight",
            summary.workspace_epoch,
            &tool.name,
            &tool.target,
            &tool.action,
        ))
        .ok(),
        ToolCategory::Search => digest_of(&(
            "search",
            summary.workspace_epoch,
            &tool.name,
            &tool.target,
            &tool.action,
        ))
        .ok(),
        ToolCategory::Fetch => digest_of(&(
            "fetch",
            summary.workspace_epoch,
            &tool.name,
            &tool.target,
            &tool.action,
        ))
        .ok(),
        ToolCategory::Exec => digest_of(&(
            "exec",
            summary.workspace_epoch,
            &tool.name,
            &tool.target,
            &tool.action,
        ))
        .ok(),
        ToolCategory::Steve => digest_of(&(
            "mcp",
            tool.origin,
            summary.workspace_epoch,
            &tool.name,
            &tool.action,
            &tool.target,
        ))
        .ok(),
        ToolCategory::Other if matches!(tool.origin, ToolOrigin::Steve | ToolOrigin::External) => {
            digest_of(&(
                "mcp",
                tool.origin,
                summary.workspace_epoch,
                &tool.name,
                &tool.action,
                &tool.target,
            ))
            .ok()
        }
        ToolCategory::Think | ToolCategory::Other => return false,
    };
    prospective.is_some_and(|d| !summary.has_progress(&d))
}

fn succeeded_tool_progress(
    tool: &ToolView,
    workspace_epoch: u64,
) -> Result<Option<(&'static str, Digest, bool)>, StoreError> {
    if let Some(ToolEvidence::ContentChanged { signature }) = &tool.evidence {
        let digest = digest_of(&("content_changed", signature)).map_err(conflict)?;
        return Ok(Some(("content_changed", digest, true)));
    }
    match tool.category {
        ToolCategory::Edit => {
            if let Some(Action::Write {
                paths,
                change: Some(change),
                ..
            }) = &tool.action
            {
                let digest = digest_of(&("edit", paths, change)).map_err(conflict)?;
                return Ok(Some(("content_changed", digest, true)));
            }
            let diffs: Vec<_> = tool
                .result
                .iter()
                .filter_map(|p| match p {
                    Part::Diff { path, old, new, .. } if old.as_deref() != Some(new.as_str()) => {
                        Some((path, Digest::sha256(new.as_bytes())))
                    }
                    _ => None,
                })
                .collect();
            if !diffs.is_empty() {
                let digest = digest_of(&("edit_diff", diffs)).map_err(conflict)?;
                return Ok(Some(("content_changed", digest, true)));
            }
            Ok(None)
        }
        ToolCategory::Delete | ToolCategory::Move => {
            let digest = digest_of(&(
                "mutate",
                tool.category,
                &tool.name,
                &tool.action,
                &tool.target,
            ))
            .map_err(conflict)?;
            Ok(Some(("tool_mutate", digest, true)))
        }
        ToolCategory::Read => {
            // Include result fingerprint so chunked reads of different line ranges in the same
            // file count as progress while re-reading the identical slice deduplicates.
            let result_digest = digest_of(&tool.result).map_err(conflict)?;
            let digest = digest_of(&(
                "read",
                workspace_epoch,
                &tool.name,
                &tool.target,
                &tool.action,
                result_digest,
            ))
            .map_err(conflict)?;
            Ok(Some(("tool_read", digest, false)))
        }
        ToolCategory::Search => {
            let digest = digest_of(&(
                "search",
                workspace_epoch,
                &tool.name,
                &tool.target,
                &tool.action,
            ))
            .map_err(conflict)?;
            Ok(Some(("tool_search", digest, false)))
        }
        ToolCategory::Fetch => {
            let digest = digest_of(&(
                "fetch",
                workspace_epoch,
                &tool.name,
                &tool.target,
                &tool.action,
            ))
            .map_err(conflict)?;
            Ok(Some(("tool_fetch", digest, false)))
        }
        ToolCategory::Exec => {
            let digest = digest_of(&(
                "exec",
                workspace_epoch,
                &tool.name,
                &tool.target,
                &tool.action,
            ))
            .map_err(conflict)?;
            Ok(Some(("tool_exec", digest, false)))
        }
        ToolCategory::Steve => {
            let digest = digest_of(&(
                "mcp",
                tool.origin,
                workspace_epoch,
                &tool.name,
                &tool.action,
                &tool.target,
            ))
            .map_err(conflict)?;
            Ok(Some(("tool_mcp", digest, false)))
        }
        ToolCategory::Other if matches!(tool.origin, ToolOrigin::Steve | ToolOrigin::External) => {
            let digest = digest_of(&(
                "mcp",
                tool.origin,
                workspace_epoch,
                &tool.name,
                &tool.action,
                &tool.target,
            ))
            .map_err(conflict)?;
            Ok(Some(("tool_mcp", digest, false)))
        }
        ToolCategory::Think | ToolCategory::Other => Ok(None),
    }
}

fn failed_tool_signature(tool: &ToolView) -> Result<Option<Digest>, StoreError> {
    if let Some(ToolEvidence::Failed { signature }) = &tool.evidence {
        return Ok(Some(signature.clone()));
    }
    let has_identity = tool.origin == ToolOrigin::Steve
        || tool.action.is_some()
        || tool.target.is_some()
        || tool.error.is_some();
    if !has_identity {
        return Ok(None);
    }
    let digest = digest_of(&(
        "tool_failed",
        tool.origin,
        &tool.name,
        tool.category,
        &tool.action,
        &tool.target,
        &tool.error,
    ))
    .map_err(conflict)?;
    Ok(Some(digest))
}

fn subtask_blocker_kind(origin: SubtaskOrigin) -> StallBlockerKind {
    match origin {
        SubtaskOrigin::Subagent => StallBlockerKind::Subagent,
        SubtaskOrigin::BackgroundTask => StallBlockerKind::BackgroundTask,
        SubtaskOrigin::NativeWorkflow => StallBlockerKind::NativeWorkflow,
    }
}

fn defer_reason(kind: StallBlockerKind) -> &'static str {
    match kind {
        StallBlockerKind::Tool => "active_tool",
        StallBlockerKind::Subagent => "active_subagent",
        StallBlockerKind::BackgroundTask => "active_background_task",
        StallBlockerKind::NativeWorkflow => "active_workflow",
    }
}

fn stall_eval_reason(evidence: &StallEvidence) -> &'static str {
    match (evidence.reason, evidence.blocker.as_ref().map(|b| b.kind)) {
        (StallReason::RepeatedFailure, _) => "repeated_failure",
        (StallReason::NoProgress, None) => "no_progress",
        (StallReason::NoProgress, Some(StallBlockerKind::Tool)) => "active_tool_timeout",
        (StallReason::NoProgress, Some(StallBlockerKind::Subagent)) => "active_subagent_timeout",
        (StallReason::NoProgress, Some(StallBlockerKind::BackgroundTask)) => {
            "active_background_task_timeout"
        }
        (StallReason::NoProgress, Some(StallBlockerKind::NativeWorkflow)) => {
            "active_workflow_timeout"
        }
    }
}

impl NodeRuntime {
    pub(super) fn stall_evidence_in(
        &self,
        w: &mut W<'_>,
        attempt: &str,
    ) -> Result<Option<StallEvidence>, StoreError> {
        let summary: Option<Progress> = stall::read_by_attempt(w, attempt)?;
        Ok(summary.and_then(|s| s.evidence))
    }

    pub(super) fn observe_tool_in(
        &self,
        w: &mut W<'_>,
        attempt: &str,
        tool: &ToolView,
        ev: &NodeEvent,
    ) -> Result<(), StoreError> {
        let Some(active) = stall::for_attempt(w, self.node_id.as_str(), attempt)? else {
            return Ok(());
        };
        let mut summary: Progress = stall::read(w, &active.turn)?.unwrap_or_default();
        let at = self.clock.now();
        let mut dirty = false;
        let mut evidence = None;
        match tool.phase {
            ToolPhase::Pending | ToolPhase::Running => {
                let work = ActiveWork {
                    id: tool.id.to_string(),
                    kind: StallBlockerKind::Tool,
                    label: tool_label(tool),
                    started_at: at,
                    extends_deadline: tool_in_flight_extends_deadline(tool, &summary),
                    needs_user: false,
                };
                dirty = summary.upsert_active_tool(work);
            }
            ToolPhase::Succeeded => {
                dirty |= summary.remove_active_tool(tool.id.as_str());
                if let Some((reason, digest, bumps_workspace)) =
                    succeeded_tool_progress(tool, summary.workspace_epoch)?
                {
                    let advanced = summary.advance(digest, at, bumps_workspace);
                    self.stage_stall_eval(
                        if advanced { "advanced" } else { "deduplicated" },
                        reason,
                    );
                    dirty = true;
                }
            }
            ToolPhase::Failed => {
                dirty |= summary.remove_active_tool(tool.id.as_str());
                if !active.waiting
                    && let Some(sig) = failed_tool_signature(tool)?
                {
                    evidence = summary.failed(sig, at, &ev.id);
                    if let Some(ev) = &evidence {
                        self.stage_stall_eval("stalled", stall_eval_reason(ev));
                    }
                    dirty = true;
                }
            }
        }
        if !dirty && evidence.is_none() {
            return Ok(());
        }
        self.save_progress(w, &active, summary, evidence)
    }

    pub(super) fn observe_subtask_in(
        &self,
        w: &mut W<'_>,
        attempt: &str,
        subtask: &SubtaskView,
    ) -> Result<(), StoreError> {
        let Some(active) = stall::for_attempt(w, self.node_id.as_str(), attempt)? else {
            return Ok(());
        };
        let mut summary: Progress = stall::read(w, &active.turn)?.unwrap_or_default();
        let at = self.clock.now();
        match subtask.status {
            SubtaskStatus::Queued | SubtaskStatus::Running | SubtaskStatus::NeedsYou => {
                let is_daemon = subtask.actor.as_deref() == Some("daemon");
                let label = if subtask.title.trim().is_empty() {
                    subtask.id.to_string()
                } else {
                    bound_label(&subtask.title)
                };
                summary.upsert_active_subtask(ActiveWork {
                    id: subtask.id.to_string(),
                    kind: subtask_blocker_kind(subtask.origin),
                    label,
                    started_at: at,
                    extends_deadline: !is_daemon,
                    needs_user: subtask.status == SubtaskStatus::NeedsYou,
                });
            }
            SubtaskStatus::Done | SubtaskStatus::Failed | SubtaskStatus::Cancelled => {
                summary.remove_active_subtask(subtask.id.as_str());
            }
        }
        let digest = digest_of(&(
            "subtask",
            &subtask.id,
            subtask.origin,
            subtask.status,
            &subtask.title,
            &subtask.progress,
            &subtask.summary,
            &subtask.error,
        ))
        .map_err(conflict)?;
        let advanced = summary.advance(digest, at, false);
        self.stage_stall_eval(
            if advanced { "advanced" } else { "deduplicated" },
            "subtask_changed",
        );
        self.save_progress(w, &active, summary, None)
    }

    pub(super) fn observe_plan_in(
        &self,
        w: &mut W<'_>,
        attempt: &str,
        entries: &[PlanEntry],
    ) -> Result<(), StoreError> {
        if entries.is_empty() {
            return Ok(());
        }
        let Some(active) = stall::for_attempt(w, self.node_id.as_str(), attempt)? else {
            return Ok(());
        };
        let mut summary: Progress = stall::read(w, &active.turn)?.unwrap_or_default();
        let digest = digest_of(&("plan", entries)).map_err(conflict)?;
        let advanced = summary.advance(digest, self.clock.now(), false);
        self.stage_stall_eval(
            if advanced { "advanced" } else { "deduplicated" },
            "plan_changed",
        );
        self.save_progress(w, &active, summary, None)
    }

    pub(super) fn observe_turns(&self) -> Result<(), StoreError> {
        let generation = self.observation_generation();
        let at = self.clock.now();
        self.write(|w| {
            for active in stall::running(w, self.node_id.as_str())? {
                self.observe_active_in(w, &active, generation, at)?;
            }
            Ok(())
        })
    }

    fn observation_generation(&self) -> Option<u64> {
        (self.is_reconciled() && !self.is_holding() && !self.paused.load(Ordering::Acquire))
            .then(|| self.node.execution_generation())
            .flatten()
    }

    pub(super) fn observe_attempt_start(
        &self,
        w: &mut W<'_>,
        attempt: &str,
    ) -> Result<(), StoreError> {
        if let Some(active) = stall::for_attempt(w, self.node_id.as_str(), attempt)? {
            self.observe_active_in(w, &active, self.observation_generation(), self.clock.now())?;
        }
        Ok(())
    }

    pub(super) fn observe_workspace_in(
        &self,
        w: &mut W<'_>,
        attempt: &str,
        workspace: &WorkspaceRef,
        revision: &Revision,
    ) -> Result<(), StoreError> {
        let Some(active) = stall::for_attempt(w, self.node_id.as_str(), attempt)? else {
            return Ok(());
        };
        if !stall::owns_workspace(w, &active.session, workspace)? {
            return Ok(());
        }
        let mut summary: Progress = stall::read(w, &active.turn)?.unwrap_or_default();
        let signature = digest_of(&("workspace", workspace, revision)).map_err(conflict)?;
        let advanced = summary.advance(signature, self.clock.now(), true);
        self.stage_stall_eval(
            if advanced { "advanced" } else { "deduplicated" },
            "workspace_changed",
        );
        self.save_progress(w, &active, summary, None)
    }

    fn observe_active_in(
        &self,
        w: &mut W<'_>,
        active: &Active,
        generation: Option<u64>,
        at: i64,
    ) -> Result<(), StoreError> {
        let mut summary: Progress = stall::read(w, &active.turn)?.unwrap_or_default();
        let paused_for_user = active.waiting || summary.has_subtask_needs_user();
        let generation = generation.filter(|_| !paused_for_user);
        if summary.evidence.is_some() || (generation.is_none() && summary.observation.is_none()) {
            return Ok(());
        }
        let evidence = match summary.observe(
            at,
            generation.map(|g| (self.owner.as_str(), active.attempt.as_str(), g)),
        ) {
            ObserveOutcome::Continue => None,
            ObserveOutcome::Deferred(kind) => {
                self.stage_stall_eval("deferred", defer_reason(kind));
                None
            }
            ObserveOutcome::Stalled(ev) => {
                self.stage_stall_eval("stalled", stall_eval_reason(&ev));
                Some(ev)
            }
        };
        self.save_progress(w, active, summary, evidence)
    }

    fn stage_stall_eval(&self, outcome: &'static str, reason: &'static str) {
        self.staged
            .lock()
            .unwrap()
            .push(Tell::StallEval { outcome, reason });
    }

    fn save_progress(
        &self,
        w: &mut W<'_>,
        a: &Active,
        mut summary: Progress,
        evidence: Option<StallEvidence>,
    ) -> Result<(), StoreError> {
        if let Some(evidence) = &evidence {
            summary.evidence = Some(evidence.clone());
        }
        stall::save(w, &a.turn, &summary)?;
        let Some(evidence) = evidence else {
            return Ok(());
        };
        let scope = node_sessions::startup::scope(w, &a.session)?;
        let template = match evidence.reason {
            StallReason::RepeatedFailure => steve_types::DecisionPrompt::TurnRepeatedFailure,
            StallReason::NoProgress => steve_types::DecisionPrompt::TurnNoProgress {
                minutes: Some((evidence.observed_ms.unwrap_or(NO_PROGRESS_MS) / 60_000).max(1)),
                blocker: evidence.blocker.clone(),
            },
        };
        let card = steve_store::DecisionRows(w).open(&steve_store::NewDecision {
            workspace_id: scope.workspace,
            kind: "fail".into(),
            subject_type: TURN_STALL.into(),
            subject_id: a.turn.clone(),
            item_id: scope.item,
            session_id: Some(a.session.clone()),
            options: Some(vec!["continue".into(), "drop".into()]),
            requested_by: "steve".into(),
            question_template: Some(template),
            source: Some(steve_store::DecisionSource::TurnStall {
                attempt: a.attempt.clone(),
            }),
            ..Default::default()
        })?;
        stall::bind_decision(w, &a.turn, &a.attempt, &card.id)?;
        node_write::cancel_attempt_in(w, &a.attempt, a.revision, self.clock.now())?;
        self.stop_command_in(w, &a.attempt, &a.operation, "cancel")?;
        self.staged.lock().unwrap().push(Tell::Stalled(evidence));
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stalled(outcome: ObserveOutcome) -> Option<StallEvidence> {
        match outcome {
            ObserveOutcome::Stalled(ev) => Some(ev),
            _ => None,
        }
    }

    #[test]
    fn fifth_failure_survives_restore_and_a_different_failure_does_not_clear_it() {
        let mut summary = Progress::default();
        let event = EventId::new("ev1").unwrap();
        let hash = Digest::sha256(b"same command");
        let other = Digest::sha256(b"other command");
        for i in 0..4 {
            assert!(summary.failed(hash.clone(), i, &event).is_none());
            assert!(summary.failed(other.clone(), i * 2, &event).is_none());
        }
        let raw = serde_json::to_string(&summary).unwrap();
        let mut restored: Progress = serde_json::from_str(&raw).unwrap();
        let evidence = restored.failed(hash.clone(), 9, &event).unwrap();
        assert_eq!(evidence.repetitions, 5);
        restored.evidence = Some(evidence);
        assert!(restored.failed(hash, 10, &event).is_none());
    }

    #[test]
    fn downtime_and_approval_do_not_spend_time_and_progress_keeps_extending_the_deadline() {
        let mut summary = Progress::default();
        let started = Some(("hub1", "a", 1));
        let restarted = Some(("hub2", "a", 1));
        assert!(stalled(summary.observe(0, started)).is_none());
        assert!(stalled(summary.observe(800_000, started)).is_none());
        assert!(stalled(summary.observe(2_000_000, restarted)).is_none());
        assert!(stalled(summary.observe(2_020_000, None)).is_none());
        assert!(stalled(summary.observe(3_000_000, restarted)).is_none());
        assert!(stalled(summary.observe(3_099_999, restarted)).is_none());
        assert_eq!(
            stalled(summary.observe(3_100_000, restarted))
                .unwrap()
                .reason,
            StallReason::NoProgress
        );

        let mut summary = Progress::default();
        for i in 0_i64..40 {
            let at = i * 800_000;
            assert!(stalled(summary.observe(at, started)).is_none());
            assert!(summary.advance(Digest::sha256(&i.to_le_bytes()), at, false));
        }
        assert!(stalled(summary.observe(39 * 800_000 + NO_PROGRESS_MS - 1, started)).is_none());
        assert!(stalled(summary.observe(39 * 800_000 + NO_PROGRESS_MS, started)).is_some());
    }

    #[test]
    fn active_work_extends_budget_and_reports_real_blocker_at_extended_limit() {
        let mut summary = Progress::default();
        let run = Some(("hub1", "a", 1));
        assert!(summary.upsert_active_subtask(ActiveWork {
            id: "sub-1".into(),
            kind: StallBlockerKind::Subagent,
            label: "Codebase Researcher".into(),
            started_at: 0,
            extends_deadline: true,
            needs_user: false,
        }));
        assert!(matches!(summary.observe(0, run), ObserveOutcome::Continue));
        // Past 15m silent budget -> deferred once, not stalled
        assert!(matches!(
            summary.observe(NO_PROGRESS_MS, run),
            ObserveOutcome::Deferred(StallBlockerKind::Subagent)
        ));
        assert!(matches!(
            summary.observe(ACTIVE_WORK_NO_PROGRESS_MS - 1, run),
            ObserveOutcome::Continue
        ));
        let ev = stalled(summary.observe(ACTIVE_WORK_NO_PROGRESS_MS, run)).expect("stalls at 60m");
        assert_eq!(ev.reason, StallReason::NoProgress);
        assert_eq!(ev.observed_ms, Some(ACTIVE_WORK_NO_PROGRESS_MS));
        assert_eq!(
            ev.blocker,
            Some(StallBlocker {
                kind: StallBlockerKind::Subagent,
                label: "Codebase Researcher".into(),
            })
        );
    }

    #[test]
    fn duplicate_spin_loop_is_caught_while_workspace_epoch_allows_post_edit_recheck() {
        let mut summary = Progress::default();
        let run = Some(("hub1", "a", 1));
        let exec_tool = ToolView {
            id: CallId::new("c1").unwrap(),
            parent: None,
            origin: ToolOrigin::Harness,
            name: "run_command".into(),
            category: ToolCategory::Exec,
            title: "cargo test".into(),
            description: None,
            target: Some("cargo test".into()),
            phase: ToolPhase::Succeeded,
            action: Some(Action::Exec {
                command: "cargo test".into(),
                cwd: None,
                network: NetworkScope::Unrestricted,
            }),
            evidence: None,
            result: vec![],
            error: None,
            duration_ms: None,
        };
        assert!(matches!(summary.observe(0, run), ObserveOutcome::Continue));
        let (_, d1, _) = succeeded_tool_progress(&exec_tool, summary.workspace_epoch)
            .unwrap()
            .unwrap();
        assert!(summary.advance(d1, 0, false));
        // Repeating the exact same command without any edit is deduplicated
        for min in 1..=15 {
            let at = min * 60_000;
            let (_, d_dup, _) = succeeded_tool_progress(&exec_tool, summary.workspace_epoch)
                .unwrap()
                .unwrap();
            assert!(!summary.advance(d_dup, at, false));
            if min < 15 {
                assert!(stalled(summary.observe(at, run)).is_none());
            } else {
                assert_eq!(
                    stalled(summary.observe(at, run)).unwrap().reason,
                    StallReason::NoProgress
                );
            }
        }

        // After a real workspace change bumps workspace_epoch, re-running cargo test counts as progress
        let mut summary = Progress::default();
        let (_, d1, _) = succeeded_tool_progress(&exec_tool, summary.workspace_epoch)
            .unwrap()
            .unwrap();
        assert!(summary.advance(d1, 0, false));
        assert!(summary.advance(Digest::sha256(b"ws-rev-2"), 1000, true));
        let (_, d2, _) = succeeded_tool_progress(&exec_tool, summary.workspace_epoch)
            .unwrap()
            .unwrap();
        assert!(summary.advance(d2, 2000, false));
    }

    #[test]
    fn daemon_subtask_does_not_extend_silent_turn_deadline() {
        let mut summary = Progress::default();
        let run = Some(("hub1", "a", 1));
        assert!(summary.upsert_active_subtask(ActiveWork {
            id: "bg-daemon".into(),
            kind: StallBlockerKind::BackgroundTask,
            label: "dev server".into(),
            started_at: 0,
            extends_deadline: false,
            needs_user: false,
        }));
        assert!(stalled(summary.observe(0, run)).is_none());
        let ev = stalled(summary.observe(NO_PROGRESS_MS, run)).expect("stalls at 15m");
        assert_eq!(ev.reason, StallReason::NoProgress);
        assert!(ev.blocker.is_none());
    }
}
