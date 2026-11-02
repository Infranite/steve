//! Hourly chores (housekeeping.ts): archive finished items after the workspace's archive period; give back
//! worktrees of finished items.

use crate::Core;
use crate::error::Result;
use crate::patch;
use crate::repo::ItemExt;
use crate::util::now;
use std::path::Path;
use std::time::Duration;

impl Core {
    /// One pass over every workspace.
    pub fn housekeeping(&self) -> Result<()> {
        let mut t = self.tx();
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
                if i.cwd
                    .as_deref()
                    .is_some_and(|c| !c.is_empty() && Path::new(c).exists())
                    && let Err(e) = t.tidy_branch(&i.id)
                {
                    tracing::error!("tidy {} {e}", i.id);
                }
            }
            /* one notification per run, not one per item */
            if !archived.is_empty() {
                let more = if archived.len() > 3 { " 等" } else { "" };
                let text = format!(
                    "自动归档了 {} 件完成满 {days} 天的事项：{}{more}",
                    archived.len(),
                    archived[..archived.len().min(3)].join("、")
                );
                let one = (archived.len() == 1).then(|| archived[0].as_str());
                crate::notify::notify(t.db(), &ws, "archive", "info", &text, one)?;
            }
        }
        Ok(())
    }

    /// At boot, then every hour. Runs forever.
    pub async fn run_housekeeping(self) {
        let mut tick = tokio::time::interval(Duration::from_secs(3600));
        loop {
            tick.tick().await;
            if let Err(e) = self.housekeeping() {
                tracing::error!("housekeeping {e}");
            }
        }
    }
}
