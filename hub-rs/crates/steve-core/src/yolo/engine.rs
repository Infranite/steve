//! The executor: looks at each new or changed decision, judges it
//! with the facts from the database, and answers it the way the user's button would — core decisions
//! through [`Core::resolve`], module decisions through `steve_modules::decisions::dispatch` — with the
//! actor `yolo`. What it did or left alone goes to the ledger.
//!
//! - A verdict with a delay is scheduled (`yolo_actions.outcome = scheduled`, `due_at`); [`Engine::tick`]
//!   runs it when it is due, after checking again that the card is still open, YOLO still applies and
//!   the judgement is the same — otherwise it is cancelled. Scheduled rows live in the database, so a
//!   restart picks them up; each runs once.
//! - Right before answering, the facts are gathered and judged again. An immediate verdict that
//!   turned into “not now” (the user typed meanwhile) is left for the next look, one that turned
//!   into “leave it to you” is recorded so; only a different answer means the executor is confused
//!   and trips breaker B5. Errors before anything was answered trip it when they repeat
//!   ([`B5_ERRORS`] within [`B5_WINDOW_MS`]); a failure answering a card, or recording an answer
//!   already given, trips it at once.
//! - Tool permission requests are answered only while the runner waits for them: the Hub hands the
//!   full tool input over ([`Engine::stash`]) and arms it once the runner registered the request
//!   ([`Engine::arm`]); after a restart the Hub arms again the ones a Node still waits for.
//! - A tool request left to the user is looked at again once full access applies to it (the user
//!   switched while the runner waited): a decline holds at the level it was made, an answer is
//!   given once ([`Engine::kick_waiting_tools`] when the switch or a scope changes). That holds for
//!   one that came while YOLO was off too: a window for new cards only keeps YOLO off the other
//!   cards from before it, not off a request a runner still waits for.
//! - It never runs inside the core's critical section: every call takes the lock itself.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::Value;
use steve_modules::outbound::YOLO_ACTOR;
use steve_modules::{ApiError, ApiResult, Caller};
use steve_store::yolo::{self as repo, ActionRow};

use super::judge::{Policy, Verdict, judge};
use super::ledger::{self, REBASE_REASON, STUCK_REASON};
use super::{WORKSPACE, Yolo};
use crate::Core;
use crate::repo::Decision;

/// Open-card reconciliation and transient scheduled-action retry cadence.
pub const TICK: Duration = Duration::from_secs(15);
/// The workspace setting with the user's [`Policy`] (JSON; missing keys keep their default).
pub const POLICY_KEY: &str = "yolo.policy";
/// B5: this many executor errors before answering anything…
pub const B5_ERRORS: usize = 3;
/// …within this window trip the breaker.
pub const B5_WINDOW_MS: i64 = 10 * 60_000;

/// The untruncated tool call of a permission request (steve-acp `PermissionRequest`).
#[derive(Debug, Clone, PartialEq)]
pub struct ToolInput {
    pub raw: Value,
    /// ACP `toolCall.kind`.
    pub kind: Option<String>,
    /// `toolCall.locations[].path`.
    pub locations: Vec<String>,
    /// The executor's own directory (a worktree or scratch dir it made), as the executor reports
    /// it. `None`: the legacy rule on the item's cwd decides.
    pub own_dir: Option<PathBuf>,
    /// `Some`: the processes the harness started, as its executor reports them, used as they are
    /// (a Node session; the Hub never reads another machine's process table). `None`: read here.
    pub own_pids: Option<Vec<u32>>,
}

#[derive(Default)]
struct Tools {
    /// Handed over, the runner not yet waiting.
    stashed: HashMap<String, ToolInput>,
    /// The runner waits for an answer.
    armed: HashMap<String, ToolInput>,
}

struct Inner {
    core: Core,
    yolo: Yolo,
    tools: Mutex<Tools>,
    /// The model's answer per question card, asked once.
    answers: Mutex<HashMap<String, super::judge::Answer>>,
    /// The channel recall for `recall` undos (set by the Hub).
    recall: std::sync::OnceLock<Arc<dyn super::undo::Recall>>,
    /// Decisions being looked at right now (an event and the tick may arrive together).
    inflight: Mutex<HashSet<String>>,
    /// When the executor failed before answering, per workspace (B5; not kept over a restart).
    errors: Mutex<HashMap<String, Vec<i64>>>,
}

/// The executor. Cheap to clone.
#[derive(Clone)]
pub struct Engine(Arc<Inner>);

/// Releases the in-flight mark of a decision.
struct Claim<'a>(&'a Engine, String);

impl Drop for Claim<'_> {
    fn drop(&mut self) {
        lock(&self.0.0.inflight).remove(&self.1);
        // Claim ownership lives only in memory; there is no database commit to observe.
        self.0.yolo().0.due_wake.notify_one();
    }
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

fn ce(e: crate::CoreError) -> ApiError {
    match e {
        crate::CoreError::Module(e) => e,
        e => ApiError::Internal(e.to_string()),
    }
}

/// Someone (the user) answered first. The refusal is in the request's language (`core.decision.handled`).
fn already_handled(e: &ApiError) -> bool {
    let s = e.to_string();
    s.contains("已经处理过了") || s.contains("already been handled")
}

/// What a tool request judged under full access records as `checks.level`.
pub(crate) const FULL_LEVEL: &str = "full";

/// The action was judged under full access (a decline then does not hold for ordinary YOLO's).
fn full_judged(a: &ActionRow) -> bool {
    a.checks.get("level").and_then(Value::as_str) == Some(FULL_LEVEL)
}

/// Same answer: same choice by the same rule.
fn same(a: &Verdict, b: &Verdict) -> bool {
    match (a, b) {
        (
            Verdict::Auto {
                choice: c1,
                rule: r1,
                ..
            },
            Verdict::Auto {
                choice: c2,
                rule: r2,
                ..
            },
        ) => c1 == c2 && r1 == r2,
        _ => false,
    }
}

impl Engine {
    pub fn new(core: Core, yolo: Yolo) -> Engine {
        Engine(Arc::new(Inner {
            core,
            yolo,
            tools: Mutex::new(Tools::default()),
            answers: Mutex::new(HashMap::new()),
            recall: std::sync::OnceLock::new(),
            inflight: Mutex::new(HashSet::new()),
            errors: Mutex::new(HashMap::new()),
        }))
    }

