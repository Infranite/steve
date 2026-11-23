//! Deterministic progress observation for the active logical turn.

use super::*;
use serde::{Deserialize, Serialize};
use steve_store::node_write::stall::{self, Active};
use steve_types::orchestration::{StallEvidence, StallReason};

const REPEAT_LIMIT: u8 = 5;
const NO_PROGRESS_MS: i64 = 15 * 60_000;
const SAMPLE_MS: i64 = 1_000;
const SAMPLES: usize = 32;

#[derive(Serialize, Deserialize)]
struct Failure {
    digest: Digest,
    first: i64,
    count: u8,
}

#[derive(Default, Serialize, Deserialize)]
struct Progress {
    at: i64,
    observing: bool,
    observed_ms: i64,
    progress: Vec<Digest>,
    failures: Vec<Failure>,
    evidence: Option<StallEvidence>,
}

impl Progress {
    fn observe(&mut self, at: i64, observable: bool) -> Option<StallEvidence> {
        if self.evidence.is_some() {
            return None;
        }
        if self.observing && observable {
            self.observed_ms += at.saturating_sub(self.at).clamp(0, SAMPLE_MS);
        }
        self.at = at;
        self.observing = observable;
        (self.observed_ms >= NO_PROGRESS_MS).then(|| StallEvidence {
            reason: StallReason::NoProgress,
            first_at: at - self.observed_ms,
            detected_at: at,
            repetitions: 0,
            event: None,
        })
    }

    fn advance(&mut self, digest: Digest) {
        // Workspace facts already seen cannot turn retries or heartbeats into fresh progress.
        if self.progress.contains(&digest) || self.progress.len() == SAMPLES {
            return;
        }
        self.progress.push(digest);
        self.observed_ms = 0;
        self.failures.clear();
    }

    fn failed(&mut self, digest: Digest, at: i64, event: &EventId) -> Option<StallEvidence> {
        if self.evidence.is_some() {
            return None;
        }
        if let Some(f) = self.failures.iter_mut().find(|f| f.digest == digest) {
            f.count += 1;
            return (f.count == REPEAT_LIMIT).then(|| StallEvidence {
                reason: StallReason::RepeatedFailure,
                first_at: f.first,
                detected_at: at,
                repetitions: f.count,
                event: Some(event.to_string()),
            });
        }
        if self.failures.len() < SAMPLES {
            self.failures.push(Failure {
                digest,
                first: at,
                count: 1,
            });
        }
        None
    }
}

impl NodeRuntime {
    pub(super) fn observe_tool_in(
        &self,
        w: &mut W<'_>,
        attempt: &str,
        tool: &ToolView,
        ev: &NodeEvent,
        received: i64,
    ) -> Result<(), StoreError> {
        let Some(action) = &tool.action else {
            return Ok(());
        };
        let signature = match (tool.phase, action) {
            (
                ToolPhase::Failed,
                Action::Exec {
                    command,
                    cwd,
                    network,
                },
            ) if !command.is_empty() => {
                digest_of(&(&tool.name, tool.category, command, cwd, network))
            }
            (ToolPhase::Failed, Action::Read { paths }) if !paths.is_empty() => {
                digest_of(&(&tool.name, tool.category, paths))
            }
            (
                ToolPhase::Succeeded,
                Action::Write {
                    paths,
                    change: Some(digest),
                    ..
                },
            ) if !paths.is_empty() => digest_of(&(paths, digest)),
            _ => return Ok(()),
        }
        .map_err(conflict)?;
        let Some(active) = stall::for_attempt(w, self.node_id.as_str(), attempt)? else {
            return Ok(());
        };
        let mut summary: Progress = stall::read(w, &active.turn)?.unwrap_or_default();
        let evidence = if tool.phase == ToolPhase::Failed {
            if active.waiting {
                return Ok(());
            }
            summary.failed(signature, received, &ev.id)
        } else {
            summary.advance(signature);
            None
        };
        self.save_progress(w, &active, summary, evidence)
    }

    pub(super) fn observe_turns(&self) -> Result<(), StoreError> {
        let observable =
            self.is_reconciled() && !self.is_holding() && self.node.observes_execution();
        let at = self.clock.now();
        self.write(|w| {
            for active in stall::running(w, self.node_id.as_str())? {
                let mut summary: Progress = stall::read(w, &active.turn)?.unwrap_or_default();
                if summary.evidence.is_some() {
                    continue;
                }
                let evidence = summary.observe(at, observable && !active.waiting);
                self.save_progress(w, &active, summary, evidence)?;
            }
            Ok(())
        })
    }

    fn save_progress(
        &self,
        w: &mut W<'_>,
        a: &Active,
        mut summary: Progress,
        evidence: Option<StallEvidence>,
    ) -> Result<(), StoreError> {
        let reason = evidence.as_ref().map(|e| e.reason);
        if evidence.is_some() {
            summary.evidence = evidence;
        }
        stall::save(w, &a.turn, &summary)?;
        let Some(reason) = reason else {
            return Ok(());
        };
        let scope = node_sessions::startup::scope(w, &a.session)?;
        let question = match reason {
            StallReason::RepeatedFailure => steve_i18n::t!("hub.node.stall.repeated_failure"),
            StallReason::NoProgress => steve_i18n::t!("hub.node.stall.no_progress"),
        };
        let card = steve_store::DecisionRows(w).open(&steve_store::NewDecision {
            workspace_id: scope.workspace,
            kind: "fail".into(),
            subject_type: "stall".into(),
            subject_id: a.turn.clone(),
            item_id: scope.item,
            session_id: Some(a.session.clone()),
            question,
            options: Some(vec!["continue".into(), "look".into(), "drop".into()]),
            requested_by: "steve".into(),
            ..Default::default()
        })?;
        stall::bind_decision(w, &a.turn, &card.id)?;
        node_write::cancel_attempt_in(w, &a.attempt, a.revision, self.clock.now())?;
        self.stop_command_in(w, &a.attempt, &a.operation, "cancel")?;
        self.staged.lock().unwrap().push(Tell::Stalled(reason));
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
    fn only_observed_time_counts_and_new_progress_resets_the_deadline() {
        let mut summary = Progress::default();
        for n in 0..1_200 {
            assert!(summary.observe(n * SAMPLE_MS, false).is_none());
        }
        let start = 1_200 * SAMPLE_MS;
        assert!(summary.observe(start, true).is_none());
        for n in 1..900 {
            assert!(summary.observe(start + n * SAMPLE_MS, true).is_none());
        }
        summary.advance(Digest::sha256(b"new output"));
        for n in 900..1_799 {
            assert!(summary.observe(start + n * SAMPLE_MS, true).is_none());
        }
        let cause = summary.observe(start + 1_799 * SAMPLE_MS, true).unwrap();
        assert_eq!(cause.reason, StallReason::NoProgress);
    }
}
