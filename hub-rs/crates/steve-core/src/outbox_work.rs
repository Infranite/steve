//! Work on the durable outbox: `work.item.lifecycle` is emitted in every transaction
//! that closes an item (completed, cancelled, or failed in [`crate::cycle`]), and
//! Work's own follow-up runs as a consumer of it after the commit: [`WorkTidy`] stops and releases
//! the item's sessions, runs owner-finish follow-up once when applicable, and gives back the item's
//! worktree and merged branch (while preserving and marking unmerged branches).
//!
//! Limit kept on purpose: the tidy still runs under the core's state lock (`core.tx()`), because it
//! checks that no session of the item is busy, releases them and removes the worktree as one step
//! against a new message bringing the item back (`ensure_cwd`). It no longer runs inside a store
//! transaction or inside `finish`/`drop`/`fail`.

use std::collections::HashSet;
use std::sync::{Arc, Mutex};

use steve_store::outbox::{ConsumeError, Consumer, Event, Since};
use steve_work_api::{ITEM_LIFECYCLE, ITEM_LIFECYCLE_V, ItemLifecycle};

use crate::{Core, Released};

/// Reason attached to synthetic `work.item.lifecycle` (`closed`) events queued by the one-time
/// closed-worktree backfill so `WorkTidy` cleans up lingering worktrees without re-running
/// owner-finish follow-up hooks or timeline notes.
pub(crate) const BACKFILL_REASON: &str = "closed_worktree_backfill";

/// The payload of a `work.item.lifecycle` event; malformed or of an unknown version: rejected.
pub fn item_lifecycle_of(ev: &Event) -> Result<ItemLifecycle, ConsumeError> {
    if ev.version != ITEM_LIFECYCLE_V {
        return Err(ConsumeError::Reject(format!(
            "{ITEM_LIFECYCLE} version {} is not {ITEM_LIFECYCLE_V}",
            ev.version
        )));
    }
    serde_json::from_value(ev.payload.clone())
        .map_err(|e| ConsumeError::Reject(format!("{ITEM_LIFECYCLE}: {e}")))
}

/// `work.tidy`: after any item closes (completed, cancelled, or failed), stop/release its sessions,
/// remove its clean worktree, delete a merged branch, and preserve + mark an unmerged branch.
///
/// Idempotent: running again on an already-cleaned worktree changes nothing. A busy or releasing
/// session (or an external process running in the worktree) retries via the outbox; a dirty
/// worktree or git error is recorded on the item, logged, and metered. An item reopened since is
/// not tidied: it needs its worktree.
pub struct WorkTidy {
    core: Core,
    /// `(item_id, cycle)` pairs whose owner-finish `after_cycle_closed` already ran in this process,
    /// preventing duplicate timeline/hook side effects if a later step retries.
    owner_follow_up_done: Mutex<HashSet<(String, i64)>>,
}

impl WorkTidy {
    pub const ID: &'static str = "work.tidy";

    pub fn new(core: Core) -> Arc<Self> {
        Arc::new(Self {
            core,
            owner_follow_up_done: Mutex::new(HashSet::new()),
        })
    }
}

impl Consumer for WorkTidy {
    fn id(&self) -> &'static str {
        Self::ID
    }

    fn topic(&self) -> &'static str {
        ITEM_LIFECYCLE
    }

    fn since(&self) -> Since {
        Since::Start
    }

    fn effect(&self, ev: &Event) -> Result<(), ConsumeError> {
        let d = item_lifecycle_of(ev)?;
        if d.transition != "closed" {
            return Ok(());
        }
        let item = self.core.store().item(&d.item_id).map_err(retry)?;
        let Some(item) = item else {
            // deleted since: nothing left to give back
            return Ok(());
        };
        if item.workspace_id != ev.workspace_id {
            return Err(ConsumeError::Reject(format!(
                "item {} is not in workspace {}",
                d.item_id, ev.workspace_id
            )));
        }
        if item.cycle != d.cycle || item.phase != crate::cycle::CLOSED {
            return Ok(());
        }

        // Step 1: stop any busy session and release every idle session BEFORE Node housekeeping
        // inspects process cwds — an idle ACP harness holds `cwd` inside the worktree until released.
        {
            let mut tx = self.core.tx();
            let item = tx.item(&d.item_id).map_err(retry)?;
            if item.cycle != d.cycle || item.phase != crate::cycle::CLOSED {
                return Ok(());
            }
            let mut waiting = false;
            for session in tx.db().sessions_of_item(&d.item_id).map_err(retry)? {
                if tx.busy(&session.id) {
                    tx.stop_now(&session.id);
                    waiting = true;
                } else if tx.rt().release(&session.id).map_err(retry)? != Released::Gone {
                    waiting = true;
                }
            }
            if waiting {
                tracing::debug!(
                    item = %d.item_id,
                    cycle = d.cycle,
                    "worktree cleanup waiting for sessions to stop and release"
                );
                return Err(ConsumeError::Retry(
                    "item sessions are still stopping or releasing".into(),
                ));
            }
            if d.acceptance.as_deref() == Some(crate::cycle::ClosePolicy::Owner.as_str())
                && d.reason != BACKFILL_REASON
            {
                let key = (d.item_id.clone(), d.cycle);
                let already = self
                    .owner_follow_up_done
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .contains(&key);
                if !already {
                    let outcome = d.outcome.as_deref().unwrap_or("completed");
                    tx.after_cycle_closed(&item, outcome, &d.acceptance, &d.reason)
                        .map_err(retry)?;
                    self.owner_follow_up_done
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .insert(key);
                }
            }
        }

        // Step 2: perform deterministic worktree and branch cleanup on the Node.
        match self.core.tx().tidy_branch_outcome(&d.item_id) {
            Ok(
                crate::worktrees::TidyOutcome::Cleaned
                | crate::worktrees::TidyOutcome::Dispatched
                | crate::worktrees::TidyOutcome::Noop
                | crate::worktrees::TidyOutcome::Failed,
            ) => {
                self.owner_follow_up_done
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .remove(&(d.item_id.clone(), d.cycle));
                Ok(())
            }
            Ok(crate::worktrees::TidyOutcome::Busy) => Err(ConsumeError::Retry(format!(
                "item {} still has active sessions; retrying worktree cleanup",
                d.item_id
            ))),
            Ok(crate::worktrees::TidyOutcome::DeferredInUse) => Err(ConsumeError::Retry(format!(
                "worktree for item {} still has a running process",
                d.item_id
            ))),
            Err(e) => {
                tracing::warn!(
                    item = %d.item_id,
                    error = %e,
                    "worktree cleanup failed; scheduling retry"
                );
                Err(ConsumeError::Retry(e.to_string()))
            }
        }
    }
}

fn retry(error: impl std::fmt::Display) -> ConsumeError {
    ConsumeError::Retry(error.to_string())
}

impl Core {
    /// Work's outbox consumers, for the dispatcher the integrator runs.
    pub fn outbox_consumers(&self) -> Vec<Arc<dyn Consumer>> {
        vec![WorkTidy::new(self.clone())]
    }
}