    pub fn core(&self) -> &Core {
        &self.0.core
    }

    pub fn yolo(&self) -> &Yolo {
        &self.0.yolo
    }

    /// The full input of permission request `decision` (call before the runner waits for it).
    pub fn stash(&self, decision: &str, input: ToolInput) {
        lock(&self.0.tools).stashed.insert(decision.into(), input);
    }

    /// The runner now waits for `decision`: from here on it may be answered.
    pub fn arm(&self, decision: &str) {
        let mut t = lock(&self.0.tools);
        if let Some(i) = t.stashed.remove(decision) {
            t.armed.insert(decision.into(), i);
        }
    }

    pub(crate) fn tool_input(&self, decision: &str) -> Option<ToolInput> {
        lock(&self.0.tools).armed.get(decision).cloned()
    }

    fn forget_tool(&self, decision: &str) {
        let mut t = lock(&self.0.tools);
        t.stashed.remove(decision);
        t.armed.remove(decision);
        drop(t);
        lock(&self.0.answers).remove(decision);
    }

    pub(crate) fn recall_slot(&self) -> &std::sync::OnceLock<Arc<dyn super::undo::Recall>> {
        &self.0.recall
    }

    pub(crate) fn cached_answer(&self, key: &str) -> Option<super::judge::Answer> {
        lock(&self.0.answers).get(key).cloned()
    }

    pub(crate) fn cache_answer(&self, key: &str, a: super::judge::Answer) {
        lock(&self.0.answers).insert(key.into(), a);
    }

