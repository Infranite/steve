//! Item cycle transitions and evaluation of automatic close requests.
//!
//! ```text
//!  受理 ─► ready ──开始（第一项实际工作被领取）──► active ──请求完成（冻结 r）──► closing ──提交──► closed
//!             └──────────── 取消 / 失败（停稳后提交）──────────────┘   │  ▲ 返工 / 新输入 / 结果变了 │
//!                                                                     └──┘ （冻结作废，回到 active）
//!  closed ──显式新周期──► ready（cycle + 1；旧周期不改）
//! ```
//!
//! Each transition commits the item and its `work.item.lifecycle` fact together. Owner Finish
//! commits a closed cycle directly. For a requested close, [`Tx::evaluate_cycle_close`] commits
//! the cycle once every applicable gate holds:
//! 1. no productive work of the item runs — a turn in flight on any of its sessions (`is_busy`
//!    covers adapter start, the turn and the gateway-retry wait), a session still `running` /
//!    `starting`, a resume intent, or messages of a person queued for a session (Steve's own queued
//!    follow-ups void nothing and are dropped once it closes, #347);
//! 2. no required operation of this cycle and revision is still open ([`steve_store::ItemOperation`]);
//! 3. (completion) the frozen result is unchanged, or the complete delivery manifest
//!    was verified on the repositories' current default branches;
//! 4. (completion) the acceptance is decided: human, automatic or waived;
//! 5. (completion) no question or approval raised after the request is still open: an answer may
//!    change what is left to do, so the close is judged again once it was handled (cards older than the
//!    request are the acceptor's to override; the close clears them).
//!
//! The gate is checked again whenever something it waits on may have changed (a turn ended, a
//! decision was answered, an operation reported) and by a periodic tick (queued turns and the release
//! hold skip the turn-end callback). A close commits once per `(item, cycle)` (`item_cycles`).

use std::path::Path;

use serde_json::{Value, json};
use steve_store::{ItemOperation, LifecycleWrite};
pub use steve_types::{CloseKind, ClosePolicy, CloseRequest, ItemPhase};
use steve_work_api::{
    ITEM_DONE, ITEM_DONE_V, ITEM_LIFECYCLE, ITEM_LIFECYCLE_V, ItemDone, ItemLifecycle,
};

use crate::Tx;
use crate::error::{CoreError, Result, msg};
use crate::repo::{Item, PlanStep};
use crate::thread::flowback::{Update, flow_quiet};

/// Why a closing was invalidated: the reason code of its `invalidate` mark, which the statistics
/// count by (migration 92 gave the timeline lines from before the same codes).
pub mod void {
    /// The agent's plan has open steps again. Only closings from before #239 (their timeline lines):
    /// a plan reported while closing is now only noted.
    pub const PLAN: &str = "plan";
    /// A new delivery replaced the one waiting for acceptance.
    pub const REDELIVER: &str = "redeliver";
    /// A message or supplement reached the item.
    pub const MESSAGE: &str = "message";
    /// The frozen result changed after the freeze.
    pub const CHANGED: &str = "changed";
    /// The work graph was replanned or reset.
    pub const GRAPH: &str = "graph";
    /// A work unit did not pass.
    pub const UNIT_FAILED: &str = "unit_failed";
    /// The acceptance asked for rework.
    pub const REWORK: &str = "rework";
    /// You said it was done after all: accepted again.
    pub const REACCEPT: &str = "reaccept";
}

pub const READY: ItemPhase = ItemPhase::Ready;
pub const ACTIVE: ItemPhase = ItemPhase::Active;
pub const PAUSED: ItemPhase = ItemPhase::Paused;
pub const CLOSING: ItemPhase = ItemPhase::Closing;
pub const CLOSED: ItemPhase = ItemPhase::Closed;

/// The `via` of a message Steve itself queued for a session (a continue, a follow-up about the
/// release or the pull request), as the queue and the conversation show it.
pub const STEVE_VIA: &str = "steve";

/// The user-facing refusal for anything sent to a closed item, in the request's language.
pub fn closed_msg() -> String {
    steve_i18n::t!("core.item.closed")
}

/// Required proof from an explicitly configured release.
pub const OP_RELEASE: &str = "release";
/// A failure card asking you to check an external effect whose result nobody knows.
pub(crate) fn unknown_effect_card(d: &steve_store::Decision) -> bool {
    d.kind == "fail" && d.options.iter().any(|x| x == "release_retry")
}

pub(crate) fn conflict(m: impl Into<String>) -> CoreError {
    CoreError::Status(409, m.into())
}

/// The worktree's content: `HEAD` and the tree id of everything in the worktree that git would see
/// (tracked and untracked files, staged or not; ignored files are not part of a result), written through a
/// throwaway index so the item's own index is never touched. Content-addressed: any change of a file's
/// bytes or mode changes it, even when `git status` reads the same. `None` when `cwd` is not a git
/// worktree or git can't tell.
pub(crate) fn fingerprint(cwd: Option<&str>) -> Option<String> {
    fingerprint_now(cwd).ok().flatten()
}

/// How long the git of one fingerprint may take in all. It runs under the core's lock: a git that
/// hangs (a huge worktree, a stuck filesystem) must not hold everything, and its answer is then
/// unknown, never "the result changed".
pub(crate) const PRINT_DEADLINE: std::time::Duration = std::time::Duration::from_secs(20);

/// The fingerprint could not be taken in time ([`PRINT_DEADLINE`]): the gate holds and asks again.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct PrintUnknown;

/// [`fingerprint`], telling a git that ran out of time (`Err`: unknown) from no worktree or no git
/// checkout (`Ok(None)`).
pub(crate) fn fingerprint_now(
    cwd: Option<&str>,
) -> std::result::Result<Option<String>, PrintUnknown> {
    fingerprint_within(cwd, PRINT_DEADLINE)
}

fn fingerprint_within(
    cwd: Option<&str>,
    deadline: std::time::Duration,
) -> std::result::Result<Option<String>, PrintUnknown> {
    let Some(cwd) = cwd.filter(|c| !c.is_empty() && Path::new(c).exists()) else {
        return Ok(None);
    };
    let index = std::env::temp_dir().join(format!("steve-fingerprint-{}", uuid::Uuid::new_v4()));
    let out = fingerprint_with(cwd, &index, std::time::Instant::now() + deadline);
    let _ = std::fs::remove_file(&index);
    /* a git killed at the deadline may leave its lock on the throwaway index */
    let mut lock = index.into_os_string();
    lock.push(".lock");
    let _ = std::fs::remove_file(lock);
    out
}

fn fingerprint_with(
    cwd: &str,
    index: &Path,
    end: std::time::Instant,
) -> std::result::Result<Option<String>, PrintUnknown> {
    /* `Ok(None)`: git said no; `Err`: out of time */
    let run =
        |args: &[&str], own_index: bool| -> std::result::Result<Option<String>, PrintUnknown> {
            let mut cmd = crate::git::command();
            cmd.arg("-C")
                .arg(cwd)
                .args(args)
                .env("GIT_TERMINAL_PROMPT", "0");
            if own_index {
                cmd.env("GIT_INDEX_FILE", index);
            }
            let left = end.saturating_duration_since(std::time::Instant::now());
            match crate::util::run_deadline(cmd, left) {
                Ok((true, out, _)) => Ok(Some(out.trim().to_string())),
                Ok((false, ..)) | Err(crate::util::RunError::Spawn(_)) => Ok(None),
                Err(e) => {
                    tracing::warn!(cwd, "fingerprint: git {}: {e}", args.join(" "));
                    Err(PrintUnknown)
                }
            }
        };
    let Some(head) = run(&["rev-parse", "HEAD"], true)? else {
        return Ok(None);
    };
    /* start from the item's index (its stat cache keeps this fast), then take in the worktree */
    let real = run(
        &["rev-parse", "--path-format=absolute", "--git-path", "index"],
        false,
    )?;
    if let Some(r) = real.filter(|r| Path::new(r).exists()) {
        if std::fs::copy(r, index).is_err() {
            return Ok(None);
        }
    } else if run(&["read-tree", "HEAD"], true)?.is_none() {
        return Ok(None);
    }
    if run(&["add", "--all"], true)?.is_none() {
        return Ok(None);
    }
    let Some(tree) = run(&["write-tree"], true)? else {
        return Ok(None);
    };
    Ok(Some(
        match crate::upstream::own_change(Path::new(cwd), &head, &tree) {
            Some((base, patch)) => format!("{head}:{tree}:{base}:{patch}"),
            None => format!("{head}:{tree}"),
        },
    ))
}

