//! The item lifecycle (CLAUDE-LIFECYCLE-DISCUSSION.txt rounds 3–4): the one place
//! that moves an item between its phases.
//!
//! ```text
//!  受理 ─► ready ──开始（第一项实际工作被领取）──► active ──请求完成（冻结 r）──► closing ──提交──► closed
//!             └──────────── 取消 / 失败（停稳后提交）──────────────┘   │  ▲ 返工 / 新输入 / 结果变了 │
//!                                                                     └──┘ （冻结作废，回到 active）
//!  closed ──显式新周期──► ready（cycle + 1；旧周期不改）
//! ```
//!
//! Every step is one store transaction: the item's lifecycle columns, their legacy projection
//! (`status` / `stage` / `now`) and a `work.item.lifecycle` fact in the outbox. A closing request
//! returns at once; the close itself is committed by [`Tx::lc_try_close`] once the gate holds:
//! 1. no productive work of the item runs — a turn in flight on any of its sessions (`is_busy`
//!    covers adapter start, the turn and the gateway-retry wait), a session still `running` /
//!    `starting`, a resume intent, or messages queued for a session;
//! 2. no required operation of this cycle and revision is still open ([`steve_store::ItemOperation`]);
//! 3. (completion) the frozen result is unchanged — same revision, same worktree fingerprint;
//! 4. (completion) the acceptance is decided: human, automatic or waived;
//! 5. (completion) no question or approval raised after the request is still open: an answer may
//!    change what is left to do, so the close is judged again once it was handled (cards older than the
//!    request are the acceptor's to override; the close clears them).
//!
//! The gate is checked again whenever something it waits on may have changed (a turn ended, a
//! decision was answered, an operation reported) and by a periodic tick (queued turns and the release
//! hold skip the turn-end callback). A close commits once per `(item, cycle)` (`item_cycles`).

use std::path::Path;

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use steve_store::{ItemOperation, LifecycleWrite};
use steve_work_api::{ITEM_DONE, ITEM_DONE_V, ITEM_LIFECYCLE, ITEM_LIFECYCLE_V, ItemDone};

use crate::Tx;
use crate::error::{CoreError, Result, msg};
use crate::repo::{Item, PlanStep};
use crate::thread::flowback::{Update, flow_quiet};

pub const READY: &str = "ready";
pub const ACTIVE: &str = "active";
pub const CLOSING: &str = "closing";
pub const CLOSED: &str = "closed";

pub const COMPLETE: &str = "complete";
pub const CANCEL: &str = "cancel";
pub const FAIL: &str = "fail";

/// The user-facing refusal for anything sent to a closed item, in the request's language.
pub fn closed_msg() -> String {
    steve_i18n::t!("core.item.closed")
}

/// Operation kinds the core knows. Modules' wrap-ups (a PR merge) run as `hooks.accepted`.
pub const OP_RELEASE: &str = "release";
pub const OP_HOOKS: &str = "hooks.accepted";
/// A build the completion tag triggered (a GitHub Release): followed on its own; it holds the close
/// only when the item's goal asks for the build to pass (`proof`: then it is required).
pub const OP_BUILD: &str = "release.build";

/// A closing intent while `phase = closing`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Closing {
    /// `complete` | `cancel` | `fail`
    pub kind: String,
    pub reason: String,
    pub by: String,
    pub at: i64,
    pub frozen_revision: i64,
    /// The worktree's `HEAD` and changes when the result was frozen; `None` without a git worktree.
    #[serde(default)]
    pub fingerprint: Option<String>,
    /// The worktree is on a Node and its fingerprint was not there yet when the result was frozen:
    /// it is taken when its facts come (`node_git`), and nothing closes before.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub print_pending: bool,
    /// `human` | `automatic` | `waived` (completion); empty for cancel / fail.
    #[serde(default)]
    pub policy: String,
    pub accepted: bool,
    /// The acceptance decision (human policy).
    #[serde(default)]
    pub decision: Option<String>,
    /// What the acceptance rests on (checks, a module's receipt, who marked it done).
    #[serde(default)]
    pub evidence: Vec<String>,
    /// The termination rule of a failure decided by a rule.
    #[serde(default)]
    pub rule: Option<String>,
}

/// How an acceptance is decided.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Policy {
    Human,
    Automatic,
    Waived,
}

impl Policy {
    pub fn as_str(self) -> &'static str {
        match self {
            Policy::Human => "human",
            Policy::Automatic => "automatic",
            Policy::Waived => "waived",
        }
    }
}

/// A failure card asking you to check an external effect whose result nobody knows.
pub(crate) fn unknown_effect_card(d: &steve_store::Decision) -> bool {
    d.kind == "fail" && d.options.iter().any(|x| x == "release_retry")
}

