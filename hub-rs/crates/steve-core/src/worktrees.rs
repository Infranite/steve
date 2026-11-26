//! An item's worktree planned and tidied via its Node backend and Node housekeeping.
//!
//! Core never invokes `git worktree` or local filesystem probes in production code: it names the
//! worktree path and branch under the lock, delegates preparation, inspection, and reclamation to
//! the Node (`AgentRuntime` / `NodeHousekeeping`), and records the outcome deterministically.

use std::collections::HashMap;
use std::path::{Component, Path, PathBuf};

use serde::Serialize;
use steve_node_api::DiskInspectTarget;
use steve_work_api::{ITEM_LIFECYCLE, ITEM_LIFECYCLE_V, ItemLifecycle};

use crate::error::Result;
use crate::outbox_work::BACKFILL_REASON;
use crate::repo::Item;
use crate::{Core, Released, Tx, patch};

/// Durable idempotent marker key in `settings` (`workspace_id = '*'`) for the one-time backfill
/// that cleans worktrees left behind by items closed before deterministic post-close cleanup.
pub const CLOSED_WORKTREE_BACKFILL_KEY: &str = "maintenance.closed_worktree_backfill.v1";

/// Result of one [`Core::backfill_closed_worktrees`] invocation.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct WorktreeBackfillReport {
    /// Already completed on an earlier boot; no scan ran.
    pub skipped: bool,
    pub scanned: usize,
    pub cleaned: usize,
    pub enqueued: usize,
    pub failed: usize,
}

/// Typed outcome of [`Tx::tidy_branch_outcome`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TidyOutcome {
    /// A session of the item is still busy or releasing; caller must retry later.
    Busy,
    /// An external process still has its cwd inside the worktree; caller must retry later.
    DeferredInUse,
    /// Nothing to clean (open item, non-worktree cwd, or already gone).
    Noop,
    /// Worktree was removed (or already absent with branch handled) on the Node.
    Cleaned,
    /// Cleanup command was durably dispatched to the item's Node backend.
    Dispatched,
    /// Worktree could not be removed (dirty, missing repo, git failure); shown on the item.
    Failed,
}