/// `HEAD` of a fingerprint.
#[cfg(test)]
pub(crate) fn fingerprint_head(f: &str) -> &str {
    f.split(':').next().unwrap_or("")
}

/// The frozen result is still what the worktree holds (`now` its fingerprint now): the same content,
/// or the same own change on another base (a rebase onto a newer main). A new `HEAD` over the same
/// content — a reset onto the merged commit, an amended message — is the same result.
pub(crate) fn same_result(frozen: &str, now: &str) -> bool {
    let (a, b) = (crate::upstream::parse(frozen), crate::upstream::parse(now));
    if !a.tree.is_empty() && a.tree == b.tree {
        return true;
    }
    matches!((a.own, b.own), (Some((_, x)), Some((_, y))) if x == y)
}

/// The worktree, whose fingerprint is `now` (`None`: no worktree any more), moved away from the frozen
/// fingerprint `frozen` (no frozen fingerprint: never).
pub(crate) fn result_moved(now: Option<&str>, frozen: Option<&str>) -> bool {
    frozen.is_some_and(|f| !now.is_some_and(|n| same_result(f, n)))
}

/// Whether the gate of closing `c` compares the worktree with what was frozen: a completion still on
/// its frozen revision with a frozen fingerprint (a new revision is a change already).
fn looks_at_worktree(i: &Item, c: &CloseRequest) -> bool {
    c.kind == CloseKind::Complete
        && c.fingerprint.is_some()
        && c.frozen_revision == i.delivery_revision
}

/// What blocks a closing commit, for the projection (`closing.waiting`) and the logs.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Gate {
    pub turn: bool,
    pub input: bool,
    pub operation: bool,
    pub acceptance: bool,
    pub delivery: bool,
    /// A question or an approval raised after the closing request waits for an answer.
    pub decision: bool,
    /// The frozen result changed: the closing is void.
    pub changed: bool,
}

impl Gate {
    pub fn waiting(&self) -> Vec<&'static str> {
        let mut v = vec![];
        if self.turn {
            v.push("turn");
        }
        if self.operation {
            v.push("operation");
        }
        if self.acceptance {
            v.push("acceptance");
        }
        if self.delivery {
            v.push("delivery");
        }
        if self.input {
            v.push("input");
        }
        if self.decision {
            v.push("decision");
        }
        v
    }
    fn clear(&self) -> bool {
        !(self.turn
            || self.input
            || self.operation
            || self.acceptance
            || self.decision
            || self.delivery
            || self.changed)
    }
}

enum CycleChange {
    Start,
    Pause,
    Resume,
    Close(CloseRequest),
    Invalidate,
    StartNewCycle,
}

impl CycleChange {
    fn apply(self, item: &Item, write: &mut LifecycleWrite) -> Result<&'static str> {
        let (phase, request, transition) = match self {
            Self::Pause if item.phase.can_run() => (PAUSED, None, "paused"),
            Self::Resume if item.phase == PAUSED => (ACTIVE, None, "resumed"),
            Self::Start if item.phase == READY => (ACTIVE, None, "started"),
            Self::Close(request) if item.phase != CLOSED => {
                (CLOSING, Some(request), "closing_requested")
            }
            Self::Invalidate if item.phase == CLOSING => (ACTIVE, None, "closing_invalidated"),
            Self::StartNewCycle if item.phase == CLOSED => (READY, None, "cycle_created"),
            Self::Start
            | Self::Pause
            | Self::Resume
            | Self::Close(_)
            | Self::Invalidate
            | Self::StartNewCycle => {
                return Err(conflict("item state changed"));
            }
        };
        write.phase = phase;
        write.closing = request;
        Ok(transition)
    }
}

struct ClosedPlan {
    outcome: &'static str,
    acceptance: Option<String>,
    plan: Vec<PlanStep>,
    reason: String,
    closed: i64,
    w: LifecycleWrite,
    note: Option<String>,
}

