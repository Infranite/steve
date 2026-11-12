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
//!   given once ([`Engine::kick_waiting_tools`] when the switch or a scope changes).
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

/// How often scheduled actions and waiting cards are looked at again.
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
        tracing::error!("yolo executor in {ws} on {id}: {e}");
        let now = self.yolo().now();
        let n = {
            let mut errs = lock(&self.0.errors);
            let xs = errs.entry(ws.clone()).or_default();
            xs.retain(|t| now - *t < B5_WINDOW_MS);
            xs.push(now);
            xs.len()
        };
        if n < B5_ERRORS {
            return;
        }
        lock(&self.0.errors).remove(&ws);
        if let Err(e2) = self.yolo().executor_failed(&ws, &e.to_string()) {
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
            .reader(|c| repo::actions_of_decision(c, id))?;
        if d.status != "open" {
            // the user (or something else) answered first: what was scheduled is moot
            for a in prior.iter().filter(|a| a.outcome == "scheduled") {
                self.yolo()
                    .finish(&a.id, "cancelled", Some("你已经处理了这张卡"))?;
            }
            self.forget_tool(id);
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
            .reader(|c| repo::window_current(c, &d.workspace_id))?;
        if let Some(w) = &window
            && !w.include_open
            && d.created < w.started
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
                    self.yolo()
                        .finish(&a.id, "cancelled", Some("权限请求已经失效"))
                        .map(drop)
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
            .reader(|c| repo::action_get(c, action))?
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
        match self.yolo().store().reader(|c| repo::due_actions(c, now)) {
            Ok(due) => {
                for a in due {
                    let id = a.decision_id.clone().unwrap_or_default();
                    match self.run_due_row(a) {
                        Ok(Some(a)) => out.push(a),
                        Ok(None) => {}
                        Err(e) => self.fail_hard(&id, &e),
                    }
                }
            }
            Err(e) => tracing::error!("yolo due actions: {e}"),
        }
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
                .reader(|c| repo::scopes(c, &ws))
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

    /// Every [`TICK`]. Runs forever.
    pub async fn run_ticks(self) {
        let mut t = tokio::time::interval(TICK);
        loop {
            t.tick().await;
            let e = self.clone();
            if let Err(err) = tokio::task::spawn_blocking(move || e.tick()).await {
                tracing::error!("yolo tick task: {err}");
            }
        }
    }

    /// Looks at every decision a `{type:"decision", id}` event names. Runs until the sender is gone.
    pub async fn run_events(self, mut rx: tokio::sync::mpsc::Receiver<Value>) {
        while let Some(v) = rx.recv().await {
            if v["type"] == "yolo" {
                // the switch or a scope changed: running sessions hear of it with their next message,
                // and a tool request waiting meanwhile is looked at again now (not at the next tick)
                if let Some(ws) = v["ws"].as_str().map(String::from) {
                    let e = self.clone();
                    tokio::task::spawn_blocking(move || {
                        e.core().yolo_sync(&ws);
                        e.kick_waiting_tools(&ws);
                    });
                }
                continue;
            }
            if v["type"] != "decision" {
                continue;
            }
            let Some(id) = v["id"].as_str().map(String::from) else {
                continue;
            };
            // not awaited: a question waits up to a minute for the model, a tool call must not wait
            // behind it (each decision is claimed, so two looks at one never overlap)
            let e = self.clone();
            tokio::task::spawn_blocking(move || e.kick(&id));
        }
    }
}