pub(crate) fn closing_of(i: &Item) -> Option<Closing> {
    i.closing
        .clone()
        .and_then(|v| serde_json::from_value(v).ok())
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
    let cwd = cwd.filter(|c| !c.is_empty() && Path::new(c).exists())?;
    let index = std::env::temp_dir().join(format!("steve-fingerprint-{}", uuid::Uuid::new_v4()));
    let out = fingerprint_with(cwd, &index);
    let _ = std::fs::remove_file(&index);
    out
}

fn fingerprint_with(cwd: &str, index: &Path) -> Option<String> {
    let run = |args: &[&str]| -> Option<String> {
        let o = crate::git::command()
            .arg("-C")
            .arg(cwd)
            .args(args)
            .env("GIT_INDEX_FILE", index)
            .output()
            .ok()?;
        o.status
            .success()
            .then(|| String::from_utf8_lossy(&o.stdout).trim().to_string())
    };
    let head = run(&["rev-parse", "HEAD"])?;
    /* start from the item's index (its stat cache keeps this fast), then take in the worktree */
    let real = crate::git::command()
        .arg("-C")
        .arg(cwd)
        .args(["rev-parse", "--path-format=absolute", "--git-path", "index"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string());
    if let Some(r) = real.filter(|r| Path::new(r).exists()) {
        std::fs::copy(r, index).ok()?;
    } else {
        run(&["read-tree", "HEAD"])?;
    }
    run(&["add", "--all"])?;
    let tree = run(&["write-tree"])?;
    Some(
        match crate::upstream::own_change(Path::new(cwd), &head, &tree) {
            Some((base, patch)) => format!("{head}:{tree}:{base}:{patch}"),
            None => format!("{head}:{tree}"),
        },
    )
}

/// `HEAD` of a fingerprint.
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
fn looks_at_worktree(i: &Item, c: &Closing) -> bool {
    c.kind == COMPLETE && c.fingerprint.is_some() && c.frozen_revision == i.revision
}

/// What blocks a closing commit, for the projection (`closing.waiting`) and the logs.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Gate {
    pub turn: bool,
    pub input: bool,
    pub operation: bool,
    pub acceptance: bool,
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
            || self.changed)
    }
}

/// The legacy `status` of a closed item.
pub(crate) fn closed_status(outcome: &str) -> &'static str {
    match outcome {
        "completed" => "done",
        "failed" => "fail",
        _ => "drop",
    }
}