impl Tx<'_> {
    /* ── writes ── */

    /// Commit the change and its event together against the aggregate revision.
    fn change_cycle(
        &self,
        i: &Item,
        mut w: LifecycleWrite,
        change: CycleChange,
        reason: &str,
        extra: impl FnOnce(&mut steve_store::W<'_>, &Item) -> Result<()>,
    ) -> Result<Item> {
        let ws = i.workspace_id.clone();
        let id = i.id.clone();
        let transition = change.apply(i, &mut w)?;
        self.db().store().write(|tx| -> Result<Item> {
            steve_store::require_item_revision_in(tx, &id, i.cycle, i.cas_revision, i.phase)?;
            let item = steve_store::lifecycle_write_in(tx, &id, &w)?
                .ok_or_else(|| msg(steve_i18n::t!("core.item.not_found")))?;
            steve_store::outbox::append(
                tx,
                &ws,
                ITEM_LIFECYCLE,
                ITEM_LIFECYCLE_V,
                &serde_json::to_value(ItemLifecycle {
                    item_id: item.id.clone(),
                    cycle: item.cycle,
                    revision: item.delivery_revision,
                    transition: transition.into(),
                    phase: item.phase.as_str().into(),
                    outcome: item.outcome.clone(),
                    acceptance: item.acceptance.clone(),
                    reason: reason.into(),
                })?,
            )?;
            extra(tx, &item)?;
            Ok(item)
        })
    }

    fn base(i: &Item) -> LifecycleWrite {
        LifecycleWrite {
            phase: i.phase,
            cycle: i.cycle,
            delivery_revision: i.delivery_revision,
            outcome: i.outcome.clone(),
            acceptance: i.acceptance.clone(),
            closing: i.closing.clone(),
            closed_at: i.closed_at,
            closed_reason: i.closed_reason.clone(),
            ..Default::default()
        }
    }

    /// 409 for anything that would act on a closed item (a message, a supplement, a goal change).
    pub(crate) fn require_open_item(&self, item_id: &str) -> Result<()> {
        match self.db().item(item_id)? {
            Some(i) if i.phase == CLOSED => Err(conflict(closed_msg())),
            _ => Ok(()),
        }
    }

    /// The first actual work of the cycle was taken up: `ready` → `active` (no-op otherwise).
    pub(crate) fn start_item_work(&mut self, item_id: &str) -> Result<()> {
        let i = self.item(item_id)?;
        if i.phase != READY {
            return Ok(());
        }
        let w = Self::base(&i);
        self.change_cycle(&i, w, CycleChange::Start, "work_started", |_, _| Ok(()))?;
        Ok(())
    }

    pub(crate) fn pause_item(&mut self, id: &str) -> Result<()> {
        let item = self.item(id)?;
        if item.is_paused() {
            return Ok(());
        }
        if item.phase == CLOSING {
            if stopping(&item) {
                return Err(conflict(steve_i18n::t!("core.item.ending")));
            }
            self.invalidate_delivery(id, void::MESSAGE, "paused")?;
        }
        let item = self.item(id)?;
        self.change_cycle(
            &item,
            Self::base(&item),
            CycleChange::Pause,
            "paused",
            |_, _| Ok(()),
        )?;
        Ok(())
    }

    pub(crate) fn resume_item(&self, item: &Item) -> Result<()> {
        if item.is_paused() {
            self.change_cycle(
                item,
                Self::base(item),
                CycleChange::Resume,
                "resumed",
                |_, _| Ok(()),
            )?;
        }
        Ok(())
    }

    /// The executor (or the user, or a module) asks to complete the cycle: the result is frozen at a new
    /// revision and the request returns at once. With `accepted`, the acceptance is already decided
    /// (automatic, waived, or the user who asked). Required operations of the project are recorded.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn request_delivery_close(
        &mut self,
        item_id: &str,
        by: &str,
        policy: ClosePolicy,
        accepted: bool,
        reason: &str,
        decision: Option<String>,
        evidence: Vec<String>,
    ) -> Result<Item> {
        self.request_delivery_close_in(
            item_id,
            by,
            policy,
            accepted,
            reason,
            decision,
            evidence,
            |_, _| Ok(()),
        )
    }

    /// Work-unit result evidence and its frozen completion intent commit in the same transaction.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn request_delivery_close_in(
        &mut self,
        item_id: &str,
        by: &str,
        policy: ClosePolicy,
        accepted: bool,
        reason: &str,
        decision: Option<String>,
        evidence: Vec<String>,
        extra: impl FnOnce(&mut steve_store::W<'_>, &Item) -> Result<()>,
    ) -> Result<Item> {
        let i = self.item(item_id)?;
        match i.phase {
            CLOSED => return Err(conflict(closed_msg())),
            CLOSING => {
                if i.closing
                    .as_ref()
                    .is_none_or(|c| c.kind != CloseKind::Complete)
                {
                    return Err(conflict(steve_i18n::t!("core.item.ending_no_complete")));
                }
                /* a new delivery replaces the one waiting: its acceptance and operations go with it */
                self.invalidate_delivery(item_id, void::REDELIVER, "有新的交付，上一次交付作废")?;
            }
            READY | ACTIVE => {}
            PAUSED => return Err(conflict(steve_i18n::t!("core.item.ending"))),
        }
        let i = self.item(item_id)?;
        let rev = i.delivery_revision + 1;
        /* on a Node: the facts gathered for this delivery, or taken once they come */
        let (print, print_pending) = if self.git_on_node(&i.id) {
            match self.git_take(&i) {
                Some(Ok(seen)) => (seen.map(|s| s.print), false),
                _ => (None, true),
            }
        } else {
            (fingerprint(i.cwd.as_deref()), false)
        };
        let c = CloseRequest {
            kind: CloseKind::Complete,
            reason: reason.into(),
            by: by.into(),
            at: crate::util::now(),
            frozen_revision: rev,
            scope_revision: self.scope_revision(item_id)?,
            workflow_revision: self
                .db()
                .store()
                .workflows()
                .get(item_id)?
                .map(|g| g.revision),
            fingerprint: print,
            print_pending,
            policy: Some(policy),
            accepted,
            decision,
            evidence,
            rule: None,
        };
        let mut w = Self::base(&i);
        w.delivery_revision = rev;
        self.change_cycle(&i, w, CycleChange::Close(c), reason, extra)?;
        self.evaluate_cycle_close(item_id)?;
        self.item(item_id)
    }

    /// The acceptance of the current closing was decided (`mode`: `human` | `automatic` | `waived`).
    pub(crate) fn accept_delivery(
        &mut self,
        item_id: &str,
        mode: ClosePolicy,
        evidence: Option<&str>,
    ) -> Result<()> {
        self.accept_delivery_in(item_id, mode, evidence, |_, _| Ok(()))
    }

    pub(crate) fn accept_delivery_in(
        &mut self,
        item_id: &str,
        mode: ClosePolicy,
        evidence: Option<&str>,
        extra: impl FnOnce(&mut steve_store::W<'_>, &Item) -> Result<()>,
    ) -> Result<()> {
        let i = self.item(item_id)?;
        let Some(mut c) = i
            .closing
            .clone()
            .filter(|c| i.phase == CLOSING && c.kind == CloseKind::Complete)
        else {
            return Err(conflict(steve_i18n::t!(
                "core.item.not_awaiting_acceptance"
            )));
        };
        if c.accepted {
            return self.db().store().write(|w| extra(w, &i));
        }
        c.accepted = true;
        c.policy = Some(mode);
        if let Some(e) = evidence {
            c.evidence.push(e.into());
        }
        let w = Self::base(&i);
        self.change_cycle(&i, w, CycleChange::Close(c), "验收通过", extra)?;
        self.evaluate_cycle_close(item_id)?;
        Ok(())
    }

    /// An undone acceptance before the close committed: waiting on the user again.
    pub(crate) fn withdraw_acceptance(&mut self, item_id: &str) -> Result<()> {
        let i = self.item(item_id)?;
        if i.phase == CLOSED {
            return Err(conflict(steve_i18n::t!("core.item.acceptance_undo_closed")));
        }
        let Some(mut c) = i
            .closing
            .clone()
            .filter(|c| i.phase == CLOSING && c.kind == CloseKind::Complete)
        else {
            return Ok(());
        };
        c.accepted = false;
        let w = Self::base(&i);
        self.change_cycle(&i, w, CycleChange::Close(c), "验收被撤销", |_, _| {
            Ok(())
        })?;
        Ok(())
    }

    /// Records an operation of the item's current cycle and revision.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn record_item_operation(
        &self,
        i: &Item,
        kind: &str,
        effect: Option<&str>,
        required: bool,
        status: &str,
        label: &str,
        evidence: Value,
    ) -> Result<(ItemOperation, bool)> {
        let (cycle, rev) = (i.cycle, i.delivery_revision);
        Ok(self.db().store().write(|tx| {
            steve_store::operation_repo::add(
                tx, &i.id, cycle, rev, kind, effect, required, status, label, &evidence,
            )
        })?)
    }

    /// Sets an operation's status; the gate is checked again.
    pub(crate) fn settle_item_operation(
        &mut self,
        op: &str,
        status: &str,
        evidence: Value,
    ) -> Result<bool> {
        let changed = self
            .db()
            .store()
            .write(|tx| steve_store::operation_repo::set(tx, op, status, &evidence))?;
        if changed && let Some(o) = self.db().store().items().operation(op)? {
            self.evaluate_cycle_close(&o.item_id)?;
        }
        Ok(changed)
    }

    /// You checked the effects of `kind` whose result nobody knew (all of this cycle, any revision, but
    /// `except`, which the caller settles itself): they become `status` (`succeeded`, `failed` or `waived`).
    /// The gate is checked again.
    pub(crate) fn reconcile_item_operations(
        &mut self,
        item_id: &str,
        kind: &str,
        status: &str,
        note: &str,
        except: Option<&str>,
    ) -> Result<usize> {
        let i = self.item(item_id)?;
        let mut n = 0;
        for o in self.db().store().items().operations(item_id, i.cycle)? {
            if o.kind == kind
                && o.status == "unknown"
                && except != Some(o.id.as_str())
                && self.db().store().write(|tx| {
                    steve_store::operation_repo::set(tx, &o.id, status, &json!({"checked": note}))
                })?
            {
                n += 1;
            }
        }
        if n > 0 {
            self.evaluate_cycle_close(item_id)?;
        }
        Ok(n)
    }

    /// The frozen result is void (rework, new input, a new result): back to `active`, the acceptance
    /// card expires. `false` when nothing was waiting to close.
    /// `code` is one of [`void`], recorded with `why` on the `invalidate` mark.
    pub(crate) fn invalidate_delivery(
        &mut self,
        item_id: &str,
        code: &str,
        why: &str,
    ) -> Result<bool> {
        let i = self.item(item_id)?;
        let Some(c) = i
            .closing
            .clone()
            .filter(|c| i.phase == CLOSING && c.kind == CloseKind::Complete)
        else {
            return Ok(false);
        };
        let w = Self::base(&i);
        self.change_cycle(&i, w, CycleChange::Invalidate, why, |tx, _| {
            let mut cards = steve_store::decision_repo::open_item_decisions(tx, item_id)?;
            cards.retain(|d| d.kind == "accept" || Some(&d.id) == c.decision.as_ref());
            for d in cards {
                steve_store::DecisionRows(tx).settle(
                    &d.id,
                    &json!({"superseded": why}),
                    steve_store::decision_repo::DecisionChange::Expire,
                    None,
                )?;
            }
            steve_store::item_repo::append_system_note(tx, item_id, &format!("收尾作废：{why}"))?;
            Ok(())
        })?;
        self.mark_invalidate(&i.workspace_id, item_id, code, why); // stats
        Ok(true)
    }

    /// New input (a message, a supplement, a queued message sent now) reaches the item: a closing for
    /// completion is void; a closed item refuses it.
    pub(crate) fn receive_item_input(&mut self, item_id: &str) -> Result<()> {
        let i = self.item(item_id)?;
        match i.phase {
            CLOSED => Err(conflict(closed_msg())),
            CLOSING => match i.closing.as_ref().map(|c| c.kind) {
                Some(CloseKind::Complete) => {
                    self.invalidate_delivery(item_id, void::MESSAGE, "收到新的消息")?;
                    Ok(())
                }
                _ => Err(conflict(steve_i18n::t!("core.item.ending_no_message"))),
            },
            READY | ACTIVE | PAUSED => Ok(()),
        }
    }

    /// Cancel (`kind = cancel`) or fail (`kind = fail`, with a reason; `rule` for a termination rule):
    /// running turns are stopped outside the lock, the close commits once the work stopped.
    pub(crate) fn request_cycle_stop(
        &mut self,
        item_id: &str,
        kind: CloseKind,
        by: &str,
        reason: &str,
        rule: Option<&str>,
    ) -> Result<Item> {
        let i = self.item(item_id)?;
        if i.phase == CLOSED {
            return Err(conflict(closed_msg()));
        }
        if let Some(c) = i.closing.clone().filter(|c| c.kind != CloseKind::Complete) {
            if c.kind == kind {
                return Ok(i);
            }
            return Err(conflict(steve_i18n::t!("core.item.already_ending")));
        }
        let c = CloseRequest {
            kind,
            reason: reason.into(),
            by: by.into(),
            at: crate::util::now(),
            frozen_revision: i.delivery_revision,
            scope_revision: self.scope_revision(item_id)?,
            workflow_revision: None,
            fingerprint: None,
            print_pending: false,
            policy: None,
            accepted: true,
            decision: None,
            evidence: vec![],
            rule: rule.map(String::from),
        };
        let w = Self::base(&i);
        self.change_cycle(&i, w, CycleChange::Close(c), reason, |_, _| Ok(()))?;
        /* the acceptance or approval cards of the work being stopped no longer apply */
        /* the acceptance, question and failure cards of the work being stopped no longer apply, except a card
         * asking you to check an effect nobody knows: the stop waits on that one */
        crate::lifecycle::supersede_where(
            self.db(),
            item_id,
            |d| (matches!(d.kind.as_str(), "accept" | "ask" | "fail") && !unknown_effect_card(d)),
            reason,
        )?;
        for s in self.db().sessions_of_item(item_id)? {
            if s.status != "ended"
                && let Some(turn) = self.rt().running(&s.id)
            {
                self.st.cancels.push((turn, item_id.into()));
            }
        }
        self.evaluate_cycle_close(item_id)?;
        self.item(item_id)
    }

    /// An explicit new cycle of a closed item: `cycle + 1`, revision 0, `ready`; the cycle before stays
    /// as it closed. A new goal and its prerequisites commit before any session starts.
    pub(crate) fn start_new_cycle(
        &mut self,
        item_id: &str,
        by: &str,
        goal: Option<&str>,
    ) -> Result<Item> {
        let i = self.item(item_id)?;
        if i.phase != CLOSED {
            return Err(conflict(steve_i18n::t!("core.item.new_cycle_needs_closed")));
        }
        let w = LifecycleWrite {
            cycle: i.cycle + 1,
            delivery_revision: 0,
            ..Default::default()
        };
        let reason = format!("{by} 开始了新周期");
        self.change_cycle(&i, w, CycleChange::StartNewCycle, &reason, |tx, _| {
            steve_store::item_repo::unarchive(tx, item_id)?;
            Ok(())
        })?;
        self.archive_cycle_plan(&i, by)?;
        self.db().timeline_add(
            item_id,
            by,
            if by == "me" { "me" } else { "sys" },
            &format!("开始第 {} 个周期", i.cycle + 1),
            None,
        )?;
        if let Some(g) = goal.map(str::trim).filter(|g| !g.is_empty()) {
            self.db().add_goal(item_id, g)?;
            if !crate::lifecycle::deps_open(self.db(), item_id)?.is_empty() {
                self.db().store().dependency_waits().arm(item_id)?;
            }
        }
        self.item(item_id)
    }

    /// The plan was the cycle before's: it is kept as an earlier round, the new cycle starts without one.
    fn archive_cycle_plan(&self, i: &Item, by: &str) -> Result<()> {
        if !i.plan.is_empty() {
            let end = if i.plan.iter().all(|p| p.done) {
                "done"
            } else {
                "replaced"
            };
            self.db()
                .plan_round_add(&i.workspace_id, &i.id, &i.plan, end, by)?;
            self.db()
                .item_patch(&i.id, crate::patch![plan: Vec::<PlanStep>::new()])?;
        }
        Ok(())
    }

    /// A work unit's retry on its own closed child (#182): a new cycle as [`Self::start_new_cycle`] makes
    /// one (`cycle + 1`, revision 0, `ready`, the plan kept as an earlier round; the cycle before stays
    /// as it closed), with the unit's claim of it in the same transaction (`claim`): the cycle never
    /// opens without its attempt, nor the attempt without its cycle. It runs at once (`run`).
    pub(crate) fn retry_unit_cycle(
        &mut self,
        item_id: &str,
        attempt: i64,
        claim: impl FnOnce(&mut steve_store::W<'_>) -> Result<()>,
    ) -> Result<Item> {
        let i = self.item(item_id)?;
        if i.phase != CLOSED {
            return Err(conflict(steve_i18n::t!("core.item.new_cycle_needs_closed")));
        }
        let w = LifecycleWrite {
            cycle: i.cycle + 1,
            delivery_revision: 0,
            ..Default::default()
        };
        let reason = format!("协作工作第 {attempt} 次尝试");
        self.change_cycle(&i, w, CycleChange::StartNewCycle, &reason, |tx, _| {
            steve_store::item_repo::unarchive(tx, item_id)?;
            claim(tx)
        })?;
        self.archive_cycle_plan(&i, "steve")?;
        self.db().timeline_add(
            item_id,
            "steve",
            "sys",
            &format!("开始第 {} 个周期：协作工作第 {attempt} 次尝试", i.cycle + 1),
            None,
        )?;
        self.item(item_id)
    }

    /* ── the gate ── */

    /// What still holds the item's closing.
    #[cfg(test)]
    pub(crate) fn delivery_blockers(&self, i: &Item, c: &CloseRequest) -> Result<Gate> {
        let looks = looks_at_worktree(i, c);
        let now = match looks {
            false => None,
            /* on a Node: what it said last, or the worktree left out */
            true if self.git_on_node(&i.id) => self.git_seen(&i.id).map(|s| Some(s.print.clone())),
            /* a fingerprint git could not take in time leaves the worktree out */
            true => fingerprint_now(i.cwd.as_deref()).ok(),
        };
        self.delivery_blockers_at(i, c, now.as_ref().map(Option::as_deref))
    }

    /// [`Self::delivery_blockers`] with the worktree's fingerprint `now` already taken (`Some(None)`: no worktree);
    /// `None` leaves the worktree out, so `changed` then only sees a new revision.
    fn delivery_blockers_at(
        &self,
        i: &Item,
        c: &CloseRequest,
        now: Option<Option<&str>>,
    ) -> Result<Gate> {
        let mut g = Gate {
            delivery: c.kind == CloseKind::Complete && self.delivery_required(i)?,
            ..Gate::default()
        };
        if !self
            .workflow_blockers(&i.id, c.kind == CloseKind::Complete)?
            .is_empty()
        {
            g.operation = true;
        }
        for s in self.db().sessions_of_item(&i.id)? {
            if s.status == "ended" {
                continue;
            }
            if self.busy(&s.id) || matches!(s.status.as_str(), "running" | "starting") {
                g.turn = true;
            }
            /* only a person's input voids a completion: what Steve queued itself does not (#347) */
            if c.kind == CloseKind::Complete
                && self.db().queued(&s.id)?.iter().any(|q| q.via != STEVE_VIA)
            {
                g.input = true;
            }
        }
        if self.st.releasing.as_ref().is_some_and(|r| r.item == i.id) {
            g.operation = true;
        }
        for o in self.db().store().items().operations(&i.id, i.cycle)? {
            /* any effect of this cycle still in flight or of unknown result holds every kind of closing, whatever
             * revision it was for and whether or not it was required: it is settled (or checked by you) first,
             * an unknown effect is never closed over. A completion also needs each of its own requirements done. */
            let open = matches!(o.status.as_str(), "running" | "unknown")
                || (c.kind == CloseKind::Complete
                    && o.required
                    && o.revision == i.delivery_revision
                    && !matches!(o.status.as_str(), "succeeded" | "waived"));
            if open {
                g.operation = true;
            }
        }
        if c.kind == CloseKind::Complete {
            if !c.accepted {
                g.acceptance = true;
            }
            /* a permission is part of a running turn; the acceptance card is the acceptance above; a
             * pull request waiting for your yes to merge is the release above (merged on the host
             * without it, the item still closes) */
            g.decision = self.db().decisions_of_item(&i.id)?.iter().any(|d| {
                d.status == steve_types::DecisionStatus::Open
                    && d.created > c.at
                    && (d.kind == "ask" || (d.kind == "approve" && d.subject_type != "tool_call"))
            });
            if c.frozen_revision != i.delivery_revision
                || c.scope_revision != self.scope_revision(&i.id)?
                || c.workflow_revision
                    != self
                        .db()
                        .store()
                        .workflows()
                        .get(&i.id)?
                        .map(|g| g.revision)
                || now.is_some_and(|n| result_moved(n, c.fingerprint.as_deref()))
            {
                g.changed = true;
            }
        }
        Ok(g)
    }

    fn pin_worktree_snapshot(
        &mut self,
        i: &Item,
        c: &mut CloseRequest,
        print: &Option<Option<String>>,
        item_id: &str,
    ) -> Result<Option<bool>> {
        if let Some(p) = &print
            && c.print_pending
            && c.frozen_revision == i.delivery_revision
        {
            /* the result frozen before its facts came: they are what was frozen */
            c.fingerprint = p.clone();
            c.print_pending = false;
            self.change_cycle(
                i,
                Self::base(i),
                CycleChange::Close(c.clone()),
                "result_frozen",
                |_, _| Ok(()),
            )?;
            /* those facts were gathered with nothing frozen to compare against or to look for in a
             * release: nothing is decided on them, the next gathering asks with what is frozen now
             * and checks the closing again */
            let i = self.item(item_id)?;
            self.git_gather(&i);
            return Ok(Some(false));
        }
        Ok(None)
    }

    /// Commits the closing when nothing holds it any more. `true` when the item is closed now.
    pub(crate) fn evaluate_cycle_close(&mut self, item_id: &str) -> Result<bool> {
        self.try_close(item_id, None)
    }

    fn delivery_refs(&self, item: &Item) -> Result<Vec<steve_types::DeliveryRef>> {
        let id = steve_types::ItemId::from(item.id.clone());
        let refs = self.db().store().read::<_, steve_store::StoreError>(|r| {
            let scope = steve_store::item_delivery::scope(r, &item.id)?;
            steve_store::item_delivery::refs(r, &id, item.cycle, scope)
        })?;
        Ok(refs.into_iter().filter(|pr| pr.required).collect())
    }

    pub(crate) fn delivery_required(&self, item: &Item) -> Result<bool> {
        Ok(!self.delivery_refs(item)?.is_empty())
    }

    fn try_close(
        &mut self,
        item_id: &str,
        proof: Option<&steve_types::DeliveryProof>,
    ) -> Result<bool> {
        let Some(i) = self.db().item(item_id)? else {
            return Ok(false);
        };
        if i.phase != CLOSING {
            return Ok(i.phase == CLOSED);
        }
        let Some(mut c) = i.closing.clone() else {
            return Ok(false);
        };
        if let Some(proof) = proof.map(|p| &p.baseline)
            && (proof.cycle != i.cycle
                || proof.revision != i.delivery_revision
                || proof.scope != c.scope_revision
                || proof.scope != self.scope_revision(item_id)?
                || proof.refs.is_empty()
                || proof.refs != self.delivery_refs(&i)?
                || c.kind != CloseKind::Complete
                || !c.accepted)
        {
            metrics::counter!("steve_item_stale_delivery_total").increment(1);
            return Ok(false);
        }
        let verified_manifest = proof.is_some();
        /* on a Node the worktree's facts are gathered off the lock: until they are here (or while
         * its Node can't be reached) nothing is decided on them, the closing holds. Only when the
         * gate uses them: a frozen result to compare, one to freeze, an accepted one to release */
        let node = !verified_manifest
            && c.kind == CloseKind::Complete
            && (c.print_pending || c.accepted || looks_at_worktree(&i, &c))
            && self.git_on_node(item_id);
        /* `Some(None)`: its Node says there is no git checkout */
        let seen = match node {
            true => match self.git_take(&i) {
                Some(Ok(seen)) => Some(seen),
                Some(Err(e)) => {
                    tracing::debug!(item = item_id, "closing check waits for its Node: {e}");
                    return Ok(false);
                }
                None => return Ok(false),
            },
            false => None,
        };
        let print = seen.as_ref().map(|s| s.as_ref().map(|s| s.print.clone()));
        if let Some(result) = self.pin_worktree_snapshot(&i, &mut c, &print, item_id)? {
            return Ok(result);
        }
        /* the worktree is fingerprinted once (it runs `git add --all` over all of it), for the gate and
         * for saying what moved. On the periodic tick, not while something else still holds the closing
         * (a turn, an acceptance, an operation, a question): the check that clears it looks then. */
        let held = self.delivery_blockers_at(&i, &c, None)?;
        let others = held.turn || held.input || held.operation || held.acceptance || held.decision;
        let now = match looks_at_worktree(&i, &c) && !verified_manifest {
            false => None,
            true if node => print.clone(),
            true if self.st.evaluating_closing_cycles && others && !held.changed => None,
            true => match fingerprint_now(i.cwd.as_deref()) {
                Ok(p) => Some(p),
                Err(PrintUnknown) => {
                    /* unknown is not "changed": the closing holds, and is checked again */
                    tracing::info!(
                        item = item_id,
                        "closing check: the worktree's fingerprint took too long"
                    );
                    return Ok(false);
                }
            },
        };
        let g = self.delivery_blockers_at(&i, &c, now.as_ref().map(Option::as_deref))?;
        /* a result that moved is void before anything is started on its acceptance (the acceptance
         * hooks, a GitHub PR card) */
        if g.changed && !g.turn {
            /* the frozen result moved after it was frozen (and nothing is still at work on it): say what
             * moved, and tell the agent, which may well believe its delivery still stands */
            let now = match now {
                Some(n) => n,
                None if node => print.clone().flatten(),
                None if c.fingerprint.is_some() => fingerprint(i.cwd.as_deref()),
                None => None,
            };
            let moved = seen
                .as_ref()
                .and_then(|s| s.as_ref())
                .and_then(|s| s.moved.clone());
            let what = match (&c.fingerprint, now) {
                (Some(f), Some(n)) if node => {
                    moved.unwrap_or_else(|| crate::upstream::describe_with(f, &n, |_, _| vec![]))
                }
                (Some(f), Some(n)) => crate::upstream::describe_change(
                    Path::new(i.cwd.as_deref().unwrap_or("")),
                    f,
                    &n,
                ),
                (Some(_), None) => "工作区不见了".into(),
                _ => "交付的版本变了".into(),
            };
            let why = format!("冻结之后结果又变了：{what}，需要重新交付");
            self.invalidate_delivery(item_id, void::CHANGED, &why)?;
            self.takeover_invalidated(item_id, &why)?;
            return Ok(false);
        }
        if g.input && !g.turn {
            self.invalidate_delivery(item_id, void::MESSAGE, "收到新的消息")?;
            return Ok(false);
        }
        let mut g = g;
        if proof.is_some() {
            g.delivery = false;
        }
        if !g.clear() {
            return Ok(false);
        }
        self.commit_cycle(&i, &c, None, proof)
    }

    fn plan_cycle_close(&self, i: &Item, c: &CloseRequest) -> Result<ClosedPlan> {
        let outcome = match c.kind {
            CloseKind::Complete => "completed",
            CloseKind::Fail => "failed",
            CloseKind::Cancel => "cancelled",
        };
        let acceptance = c.policy.map(|policy| policy.as_str().to_owned());
        /* finishing never ticks steps off: what was not done stays visible as left open */
        let plan: Vec<PlanStep> = i
            .plan
            .iter()
            .map(|p| PlanStep {
                left: !p.done,
                ..p.clone()
            })
            .collect();
        let left: Vec<&str> = i
            .plan
            .iter()
            .filter(|p| !p.done && !p.left)
            .map(|p| p.step.as_str())
            .collect();
        let effects = self.settled_effects(i)?;
        let mut reason = c.reason.clone();
        if !effects.is_empty() && c.kind != CloseKind::Complete {
            reason = format!("{reason}；已发生：{}", effects.join("；"));
        }
        let closed = crate::util::now();
        let mut w = Self::base(i);
        w.phase = CLOSED;
        w.outcome = Some(outcome.into());
        w.acceptance = acceptance.clone();
        w.closing = None;
        w.closed_at = Some(closed);
        w.closed_reason = Some(reason.clone());
        if outcome == "completed" {}
        let note = (c.kind == CloseKind::Complete && !left.is_empty()).then(|| {
            format!(
                "完成时计划里还有 {} 步没做：{}",
                left.len(),
                left.join("；")
            )
        });
        Ok(ClosedPlan {
            outcome,
            acceptance,
            plan,
            reason,
            closed,
            w,
            note,
        })
    }

    pub(crate) fn after_cycle_closed(
        &mut self,
        i: &Item,
        outcome: &str,
        acceptance: &Option<String>,
        reason: &str,
    ) -> Result<()> {
        self.retire_closed_cycle_children(&i.id)?;
        /* nothing of Steve's automation for the item is left to run */
        self.drop_steve_queued(&i.id)?;
        self.db().timeline_add(
            &i.id,
            "steve",
            "sys",
            &match outcome {
                "completed" => match acceptance.as_deref() {
                    Some("waived") => "事项完成（免确认）".to_string(),
                    Some("automatic") => "事项完成（自动确认）".to_string(),
                    _ => "事项完成".to_string(),
                },
                "failed" => format!("事项失败：{reason}"),
                _ => format!("事项已取消：{reason}"),
            },
            None,
        )?;
        /* observers: after the commit, never deciding anything about this cycle any more */
        match outcome {
            "completed" => {
                flow_quiet(
                    self.db(),
                    &i.id,
                    Update {
                        kind: "done",
                        text: format!("{} 完成：{}", i.id, i.title),
                        ..Default::default()
                    },
                );
                for d in self.db().dependents(&i.id)? {
                    self.check_blocked(&d)?;
                }
                self.hook(crate::modules::Point::Finished, &i.id)?;
            }
            _ => {
                flow_quiet(
                    self.db(),
                    &i.id,
                    Update {
                        kind: "drop",
                        text: format!(
                            "{} {}：{}",
                            i.id,
                            if outcome == "failed" {
                                "失败了"
                            } else {
                                "放弃了"
                            },
                            i.title
                        ),
                        ..Default::default()
                    },
                );
                if let Some(s) = self.active_session(&i.id)? {
                    self.end_and_release(&s.id)?;
                }
                self.tidy_branch(&i.id)?;
                self.hook(crate::modules::Point::Dropped, &i.id)?;
            }
        }
        Ok(())
    }

    pub(crate) fn finish_by_owner(
        &mut self,
        item: &Item,
        expected_cycle: i64,
        expected_revision: i64,
        actor: &str,
    ) -> Result<Item> {
        if item.cycle == expected_cycle
            && item.phase == CLOSED
            && self.db().store().items().owner_finish_recorded(
                &item.id,
                expected_cycle,
                expected_revision,
            )?
        {
            return Ok(item.clone());
        }
        if item.cycle != expected_cycle || item.cas_revision != expected_revision {
            return Err(conflict(steve_i18n::t!(
                "core.item.stale",
                cycle = item.cycle,
                revision = item.cas_revision
            )));
        }
        if item.phase == CLOSED {
            return Err(conflict(closed_msg()));
        }
        let started = std::time::Instant::now();
        let close = CloseRequest {
            kind: CloseKind::Complete,
            reason: "owner_finished".into(),
            policy: Some(ClosePolicy::Owner),
            ..CloseRequest::default()
        };
        self.commit_cycle(item, &close, Some(actor), None)?;
        metrics::histogram!("steve_item_owner_finish_seconds")
            .record(started.elapsed().as_secs_f64());
        self.item(&item.id)
    }

    fn commit_cycle(
        &mut self,
        i: &Item,
        c: &CloseRequest,
        owner: Option<&str>,
        proof: Option<&steve_types::DeliveryProof>,
    ) -> Result<bool> {
        let ClosedPlan {
            outcome,
            acceptance,
            plan,
            reason,
            closed,
            w,
            note,
        } = self.plan_cycle_close(i, c)?;
        let (ws, id) = (i.workspace_id.clone(), i.id.clone());
        let cycle = i.cycle;
        let rev = i.delivery_revision;
        let committed = self.db().store().write(|tx| -> Result<bool> {
            steve_store::require_item_revision_in(tx, &id, cycle, i.cas_revision, i.phase)?;
            if let Some(proof) = proof {
                if !steve_store::item_delivery::record_proof(tx, &ws, proof)? {
                    metrics::counter!("steve_item_stale_delivery_total").increment(1);
                    return Ok(false);
                }
            }
            if owner.is_none()
                && c.kind == CloseKind::Complete
                && steve_store::item_delivery::unsettled_github_writes(tx, &id)?
            {
                return Ok(false);
            }
            if let Some(actor) = owner {
                steve_store::finish_item_dispatch_in(tx, &id, cycle)?;
                tx.audit(
                    &ws,
                    actor,
                    "item.lifecycle.finish",
                    &id,
                    &json!({
                        "cycle": cycle, "revision": i.cas_revision,
                    }),
                )?;
            }
            /* a cycle closes once: a repeated or concurrent commit changes nothing */
            if !steve_store::item_cycle_add_in(
                tx,
                &id,
                cycle,
                rev,
                outcome,
                acceptance.as_deref(),
                Some(&reason),
                closed,
                owner.map(|_| i.cas_revision),
            )? {
                return Ok(false);
            }
            if let Some(n) = &note {
                steve_store::item_repo::append_system_note(tx, &id, n)?;
            }
            let item = steve_store::lifecycle_write_in(tx, &id, &w)?
                .ok_or_else(|| msg(steve_i18n::t!("core.item.not_found")))?;
            if outcome == "completed" {
                steve_store::item_repo::set_closed_plan(tx, &id, plan)?;
            }
            /* nothing productive runs any more: what was still open for this item is moot */
            steve_store::decision_repo::expire_open_for_item(tx, &id, &json!({"closed": outcome}))?;
            steve_store::outbox::append(
                tx,
                &ws,
                ITEM_LIFECYCLE,
                ITEM_LIFECYCLE_V,
                &json!({
                    "item_id": id, "cycle": cycle, "revision": rev, "transition": "closed",
                    "phase": CLOSED, "outcome": outcome, "acceptance": acceptance, "reason": reason,
                }),
            )?;
            if outcome == "completed" {
                let p = ItemDone {
                    item_id: item.id.clone(),
                    title: item.title.clone(),
                    status: "done".into(),
                    bot_id: item.bot_id.clone(),
                    project_id: item.project_id.clone(),
                    source: serde_json::to_value(&item.source)?,
                    cycle,
                    revision: rev,
                    acceptance: acceptance.clone(),
                };
                steve_store::outbox::append(
                    tx,
                    &ws,
                    ITEM_DONE,
                    ITEM_DONE_V,
                    &serde_json::to_value(p)?,
                )?;
            }
            Ok(true)
        })?;
        if !committed {
            return Ok(false);
        }
        if owner.is_none() {
            self.after_cycle_closed(i, outcome, &acceptance, &reason)?;
        }
        Ok(true)
    }

    /// What external effects of the cycle already happened (cancel / fail say so).
    fn settled_effects(&self, i: &Item) -> Result<Vec<String>> {
        Ok(self
            .db()
            .store()
            .items()
            .operations(&i.id, i.cycle)?
            .into_iter()
            .filter_map(|o| match o.status.as_str() {
                "succeeded" => Some(format!("{}（已完成）", o.label)),
                "unknown" => Some(format!("{}（结果未知）", o.label)),
                _ => None,
            })
            .collect())
    }

    /// Checks every item waiting to close (boot after recovery, the periodic tick).
    pub(crate) fn evaluate_closing_cycles(&mut self) -> Result<usize> {
        let mut n = 0;
        // Independent of dynamic workflows and automatic continuation: an upgrade must retire
        // historical child items too, even when their graph/attempt is no longer current.
        for id in self.db().store().items().closed_parents()? {
            self.retire_closed_cycle_children(&id)?;
        }
        let ids = self.db().store().items().closing_ids()?;
        self.st.git_polling = true;
        self.st.evaluating_closing_cycles = true;
        for id in ids {
            match self.evaluate_cycle_close(&id) {
                Ok(true) => n += 1,
                Ok(false) => {}
                Err(e) => tracing::warn!(item = %id, "closing check: {e}"),
            }
        }
        self.st.evaluating_closing_cycles = false;
        self.st.git_polling = false;
        Ok(n)
    }

    fn retire_closed_cycle_children(&mut self, parent: &str) -> Result<()> {
        let p = self.item(parent)?;
        if p.phase != CLOSED || !matches!(p.outcome.as_deref(), Some("completed" | "cancelled")) {
            return Ok(());
        }
        let why = format!(
            "随父事项 {parent} 结束：父事项已{}{}；子事项未完成，停止后归档",
            if p.outcome.as_deref() == Some("completed") {
                "完成"
            } else {
                "放弃"
            },
            p.closed_reason
                .as_deref()
                .filter(|r| !r.is_empty())
                .map(|r| format!("（{r}）"))
                .unwrap_or_default()
        );
        let mut seen = std::collections::HashSet::from([parent.to_string()]);
        let mut pending = vec![parent.to_string()];
        while let Some(id) = pending.pop() {
            for child in self.db().store().items().children(&id)? {
                if !seen.insert(child.id.clone()) {
                    continue;
                }
                // Traverse archived and successful intermediates too; old attempts can hang off
                // them. Do not alter an already archived child or its immutable cycle history.
                pending.push(child.id.clone());
                let inherited = child.outcome.as_deref() == Some("cancelled")
                    && child
                        .closed_reason
                        .as_deref()
                        .is_some_and(|r| r.starts_with("随父事项 "));
                if (child.archived && child.phase == CLOSED)
                    || (child.phase == CLOSED
                        && child.outcome.as_deref() != Some("failed")
                        && !inherited)
                {
                    continue;
                }
                if child.phase != CLOSED {
                    if stopping(&child) {
                        self.evaluate_cycle_close(&child.id)?;
                    } else {
                        self.request_cycle_stop(
                            &child.id,
                            CloseKind::Cancel,
                            "parent",
                            &why,
                            None,
                        )?;
                    }
                }
                if self.item(&child.id)?.phase != CLOSED {
                    continue;
                }
                if self
                    .db()
                    .store()
                    .items()
                    .operations(&child.id, child.cycle)?
                    .iter()
                    .any(|o| matches!(o.status.as_str(), "running" | "unknown"))
                {
                    continue;
                }
                // A historical closed/failed item can still have a leftover execution. Request
                // its stop, but do not hide it until the runtime has confirmed settlement.
                let mut busy = false;
                for s in self.db().sessions_of_item(&child.id)? {
                    if let Some(turn) = self.rt().running(&s.id) {
                        self.st.cancels.push((turn, child.id.clone()));
                    }
                    busy |= self.busy(&s.id)
                        || self.rt().uncertain(&s.id)
                        || matches!(s.status.as_str(), "starting" | "running");
                }
                if busy {
                    continue;
                }
                for s in self.db().sessions_of_item(&child.id)? {
                    for q in self.db().queued(&s.id)? {
                        self.db().store().queued().remove(&s.id, &q.id)?;
                    }
                    if s.status != "ended" {
                        self.end_and_release(&s.id)?;
                    }
                }
                let db = self.db();
                db.store().write::<_, crate::CoreError>(|_w| {
                    if db.item(&child.id)?.is_some_and(|i| !i.archived) {
                        crate::lifecycle::supersede_where(db, &child.id, |_| true, &why)?;
                        db.item_patch(&child.id, crate::patch![archived: true])?;
                        db.timeline_add(&child.id, "steve", "sys", &why, None)?;
                    }
                    Ok(())
                })?;
            }
        }
        Ok(())
    }
}