struct BackfillCandidate {
    id: String,
    workspace_id: String,
    cycle: i64,
    revision: i64,
    outcome: Option<String>,
    acceptance: Option<String>,
    cwd: String,
    in_use: Option<bool>,
}

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

    /// The item's worktree was given back by its backend (`removed`), or failed/kept with `detail`.
    pub fn workspace_tidied(&self, item: &str, removed: bool, detail: Option<&str>) -> Result<()> {
        steve_i18n::stored(|| self.tx().workspace_tidied(item, removed, detail))
    }

    /// One-time idempotent backfill for already-closed items whose worktree still exists on the
    /// colocated Node. Once completed, a durable marker in `settings` prevents rescanning on
    /// subsequent startups.
    pub fn backfill_closed_worktrees(&self) -> Result<WorktreeBackfillReport> {
        let store = self.store().store();
        if store
            .settings()
            .maintenance_done(CLOSED_WORKTREE_BACKFILL_KEY)?
        {
            return Ok(WorktreeBackfillReport {
                skipped: true,
                ..Default::default()
            });
        }
        let potential = {
            let tx = self.tx();
            let mut found = Vec::new();
            for ws in tx.db().workspace_ids()? {
                for item in tx.db().items_all(&ws)? {
                    if !item.is_closed() {
                        continue;
                    }
                    let Some(cwd) = tx.item_worktree_dir(&item)? else {
                        continue;
                    };
                    found.push(BackfillCandidate {
                        id: item.id,
                        workspace_id: item.workspace_id,
                        cycle: item.cycle,
                        revision: item.delivery_revision,
                        outcome: item.outcome,
                        acceptance: item.acceptance,
                        cwd,
                        in_use: None,
                    });
                }
            }
            found
        };

        let candidates: Vec<BackfillCandidate> = if potential.is_empty() {
            Vec::new()
        } else {
            let targets: Vec<DiskInspectTarget> = potential
                .iter()
                .map(|c| DiskInspectTarget {
                    item: c.id.clone(),
                    path: c.cwd.clone(),
                    repo: String::new(),
                    branch: String::new(),
                })
                .collect();
            let inspected = self.inspect_worktrees(targets)?;
            let by_item: HashMap<String, (bool, Option<bool>)> = inspected
                .entries
                .into_iter()
                .map(|m| (m.item, (m.exists, m.in_use)))
                .collect();
            potential
                .into_iter()
                .filter_map(|mut c| {
                    let (exists, in_use) = by_item.get(&c.id).copied().unwrap_or((false, None));
                    if !exists {
                        return None;
                    }
                    c.in_use = in_use;
                    Some(c)
                })
                .collect()
        };

        let mut report = WorktreeBackfillReport {
            skipped: false,
            scanned: candidates.len(),
            cleaned: 0,
            enqueued: 0,
            failed: 0,
        };

        for cand in candidates {
            let mut needs_outbox = false;
            {
                let tx = self.tx();
                let Some(item) = tx.db().item(&cand.id)? else {
                    continue;
                };
                if !item.is_closed() || item.cycle != cand.cycle {
                    continue;
                }
                for session in tx.db().sessions_of_item(&cand.id)? {
                    if tx.busy(&session.id) {
                        tx.stop_now(&session.id);
                        needs_outbox = true;
                    } else if tx.rt().release(&session.id)? != Released::Gone {
                        needs_outbox = true;
                    }
                }
            }
            if !needs_outbox && cand.in_use == Some(true) {
                tracing::warn!(
                    item = %cand.id,
                    cwd = %cand.cwd,
                    "backfill deferring worktree cleanup: external process still running in worktree"
                );
                metrics::counter!(
                    "steve_worktree_cleanup_total",
                    "backend" => "node",
                    "outcome" => "deferred",
                    "branch" => "kept"
                )
                .increment(1);
                let note = steve_i18n::stored(|| {
                    steve_i18n::t!("core.worktree.cleanup_in_use", cwd = cand.cwd.as_str())
                });
                let _ = self.tx().note_cleanup_once(&cand.id, &note);
                needs_outbox = true;
            }
            if !needs_outbox {
                match self.tx().tidy_branch_outcome(&cand.id) {
                    Ok(TidyOutcome::Cleaned | TidyOutcome::Dispatched) => {
                        report.cleaned += 1;
                    }
                    Ok(TidyOutcome::Noop) => {}
                    Ok(TidyOutcome::Failed) => {
                        report.failed += 1;
                    }
                    Ok(TidyOutcome::Busy | TidyOutcome::DeferredInUse) => {
                        needs_outbox = true;
                    }
                    Err(e) => {
                        tracing::warn!(
                            item = %cand.id,
                            cwd = %cand.cwd,
                            error = %e,
                            "backfill worktree cleanup hit retryable error; enqueueing via outbox"
                        );
                        report.failed += 1;
                        needs_outbox = true;
                    }
                }
            }
            if needs_outbox {
                let event = ItemLifecycle {
                    item_id: cand.id.clone(),
                    cycle: cand.cycle,
                    revision: cand.revision,
                    transition: "closed".into(),
                    phase: crate::cycle::CLOSED.as_str().into(),
                    outcome: cand.outcome.clone(),
                    acceptance: cand.acceptance.clone(),
                    reason: BACKFILL_REASON.into(),
                };
                store.write(|w| {
                    steve_store::outbox::append(
                        w,
                        &cand.workspace_id,
                        ITEM_LIFECYCLE,
                        ITEM_LIFECYCLE_V,
                        &serde_json::to_value(&event)?,
                    )?;
                    Ok::<_, crate::error::CoreError>(())
                })?;
                report.enqueued += 1;
            }
        }

        if report.cleaned > 0 {
            metrics::counter!("steve_worktree_backfill_items_total", "result" => "cleaned")
                .increment(report.cleaned as u64);
        }
        if report.enqueued > 0 {
            metrics::counter!("steve_worktree_backfill_items_total", "result" => "enqueued")
                .increment(report.enqueued as u64);
        }
        if report.failed > 0 {
            metrics::counter!("steve_worktree_backfill_items_total", "result" => "failed")
                .increment(report.failed as u64);
        }

        let detail = serde_json::to_value(&report)?;
        store
            .settings()
            .mark_maintenance_done(CLOSED_WORKTREE_BACKFILL_KEY, &detail)?;
        tracing::info!(
            scanned = report.scanned,
            cleaned = report.cleaned,
            enqueued = report.enqueued,
            failed = report.failed,
            "completed one-time closed worktree backfill"
        );
        Ok(report)
    }
}

