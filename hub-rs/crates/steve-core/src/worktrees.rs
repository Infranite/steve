//! An item's worktree planned and tidied via its session's Node backend.
//!
//! Core never invokes `git worktree` directly: it names the worktree path and branch under the
//! lock, lets the backend/Node prepare or remove it off the lock, and applies the result when it
//! comes back.

use crate::error::Result;
use crate::repo::Item;
use crate::{Core, Tx, patch};

impl Core {
    /// The worktree `session` waited for is at `path` (on `branch`).
    pub fn workspace_ready(&self, session: &str, path: &str, branch: Option<&str>) -> Result<()> {
        self.tx().workspace_ready(session, path, branch)
    }

    /// The worktree `session` waited for could not be made: `disk_low` (below the floor), or
    /// why (`detail`).
    pub fn workspace_failed(&self, session: &str, disk_low: bool, detail: &str) -> Result<()> {
        steve_i18n::stored(|| self.tx().workspace_failed(session, disk_low, detail))
    }

    /// Retire an invalid immutable binding before raising the recovery card. Continue will
    /// create a new session; the previous directory and transcript remain available.
    pub fn workspace_binding_failed(&self, session: &str, detail: &str) -> Result<()> {
        steve_i18n::stored(|| {
            let mut tx = self.tx();
            let Some((s, i)) = tx.current(session)? else {
                return Ok(());
            };
            if i.is_closed() || i.is_paused() {
                return Ok(());
            }
            tx.end_session(&s.id)?;
            tx.fail(&i.id, &format!("开不了会话：{detail}"))
        })
    }

    /// The item's worktree was given back by its backend (`removed`), or kept.
    pub fn workspace_tidied(&self, item: &str, removed: bool) -> Result<()> {
        self.tx().workspace_tidied(item, removed)
    }
}

/// Deterministic item worktree branch name (never queries local git on the Hub).
pub(crate) fn worktree_branch(item: &Item) -> String {
    item.branch
        .clone()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| format!("steve/{}", item.id.to_lowercase()))
}

impl Tx<'_> {
    /// Where the item's worktree goes on its Node: `(dir, branch)`.
    pub(crate) fn planned_worktree(&self, item: &Item) -> Result<Option<(String, String)>> {
        let Some(p) = self.db().project(item.project_id.as_deref())? else {
            return Ok(None);
        };
        if p.device_id != "hub" || p.kind != "git" || !p.worktree {
            return Ok(None);
        }
        Ok(Some((
            self.cfg().worktrees.join(&item.id).to_string_lossy().into(),
            worktree_branch(item),
        )))
    }

    /// A finished item's worktree that its backend made is given back by it. `true`: handled here
    /// (archived/cancelled tidy sent to the session backend); `false`: defer to Node housekeeping
    /// (`disk_reclaim`), which verifies landed/merged status and sheds caches safely on the Node.
    pub(crate) fn tidy_by_backend(&self, i: &Item) -> Result<bool> {
        if !self.rt().makes_worktrees() {
            return Ok(false);
        }
        let Some(s) = self
            .db()
            .sessions_of_item(&i.id)?
            .into_iter()
            .rev()
            .find(|s| self.rt().makes_workspace(&s.id))
        else {
            return Ok(false);
        };
        if i.is_completed() && !i.archived {
            return Ok(false);
        }
        self.rt().tidy(&s.id, true)
    }

    fn current(&self, session: &str) -> Result<Option<(crate::repo::Session, Item)>> {
        let Some(s) = self.db().session(session)? else {
            return Ok(None);
        };
        let Some(i) = s
            .item_id
            .as_deref()
            .map(|i| self.db().item(i))
            .transpose()?
            .flatten()
        else {
            return Ok(None);
        };
        let latest = self.db().sessions_of_item(&i.id)?.pop();
        if s.status == "ended" || latest.is_none_or(|l| l.id != s.id) {
            return Ok(None);
        }
        Ok(Some((s, i)))
    }

    fn workspace_ready(&self, session: &str, path: &str, branch: Option<&str>) -> Result<()> {
        let Some((s, i)) = self.current(session)? else {
            return Ok(());
        };
        if i.cwd.as_deref() != Some(path) {
            self.db().item_patch(&i.id, patch![cwd: path])?;
        }
        if let Some(b) = branch
            && i.branch.as_deref() != Some(b)
        {
            self.db().item_patch(&i.id, patch![branch: b])?;
        }
        if s.cwd != path {
            self.db().session_patch(&s.id, patch![cwd: path])?;
        }
        Ok(())
    }

    fn workspace_failed(&mut self, session: &str, disk_low: bool, detail: &str) -> Result<()> {
        let Some((_, i)) = self.current(session)? else {
            return Ok(());
        };
        if i.is_closed() || i.is_paused() {
            return Ok(());
        }
        let why = if disk_low {
            self.c.disk_low(self.c.worktrees_free().unwrap_or(0))
        } else {
            detail.to_string()
        };
        self.fail(&i.id, &format!("开不了会话：{why}"))
    }

    fn workspace_tidied(&self, item_id: &str, removed: bool) -> Result<()> {
        let Some(i) = self.db().item(item_id)? else {
            return Ok(());
        };
        let (Some(branch), Some(cwd)) = (
            i.branch.as_deref().filter(|b| !b.is_empty()),
            i.cwd.as_deref().filter(|c| !c.is_empty()),
        ) else {
            return Ok(());
        };
        if !removed {
            return Ok(());
        }
        let said = format!("删除了工作区 {cwd}（分支 {branch} 保留，继续时从分支重建）");
        self.db()
            .timeline_add(item_id, "steve", "sys", &said, None)?;
        Ok(())
    }
}