fn decision_blocker(d: &crate::repo::Decision) -> (&'static str, String) {
    let q = crate::util::js_head(&d.question, 60).to_string();
    match (d.kind.as_str(), d.subject_type.as_str()) {
        ("accept", _) => (
            "acceptance",
            steve_i18n::t!("core.blocker.acceptance", q = q),
        ),
        ("approve", "tool_call") => (
            "permission",
            steve_i18n::t!("core.blocker.permission", q = q),
        ),
        ("approve", _) => ("approval", steve_i18n::t!("core.blocker.approval", q = q)),
        ("ask", _) => ("question", steve_i18n::t!("core.blocker.question", q = q)),
        ("fail", _) => ("failure", steve_i18n::t!("core.blocker.failure", q = q)),
        ("propose", _) => ("approval", steve_i18n::t!("core.blocker.propose", q = q)),
        _ => ("question", q),
    }
}

fn operation_blockers(
    t: &Tx,
    i: &Item,
    push: &mut impl FnMut(&str, String),
    active: &mut i32,
) -> Result<()> {
    for o in t.db().store().items().operations(&i.id, i.cycle)? {
        match o.status.as_str() {
            "running" => *active += 1,
            "unknown" => push(
                "unknown_effect",
                steve_i18n::t!("core.blocker.unknown_effect", label = o.label),
            ),
            "failed" if o.required && o.revision == i.delivery_revision && i.phase == CLOSING => {
                push(
                    "failure",
                    steve_i18n::t!("core.blocker.operation_failed", label = o.label),
                )
            }
            _ => {}
        }
    }
    Ok(())
}

