//! Deterministic progress observation for the active logical turn.

use super::*;
use serde::{Deserialize, Serialize};
use std::time::Duration;
use steve_store::node_write::stall::{self, Active};
use steve_types::orchestration::{StallEvidence, StallReason, TURN_STALL};

const REPEAT_LIMIT: u8 = 5;
const NO_PROGRESS_MS: i64 = 15 * 60_000;
pub(super) const POLL_INTERVAL: Duration = Duration::from_secs(1);
const SAMPLES: usize = 32;

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

#[derive(Default, Serialize, Deserialize)]
struct Progress {
    observation: Option<Observation>,
    first_observed: Option<i64>,
    observed_ms: i64,
    progress: Vec<Digest>,
    failures: Vec<Failure>,
    evidence: Option<StallEvidence>,
}

impl Progress {
    fn observe(&mut self, at: i64, active: Option<(&str, &str, u64)>) -> Option<StallEvidence> {
        if self.evidence.is_some() {
            return None;
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
        (self.observed_ms >= NO_PROGRESS_MS).then(|| StallEvidence {
            reason: StallReason::NoProgress,
            first_at: self.first_observed.unwrap_or(at),
            detected_at: at,
            repetitions: 0,
            event: None,
        })
    }

    fn advance(&mut self, digest: Digest, at: i64) {
        if let Some(index) = self.progress.iter().position(|seen| *seen == digest) {
            self.progress.remove(index);
            self.progress.push(digest);
            return;
        }
        if self.progress.len() == SAMPLES {
            self.progress.remove(0);
        }
        self.progress.push(digest);
        self.observed_ms = 0;
        self.first_observed = None;
        if let Some(observation) = &mut self.observation {
            observation.at = observation.at.max(at);
        }
        self.failures.clear();
    }

    fn failed(&mut self, digest: Digest, at: i64, event: &EventId) -> Option<StallEvidence> {
        if self.evidence.is_some() {
            return None;
        }
        let mut failure = match self.failures.iter().position(|f| f.digest == digest) {
            Some(index) => self.failures.remove(index),
            None => {
                if self.failures.len() == SAMPLES {
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
        });
        self.failures.push(failure);
        evidence
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
        let Some(evidence) = &tool.evidence else {
            return Ok(());
        };
        let Some(active) = stall::for_attempt(w, self.node_id.as_str(), attempt)? else {
            return Ok(());
        };
        let mut summary: Progress = stall::read(w, &active.turn)?.unwrap_or_default();
        let at = self.clock.now();
        let evidence = match (tool.phase, evidence) {
            (ToolPhase::Failed, ToolEvidence::Failed { signature }) if !active.waiting => {
                summary.failed(signature.clone(), at, &ev.id)
            }
            (ToolPhase::Succeeded, ToolEvidence::ContentChanged { signature }) => {
                summary.advance(signature.clone(), at);
                None
            }
            _ => return Ok(()),
        };
        self.save_progress(w, &active, summary, evidence)
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
        let signature = digest_of(&(workspace, revision)).map_err(conflict)?;
        summary.advance(signature, self.clock.now());
        self.save_progress(w, &active, summary, None)
    }

    fn observe_active_in(
        &self,
        w: &mut W<'_>,
        active: &Active,
        generation: Option<u64>,
        at: i64,
    ) -> Result<(), StoreError> {
        let generation = generation.filter(|_| !active.waiting);
        let mut summary: Progress = stall::read(w, &active.turn)?.unwrap_or_default();
        if summary.evidence.is_some() || (generation.is_none() && summary.observation.is_none()) {
            return Ok(());
        }
        let evidence = summary.observe(
            at,
            generation.map(|g| (self.owner.as_str(), active.attempt.as_str(), g)),
        );
        self.save_progress(w, active, summary, evidence)
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
        let question = steve_i18n::stored(|| match evidence.reason {
            StallReason::RepeatedFailure => steve_i18n::t!("hub.node.stall.repeated_failure"),
            StallReason::NoProgress => steve_i18n::t!("hub.node.stall.no_progress"),
        });
        let card = steve_store::DecisionRows(w).open(&steve_store::NewDecision {
            workspace_id: scope.workspace,
            kind: "fail".into(),
            subject_type: TURN_STALL.into(),
            subject_id: a.turn.clone(),
            item_id: scope.item,
            session_id: Some(a.session.clone()),
            question,
            options: Some(vec!["continue".into(), "drop".into()]),
            requested_by: "steve".into(),
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
        assert!(summary.observe(0, started).is_none());
        assert!(summary.observe(800_000, started).is_none());
        assert!(summary.observe(2_000_000, restarted).is_none());
        assert!(summary.observe(2_020_000, None).is_none());
        assert!(summary.observe(3_000_000, restarted).is_none());
        assert!(summary.observe(3_099_999, restarted).is_none());
        assert_eq!(
            summary.observe(3_100_000, restarted).unwrap().reason,
            StallReason::NoProgress
        );

        let mut summary = Progress::default();
        for i in 0_i64..40 {
            let at = i * 800_000;
            assert!(summary.observe(at, started).is_none());
            summary.advance(Digest::sha256(&i.to_le_bytes()), at);
        }
        assert!(
            summary
                .observe(39 * 800_000 + NO_PROGRESS_MS - 1, started)
                .is_none()
        );
        assert!(
            summary
                .observe(39 * 800_000 + NO_PROGRESS_MS, started)
                .is_some()
        );
    }
}