    fn claim(&self, id: &str) -> Option<Claim<'_>> {
        lock(&self.0.inflight)
            .insert(id.to_string())
            .then(|| Claim(self, id.to_string()))
    }

    /// The user's policy of `ws` (setting [`POLICY_KEY`]); unreadable → the defaults.
    pub fn policy(&self, ws: &str) -> Policy {
        let v: Option<Value> = self
            .core()
            .store()
            .store()
            .settings()
            .get(ws, POLICY_KEY, None)
            .unwrap_or(None);
        match v {
            Some(Value::String(s)) => serde_json::from_str(&s).unwrap_or_default(),
            Some(v @ Value::Object(_)) => serde_json::from_value(v).unwrap_or_default(),
            _ => Policy::default(),
        }
    }

    /// [`Engine::consider`] for a background caller: errors are logged and trip B5 when they repeat.
    pub fn kick(&self, id: &str) -> Option<ActionRow> {
        let store = self.core().store().store();
        let _permit = store.try_activity("yolo")?;
        store.admitted_sync(|| match self.consider(id) {
            Ok(a) => a,
            Err(e) => {
                self.fail_hard(id, &e);
                None
            }
        })
    }

    pub(super) fn fail_hard(&self, id: &str, e: &ApiError) {
        let ws = self
            .core()
            .store()
            .decision(id)
            .ok()
            .flatten()
            .map(|d| d.workspace_id);
        let Some(ws) = ws else {
            tracing::error!("yolo {id}: {e}");
            return;
        };
        self.fail_workspace(&ws, id, e);
    }

    fn fail_workspace(&self, ws: &str, id: &str, e: &ApiError) {
        tracing::error!("yolo executor in {ws} on {id}: {e}");
        let now = self.yolo().now();
        let n = {
            let mut errs = lock(&self.0.errors);
            let xs = errs.entry(ws.to_owned()).or_default();
            xs.retain(|t| now - *t < B5_WINDOW_MS);
            xs.push(now);
            xs.len()
        };
        if n < B5_ERRORS {
            return;
        }
        lock(&self.0.errors).remove(ws);
        if let Err(e2) = self.yolo().executor_failed(ws, &e.to_string()) {
            tracing::error!("yolo trip after {e}: {e2}");
        }
    }

    /// Records `v` (a decline) for `d`: the card is left to the user, and a stuck item leaves YOLO.
    /// `full`: a tool request judged under full access ([`full_judged`]).
    fn leave(
        &self,
        d: &Decision,
        window: Option<&str>,
        v: &Verdict,
        progress: &Value,
        full: bool,
    ) -> ApiResult<ActionRow> {
        let Verdict::Decline { rule, reason } = v else {
            return Err(ApiError::Internal("not a decline".into()));
        };
        let mut row = ledger::new_action(d, window, v, "declined", None, progress);
        if full {
            row.checks = serde_json::json!({ "level": FULL_LEVEL });
        }
        let a = self.yolo().record(&row)?;
        let item_out = match rule.as_str() {
            "continue.stuck" | "accept.stuck" => Some(STUCK_REASON),
            "release.rebase" => Some(REBASE_REASON),
            _ => None,
        };
        if let (Some(why), Some(item)) = (item_out, d.item_id.as_deref()) {
            let title = self
                .core()
                .store()
                .item(item)
                .map_err(ce)?
                .map(|i| i.title)
                .unwrap_or_default();
            self.yolo().item_off(&d.workspace_id, item, &title, why)?;
        }
        tracing::info!(decision = d.id, rule, reason, "yolo leaves it to you");
        Ok(a)
    }

    /// Looks at decision `id` once: answers it, schedules it, leaves it to the user (recorded once),
    /// or does nothing (out of scope, waiting for the user, already handled). Returns the action
    /// written, if any.
    pub fn consider(&self, id: &str) -> ApiResult<Option<ActionRow>> {
        let Some(_claim) = self.claim(id) else {
            return Ok(None);
        };
        let Some(d) = self.core().store().decision(id).map_err(ce)? else {
            self.forget_tool(id);
            return Ok(None);
        };
        let prior = self
            .yolo()
            .store()
            .read(|c| repo::actions_of_decision(c, id))?;
        if d.status != "open" {
            // the user (or something else) answered first: what was scheduled is moot
            for a in prior.iter().filter(|a| a.outcome == "scheduled") {
                self.yolo()
                    .finish(&a.id, "cancelled", Some("你已经处理了这张卡"))?;
            }
            self.forget_tool(id);
            return Ok(None);
        }
        // Startup blockers are explicitly human-owned, even at full YOLO. An old queued
        // protocol is not authorization to keep restarting a harness that cannot open.
        if d.subject_type == steve_store::SESSION_START_FAILURE {
            return Ok(None);
        }
        // a release YOLO approved went wrong (B1): looked at as its failure card comes up
        if d.kind == "fail" && self.release_breaker(&d)? {
            return Ok(None);
        }
        // open again after YOLO answered it: the user undid it (B3), and it stays theirs
        if let Some(a) = prior.iter().rev().find(|a| a.outcome == "decided") {
            if a.undone.is_none() {
                self.yolo().undone(&d.workspace_id, &a.id)?;
            }
            return Ok(None);
        }
        // a tool request left to the user under ordinary YOLO is judged again once full access
        // applies to it (the user switched while it waited): a decline holds at the level it was made
        let full_now = d.subject_type == "tool_call" && self.full_access(&d);
        if prior.iter().any(|a| {
            matches!(a.outcome.as_str(), "scheduled" | "muted" | "failed")
                || a.outcome == "declined" && (!full_now || full_judged(a))
                || a.outcome == "cancelled" && a.reason == super::undo::USER_CANCEL
        }) {
            return Ok(None);
        }
        if d.subject_type == "tool_call" && self.tool_input(id).is_none() {
            return Ok(None);
        }
        let eff = self.yolo().effective(&d.workspace_id, d.item_id.as_deref());
        let eff = match eff {
            Ok(e) => e,
            Err(ApiError::NotFound(_)) => return Ok(None),
            Err(e) => return Err(e),
        };
        if !eff.on {
            return Ok(None);
        }
        let window = self
            .yolo()
            .store()
            .read(|c| repo::window_current(c, &d.workspace_id))?;
        // a tool request a runner still waits for (armed above) belongs to the turn full access now
        // applies to, however old: the window's start only keeps YOLO off other cards left from before
        if let Some(w) = &window
            && !w.include_open
            && d.created < w.started
            && !full_now
        {
            return Ok(None);
        }
        if d.kind == "fail" && self.yolo().check_systemic(&d.workspace_id)? {
            return Ok(None);
        }
        let policy = self.policy(&d.workspace_id);
        let g = self.gather(&d, &policy)?;
        let v = judge(&g.facts, &policy, self.yolo().env());
        let window = window.map(|w| w.id);
        match &v {
            Verdict::Off | Verdict::Wait { .. } => Ok(None),
            Verdict::Decline { .. } => self
                .leave(&d, window.as_deref(), &v, &g.progress, g.facts.full_tool())
                .map(Some),
            Verdict::Mute { .. } => Ok(Some(self.yolo().record(&ledger::new_action(
                &d,
                window.as_deref(),
                &v,
                "muted",
                None,
                &g.progress,
            ))?)),
            Verdict::Auto { delay_s, .. } if *delay_s > 0 => {
                let due = self.yolo().now() + *delay_s as i64 * 1000;
                Ok(Some(self.yolo().record(&ledger::new_action(
                    &d,
                    window.as_deref(),
                    &v,
                    "scheduled",
                    Some(due),
                    &g.progress,
                ))?))
            }
            Verdict::Auto { .. } => self.execute(&d, &v, window.as_deref(), None),
        }
    }

    /// Answers `d` as judged in `v` (`scheduled`: the row written when it was scheduled). Judges
    /// again first; returns the settled action (none: an immediate answer that is “not now” after all).
    pub(super) fn execute(
        &self,
        d: &Decision,
        v: &Verdict,
        window: Option<&str>,
        scheduled: Option<ActionRow>,
    ) -> ApiResult<Option<ActionRow>> {
        let policy = self.policy(&d.workspace_id);
        let again = self.gather(d, &policy)?;
        let v2 = judge(&again.facts, &policy, self.yolo().env());
        if !same(v, &v2) {
            // “not yet” (a release running, the user around) keeps the answer scheduled
            if let (
                Some(a),
                Verdict::Wait {
                    delay_s, reason, ..
                },
            ) = (&scheduled, &v2)
            {
                let due = self.yolo().now() + (*delay_s).max(1) as i64 * 1000;
                self.yolo().postpone(&a.id, due, reason)?;
                return self.reread(&a.id).map(Some);
            }
            if let Some(a) = scheduled {
                let why = match &v2 {
                    Verdict::Decline { reason, .. } | Verdict::Wait { reason, .. } => {
                        format!("执行前复查结果变了：{reason}")
                    }
                    _ => "执行前复查结果变了".to_string(),
                };
                self.yolo().finish(&a.id, "cancelled", Some(&why))?;
                return self.reread(&a.id).map(Some);
            }
            match &v2 {
                // not now (the user typed meanwhile, YOLO went off): the next look judges it again
                Verdict::Wait { .. } | Verdict::Off => return Ok(None),
                // left to you after all: recorded like any decline
                Verdict::Decline { .. } => {
                    return self
                        .leave(d, window, &v2, &again.progress, again.facts.full_tool())
                        .map(Some);
                }
                // the same answer by another rule: answered as judged now
                Verdict::Auto { choice, .. } if matches!(v, Verdict::Auto { choice: c, .. } if c == choice) =>
                    {}
                _ => {
                    let msg = format!(
                        "「{}」判定为 {}，执行前复查为 {}",
                        d.question,
                        v.rule(),
                        v2.rule()
                    );
                    self.yolo().executor_failed(&d.workspace_id, &msg)?;
                    return Ok(None);
                }
            }
        }
        let Verdict::Auto { choice, text, .. } = &v2 else {
            return Err(ApiError::Internal("not an answer".into()));
        };
        let a = match scheduled {
            Some(a) => a,
            None => {
                let now = self.yolo().now();
                self.yolo().record(&ledger::new_action(
                    d,
                    window,
                    &v2,
                    "scheduled",
                    Some(now),
                    &again.progress,
                ))?
            }
        };
        match self.answer(d, choice, text.as_deref().unwrap_or("")) {
            /* the card was answered: a failure recording it trips B5 at once, as a failure
            answering it does (only errors before anything was answered wait for a repeat) */
            Ok(after) => {
                let settled = if after.status == "expired" {
                    // the handler closed the card instead (e.g. you already answered the chat yourself)
                    let why = after.result["superseded"]
                        .as_str()
                        .unwrap_or("权限请求已经失效");
                    self.yolo().finish(&a.id, "cancelled", Some(why)).map(drop)
                } else {
                    self.record_answer(&after, &a)
                        .and_then(|()| self.yolo().finish(&a.id, "decided", None).map(drop))
                };
                if let Err(e) = settled {
                    self.forget_tool(&d.id);
                    self.yolo().executor_failed(
                        &d.workspace_id,
                        &format!("「{}」已经回答，记录时出错：{e}", d.question),
                    )?;
                    /* tripped here: not counted again as an error before answering */
                    return self.reread(&a.id).map(Some);
                }
            }
            Err(e) if already_handled(&e) => {
                self.yolo()
                    .finish(&a.id, "cancelled", Some("你已经处理了这张卡"))?;
            }
            Err(e) => {
                self.yolo()
                    .finish(&a.id, "failed", Some(&format!("执行失败：{e}")))?;
                self.yolo()
                    .executor_failed(&d.workspace_id, &format!("「{}」：{e}", d.question))?;
            }
        }
        self.forget_tool(&d.id);
        self.reread(&a.id).map(Some)
    }

    fn reread(&self, action: &str) -> ApiResult<ActionRow> {
        self.yolo()
            .store()
            .read(|c| repo::action_get(c, action))?
            .ok_or_else(|| ApiError::not_found("记录"))
    }

    /// Stamp only YOLO metadata, never the result snapshot returned before a background release
    /// could finish. The Core lock also serializes this with release_outcome's read/modify/write.
    pub(super) fn record_answer(&self, after: &Decision, action: &ActionRow) -> ApiResult<()> {
        let tx = self.core().tx();
        tx.db()
            .store()
            .decisions()
            .merge_result(
                &after.id,
                &ledger::result_with_yolo(&serde_json::json!({}), action),
            )
            .map_err(|e| ApiError::Internal(e.to_string()))?;
        Ok(())
    }

    /// Answers the way the user's button does, as `yolo`.
    fn answer(&self, d: &Decision, choice: &str, text: &str) -> ApiResult<Decision> {
        if let Some(m) = self.core().modules() {
            let caller = Caller {
                ws: d.workspace_id.clone(),
                me: YOLO_ACTOR.into(),
            };
            if let Some(r) = steve_modules::decisions::dispatch(m, &caller, &d.id, choice, text) {
                return r;
            }
        }
        self.core()
            .resolve(&d.id, choice, text, YOLO_ACTOR)
            .map_err(ce)
    }

    /// Runs one scheduled action that is due (`now ≥ due_at`).
    pub(crate) fn run_due_row(&self, a: ActionRow) -> ApiResult<Option<ActionRow>> {
        let Some(id) = a.decision_id.clone() else {
            self.yolo()
                .finish(&a.id, "cancelled", Some("没有对应的决定"))?;
            return Ok(None);
        };
        let Some(_claim) = self.claim(&id) else {
            return Ok(None);
        };
        let d = self.core().store().decision(&id).map_err(ce)?;
        let Some(d) = d.filter(|d| d.status == "open") else {
            self.yolo()
                .finish(&a.id, "cancelled", Some("你已经处理了这张卡"))?;
            return Ok(Some(self.reread(&a.id)?));
        };
        let on = self
            .yolo()
            .effective(&d.workspace_id, d.item_id.as_deref())
            .map(|e| e.on)
            .unwrap_or(false);
        if !on {
            self.yolo()
                .finish(&a.id, "cancelled", Some("YOLO 已经不再处理这件"))?;
            return Ok(Some(self.reread(&a.id)?));
        }
        let v = Verdict::Auto {
            choice: a.choice.clone().unwrap_or_default(),
            text: a.text.clone(),
            delay_s: 0,
            rule: a.rule.clone(),
            reason: a.reason.clone(),
            undo: None,
            checks: a.checks.clone(),
        };
        let window = a.window_id.clone();
        self.execute(&d, &v, window.as_deref(), Some(a))
    }

    /// One pass: scheduled actions that are due, then every open card where YOLO may apply (cards
    /// that waited for the user, and anything an event missed). Returns the actions written.
    pub fn tick(&self) -> Vec<ActionRow> {
        let store = self.core().store().store();
        let Some(_permit) = store.try_activity("yolo_tick") else {
            return vec![];
        };
        store.admitted_sync(|| self.tick_admitted())
    }

    fn tick_admitted(&self) -> Vec<ActionRow> {
        let mut out = Vec::new();
        let now = self.yolo().now();
        loop {
            let (rows, error, selected) = self.scheduled_pass(now, DUE_BATCH);
            out.extend(rows);
            if error.is_some() || selected < DUE_BATCH {
                break;
            }
        }
        out.extend(self.waiting_admitted());
        out
    }

    /// The sole eligibility query for deadlines, batches and an unsettled row's retry.
    /// Optional filters only narrow this set; every filter is applied before LIMIT.
    fn scheduled_rows(
        &self,
        through: Option<i64>,
        only: Option<&str>,
        limit: usize,
    ) -> steve_store::Result<Vec<ActionRow>> {
        let claimed: Vec<String> = lock(&self.0.inflight).iter().cloned().collect();
        self.yolo()
            .store()
            .read(|r| repo::scheduled_actions(r, &claimed, through, only, limit as u64))
    }

    fn scheduled_pass(
        &self,
        now: i64,
        limit: usize,
    ) -> (Vec<ActionRow>, Option<steve_runtime::due::DueError>, usize) {
        let mut out = Vec::new();
        let due = self.scheduled_rows(Some(now), None, limit);
        let due = match due {
            Ok(rows) => rows,
            Err(e) => {
                tracing::error!("yolo due actions: {e}");
                return (out, Some(steve_runtime::due::DueError(e.to_string())), 0);
            }
        };
        let selected = due.len();
        for a in due {
            let row = a.id.clone();
            let id = a.decision_id.clone().unwrap_or_default();
            let ws = a.workspace_id.clone();
            match self.run_due_row(a) {
                Ok(Some(a)) => out.push(a),
                Ok(None) => {}
                Err(e) => self.fail_workspace(&ws, &id, &e),
            }
            // Recheck through the same query: settled, already-postponed or claimed actions
            // are not retried. Preserve the current business reason visible in the ledger.
            let retry = match self.scheduled_rows(Some(now), Some(&row), 1) {
                Ok(rows) => rows.into_iter().next(),
                Err(e) => {
                    return (
                        out,
                        Some(steve_runtime::due::DueError(e.to_string())),
                        selected,
                    );
                }
            };
            if let Some(a) = retry
                && let Err(e) = self.yolo().postpone(
                    &row,
                    now.saturating_add(TICK.as_millis() as i64),
                    &a.reason,
                )
            {
                return (
                    out,
                    Some(steve_runtime::due::DueError(e.to_string())),
                    selected,
                );
            }
        }
        (out, None, selected)
    }

    fn tick_waiting(&self) -> Vec<ActionRow> {
        let store = self.core().store().store();
        let Some(_permit) = store.try_activity("yolo_tick") else {
            return vec![];
        };
        store.admitted_sync(|| self.waiting_admitted())
    }

    fn waiting_admitted(&self) -> Vec<ActionRow> {
        let mut out = Vec::new();
        if self.yolo().forced().is_some() {
            return out;
        }
        let wss = match self.core().store().workspace_ids() {
            Ok(w) => w,
            Err(e) => {
                tracing::error!("yolo tick: {e}");
                return out;
            }
        };
        for ws in wss {
            // anything an event missed (a scope written by a draft, an item taken out)
            self.core().yolo_sync(&ws);
            let scopes = self
                .yolo()
                .store()
                .read(|c| repo::scopes(c, &ws))
                .unwrap_or_default();
            let paused = scopes
                .iter()
                .any(|s| s.object_type == WORKSPACE && s.paused.is_some());
            if paused || !scopes.iter().any(|s| s.mode == "on" || s.mode == "full") {
                continue;
            }
            let open = self.core().store().decisions_open(&ws).unwrap_or_default();
            for d in open {
                if let Some(a) = self.kick(&d.id) {
                    out.push(a);
                }
            }
        }
        out
    }

    /// Looks again at every tool request of `ws` still waiting for an answer (after the switch or a
    /// scope changed: full access answers what ordinary YOLO left to the user). Each is claimed
    /// and answered at most once ([`Engine::consider`]).
    pub fn kick_waiting_tools(&self, ws: &str) -> Vec<ActionRow> {
        let open = self.core().store().decisions_open(ws).unwrap_or_default();
        open.iter()
            .filter(|d| d.subject_type == "tool_call")
            .filter_map(|d| self.kick(&d.id))
            .collect()
    }

    /// Follows stored action deadlines; open-card reconciliation retains [`TICK`].
    pub async fn run_ticks(self) {
        let scheduled = steve_runtime::due::due_loop(
            Arc::new(Scheduled(self.clone())),
            self.yolo().0.clock.clone(),
            self.yolo().0.due_wake.clone(),
            steve_runtime::policy::Backoff {
                base: TICK,
                max: TICK,
                factor: 1,
                jitter: steve_runtime::policy::Jitter::None,
                attempts: None,
                reset_after: None,
            },
        );
        // Reconciliation is idempotent; missed scans are intentionally not replayed.
        let waiting = steve_runtime::every(
            "yolo.open_cards",
            self.yolo().0.clock.clone(),
            TICK,
            steve_runtime::FirstRun::Immediate,
            move || {
                let e = self.clone();
                async move {
                    if let Err(err) = tokio::task::spawn_blocking(move || e.tick_waiting()).await {
                        tracing::error!("yolo tick task: {err}");
                    }
                }
            },
        );
        tokio::join!(scheduled, waiting);
    }

    /// Looks at every decision a `{type:"decision", id}` event names. Runs until the sender is gone.
    pub async fn run_events(self, mut rx: tokio::sync::mpsc::Receiver<steve_types::Notification>) {
        while let Some(v) = rx.recv().await {
            let id = match v.event.as_domain() {
                Some(steve_types::DomainEvent::Autonomy(steve_types::AutonomyEvent::Yolo {
                    ws,
                })) => {
                    let ws = ws.to_string();
                    let e = self.clone();
                    tokio::task::spawn_blocking(move || {
                        e.core().yolo_sync(&ws);
                        e.kick_waiting_tools(&ws);
                    });
                    continue;
                }
                Some(steve_types::DomainEvent::Work(steve_types::WorkEvent::Decision {
                    id,
                    ..
                })) => id.to_string(),
                _ => continue,
            };
            // not awaited: a question waits up to a minute for the model, a tool call must not wait
            // behind it (each decision is claimed, so two looks at one never overlap)
            let e = self.clone();
            tokio::task::spawn_blocking(move || e.kick(&id));
        }
    }
}