fn blocker_rank(k: &str) -> i32 {
    match k {
        "failure" => 0,
        "unknown_effect" => 1,
        "permission" => 2,
        "acceptance" => 3,
        "approval" => 4,
        "question" => 5,
        "dependency" => 6,
        "paused" => 7,
        "operation" => 8,
        _ => 9,
    }
}

/// The lifecycle object of an item, computed from the item, its decisions,
/// sessions and operations.
pub(crate) fn lifecycle_view(t: &Tx, i: &Item) -> Result<Value> {
    let c = i.closing.as_ref();
    let mut blockers: Vec<Value> = vec![];
    let mut push = |kind: &str, label: String| {
        if !blockers
            .iter()
            .any(|b| b["kind"] == kind && b["label"] == label)
        {
            blockers.push(json!({"kind": kind, "label": label}));
        }
    };
    let mut active = 0;
    /* a required operation already said by name stands for the generic "waiting for operations" */
    /* the projection shows only what the gate waits for, never whether the result moved, so it leaves
     * the worktree alone: fingerprinting it is a `git add --all` per closing item on every look */
    let gate = match &c {
        Some(c) if i.phase == CLOSING => Some(t.delivery_blockers_at(i, c, None)?),
        _ => None,
    };
    if i.phase != CLOSED {
        for d in t.db().decisions_of_item(&i.id)? {
            if d.status != steve_types::DecisionStatus::Open {
                continue;
            }
            let (kind, label) = decision_blocker(&d);
            push(kind, label);
        }
        for s in t.db().sessions_of_item(&i.id)? {
            if s.status != "ended"
                && (t.busy(&s.id) || matches!(s.status.as_str(), "running" | "starting"))
            {
                active += 1;
            }
        }
        operation_blockers(t, i, &mut push, &mut active)?;
        if t.dependency_waiting(&i.id)? || !crate::lifecycle::deps_open(t.db(), &i.id)?.is_empty() {
            push("dependency", steve_i18n::t!("core.blocker.dependency"));
        }
        if i.is_paused() {
            push("paused", steve_i18n::t!("core.blocker.paused"));
        }
        if let Some(g) = &gate {
            if g.turn {
                push("turn", steve_i18n::t!("core.blocker.turn"));
            }
            if g.operation {
                push("operation", steve_i18n::t!("core.blocker.operation"));
            }
        }
    }
    blockers.sort_by_key(|b| blocker_rank(b["kind"].as_str().unwrap_or("")));
    let closing = match (&c, &gate) {
        (Some(c), Some(g)) => json!({
            "kind": c.kind, "reason": c.reason, "requested_by": c.by, "requested_at": c.at,
            "frozen_revision": c.frozen_revision, "accepted": c.accepted, "waiting": g.waiting(),
        }),
        _ => Value::Null,
    };
    let acceptance = match (&c, i.phase) {
        (_, CLOSED) => json!(i.acceptance),
        (Some(c), CLOSING) if c.kind == CloseKind::Complete => json!(c.policy),
        _ => Value::Null,
    };
    Ok(json!({
        "phase": i.phase,
        "cycle": i.cycle,
        "revision": i.cas_revision, "delivery_revision": i.delivery_revision,
        "outcome": if i.phase == CLOSED { json!(i.outcome) } else { Value::Null },
        "acceptance": acceptance,
        "closing": closing,
        "closing_reason": match (&c, i.phase) {
            (Some(c), CLOSING) => json!(c.reason),
            (_, CLOSED) => json!(i.closed_reason),
            _ => Value::Null,
        },
        "closed_at": i.closed_at,
        "blockers": blockers,
        "active_work": active,
    }))
}

