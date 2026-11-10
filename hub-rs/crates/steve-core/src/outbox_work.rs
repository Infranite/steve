//! Work on the durable outbox: `work.item.done` is emitted in the transaction
//! that finishes an item (the closing commit in [`crate::cycle`]), and
//! Work's own follow-up runs as a consumer of it after the commit: [`WorkTidy`] gives back the item's
//! worktree and merged branch (git), which used to run inside `finish`.
//!
//! Limit kept on purpose: the tidy still runs under the core's state lock (`core.tx()`), because it
//! checks that no session of the item is busy, releases them and removes the worktree as one step
//! against a new message bringing the item back (`ensure_cwd`). It no longer runs inside a store
//! transaction or inside `finish`.

use std::sync::Arc;

use steve_store::outbox::{ConsumeError, Consumer, Event, Since};
use steve_work_api::{ITEM_DONE, ITEM_DONE_V, ItemDone};

use crate::Core;

/// The payload of a `work.item.done` event; malformed or of an unknown version: rejected.
pub fn item_done_of(ev: &Event) -> Result<ItemDone, ConsumeError> {
    if ev.version != ITEM_DONE_V {
        return Err(ConsumeError::Reject(format!(
            "{ITEM_DONE} version {} is not {ITEM_DONE_V}",
            ev.version
        )));
    }
    serde_json::from_value(ev.payload.clone())
        .map_err(|e| ConsumeError::Reject(format!("{ITEM_DONE}: {e}")))
}

/// `work.tidy`: after an item is finished, its clean worktree and merged branch are given back.
///
/// Idempotent: the tidy itself only removes a clean worktree of a finished item and deletes a merged
/// branch, so running it again changes nothing. A busy session makes it try again later (it used to
/// skip for good); a worktree kept on purpose (dirty, unmerged) is acknowledged, and the hourly
/// housekeeping looks at it again. An item reopened since is not tidied: it needs its worktree.
pub struct WorkTidy {
    core: Core,
}

impl WorkTidy {
    pub const ID: &'static str = "work.tidy";

    pub fn new(core: Core) -> Arc<Self> {
        Arc::new(Self { core })
    }
}

impl Consumer for WorkTidy {
    fn id(&self) -> &'static str {
        Self::ID
    }

    fn topic(&self) -> &'static str {
        ITEM_DONE
    }

    fn since(&self) -> Since {
        Since::Start
    }

    fn effect(&self, ev: &Event) -> Result<(), ConsumeError> {
        let d = item_done_of(ev)?;
        let item = self
            .core
            .store()
            .item(&d.item_id)
            .map_err(|e| ConsumeError::Retry(e.to_string()))?;
        let Some(item) = item else {
            // deleted since: nothing left to give back
            return Ok(());
        };
        if item.workspace_id != ev.workspace_id {
            return Err(ConsumeError::Reject(format!(
                "{} 不在工作空间 {}",
                d.item_id, ev.workspace_id
            )));
        }
        /* a process still running in the worktree (a dev server) keeps it; the workspace watcher looks
         * again. Listed before the lock: it takes about a second */
        if let Some(cwd) = item.cwd.as_deref().filter(|c| !c.is_empty())
            && crate::workspace::process_dirs()
                .is_some_and(|p| crate::workspace::in_use(std::path::Path::new(cwd), &p))
        {
            return Ok(());
        }
        match self.core.tx().tidy_branch_now(&d.item_id) {
            Ok(true) => Ok(()),
            Ok(false) => Err(ConsumeError::Retry(format!(
                "{} 还有会话在运行，稍后再清理工作区",
                d.item_id
            ))),
            Err(e) => Err(ConsumeError::Retry(e.to_string())),
        }
    }
}

impl Core {
    /// Work's outbox consumers, for the dispatcher the integrator runs.
    pub fn outbox_consumers(&self) -> Vec<Arc<dyn Consumer>> {
        vec![WorkTidy::new(self.clone())]
    }
}