/// Deterministic item worktree branch name (never queries local git on the Hub).
pub(crate) fn worktree_branch(item: &Item) -> String {
    item.branch
        .clone()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| format!("steve/{}", item.id.to_lowercase()))
}

/// Lexically normalizes a path (collapsing `.` and `..`, and stripping macOS `/private` prefix on
/// `/private/tmp` and `/private/var`) without touching the local filesystem.
fn normalize_lexical(p: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for c in p.components() {
        match c {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            other => out.push(other.as_os_str()),
        }
    }
    if let Ok(rest) = out.strip_prefix("/private")
        && (rest.starts_with("tmp") || rest.starts_with("var"))
    {
        return Path::new("/").join(rest);
    }
    out
}

/// Whether `cwd` lies inside `root` (pure lexical check; no local filesystem syscalls on the Hub).
fn path_under_root(root: &Path, cwd: &Path) -> bool {
    if cwd.starts_with(root) && cwd != root {
        return true;
    }
    let nr = normalize_lexical(root);
    let nc = normalize_lexical(cwd);
    nc.starts_with(&nr) && nc != nr
}

impl Tx<'_> {
    /// Whether `cwd` is a managed worktree path under `cfg.worktrees`.
    pub(crate) fn under_worktrees(&self, cwd: &str) -> bool {
        !cwd.is_empty() && path_under_root(&self.cfg().worktrees, Path::new(cwd))
    }

    /// The item's managed worktree directory under `cfg.worktrees`, if recorded or expected at the
    /// conventional `<worktrees>/<item_id>` path for a git worktree project.
    pub(crate) fn item_worktree_dir(&self, item: &Item) -> Result<Option<String>> {
        if let Some(cwd) = item.cwd.as_deref().filter(|c| self.under_worktrees(c)) {
            return Ok(Some(cwd.to_string()));
        }
        if let Some(pid) = item.project_id.as_deref() {
            let project = self.db().project(Some(pid))?;
            let is_worktree_project = project
                .as_ref()
                .is_none_or(|p| p.kind == "git" && p.worktree);
            if is_worktree_project {
                let fallback: PathBuf = self.cfg().worktrees.join(&item.id);
                return Ok(Some(fallback.to_string_lossy().into()));
            }
        }
        Ok(None)
    }

    /// Appends a system timeline note on `item_id` unless an identical note is already recorded.
    pub(crate) fn note_cleanup_once(&self, item_id: &str, text: &str) -> Result<()> {
        let exists = self
            .db()
            .timeline(item_id)?
            .iter()
            .any(|e| e.actor == "steve" && e.kind == "sys" && e.text == text);
        if !exists {
            self.db()
                .timeline_add(item_id, "steve", "sys", text, None)?;
        }
        Ok(())
    }

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

    /// A closed item's worktree that its backend made is given back by it. `true`: dispatched to
    /// the backend; `false`: fall through to Node housekeeping (`disk_reclaim`).
    pub(crate) fn tidy_by_backend(
        &self,
        i: &Item,
        p: &crate::repo::Project,
        branch: Option<&str>,
        cwd: &str,
    ) -> Result<bool> {
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
        let keep_branch = branch.is_none_or(|b| {
            !self
                .c
                .inspect_branch(&p.path, b)
                .is_some_and(|(merged, _)| merged)
        });
        match self.rt().tidy(&s.id, keep_branch) {
            Ok(true) => {
                let branch_label = if keep_branch { "preserved" } else { "deleted" };
                tracing::info!(
                    item = %i.id,
                    session = %s.id,
                    cwd,
                    branch = ?branch,
                    keep_branch,
                    "dispatched closed item worktree cleanup to node backend"
                );
                metrics::counter!(
                    "steve_worktree_cleanup_total",
                    "backend" => "node",
                    "outcome" => "dispatched",
                    "branch" => branch_label
                )
                .increment(1);
                Ok(true)
            }
            Ok(false) => Ok(false),
            Err(e) => {
                let detail = e.to_string();
                tracing::warn!(
                    item = %i.id,
                    session = %s.id,
                    cwd,
                    error = %detail,
                    "failed to dispatch worktree cleanup to node backend"
                );
                metrics::counter!(
                    "steve_worktree_cleanup_total",
                    "backend" => "node",
                    "outcome" => "failed",
                    "branch" => "kept"
                )
                .increment(1);
                let note = steve_i18n::stored(|| {
                    steve_i18n::t!(
                        "core.worktree.cleanup_failed",
                        cwd = cwd,
                        error = detail.as_str()
                    )
                });
                let _ = self.note_cleanup_once(&i.id, &note);
                Err(e)
            }
        }
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

    fn workspace_tidied(&self, item_id: &str, removed: bool, detail: Option<&str>) -> Result<()> {
        let Some(i) = self.db().item(item_id)? else {
            return Ok(());
        };
        let cwd = self
            .item_worktree_dir(&i)?
            .or_else(|| i.cwd.clone().filter(|c| !c.is_empty()))
            .unwrap_or_else(|| self.cfg().worktrees.join(item_id).to_string_lossy().into());
        let project = self.db().project(i.project_id.as_deref())?;
        let branch = i.branch.as_deref().filter(|b| !b.is_empty());

        if !removed {
            let reason = detail
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .unwrap_or("unknown error");
            tracing::warn!(
                item = item_id,
                cwd = %cwd,
                branch = ?branch,
                error = reason,
                "node worktree cleanup failed"
            );
            metrics::counter!(
                "steve_worktree_cleanup_total",
                "backend" => "node",
                "outcome" => "failed",
                "branch" => "kept"
            )
            .increment(1);
            let note = steve_i18n::t!(
                "core.worktree.cleanup_failed",
                cwd = cwd.as_str(),
                error = reason
            );
            self.note_cleanup_once(item_id, &note)?;
            return Ok(());
        }

        let partial_error = detail.map(str::trim).filter(|s| !s.is_empty());
        let (note, outcome, branch_label) = match (project.as_ref(), branch) {
            (Some(_), Some(branch)) if partial_error.is_some() => {
                let err = partial_error.unwrap_or_default();
                tracing::warn!(
                    item = item_id,
                    cwd = %cwd,
                    branch,
                    error = err,
                    "node removed worktree but failed to delete branch"
                );
                (
                    steve_i18n::t!(
                        "core.worktree.removed_branch_failed",
                        cwd = cwd.as_str(),
                        branch = branch,
                        error = err
                    ),
                    "partial",
                    "delete_failed",
                )
            }
            (Some(p), Some(branch)) => match self.c.inspect_branch(&p.path, branch) {
                Some((_, Some(h))) => {
                    tracing::info!(
                        item = item_id,
                        cwd = %cwd,
                        branch,
                        head = %h,
                        "node removed worktree and preserved unmerged branch"
                    );
                    (
                        steve_i18n::t!(
                            "core.worktree.removed_unmerged",
                            cwd = cwd.as_str(),
                            branch = branch,
                            head = h.as_str()
                        ),
                        "removed",
                        "preserved",
                    )
                }
                Some((_, None)) => {
                    tracing::info!(
                        item = item_id,
                        cwd = %cwd,
                        branch,
                        "node removed worktree and deleted merged branch"
                    );
                    (
                        steve_i18n::t!(
                            "core.worktree.removed_merged",
                            cwd = cwd.as_str(),
                            branch = branch
                        ),
                        "removed",
                        "deleted",
                    )
                }
                None => {
                    tracing::info!(
                        item = item_id,
                        cwd = %cwd,
                        branch,
                        "node removed worktree; branch status unavailable"
                    );
                    (
                        steve_i18n::t!(
                            "core.worktree.removed_unmerged_no_sha",
                            cwd = cwd.as_str(),
                            branch = branch
                        ),
                        "removed",
                        "preserved",
                    )
                }
            },
            _ => {
                tracing::info!(item = item_id, cwd = %cwd, "node removed worktree");
                (
                    steve_i18n::t!("core.worktree.removed_only", cwd = cwd.as_str()),
                    "removed",
                    "none",
                )
            }
        };
        metrics::counter!(
            "steve_worktree_cleanup_total",
            "backend" => "node",
            "outcome" => outcome,
            "branch" => branch_label
        )
        .increment(1);
        self.note_cleanup_once(item_id, &note)?;
        Ok(())
    }
}