/// `true` for an item that is not closed but whose closing intent is a cancel or a failure.
pub(crate) fn stopping(i: &Item) -> bool {
    i.phase == CLOSING
        && i.closing
            .as_ref()
            .is_some_and(|c| c.kind != CloseKind::Complete)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn closed_statuses_and_gate_waits() {
        let g = Gate {
            turn: true,
            acceptance: true,
            ..Default::default()
        };
        assert_eq!(g.waiting(), ["turn", "acceptance"]);
        assert!(!g.clear());
        assert!(Gate::default().clear());
        assert_eq!(fingerprint_head("abc:0011"), "abc");
        assert_eq!(fingerprint(Some("/nonexistent/x")), None);
    }

    #[test]
    fn a_fingerprint_out_of_time_is_unknown_not_changed() {
        let d = tempfile::tempdir().unwrap();
        let cwd = d.path().to_string_lossy().into_owned();
        let git = |args: &[&str]| {
            let o = crate::git::command()
                .arg("-C")
                .arg(&cwd)
                .args(args)
                .env("GIT_AUTHOR_NAME", "t")
                .env("GIT_AUTHOR_EMAIL", "t@example.com")
                .env("GIT_COMMITTER_NAME", "t")
                .env("GIT_COMMITTER_EMAIL", "t@example.com")
                .output()
                .unwrap();
            assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
        };
        git(&["init", "-q"]);
        git(&["config", "commit.gpgsign", "false"]);
        std::fs::write(d.path().join("a.txt"), "a").unwrap();
        git(&["add", "."]);
        git(&["commit", "-q", "-m", "a"]);
        let full = fingerprint_within(Some(&cwd), PRINT_DEADLINE).unwrap();
        assert!(full.is_some());
        // no time at all: git is killed and the answer is unknown, which the gate holds on
        assert_eq!(
            fingerprint_within(Some(&cwd), std::time::Duration::ZERO),
            Err(PrintUnknown)
        );
        // a missing worktree stays a known "no worktree"
        assert_eq!(
            fingerprint_within(Some("/nonexistent/x"), std::time::Duration::ZERO),
            Ok(None)
        );
        // not a git checkout: known as well
        let plain = tempfile::tempdir().unwrap();
        assert_eq!(
            fingerprint_now(Some(&plain.path().to_string_lossy())),
            Ok(None)
        );
    }
}

impl crate::Core {
    /// A proof is usable only by the closing request whose complete manifest was read.
    pub fn delivery_verified(&self, proof: &steve_types::DeliveryProof) -> Result<()> {
        let baseline = &proof.baseline;
        let mut tx = self.tx();
        let item = tx.db().item(baseline.item.as_str())?;
        if !item.is_some_and(|i| i.phase == CLOSING) {
            metrics::counter!("steve_item_stale_delivery_total").increment(1);
            return Ok(());
        }
        tx.try_close(baseline.item.as_str(), Some(proof))?;
        Ok(())
    }
}
