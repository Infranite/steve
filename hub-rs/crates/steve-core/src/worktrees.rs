//! An item's worktree made and given back by its session's backend (N0e-E11a): with the local Node
//! on, the core no longer runs `git worktree add` or `git worktree remove` under its lock for a
//! session bound there.
//!
//! - Plan, under the lock: a new session of an item on a git worktree is created with the
//!   directory its worktree goes to (`<worktrees>/<item id>`, on `steve/<id>`), made or not. The
//!   backend binds it with that plan ([`AgentRuntime::created`](crate::AgentRuntime::created)); one
//!   that does not make it after all leaves it to the core, which makes it as before.
//! - Off the lock, the backend makes it (refusing below the disk floor) and opens the session.
//! - Apply, under the lock again: [`Core::workspace_ready`] records where it is, only while the
//!   session is still the item's current one (a late answer for an older session changes nothing);
//!   [`Core::workspace_failed`] raises the card a start that could not prepare its worktree raises.
//! - A finished item's worktree it made is given back by it ([`Tx::tidy_by_backend`]): the core
//!   decides from the branch's facts (unmerged work stays; a merged branch goes too), the backend
//!   removes it once its harness is gone, and [`Core::workspace_tidied`] writes what was done.

use std::path::Path;

use crate::error::Result;
use crate::repo::{Item, ItemExt};
use crate::{Core, Tx, git, patch};

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

    /// The item's worktree was given back by its backend (`removed`), or kept.
    pub fn workspace_tidied(&self, item: &str, removed: bool) -> Result<()> {
        self.tx().workspace_tidied(item, removed)
    }
}

/// Two spellings of the same directory (a symlinked temp dir, say).
fn same_dir(a: &str, b: &str) -> bool {
    a == b
        || matches!(
            (Path::new(a).canonicalize(), Path::new(b).canonicalize()),
            (Ok(x), Ok(y)) if x == y
        )
}

impl Tx<'_> {
    /// Where the item's worktree goes when its session's backend may make it: `(dir, branch)`.
    /// `None`: the core makes the item's directory itself ([`Self::prepare_cwd`] says why not, if
    /// it cannot).
    pub(crate) fn planned_worktree(&self, item: &Item) -> Result<Option<(String, String)>> {
        if !self.rt().makes_worktrees() {
            return Ok(None);
        }
        let Some(p) = self.db().project(item.project_id.as_deref())? else {
            return Ok(None);
        };
        let path = Path::new(&p.path);
        if p.device_id != "hub"
            || p.kind != "git"
            || !p.worktree
            || !path.exists()
            || !git::is_git(path)
        {
            return Ok(None);
        }
        Ok(Some((
            self.cfg().worktrees.join(&item.id).to_string_lossy().into(),
            format!("steve/{}", item.id.to_lowercase()),
        )))
    }

    /// The item's current session works in a worktree its backend makes again when it opens:
    /// a removed one is not made here.
    pub(crate) fn backend_remakes(&self, item_id: &str) -> Result<bool> {
        if !self.rt().makes_worktrees() {
            return Ok(false);
        }
        Ok(self
            .active_session(item_id)?
            .is_some_and(|s| self.rt().makes_workspace(&s.id)))
    }

    /// A finished item's worktree that its backend made is given back by it. `true`: decided
    /// (unmerged work stays, or the backend has it on its way); `false`: the core tidies as
    /// before (no such backend, or nothing of it there).
    pub(crate) fn tidy_by_backend(
        &self,
        i: &Item,
        p: &crate::repo::Project,
        branch: &str,
        cwd: &str,
    ) -> Result<bool> {
        if !self.rt().makes_worktrees() || !Path::new(cwd).exists() {
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
        let f = git::branch_facts(Path::new(&p.path), branch, "main");
        /* an archived item gives back a clean worktree even when its branch is not in main */
        if i.status == "done" && !i.archived && !f.landed {
            return Ok(true);
        }
        self.rt().tidy(&s.id, !f.merged)
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
        if !i.cwd.as_deref().is_some_and(|c| same_dir(c, path)) {
            self.db().item_patch(&i.id, patch![cwd: path])?;
        }
        if let Some(b) = branch
            && i.branch.as_deref() != Some(b)
        {
            self.db().item_patch(&i.id, patch![branch: b])?;
        }
        if !same_dir(&s.cwd, path) {
            self.db().session_patch(&s.id, patch![cwd: path])?;
        }
        Ok(())
    }

    fn workspace_failed(&mut self, session: &str, disk_low: bool, detail: &str) -> Result<()> {
        let Some((_, i)) = self.current(session)? else {
            return Ok(());
        };
        if i.is_settled() {
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
        let (Some(p), Some(branch), Some(cwd)) = (
            self.db().project(i.project_id.as_deref())?,
            i.branch.as_deref().filter(|b| !b.is_empty()),
            i.cwd.as_deref().filter(|c| !c.is_empty()),
        ) else {
            return Ok(());
        };
        if !removed {
            return Ok(());
        }
        let head = git::git(
            Path::new(&p.path),
            &["rev-parse", "--short", &format!("refs/heads/{branch}")],
        );
        let said = match head {
            Ok(h) => {
                format!("删除了工作区 {cwd}（分支 {branch} 保留在 {h}，继续时从分支重建）")
            }
            Err(_) => format!("删除了工作区 {cwd}；删除了已合并的分支 {branch}"),
        };
        self.db()
            .timeline_add(item_id, "steve", "sys", &said, None)?;
        Ok(())
    }
}