/// Persisted scheduled actions; every read shares Engine::scheduled_rows eligibility.
struct Scheduled(Engine);
const DUE_BATCH: usize = 64;
impl steve_runtime::due::DueSource for Scheduled {
    fn name(&self) -> &'static str {
        "yolo.scheduled"
    }
    async fn next_due(&self) -> Result<Option<i64>, steve_runtime::due::DueError> {
        let e = self.0.clone();
        tokio::task::spawn_blocking(move || {
            let store = e.core().store().store();
            let Some(_permit) = store.try_activity("yolo_tick") else {
                return Ok(Some(e.yolo().now().saturating_add(TICK.as_millis() as i64)));
            };
            store.admitted_sync(|| {
                e.scheduled_rows(None, None, 1)
                    .map(|rows| rows.first().and_then(|r| r.due_at))
                    .map_err(|e| steve_runtime::due::DueError(e.to_string()))
            })
        })
        .await
        .map_err(|e| steve_runtime::due::DueError(e.to_string()))?
    }
    async fn run_due(&self, now: i64) -> Result<(), steve_runtime::due::DueError> {
        let e = self.0.clone();
        tokio::task::spawn_blocking(move || {
            let store = e.core().store().store();
            let Some(_permit) = store.try_activity("yolo_tick") else {
                return Ok(());
            };
            store.admitted_sync(|| {
                let (_, error, _) = e.scheduled_pass(now, DUE_BATCH);
                error.map_or(Ok(()), Err)
            })
        })
        .await
        .map_err(|e| steve_runtime::due::DueError(e.to_string()))?
    }
}

