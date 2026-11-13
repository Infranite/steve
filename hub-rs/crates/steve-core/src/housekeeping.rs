//! Hourly chores (housekeeping.ts): archive finished items after the workspace's archive period, and
//! compact the session logs of archived items once they have been archived for the workspace's
//! `keepSess` (the default, 永久, keeps them whole). Giving back finished items' worktrees moved to the
//! workspace watcher (`workspace`), which also deletes their caches outside the lock and leaves worktrees
//! alone while a process runs in them.

use crate::error::Result;
use crate::patch;
use crate::repo::ItemExt;
use crate::util::now;
use crate::{Core, Tx};
use std::time::Duration;

impl Core {
    /// One pass over every workspace.
    pub fn housekeeping(&self) -> Result<()> {
        let compact = self.archive_pass()?;
        /* compacting rewrites whole logs: one session at a time, reading outside the core's lock; each
        page is written under it, after checking the session is still due (the item may have been taken
        back up and the session resumed since the pass), and only up to the seq it had when found due */
        for (sid, upto) in compact {
            self.compact_session(&sid, upto);
        }
        Ok(())
    }

    /// Compacts session `sid` up to seq `upto` while it stays due ([`compaction_due`]).
    pub(crate) fn compact_session(&self, sid: &str, upto: i64) {
        let still_due = || {
            let t = self.tx();
            match compaction_due(&t, sid) {
                Ok(true) => Some(t),
                Ok(false) => None,
                Err(e) => {
                    tracing::warn!(session = %sid, "checking a compaction {e}");
                    None
                }
            }
        };
        match self.store().store().events().compact(sid, upto, still_due) {
            Ok(Some(n)) if n > 0 => {
                tracing::info!(session = %sid, events = n, "compacted an archived session")
            }
            Ok(Some(_)) => {}
            Ok(None) => tracing::info!(session = %sid, "compaction stopped: no longer due"),
            Err(e) => tracing::error!(session = %sid, "compact session {e}"),
        }
    }

    /// Archives what is due and returns the sessions due for compaction with their last seq (under the
    /// lock).
    pub(crate) fn archive_pass(&self) -> Result<Vec<(String, i64)>> {
        let t = self.tx();
        let mut compact: Vec<(String, i64)> = vec![];
        for ws in t.db().workspace_ids()? {
            let days: i64 = t.db().setting(&ws, "archDays", "30")?.parse().unwrap_or(0);
            let before = now() - days * 24 * 3_600_000;
            let mut archived: Vec<String> = vec![];
            for i in t.db().items_all(&ws)? {
                if !i.is_finished() {
                    continue;
                }
                if days != 0 && !i.archived && i.updated < before {
                    t.db().item_patch(&i.id, patch![archived: true])?;
                    t.db().timeline_add(
                        &i.id,
                        "steve",
                        "sys",
                        &format!("完成满 {days} 天，自动归档"),
                        None,
                    )?;
                    archived.push(i.id.clone());
                }
            }
            /* one notification per run, not one per item */
            if !archived.is_empty() {
                let text = crate::notice_line::archived(&archived, &days.to_string());
                let one = (archived.len() == 1).then(|| archived[0].as_str());
                crate::notify::notify(t.db(), &ws, "archive", "info", &text, one)?;
            }
            if keep_sessions_ms(&t.db().setting(&ws, "keepSess", "永久")?).is_none() {
                continue;
            }
            for i in t.db().items_all(&ws)? {
                if !item_due(&t, &i)? {
                    continue;
                }
                for s in t.db().sessions_of_item(&i.id)? {
                    if t.busy(&s.id) {
                        continue;
                    }
                    let events = t.db().store().events();
                    let last = events.last_seq(&s.id)?;
                    if last > events.compacted_upto(&s.id)? {
                        compact.push((s.id, last));
                    }
                }
            }
        }
        Ok(compact)
    }

    /// At boot, then every hour. Runs forever.
    pub async fn run_housekeeping(self) {
        let mut tick = tokio::time::interval(Duration::from_secs(3600));
        loop {
            tick.tick().await;
            /* it waits on the core's lock and writes the store: off the runtime's workers */
            let c = self.clone();
            match tokio::task::spawn_blocking(move || c.housekeeping()).await {
                Ok(Err(e)) => tracing::error!("housekeeping {e}"),
                Err(e) => tracing::error!("housekeeping task {e}"),
                Ok(Ok(())) => {}
            }
        }
    }
}

/// Whether item `i`'s sessions are due for compaction: archived and finished for its workspace's
/// keepSess (taking an item back up unarchives it and moves `updated`).
fn item_due(t: &Tx, i: &steve_store::Item) -> Result<bool> {
    let Some(keep) = keep_sessions_ms(&t.db().setting(&i.workspace_id, "keepSess", "永久")?)
    else {
        return Ok(false);
    };
    Ok(i.archived && i.is_finished() && i.updated < now() - keep)
}

/// Whether session `sid` is still due for compaction: not running, and its item still due.
fn compaction_due(t: &Tx, sid: &str) -> Result<bool> {
    if t.busy(sid) {
        return Ok(false);
    }
    let Some(item) = t.db().session(sid)?.and_then(|s| s.item_id) else {
        return Ok(false);
    };
    match t.db().item(&item)? {
        Some(i) => item_due(t, &i),
        None => Ok(false),
    }
}

/// How long an archived item's sessions keep their whole event log (`keepSess`: 90 天, 1 年); `None`
/// (永久, the default, or anything else) keeps it for good.
pub(crate) fn keep_sessions_ms(v: &str) -> Option<i64> {
    const DAY: i64 = 24 * 3_600_000;
    match v.trim() {
        "90 天" => Some(90 * DAY),
        "1 年" => Some(365 * DAY),
        _ => None,
    }
}