impl Tx<'_> {
    /* ── writes ── */

    /// One lifecycle step in one store transaction: the columns, the legacy projection, the fact.
    #[allow(clippy::too_many_arguments)]
    fn lc_write(
        &self,
        i: &Item,
        w: LifecycleWrite,
        transition: &str,
        reason: &str,
        extra: impl FnOnce(&steve_store::Tx, &Item) -> Result<()>,
    ) -> Result<Item> {
        let ws = i.workspace_id.clone();
        let id = i.id.clone();
        self.db().store().transaction(|tx| -> Result<Item> {
            let item = steve_store::lifecycle_write_in(tx, &id, &w)?
                .ok_or_else(|| msg(steve_i18n::t!("core.item.not_found")))?;
            steve_store::outbox::emit(
                tx,
                &ws,
                ITEM_LIFECYCLE,
                ITEM_LIFECYCLE_V,
                &json!({
                    "item_id": item.id, "cycle": item.cycle, "revision": item.revision,
                    "transition": transition, "phase": item.phase, "outcome": item.outcome,
                    "acceptance": item.acceptance, "reason": reason,
                }),
            )?;
            extra(tx, &item)?;
            Ok(item)
        })
    }

    fn base(i: &Item) -> LifecycleWrite {
        LifecycleWrite {
            phase: i.phase.clone(),
            cycle: i.cycle,
            revision: i.revision,
            outcome: i.outcome.clone(),
            acceptance: i.acceptance.clone(),
            closing: i.closing.clone(),
            closed_at: i.closed_at,
            closed_reason: i.closed_reason.clone(),
            ..Default::default()
        }
    }

    /// 409 for anything that would act on a closed item (a message, a supplement, a goal change).
    pub(crate) fn lc_guard_open(&self, item_id: &str) -> Result<()> {
        match self.db().item(item_id)? {
            Some(i) if i.phase == CLOSED => Err(conflict(closed_msg())),
            _ => Ok(()),
        }
    }

    /// The first actual work of the cycle was taken up: `ready` → `active` (no-op otherwise).
    pub(crate) fn lc_start(&mut self, item_id: &str) -> Result<()> {
        let i = self.item(item_id)?;
        if i.phase != READY {
            return Ok(());
        }
        let mut w = Self::base(&i);
        w.phase = ACTIVE.into();
        self.lc_write(&i, w, "started", "开始工作", |_, _| Ok(()))?;
        Ok(())
    }

    /// The executor (or the user, or a module) asks to complete the cycle: the result is frozen at a new
    /// revision and the request returns at once. With `accepted`, the acceptance is already decided
    /// (automatic, waived, or the user who asked). Required operations of the project are recorded.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn lc_request_complete(
        &mut self,
        item_id: &str,
        by: &str,
        policy: Policy,
        accepted: bool,
        reason: &str,
        decision: Option<String>,
        evidence: Vec<String>,
    ) -> Result<Item> {
        let i = self.item(item_id)?;
        match i.phase.as_str() {
            CLOSED => return Err(conflict(closed_msg())),
            CLOSING => {
                let c = closing_of(&i).unwrap_or_default();
                if c.kind != COMPLETE {
                    return Err(conflict(steve_i18n::t!("core.item.ending_no_complete")));
                }
                /* a new delivery replaces the one waiting: its acceptance and operations go with it */
                self.lc_invalidate(item_id, "有新的交付，上一次交付作废")?;
            }
            _ => {}
        }
        let i = self.item(item_id)?;
        let rev = i.revision + 1;
        /* on a Node: the facts gathered for this delivery, or taken once they come */
        let (print, print_pending) = if self.git_on_node(&i.id) {
            match self.git_take(&i) {
                Some(Ok(seen)) => (seen.map(|s| s.print), false),
                _ => (None, true),
            }
        } else {
            (fingerprint(i.cwd.as_deref()), false)
        };
        let c = Closing {
            kind: COMPLETE.into(),
            reason: reason.into(),
            by: by.into(),
            at: crate::util::now(),
            frozen_revision: rev,
            fingerprint: print,
            print_pending,
            policy: policy.as_str().into(),
            accepted,
            decision,
            evidence,
            rule: None,
        };
        let mut w = Self::base(&i);
        w.phase = CLOSING.into();
        w.revision = rev;
        w.closing = Some(serde_json::to_value(&c)?);
        w.stage = Some("收尾".into());
        w.now = Some(
            if accepted {
                "正在结束"
            } else {
                "交付了，等你验收"
            }
            .into(),
        );
        let i = self.lc_write(&i, w, "closing_requested", reason, |_, _| Ok(()))?;
        if accepted {
            self.lc_accepted(&i.id)?;
        }
        self.lc_try_close(item_id)?;
        self.item(item_id)
    }

    /// The acceptance of the current closing was decided (`mode`: `human` | `automatic` | `waived`).
    pub(crate) fn lc_accept(
        &mut self,
        item_id: &str,
        mode: Policy,
        evidence: Option<&str>,
    ) -> Result<()> {
        let i = self.item(item_id)?;
        let Some(mut c) = closing_of(&i).filter(|c| i.phase == CLOSING && c.kind == COMPLETE)
        else {
            return Err(conflict(steve_i18n::t!(
                "core.item.not_awaiting_acceptance"
            )));
        };
        if c.accepted {
            return Ok(());
        }
        c.accepted = true;
        c.policy = mode.as_str().into();
        if let Some(e) = evidence {
            c.evidence.push(e.into());
        }
        let mut w = Self::base(&i);
        w.closing = Some(serde_json::to_value(&c)?);
        w.now = Some("已验收，正在结束".into());
        self.lc_write(&i, w, "closing_requested", "验收通过", |_, _| Ok(()))?;
        self.lc_accepted(item_id)?;
        self.lc_try_close(item_id)?;
        Ok(())
    }

    /// An undone acceptance before the close committed: waiting on the user again.
    pub(crate) fn lc_unaccept(&mut self, item_id: &str) -> Result<()> {
        let i = self.item(item_id)?;
        if i.phase == CLOSED {
            return Err(conflict(steve_i18n::t!("core.item.acceptance_undo_closed")));
        }
        let Some(mut c) = closing_of(&i).filter(|c| i.phase == CLOSING && c.kind == COMPLETE)
        else {
            return Ok(());
        };
        c.accepted = false;
        let mut w = Self::base(&i);
        w.closing = Some(serde_json::to_value(&c)?);
        w.now = Some("交付了，等你验收".into());
        /* operations started on the strength of the acceptance stay as they are: their effects happened */
        self.lc_write(&i, w, "closing_requested", "验收被撤销", |_, _| Ok(()))?;
        Ok(())
    }

    /// The acceptance was decided: the operations this result needs before it may close are recorded,
    /// and those that can start, start. Each source of requirements is an adapter: the hooks (a module
    /// may take the close over) and the release of a release project (`release.rs`). Idempotent — every
    /// operation has a stable effect key — so the gate calls it again before each check: a crash between
    /// the accepted closing and its operations is completed before anything can close over it.
    fn lc_accepted(&mut self, item_id: &str) -> Result<()> {
        let i = self.item(item_id)?;
        let Some(c) =
            closing_of(&i).filter(|c| i.phase == CLOSING && c.kind == COMPLETE && c.accepted)
        else {
            return Ok(());
        };
        if self.hooked() {
            let (op, new) = self.lc_op_add(
                &i,
                OP_HOOKS,
                Some(&format!("{}:{}:{}", OP_HOOKS, i.cycle, i.revision)),
                true,
                "pending",
                "收尾模块确认",
                json!({}),
            )?;
            if new {
                self.st.hooks.push(crate::modules::HookEvent::Accepted {
                    item: i.clone(),
                    op: op.id,
                });
            }
        }
        self.release_requirement(&i, &c)?;
        /* what the item's own goal asks to be shown (ST-57): the result running here */
        self.deploy_requirement(&i, &c)?;
        Ok(())
    }

    /// Records an operation of the item's current cycle and revision.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn lc_op_add(
        &self,
        i: &Item,
        kind: &str,
        effect: Option<&str>,
        required: bool,
        status: &str,
        label: &str,
        evidence: Value,
    ) -> Result<(ItemOperation, bool)> {
        let (cycle, rev) = (i.cycle, i.revision);
        Ok(self.db().store().transaction(|tx| {
            steve_store::operation_add_in(
                tx, &i.id, cycle, rev, kind, effect, required, status, label, &evidence,
            )
        })?)
    }

    /// Sets an operation's status; the gate is checked again.
    pub(crate) fn lc_op_set(&mut self, op: &str, status: &str, evidence: Value) -> Result<bool> {
        let changed = self
            .db()
            .store()
            .transaction(|tx| steve_store::operation_set_in(tx, op, status, &evidence))?;
        if changed && let Some(o) = self.db().store().items().operation(op)? {
            self.lc_try_close(&o.item_id)?;
        }
        Ok(changed)
    }

    /// The open required operation of `kind` in the item's current cycle and revision.
    pub(crate) fn lc_open_op(&self, item_id: &str, kind: &str) -> Result<Option<ItemOperation>> {
        let i = self.item(item_id)?;
        Ok(self
            .db()
            .store()
            .items()
            .operations(item_id, i.cycle)?
            .into_iter()
            .filter(|o| o.kind == kind && o.revision == i.revision && o.required)
            .rfind(|o| matches!(o.status.as_str(), "pending" | "running" | "unknown")))
    }

    /// You checked the effects of `kind` whose result nobody knew (all of this cycle, any revision, but
    /// `except`, which the caller settles itself): they become `status` (`succeeded`, `failed` or `waived`).
    /// The gate is checked again.
    pub(crate) fn lc_reconcile(
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
                && self.db().store().transaction(|tx| {
                    steve_store::operation_set_in(tx, &o.id, status, &json!({"checked": note}))
                })?
            {
                n += 1;
            }
        }
        if n > 0 {
            self.lc_try_close(item_id)?;
        }
        Ok(n)
    }

    /// The frozen result is void (rework, new input, a new result): back to `active`, the acceptance
    /// card expires. `false` when nothing was waiting to close.
    pub(crate) fn lc_invalidate(&mut self, item_id: &str, why: &str) -> Result<bool> {
        let i = self.item(item_id)?;
        let Some(c) = closing_of(&i).filter(|c| i.phase == CLOSING && c.kind == COMPLETE) else {
            return Ok(false);
        };
        let mut w = Self::base(&i);
        w.phase = ACTIVE.into();
        w.closing = None;
        w.stage = Some("执行".into());
        w.now = Some(format!("收尾作废：{why}"));
        w.status = Some("run".into());
        self.lc_write(&i, w, "closing_invalidated", why, |_, _| Ok(()))?;
        /* the acceptance asked for this result no longer applies */
        if let Some(d) = c.decision.as_deref()
            && self.db().decision(d)?.is_some_and(|d| d.status == "open")
        {
            self.db()
                .decision_resolve(d, json!({"superseded": why}), "expired", None)?;
        }
        crate::lifecycle::supersede(self.db(), item_id, &["accept"], why)?;
        self.db()
            .timeline_add(item_id, "steve", "sys", &format!("收尾作废：{why}"), None)?;
        Ok(true)
    }

    /// New input (a message, a supplement, a queued message sent now) reaches the item: a closing for
    /// completion is void; a closed item refuses it.
    pub(crate) fn lc_new_input(&mut self, item_id: &str) -> Result<()> {
        let i = self.item(item_id)?;
        match i.phase.as_str() {
            CLOSED => Err(conflict(closed_msg())),
            CLOSING => match closing_of(&i).map(|c| c.kind).as_deref() {
                Some(COMPLETE) => {
                    self.lc_invalidate(item_id, "收到新的消息")?;
                    Ok(())
                }
                _ => Err(conflict(steve_i18n::t!("core.item.ending_no_message"))),
            },
            _ => Ok(()),
        }
    }

    /// Cancel (`kind = cancel`) or fail (`kind = fail`, with a reason; `rule` for a termination rule):
    /// running turns are stopped outside the lock, the close commits once the work stopped.
    pub(crate) fn lc_stop(
        &mut self,
        item_id: &str,
        kind: &str,
        by: &str,
        reason: &str,
        rule: Option<&str>,
    ) -> Result<Item> {
        let i = self.item(item_id)?;
        if i.phase == CLOSED {
            return Err(conflict(closed_msg()));
        }
        if let Some(c) = closing_of(&i).filter(|c| c.kind != COMPLETE) {
            if c.kind == kind {
                return Ok(i);
            }
            return Err(conflict(steve_i18n::t!("core.item.already_ending")));
        }
        let c = Closing {
            kind: kind.into(),
            reason: reason.into(),
            by: by.into(),
            at: crate::util::now(),
            frozen_revision: i.revision,
            fingerprint: None,
            print_pending: false,
            policy: String::new(),
            accepted: true,
            decision: None,
            evidence: vec![],
            rule: rule.map(String::from),
        };
        let mut w = Self::base(&i);
        w.phase = CLOSING.into();
        w.closing = Some(serde_json::to_value(&c)?);
        w.now = Some(
            if kind == CANCEL {
                "正在取消"
            } else {
                "正在标记失败"
            }
            .into(),
        );
        self.lc_write(&i, w, "closing_requested", reason, |_, _| Ok(()))?;
        /* the acceptance or approval cards of the work being stopped no longer apply */
        /* the acceptance, question and failure cards of the work being stopped no longer apply, except a card
         * asking you to check an effect nobody knows: the stop waits on that one */
        crate::lifecycle::supersede_where(
            self.db(),
            item_id,
            |d| matches!(d.kind.as_str(), "accept" | "ask" | "fail") && !unknown_effect_card(d),
            reason,
        )?;
        for s in self.db().sessions_of_item(item_id)? {
            if s.status != "ended"
                && let Some(turn) = self.rt().running(&s.id)
            {
                self.st.cancels.push((turn, item_id.into()));
            }
        }
        self.lc_try_close(item_id)?;
        self.item(item_id)
    }

    /// An explicit new cycle of a closed item: `cycle + 1`, revision 0, `ready`; the cycle before stays
    /// as it closed. With a goal, work starts by the usual rules.
    pub(crate) fn lc_new_cycle(
        &mut self,
        item_id: &str,
        by: &str,
        goal: Option<&str>,
    ) -> Result<Item> {
        let i = self.item(item_id)?;
        if i.phase != CLOSED {
            return Err(conflict(steve_i18n::t!("core.item.new_cycle_needs_closed")));
        }
        let mut w = LifecycleWrite {
            phase: READY.into(),
            cycle: i.cycle + 1,
            revision: 0,
            status: Some("wait".into()),
            stage: Some("定义".into()),
            now: Some("新周期，等你说要做什么".into()),
            ..Default::default()
        };
        if goal.is_some_and(|g| !g.trim().is_empty()) {
            w.now = Some("新周期，准备开始".into());
        }
        let reason = format!("{by} 开始了新周期");
        self.lc_write(&i, w, "cycle_created", &reason, |tx, _| {
            tx.execute("UPDATE items SET archived = 0 WHERE id = ?", [item_id])
                .map_err(steve_store::StoreError::from)?;
            Ok(())
        })?;
        /* the plan was the cycle before's: it is kept as an earlier round, the new cycle starts without one */
        if !i.plan.is_empty() {
            let end = if i.plan.iter().all(|p| p.done) {
                "done"
            } else {
                "replaced"
            };
            self.db()
                .plan_round_add(&i.workspace_id, item_id, &i.plan, end, by)?;
            self.db()
                .item_patch(item_id, crate::patch![plan: Vec::<PlanStep>::new()])?;
        }
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
                self.db().item_patch(
                    item_id,
                    crate::patch![status: "block", now: "等前置事项完成后自动开始"],
                )?;
            } else {
                self.ensure_cwd(item_id)?;
                self.start_or_fail(item_id, Some(g), None)?;
            }
        }
        self.item(item_id)
    }

    /* ── the gate ── */

    /// What still holds the item's closing.
    pub(crate) fn lc_gate(&self, i: &Item, c: &Closing) -> Result<Gate> {
        let now = match looks_at_worktree(i, c) {
            false => None,
            /* on a Node: what it said last, or the worktree left out */
            true if self.git_on_node(&i.id) => self.git_seen(&i.id).map(|s| Some(s.print.clone())),
            true => Some(fingerprint(i.cwd.as_deref())),
        };
        self.lc_gate_at(i, c, now.as_ref().map(Option::as_deref))
    }

    /// [`Self::lc_gate`] with the worktree's fingerprint `now` already taken (`Some(None)`: no worktree);
    /// `None` leaves the worktree out, so `changed` then only sees a new revision.
    fn lc_gate_at(&self, i: &Item, c: &Closing, now: Option<Option<&str>>) -> Result<Gate> {
        let mut g = Gate::default();
        let resume = self.db().store().queued().resume_ids()?;
        for s in self.db().sessions_of_item(&i.id)? {
            if s.status == "ended" {
                continue;
            }
            if self.busy(&s.id)
                || matches!(s.status.as_str(), "running" | "starting")
                || resume.contains(&s.id)
            {
                g.turn = true;
            }
            if c.kind == COMPLETE && !self.db().queued(&s.id)?.is_empty() {
                g.input = true;
            }
        }
        if self.st.releasing.as_ref().is_some_and(|r| r.item == i.id) {
            g.operation = true;
        }
        for o in self.db().store().items().operations(&i.id, i.cycle)? {
            if o.kind == OP_BUILD {
                /* followed on its own, unless the item's goal asks for the build to pass: then this
                 * result's build is one of its requirements (`proof`) */
                if c.kind == COMPLETE
                    && o.required
                    && o.revision == i.revision
                    && !matches!(o.status.as_str(), "succeeded" | "waived")
                {
                    g.operation = true;
                }
                continue;
            }
            /* any effect of this cycle still in flight or of unknown result holds every kind of closing, whatever
             * revision it was for and whether or not it was required: it is settled (or checked by you) first,
             * an unknown effect is never closed over. A completion also needs each of its own requirements done. */
            let open = matches!(o.status.as_str(), "running" | "unknown")
                || (c.kind == COMPLETE
                    && o.required
                    && o.revision == i.revision
                    && !matches!(o.status.as_str(), "succeeded" | "waived"));
            if open {
                g.operation = true;
            }
        }
        if c.kind == COMPLETE {
            if !c.accepted {
                g.acceptance = true;
            }
            /* a permission is part of a running turn; the acceptance card is the acceptance above */
            g.decision = self.db().decisions_of_item(&i.id)?.iter().any(|d| {
                d.status == "open"
                    && d.created > c.at
                    && (d.kind == "ask" || (d.kind == "approve" && d.subject_type != "tool_call"))
            });
            if c.frozen_revision != i.revision
                || now.is_some_and(|n| result_moved(n, c.fingerprint.as_deref()))
            {
                g.changed = true;
            }
        }
        Ok(g)
    }

    /// Commits the closing when nothing holds it any more. `true` when the item is closed now.
    pub(crate) fn lc_try_close(&mut self, item_id: &str) -> Result<bool> {
        let Some(i) = self.db().item(item_id)? else {
            return Ok(false);
        };
        if i.phase != CLOSING {
            return Ok(i.phase == CLOSED);
        }
        let Some(mut c) = closing_of(&i) else {
            return Ok(false);
        };
        /* on a Node the worktree's facts are gathered off the lock: until they are here (or while
         * its Node can't be reached) nothing is decided on them, the closing holds. Only when the
         * gate uses them: a frozen result to compare, one to freeze, an accepted one to release */
        let node = c.kind == COMPLETE
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
        if let Some(p) = &print
            && c.print_pending
            && c.frozen_revision == i.revision
        {
            /* the result frozen before its facts came: they are what was frozen */
            c.fingerprint = p.clone();
            c.print_pending = false;
            let mut w = Self::base(&i);
            w.closing = Some(serde_json::to_value(&c)?);
            self.db()
                .store()
                .transaction(|tx| steve_store::lifecycle_write_in(tx, item_id, &w))?;
            /* those facts were gathered with nothing frozen to compare against or to look for in a
             * release: nothing is decided on them, the next gathering asks with what is frozen now
             * and checks the closing again */
            let i = self.item(item_id)?;
            self.git_gather(&i);
            return Ok(false);
        }
        if c.kind == COMPLETE && c.accepted {
            self.lc_accepted(item_id)?;
            /* the requirements were decided on the same facts as the gate below, or the closing holds
             * until facts for what is asked now are here */
            if node && self.git_take(&self.item(item_id)?).is_none() {
                return Ok(false);
            }
        }
        /* the worktree is fingerprinted once (it runs `git add --all` over all of it), for the gate and
         * for saying what moved */
        let now = match looks_at_worktree(&i, &c) {
            false => None,
            true if node => print.clone(),
            true => Some(fingerprint(i.cwd.as_deref())),
        };
        let g = self.lc_gate_at(&i, &c, now.as_ref().map(Option::as_deref))?;
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
            self.lc_invalidate(item_id, &why)?;
            self.takeover_invalidated(item_id, &why)?;
            return Ok(false);
        }
        if g.input && !g.turn {
            self.lc_invalidate(item_id, "收到新的消息")?;
            return Ok(false);
        }
        if !g.clear() {
            return Ok(false);
        }
        self.lc_commit(&i, &c)
    }

    fn lc_commit(&mut self, i: &Item, c: &Closing) -> Result<bool> {
        let outcome = match c.kind.as_str() {
            COMPLETE => "completed",
            FAIL => "failed",
            _ => "cancelled",
        };
        let acceptance = (c.kind == COMPLETE).then(|| c.policy.clone());
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
        let effects = self.lc_effects(i)?;
        let mut reason = c.reason.clone();
        if !effects.is_empty() && c.kind != COMPLETE {
            reason = format!("{reason}；已发生：{}", effects.join("；"));
        }
        let now_text = match c.kind.as_str() {
            COMPLETE => match c.policy.as_str() {
                "waived" => "已完成（免确认）".to_string(),
                // a module's result (a mail that went out) says what happened
                "automatic" if c.by == "steve" => c.reason.clone(),
                "automatic" => "已自动验收".to_string(),
                _ if c.reason == crate::proof::MARKED => c.reason.clone(),
                _ => "已验收".to_string(),
            },
            FAIL => format!("失败：{}", c.reason),
            _ => c.reason.clone(),
        };
        let closed = crate::util::now();
        let mut w = Self::base(i);
        w.phase = CLOSED.into();
        w.outcome = Some(outcome.into());
        w.acceptance = acceptance.clone();
        w.closing = None;
        w.closed_at = Some(closed);
        w.closed_reason = Some(reason.clone());
        w.status = Some(closed_status(outcome).into());
        if outcome == "completed" {
            w.stage = Some("收尾".into());
        }
        w.now = Some(now_text);
        let (ws, id) = (i.workspace_id.clone(), i.id.clone());
        let note = (c.kind == COMPLETE && !left.is_empty()).then(|| {
            format!(
                "完成时计划里还有 {} 步没做：{}",
                left.len(),
                left.join("；")
            )
        });
        let (cycle, rev) = (i.cycle, i.revision);
        let committed = self.db().store().transaction(|tx| -> Result<bool> {
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
            )? {
                return Ok(false);
            }
            if let Some(n) = &note {
                steve_store::timeline_add_in(tx, &id, "steve", "sys", n, None)?;
            }
            let item = steve_store::lifecycle_write_in(tx, &id, &w)?
                .ok_or_else(|| msg(steve_i18n::t!("core.item.not_found")))?;
            if outcome == "completed" {
                steve_store::item_patch_in(
                    tx,
                    &id,
                    &steve_store::ItemPatch {
                        plan: Some(plan.clone()),
                        ..Default::default()
                    },
                )?;
            }
            /* nothing productive runs any more: what was still open for this item is moot */
            steve_store::decisions_expire_open_in(tx, &id, &json!({"closed": outcome}))?;
            steve_store::outbox::emit(
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
                    status: item.status.clone(),
                    bot_id: item.bot_id.clone(),
                    project_id: item.project_id.clone(),
                    source: serde_json::to_value(&item.source)?,
                    cycle: Some(cycle),
                    revision: Some(rev),
                    acceptance: acceptance.clone(),
                };
                steve_store::outbox::emit(
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
        Ok(true)
    }

    /// What external effects of the cycle already happened (cancel / fail say so).
    fn lc_effects(&self, i: &Item) -> Result<Vec<String>> {
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
    pub(crate) fn lc_tick(&mut self) -> Result<usize> {
        let mut n = 0;
        let ids = self.db().store().items().closing_ids()?;
        self.st.git_polling = true;
        for id in ids {
            match self.lc_try_close(&id) {
                Ok(true) => n += 1,
                Ok(false) => {}
                Err(e) => tracing::warn!(item = %id, "closing check: {e}"),
            }
        }
        self.st.git_polling = false;
        Ok(n)
    }

    /// Boot: hook consultations that were pending when the Hub stopped are asked again.
    pub(crate) fn lc_recover_ops(&mut self) -> Result<()> {
        for id in self.db().store().items().closing_ids()? {
            let Some(op) = self.lc_open_op(&id, OP_HOOKS)? else {
                continue;
            };
            if op.status == "pending" && self.hooked() {
                let item = self.item(&id)?;
                self.st
                    .hooks
                    .push(crate::modules::HookEvent::Accepted { item, op: op.id });
            } else if op.status == "pending" {
                /* the modules that would have been asked are gone: nothing takes the wrap-up over */
                self.lc_op_set(&op.id, "succeeded", json!({"note": "没有收尾模块"}))?;
            }
        }
        Ok(())
    }

    /// The projection of the item's lifecycle.
    pub(crate) fn lc_view(&self, i: &Item) -> Result<Value> {
        lifecycle_view(self, i)
    }
}

/// The lifecycle object of an item, computed from the item, its decisions,
/// sessions and operations.
pub(crate) fn lifecycle_view(t: &Tx, i: &Item) -> Result<Value> {
    let c = closing_of(i);
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
    let mut named = false;
    /* the projection shows only what the gate waits for, never whether the result moved, so it leaves
     * the worktree alone: fingerprinting it is a `git add --all` per closing item on every look */
    let gate = match &c {
        Some(c) if i.phase == CLOSING => Some(t.lc_gate_at(i, c, None)?),
        _ => None,
    };
    if i.phase != CLOSED {
        for d in t.db().decisions_of_item(&i.id)? {
            if d.status != "open" {
                continue;
            }
            let q = crate::util::js_head(&d.question, 60).to_string();
            match (d.kind.as_str(), d.subject_type.as_str()) {
                ("accept", _) => push(
                    "acceptance",
                    steve_i18n::t!("core.blocker.acceptance", q = q),
                ),
                ("approve", "tool_call") => push(
                    "permission",
                    steve_i18n::t!("core.blocker.permission", q = q),
                ),
                ("approve", _) => push("approval", steve_i18n::t!("core.blocker.approval", q = q)),
                ("ask", _) => push("question", steve_i18n::t!("core.blocker.question", q = q)),
                ("fail", _) => push("failure", steve_i18n::t!("core.blocker.failure", q = q)),
                ("propose", _) => push("approval", steve_i18n::t!("core.blocker.propose", q = q)),
                _ => push("question", q),
            }
        }
        for s in t.db().sessions_of_item(&i.id)? {
            if s.status != "ended"
                && (t.busy(&s.id) || matches!(s.status.as_str(), "running" | "starting"))
            {
                active += 1;
            }
        }
        for o in t.db().store().items().operations(&i.id, i.cycle)? {
            /* the evidence the item's goal asks for, said by name (ST-57) */
            if matches!(o.status.as_str(), "pending" | "running")
                && o.required
                && o.revision == i.revision
                && i.phase == CLOSING
                && matches!(o.kind.as_str(), OP_BUILD | crate::proof::OP_DEPLOY)
            {
                let tag = o.evidence["tag"].as_str();
                let label = match (o.kind.as_str(), tag) {
                    (OP_BUILD, t) => {
                        steve_i18n::t!("core.blocker.build", tag = t.unwrap_or_default())
                    }
                    (_, Some(t)) => steve_i18n::t!("core.blocker.deploy", tag = t),
                    (_, None) => steve_i18n::t!("core.blocker.deploy_result"),
                };
                push("operation", label);
                named = true;
            }
            match o.status.as_str() {
                "running" if o.kind == OP_BUILD => {}
                "running" => active += 1,
                "unknown" => push(
                    "unknown_effect",
                    steve_i18n::t!("core.blocker.unknown_effect", label = o.label),
                ),
                "failed" if o.required && o.revision == i.revision && i.phase == CLOSING => push(
                    "failure",
                    steve_i18n::t!("core.blocker.operation_failed", label = o.label),
                ),
                _ => {}
            }
        }
        if i.status == "block" && !crate::lifecycle::deps_open(t.db(), &i.id)?.is_empty() {
            push("dependency", steve_i18n::t!("core.blocker.dependency"));
        }
        if i.status == "pause" {
            push("paused", steve_i18n::t!("core.blocker.paused"));
        }
        if let Some(g) = &gate {
            if g.turn {
                push("turn", steve_i18n::t!("core.blocker.turn"));
            }
            if g.operation && !named {
                push("operation", steve_i18n::t!("core.blocker.operation"));
            }
        }
    }
    let order = |k: &str| match k {
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
    };
    blockers.sort_by_key(|b| order(b["kind"].as_str().unwrap_or("")));
    let closing = match (&c, &gate) {
        (Some(c), Some(g)) => json!({
            "kind": c.kind, "reason": c.reason, "requested_by": c.by, "requested_at": c.at,
            "frozen_revision": c.frozen_revision, "accepted": c.accepted, "waiting": g.waiting(),
        }),
        _ => Value::Null,
    };
    let acceptance = match (&c, i.phase.as_str()) {
        (_, CLOSED) => json!(i.acceptance),
        (Some(c), CLOSING) if c.kind == COMPLETE => json!(c.policy),
        _ => Value::Null,
    };
    Ok(json!({
        "phase": i.phase,
        "cycle": i.cycle,
        "revision": i.revision,
        "outcome": if i.phase == CLOSED { json!(i.outcome) } else { Value::Null },
        "acceptance": acceptance,
        "closing": closing,
        "closing_reason": match (&c, i.phase.as_str()) {
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
    i.phase == CLOSING && closing_of(i).is_some_and(|c| c.kind != COMPLETE)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn closed_statuses_and_gate_waits() {
        assert_eq!(closed_status("completed"), "done");
        assert_eq!(closed_status("failed"), "fail");
        assert_eq!(closed_status("cancelled"), "drop");
        assert_eq!(closed_status("superseded"), "drop");
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
}