#[cfg(test)]
mod due_contract_tests {
    use super::*;
    use crate::repo::WS;
    use crate::testkit::Kit;
    use steve_runtime::Clock;
    use steve_runtime::due::{DueSource, contract};

    struct Fixture {
        _kit: Kit,
        source: Scheduled,
        clock: Arc<steve_runtime::FakeClock>,
        poison: Mutex<HashSet<String>>,
    }
    impl Fixture {
        fn new() -> Self {
            let kit = Kit::new();
            let clock = Arc::new(steve_runtime::FakeClock::new(1000));
            let yolo = Yolo::with_clock(kit.db().clone(), None, kit.home(), clock.clone());
            let source = Scheduled(Engine::new(kit.core.clone(), yolo));
            Self {
                _kit: kit,
                source,
                clock,
                poison: Mutex::default(),
            }
        }
        fn add_in(&self, ws: &str, at: i64, poison: bool) -> String {
            let yolo = self.source.0.yolo();
            if yolo
                .store()
                .read(|c| repo::window_current(c, ws))
                .unwrap()
                .is_none()
            {
                yolo.put(
                    ws,
                    "u1",
                    super::super::Switch {
                        on: true,
                        full: false,
                        preview: false,
                        include_open: false,
                    },
                )
                .unwrap();
            }
            let window = yolo
                .store()
                .read(|c| repo::window_current(c, ws))
                .unwrap()
                .unwrap();
            let decision_id = if poison {
                let d = self
                    .source
                    .0
                    .core()
                    .store()
                    .decision_create(crate::repo::NewDecision {
                        workspace_id: ws.into(),
                        kind: "approve".into(),
                        subject_type: "release".into(),
                        subject_id: "test".into(),
                        question: "test".into(),
                        ..Default::default()
                    })
                    .unwrap();
                // Corrupt a real decision: decoding really fails before execution. B5 must use
                // the action's workspace even when the decision cannot be decoded.
                self.source
                    .0
                    .yolo()
                    .store()
                    .with_conn(|c| {
                        Ok(c.execute("UPDATE decisions SET options=x'80' WHERE id=?", [&d.id])?)
                    })
                    .unwrap();
                assert!(self.source.0.core().store().decision(&d.id).is_err());
                Some(d.id)
            } else {
                Some("already-handled".into())
            };
            let a = self
                .source
                .0
                .yolo()
                .record(&repo::NewAction {
                    workspace_id: ws.into(),
                    window_id: Some(window.id),
                    kind: "approve".into(),
                    subject_type: "release".into(),
                    rule: "release.self".into(),
                    reason: "原始裁决理由".into(),
                    outcome: "scheduled".into(),
                    due_at: Some(at),
                    decision_id,
                    ..Default::default()
                })
                .unwrap();
            if poison {
                lock(&self.poison).insert(a.id.clone());
            }
            a.id
        }
        fn terminal(&self, poison: bool) -> Vec<String> {
            let ids = self
                .source
                .0
                .yolo()
                .store()
                .reader(|c| {
                    Ok::<_, steve_store::StoreError>(
                        c.prepare("SELECT id FROM yolo_actions WHERE outcome<>'scheduled'")?
                            .query_map([], |r| r.get::<_, String>(0))?
                            .collect::<rusqlite::Result<Vec<_>>>()?,
                    )
                })
                .unwrap();
            ids.into_iter()
                .filter(|id| lock(&self.poison).contains(id) == poison)
                .collect()
        }
    }
    impl contract::Fixture for Fixture {
        type Source = Scheduled;
        fn source(&self) -> &Scheduled {
            &self.source
        }
        fn batch(&self) -> usize {
            DUE_BATCH
        }
        fn add(&self, at: i64, poison: bool) -> String {
            self.add_in(WS, at, poison)
        }
        fn done(&self) -> Vec<String> {
            self.terminal(false)
        }
        fn dead(&self) -> Vec<String> {
            self.terminal(true)
        }
    }
    #[tokio::test]
    async fn r7_scheduled_actions_obey_the_real_store_due_contract() {
        contract::check(Fixture::new, 1000, 3).await;
    }
    #[tokio::test]
    async fn r7b_a_real_row_failure_keeps_its_retry_and_does_not_delay_a_new_action() {
        let f = Fixture::new();
        let poison = contract::Fixture::add(&f, 999, true);
        f.source.run_due(1000).await.unwrap();
        let row = f.source.0.reread(&poison).unwrap();
        assert_eq!(row.outcome, "scheduled");
        assert_eq!(row.due_at, Some(16_000));
        assert_eq!(lock(&f.source.0.0.errors).get(WS).unwrap().len(), 1);
        let good = contract::Fixture::add(&f, 1100, false);
        assert_eq!(f.source.next_due().await.unwrap(), Some(1100));
        f.clock.set(1100);
        f.source.run_due(1100).await.unwrap();
        assert!(contract::Fixture::done(&f).contains(&good));
        assert_eq!(f.source.next_due().await.unwrap(), Some(16_000));
        f.clock.set(16_000);
        f.source.run_due(16_000).await.unwrap();
        assert_eq!(f.source.0.reread(&poison).unwrap().outcome, "scheduled");
        assert_eq!(lock(&f.source.0.0.errors).get(WS).unwrap().len(), 2);
        f.clock.set(31_000);
        f.source.run_due(31_000).await.unwrap();
        assert_eq!(f.source.0.reread(&poison).unwrap().outcome, "cancelled");
        assert_eq!(f.source.next_due().await.unwrap(), None);
    }
    #[tokio::test]
    async fn r7b_more_than_a_batch_of_real_failures_cannot_starve_a_healthy_action() {
        let f = Fixture::new();
        for n in 0..DUE_BATCH + 10 {
            // Separate workspaces keep each below B5's workspace-wide threshold, so this
            // regression must make progress by row-local retry rather than mass cancellation.
            let ws = format!("poison-ws-{n}");
            f.source
                .0
                .yolo()
                .store()
                .with_conn(|c| {
                    c.execute(
                        "INSERT INTO workspaces (id, name, created) VALUES (?1, ?1, 1)",
                        [&ws],
                    )?;
                    Ok(())
                })
                .unwrap();
            f.add_in(&ws, 999, true);
        }
        let good = contract::Fixture::add(&f, 1000, false);
        f.source.run_due(1000).await.unwrap();
        assert!(!contract::Fixture::done(&f).contains(&good));
        f.source.run_due(1000).await.unwrap();
        assert!(contract::Fixture::done(&f).contains(&good));
        assert_eq!(f.source.next_due().await.unwrap(), Some(16_000));
        assert_eq!(lock(&f.source.0.0.errors).len(), DUE_BATCH + 10);
    }
    #[tokio::test]
    async fn r7_terminal_actions_are_not_due_and_writes_wake_the_runner() {
        let f = Fixture::new();
        let yolo = f.source.0.yolo();
        let id = contract::Fixture::add(&f, 1000, false);
        assert_eq!(f.source.next_due().await.unwrap(), Some(1000));
        f.source.run_due(1000).await.unwrap();
        assert!(contract::Fixture::done(&f).contains(&id));
        assert_eq!(f.source.next_due().await.unwrap(), None);
        assert!(
            tokio::time::timeout(Duration::ZERO, yolo.0.due_wake.notified())
                .await
                .is_ok()
        );
    }
    async fn until(f: impl Fn() -> bool) {
        tokio::time::timeout(Duration::from_secs(30), async {
            while !f() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("due runner made progress");
    }
    #[tokio::test]
    async fn r7_due_runner_uses_the_real_clock_deadline_and_commit_wakeup() {
        let f = Fixture::new();
        let task = steve_runtime::spawn("test.yolo.due", f.source.0.clone().run_ticks());
        until(|| f.clock.observed(1000)).await;
        let id = contract::Fixture::add(&f, 1100, false);
        f.clock.advance(99);
        until(|| f.clock.observed(1099)).await;
        assert!(!contract::Fixture::done(&f).contains(&id));
        f.clock.advance(1);
        until(|| contract::Fixture::done(&f).contains(&id)).await;
        task.stop();
        task.await;
        let later = contract::Fixture::add(&f, 1200, false);
        f.clock.advance(1000);
        tokio::task::yield_now().await;
        assert!(!contract::Fixture::done(&f).contains(&later));
    }
    #[tokio::test]
    async fn r7_a_busy_claim_waits_for_release_without_blocking_the_rest_of_the_batch() {
        let f = Fixture::new();
        let busy = contract::Fixture::add(&f, 999, false);
        let malformed = f
            .source
            .0
            .yolo()
            .record(&repo::NewAction {
                workspace_id: WS.into(),
                outcome: "scheduled".into(),
                due_at: Some(1000),
                ..Default::default()
            })
            .unwrap()
            .id;
        let claim = f.source.0.claim("already-handled").unwrap();
        f.source.run_due(1000).await.unwrap();
        assert_eq!(f.source.0.reread(&malformed).unwrap().outcome, "cancelled");
        assert_eq!(f.source.next_due().await.unwrap(), None);
        f.source.run_due(1000).await.unwrap();
        drop(claim);
        assert_eq!(f.source.next_due().await.unwrap(), Some(999));
        f.source.run_due(1000).await.unwrap();
        assert!(contract::Fixture::done(&f).contains(&busy));
        assert_eq!(f.source.next_due().await.unwrap(), None);
    }
    #[tokio::test]
    async fn r7_more_than_a_batch_of_claimed_actions_cannot_hide_an_available_row() {
        let f = Fixture::new();
        let mut claims = Vec::new();
        for n in 0..DUE_BATCH + 10 {
            let decision = format!("busy-{n}");
            f.source
                .0
                .yolo()
                .record(&repo::NewAction {
                    workspace_id: WS.into(),
                    kind: "approve".into(),
                    outcome: "scheduled".into(),
                    due_at: Some(999),
                    decision_id: Some(decision.clone()),
                    ..Default::default()
                })
                .unwrap();
            claims.push(f.source.0.claim(&decision).unwrap());
        }
        let available = contract::Fixture::add(&f, 1000, false);
        f.source.run_due(1000).await.unwrap();
        assert!(contract::Fixture::done(&f).contains(&available));
        assert_eq!(
            f.source.next_due().await.unwrap(),
            None,
            "claimed rows are not runnable"
        );
        f.source.run_due(1000).await.unwrap();
        // Drain previous write/settlement wakes, then prove releasing a claim wakes the runner.
        let _ = tokio::time::timeout(Duration::ZERO, f.source.0.yolo().0.due_wake.notified()).await;
        drop(claims);
        assert_eq!(f.source.next_due().await.unwrap(), Some(999));
        assert!(
            tokio::time::timeout(Duration::ZERO, f.source.0.yolo().0.due_wake.notified())
                .await
                .is_ok()
        );
    }
    fn take_wake(wake: &tokio::sync::Notify) -> bool {
        use std::future::Future;
        let mut notified = std::pin::pin!(wake.notified());
        notified
            .as_mut()
            .poll(&mut std::task::Context::from_waker(std::task::Waker::noop()))
            .is_ready()
    }

    #[tokio::test]
    async fn r7c_action_wakes_follow_outer_commit_and_rollback() {
        let f = Fixture::new();
        let yolo = f.source.0.yolo();
        let store = yolo.store();
        let wake = &yolo.0.due_wake;
        assert!(!take_wake(wake));
        let id = store
            .transaction(|_| -> ApiResult<_> {
                let row = yolo.record(&repo::NewAction {
                    workspace_id: WS.into(),
                    outcome: "scheduled".into(),
                    due_at: Some(1000),
                    reason: "original reason".into(),
                    ..Default::default()
                })?;
                assert!(
                    !take_wake(wake),
                    "record must wait for the outermost commit"
                );
                yolo.postpone(&row.id, 2000, &row.reason)?;
                assert!(
                    !take_wake(wake),
                    "postpone must wait for the outermost commit"
                );
                Ok(row.id)
            })
            .unwrap();
        assert!(take_wake(wake));
        assert!(!take_wake(wake));
        let rolled_back = store.transaction(|_| -> ApiResult<()> {
            yolo.postpone(&id, 3000, "rolled back")?;
            assert!(!take_wake(wake));
            Err(ApiError::rule("rollback"))
        });
        assert!(rolled_back.is_err());
        assert!(!take_wake(wake), "a root rollback must not wake the source");
        assert_eq!(f.source.0.reread(&id).unwrap().due_at, Some(2000));
        // A write that bypasses ledger helpers must use the very same commit subscription.
        store
            .transaction(|tx| {
                tx.execute("UPDATE yolo_actions SET due_at=4000 WHERE id=?", [&id])?;
                Ok::<_, steve_store::StoreError>(())
            })
            .unwrap();
        assert!(take_wake(wake));
        assert_eq!(f.source.next_due().await.unwrap(), Some(4000));
    }

    #[tokio::test]
    async fn r7c_retry_preserves_the_business_reason_through_finish() {
        let f = Fixture::new();
        let id = contract::Fixture::add(&f, 1000, true);
        let reason = f.source.0.reread(&id).unwrap().reason;
        assert!(!reason.is_empty());
        f.source.run_due(1000).await.unwrap();
        assert_eq!(f.source.0.reread(&id).unwrap().reason, reason);
        f.source.0.yolo().finish(&id, "failed", None).unwrap();
        let row = f.source.0.reread(&id).unwrap();
        assert_eq!(row.reason, reason);
        assert_eq!(
            ledger::result_with_yolo(&serde_json::json!({}), &row)["yolo_reason"],
            reason
        );
    }

    #[tokio::test]
    async fn r7c_maintenance_retries_within_the_original_tick() {
        let f = Fixture::new();
        let id = contract::Fixture::add(&f, 1000, false);
        let store = f.source.0.yolo().store();
        store.maintenance_set(true).unwrap();
        assert_eq!(f.source.next_due().await.unwrap(), Some(16_000));
        f.source.run_due(1000).await.unwrap();
        assert!(contract::Fixture::done(&f).is_empty());
        take_wake(&f.source.0.yolo().0.due_wake);
        let task = steve_runtime::spawn(
            "test.yolo.maintenance",
            steve_runtime::due::due_loop(
                Arc::new(Scheduled(f.source.0.clone())),
                f.clock.clone(),
                f.source.0.yolo().0.due_wake.clone(),
                steve_runtime::policy::Backoff {
                    base: TICK,
                    max: TICK,
                    factor: 1,
                    jitter: steve_runtime::policy::Jitter::None,
                    attempts: None,
                    reset_after: None,
                },
            ),
        );
        until(|| f.clock.observed(1000)).await;
        store.maintenance_set(false).unwrap();
        f.clock.advance(14_999);
        until(|| f.clock.observed(15_999)).await;
        assert!(!contract::Fixture::done(&f).contains(&id));
        f.clock.advance(1);
        until(|| contract::Fixture::done(&f).contains(&id)).await;
        task.stop();
        task.await;
    }

    #[tokio::test]
    async fn r7c_scheduled_commits_do_not_self_wake_into_a_busy_loop() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        struct FixedWall {
            elapsed: Arc<steve_runtime::FakeClock>,
        }
        impl steve_runtime::Clock for FixedWall {
            fn now(&self) -> i64 {
                1000
            }
            fn sleep(
                &self,
                d: Duration,
            ) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send + '_>> {
                self.elapsed.sleep(d)
            }
        }
        struct Counted {
            source: Scheduled,
            runs: AtomicUsize,
        }
        impl DueSource for Counted {
            fn name(&self) -> &'static str {
                self.source.name()
            }
            async fn next_due(&self) -> Result<Option<i64>, steve_runtime::due::DueError> {
                self.source.next_due().await
            }
            async fn run_due(&self, now: i64) -> Result<(), steve_runtime::due::DueError> {
                self.runs.fetch_add(1, Ordering::SeqCst);
                tokio::task::yield_now().await;
                self.source.run_due(now).await
            }
        }
        for poison in [false, true] {
            let f = Fixture::new();
            let id = contract::Fixture::add(&f, 1000, poison);
            let elapsed = Arc::new(steve_runtime::FakeClock::new(0));
            let source = Arc::new(Counted {
                source: Scheduled(f.source.0.clone()),
                runs: AtomicUsize::new(0),
            });
            let task = steve_runtime::spawn(
                "test.yolo.self_wake",
                steve_runtime::due::due_loop(
                    source.clone(),
                    Arc::new(FixedWall {
                        elapsed: elapsed.clone(),
                    }),
                    f.source.0.yolo().0.due_wake.clone(),
                    steve_runtime::policy::Backoff {
                        base: TICK,
                        max: TICK,
                        factor: 1,
                        jitter: steve_runtime::policy::Jitter::None,
                        attempts: None,
                        reset_after: None,
                    },
                ),
            );
            until(|| source.runs.load(Ordering::SeqCst) >= 1 && elapsed.observed(0)).await;
            let row = f.source.0.reread(&id).unwrap();
            assert_eq!(row.outcome, if poison { "scheduled" } else { "cancelled" });
            if poison {
                assert_eq!(row.due_at, Some(16_000));
            }
            for at in [60_000, 120_000, 180_000] {
                elapsed.advance(60_000);
                until(|| elapsed.observed(at)).await;
                assert_eq!(f.clock.now(), 1000, "domain wall time stays frozen");
                assert!(
                    !task.is_finished(),
                    "a dead loop cannot masquerade as bounded work"
                );
                assert_eq!(
                    source.runs.load(Ordering::SeqCst),
                    1,
                    "self commits may recheck but never rerun work before its deadline"
                );
            }
            task.stop();
            task.await;
        }
    }
    #[tokio::test]
    async fn r7c_manual_tick_drains_batches_with_the_same_deadline_and_claim_filters() {
        let f = Fixture::new();
        let mut ids = Vec::new();
        for _ in 0..2 * DUE_BATCH + 1 {
            ids.push(contract::Fixture::add(&f, 1000, false));
        }
        let future = contract::Fixture::add(&f, 1001, false);
        let busy = f
            .source
            .0
            .yolo()
            .record(&repo::NewAction {
                workspace_id: WS.into(),
                outcome: "scheduled".into(),
                due_at: Some(999),
                decision_id: Some("held".into()),
                ..Default::default()
            })
            .unwrap()
            .id;
        let _claim = f.source.0.claim("held").unwrap();
        let rows = f.source.0.tick();
        assert_eq!(rows.len(), ids.len());
        let done = contract::Fixture::done(&f);
        assert!(ids.iter().all(|id| done.contains(id)));
        assert!(!done.contains(&future) && !done.contains(&busy));
        assert_eq!(f.source.next_due().await.unwrap(), Some(1001));
    }
}
