//! The sweep (#334), observe-only: every stall-watch tick (30 s) it looks at **every open task item**
//! — `ready`, `active` and `closing`, archived ones aside — derives its state ([`crate::derive`])
//! and decides the one thing it would do to move the item on: wake the agent, raise a card for you,
//! or remind you. Deterministic: no model, the same facts give the same decision; every clock is a
//! named entry of the work policy ([`crate::work_policy`]).
//!
//! Nothing is carried out. Each action has an execution switch ([`EXECUTE`]) and every switch is
//! off, so the stall card and Steve's continues ([`crate::stall`], [`crate::takeover`]) stay the
//! only things that act on an item: one advancer per item. The decision is recorded in a shadow log
//! (`item_would_do`) under (item, cycle, session, turn, scope revision, action, reason, target):
//! seeing it again on the next pass, or after a restart, is the same record, while the same reason
//! about another operation, card or wait (or about a changed goal) is a new one, so a later change that turns an action on can carry
//! each decision out exactly once. It is shown in the item's Debug view next to the derived state.
//!
//! The rules ([`RULES`]), first match wins. Above all, an open card already asks you, so the sweep
//! adds nothing. Then the states nothing moves on today, each a card: a merge you declined; the
//! current session's Node gone; a card you answered on a paused item, or a paused item nobody
//! touched; a closing that waits for this Hub to run the result, has a failed required operation, a
//! pull request problem the agent was told about and left on the same head, or a wrap-up module that
//! went silent. Then the gaps of the derived state (nobody can bring it back, follow-ups used up, a
//! state nothing owns), Steve's follow-up and a status question to a turn that went quiet, and
//! reminders of a build that runs long, a wait looked at often, an item silent for long.

use serde::Serialize;
use serde_json::{Value, json};
use steve_store::ItemOperation;
use steve_store::would_do::{NewWouldDo, WouldDoRow};

use crate::cycle::{CLOSED, CLOSING, OP_BUILD, OP_HOOKS, OP_RELEASE};
use crate::derive::{Derived, Facts, Lane, NextStep, Settled, derive};
use crate::proof::OP_DEPLOY;
use crate::repo::{Decision, Item};
use crate::util::now;
use crate::work_policy::WorkPolicy;
use crate::{Core, Result, Tx};

/// What the sweep may do.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Action {
    /// Send the agent's session a prompt (Steve's continue, a status question).
    Wake,
    /// Raise a card for you.
    Card,
    /// Remind you of something that goes on (it is still followed).
    Remind,
}

impl Action {
    pub fn as_str(self) -> &'static str {
        match self {
            Action::Wake => "wake",
            Action::Card => "card",
            Action::Remind => "remind",
        }
    }
}

/// Whether the sweep carries an action out itself, per action.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct Switches {
    pub wake: bool,
    pub card: bool,
    pub remind: bool,
}

impl Switches {
    pub const fn on(&self, a: Action) -> bool {
        match a {
            Action::Wake => self.wake,
            Action::Card => self.card,
            Action::Remind => self.remind,
        }
    }
}

/// All off in this release: the sweep only observes. A switch is turned on in code, together with
/// the executor of its action (an operator setting could otherwise turn on an action nothing
/// carries out, or two advancers at once).
pub const EXECUTE: Switches = Switches {
    wake: false,
    card: false,
    remind: false,
};

/// From when a rule's clock runs to when it comes due.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(tag = "type", content = "entry", rename_all = "snake_case")]
pub enum Clock {
    /// At once.
    Now,
    /// After the named work-policy entry.
    Policy(&'static str),
    /// After the stall watch's idle time ([`crate::stall::STALL_AFTER_MS`]): the agent had its turn.
    Quiet,
    /// When the derived state's next step comes due.
    Next,
}

/// One rule of the sweep.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct Rule {
    pub reason: &'static str,
    pub action: Action,
    pub clock: Clock,
    pub meaning: &'static str,
}

const fn rule(reason: &'static str, action: Action, clock: Clock, meaning: &'static str) -> Rule {
    Rule {
        reason,
        action,
        clock,
        meaning,
    }
}

/// Every rule, in the order they are tried.
pub const RULES: &[Rule] = &[
    rule(
        "merge_declined",
        Action::Card,
        Clock::Now,
        "you declined to merge the pull request and nothing follows: rework, close or keep the branch",
    ),
    rule(
        "node_gone_closing",
        Action::Card,
        Clock::Policy("nobody_offline"),
        "closing, and the Node of the current session is gone",
    ),
    rule(
        "node_gone",
        Action::Card,
        Clock::Policy("nobody_offline"),
        "the Node of the current session is gone",
    ),
    rule(
        "answered_while_paused",
        Action::Card,
        Clock::Now,
        "you answered a card of a paused item, and it stays paused",
    ),
    rule(
        "paused_silent",
        Action::Card,
        Clock::Policy("silent_min"),
        "paused (by you, or when you dismissed or looked at a card) and nothing since",
    ),
    rule(
        "deploy_waiting",
        Action::Card,
        Clock::Now,
        "closing waits for this Hub to run the result: only an upgrade or you move it on",
    ),
    rule(
        "required_failed",
        Action::Card,
        Clock::Quiet,
        "a required operation of the result (a build) failed and the agent's turn after it is over",
    ),
    rule(
        "pr_unfixed",
        Action::Card,
        Clock::Quiet,
        "the agent was told what keeps the pull request from merging, its turn is over and the problem is on the same head",
    ),
    rule(
        "hooks_silent",
        Action::Card,
        Clock::Policy("silent_min"),
        "the wrap-up module took the close over (or was asked) and has said nothing since",
    ),
    rule(
        "build_long",
        Action::Remind,
        Clock::Policy("build_remind"),
        "a required build has run long (it is still followed)",
    ),
    rule(
        "nobody",
        Action::Card,
        Clock::Now,
        "nothing can bring it back: no session, or its state is unknown",
    ),
    rule(
        "stopped",
        Action::Card,
        Clock::Now,
        "stopped, not done, and Steve's follow-ups are used up",
    ),
    rule(
        "unowned",
        Action::Card,
        Clock::Quiet,
        "settled, and nothing today looks at it (not `run`, or a closing with nothing open)",
    ),
    rule(
        "follow_up",
        Action::Wake,
        Clock::Next,
        "settled with nothing delivered or asked: Steve's follow-up (today's takeover does it)",
    ),
    rule(
        "stall_question",
        Action::Wake,
        Clock::Policy("stall_min"),
        "a running turn with no new output: one status question, for its next turn",
    ),
    rule(
        "long_wait",
        Action::Remind,
        Clock::Now,
        "a wait looked at as often as the policy notes",
    ),
    rule(
        "silent",
        Action::Card,
        Clock::Policy("silent_min"),
        "waiting or closing, and nothing recorded for long",
    ),
];

/// A rule by its reason (every reason the sweep names is in [`RULES`]; a test checks it).
fn rule_of(reason: &str) -> Option<&'static Rule> {
    let r = RULES.iter().find(|r| r.reason == reason);
    debug_assert!(r.is_some(), "sweep reason {reason} is not a rule");
    r
}

/// What the agent can fix in its pull request, and the `pr_told` key prefix of having been told.
const PR_FIXABLE: &[(&str, &str)] = &[
    ("conflict", "conflict:"),
    ("behind", "behind:"),
    ("ci", "ci:"),
    ("draft", "draft:"),
    ("head", "head:"),
    ("uncommitted", "dirty:"),
];
/// The `pr_told` key of the card saying the agent was woken as often as it may be.
const TOLD_TELLS: &str = "tells";

/// An operation the sweep looks at, and since when it is as it is.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct OpSignal {
    pub id: String,
    pub kind: String,
    pub label: String,
    pub since: i64,
}

/// A pull request problem of the current result the agent can fix.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct PrProblem {
    pub op: String,
    pub problem: String,
    pub pr: Option<u64>,
    /// The agent was told about it (or was woken as often as it may be).
    pub told: bool,
}

/// A card you answered.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct Answered {
    pub decision_id: String,
    pub at: i64,
}

/// What the sweep looks at beyond the derived state's facts.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct Signals {
    pub pr_problem: Option<PrProblem>,
    pub pr_declined: Option<u64>,
    /// The release operation whose merge you declined.
    pub declined_op: Option<String>,
    pub required_failed: Option<OpSignal>,
    pub deploy_waiting: Option<OpSignal>,
    pub hooks_open: Option<OpSignal>,
    pub build_running: Option<OpSignal>,
    /// When the item was last paused (the item action, or a card you dismissed or looked at).
    pub paused_at: Option<i64>,
    /// The newest card you answered after that.
    pub answered: Option<Answered>,
    /// The newest thing recorded on the item (the item, its session, its operations).
    pub last_record: i64,
}

/// What the operations of the item's cycle say about the current result (`revision`). Pure.
pub fn from_operations(revision: i64, ops: &[ItemOperation]) -> Signals {
    let mine = || ops.iter().filter(|o| o.revision == revision);
    let sig = |o: &ItemOperation, since: i64| OpSignal {
        id: o.id.clone(),
        kind: o.kind.clone(),
        label: o.label.clone(),
        since,
    };
    let release = mine().find(|o| o.kind == OP_RELEASE && o.status == "pending");
    let pr_problem = release.and_then(|o| {
        let problem = o.evidence["pr_problem"].as_str()?;
        let prefix = PR_FIXABLE.iter().find(|(p, _)| *p == problem)?.1;
        let told = o.evidence["pr_told"].as_array().is_some_and(|keys| {
            keys.iter()
                .filter_map(Value::as_str)
                .any(|k| k.starts_with(prefix) || k == TOLD_TELLS)
        });
        Some(PrProblem {
            op: o.id.clone(),
            problem: problem.to_owned(),
            pr: o.evidence["pr"].as_u64(),
            told,
        })
    });
    let declined =
        mine().find(|o| o.kind == OP_RELEASE && o.evidence["pr_merge_declined"] == json!(true));
    Signals {
        pr_problem,
        pr_declined: declined.map(|o| o.evidence["pr"].as_u64().unwrap_or(0)),
        declined_op: declined.map(|o| o.id.clone()),
        required_failed: mine()
            .find(|o| o.required && o.status == "failed")
            .map(|o| sig(o, o.updated)),
        deploy_waiting: mine()
            .find(|o| o.kind == OP_DEPLOY && o.required && o.status == "pending")
            .map(|o| sig(o, o.created)),
        hooks_open: mine()
            .find(|o| o.kind == OP_HOOKS && matches!(o.status.as_str(), "pending" | "running"))
            .map(|o| sig(o, o.updated)),
        build_running: mine()
            .find(|o| o.kind == OP_BUILD && o.required && o.status == "running")
            .map(|o| sig(o, o.created)),
        last_record: ops.iter().map(|o| o.updated).max().unwrap_or(0),
        ..Default::default()
    }
}

/// When the item was last paused and the newest card you answered after that, from the item
/// action's audit time and the item's cards. A card you dismissed or chose to look at pauses the
/// item (it is the pause, not an answer); an expired or superseded one was nobody's answer. Pure.
pub fn pause_and_answer(
    paused_by_action: Option<i64>,
    decisions: &[Decision],
) -> (Option<i64>, Option<Answered>) {
    let pauses =
        |d: &Decision| d.result["dismissed"] == json!(true) || d.result["choice"] == "look";
    let resolved = || {
        decisions
            .iter()
            .filter(|d| d.status == "resolved")
            .filter_map(|d| d.resolved.map(|at| (d, at)))
    };
    let paused_at = resolved()
        .filter(|(d, _)| pauses(d))
        .map(|(_, at)| at)
        .chain(paused_by_action)
        .max();
    let answered = paused_at.and_then(|p| {
        resolved()
            .filter(|(d, at)| !pauses(d) && d.result.get("superseded").is_none() && *at > p)
            .max_by_key(|(_, at)| *at)
            .map(|(d, at)| Answered {
                decision_id: d.id.clone(),
                at,
            })
    });
    (paused_at, answered)
}

/// What the sweep would do for one item now.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct WouldDo {
    pub action: Action,
    pub reason: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    /// When it comes due; until then it is only shown, not recorded.
    pub due_at: i64,
    pub session_id: String,
    pub turn_id: String,
    /// The item's scope revision it was decided on (the version of its goal).
    pub scope_revision: i64,
    /// What it is about, when the reason names one thing: an operation, a card or a wait (`''`:
    /// the item as a whole). The same reason about another one is another decision.
    pub target: String,
    /// The derived state it was decided on.
    pub row: u8,
    pub code: &'static str,
}

impl WouldDo {
    pub fn due(&self, now: i64) -> bool {
        self.due_at <= now
    }
}

/// The one thing the sweep would do for the item, first matching rule wins (`None`: nothing). Pure.
pub fn plan(f: &Facts, d: &Derived, s: &Signals, p: &WorkPolicy) -> Option<WouldDo> {
    if d.lane == Lane::Done || f.item.phase == CLOSED {
        return None;
    }
    /* a card already asks you: nothing is added on top of it */
    if !f.cards.is_empty() {
        return None;
    }
    let Matched {
        reason,
        since,
        detail,
        target,
    } = matched(f, d, s, p)?;
    let r = rule_of(reason)?;
    let due_at = match r.clock {
        Clock::Now => since.min(f.now),
        Clock::Policy(entry) => since + p.get(entry),
        Clock::Quiet => since + f.stall_after,
        Clock::Next => d.next_at.unwrap_or(since),
    };
    let (session_id, turn_id) = f
        .session
        .as_ref()
        .map(|x| (x.id.clone(), x.turn.clone().unwrap_or_default()))
        .unwrap_or_default();
    Some(WouldDo {
        action: r.action,
        reason: r.reason,
        detail,
        due_at,
        session_id,
        turn_id,
        scope_revision: f.item.scope_revision,
        target: target.unwrap_or_default(),
        row: d.row,
        code: d.code,
    })
}

/// The rule that holds: its reason, from when its clock runs, what it says, and the one thing it is
/// about when it names one (an operation, a card, a wait).
struct Matched {
    reason: &'static str,
    since: i64,
    detail: Option<String>,
    target: Option<String>,
}

fn hit(
    reason: &'static str,
    since: i64,
    detail: Option<String>,
    target: Option<String>,
) -> Option<Matched> {
    Some(Matched {
        reason,
        since,
        detail,
        target,
    })
}

/// The first rule that holds.
fn matched(f: &Facts, d: &Derived, s: &Signals, p: &WorkPolicy) -> Option<Matched> {
    let now = f.now;
    let closing = f.item.phase == CLOSING;
    let settled = d.settled == Settled::Evaluable;
    let session = f.session.as_ref();
    let activity = session.map_or(0, |x| x.last_activity);
    let pr = |n: Option<u64>| n.filter(|n| *n > 0).map(|n| format!("#{n}"));
    if f.merge_declined {
        return hit(
            "merge_declined",
            now,
            pr(s.pr_declined),
            s.declined_op.clone(),
        );
    }
    if let Some(since) = session.and_then(|x| x.offline_since) {
        let reason = if closing {
            "node_gone_closing"
        } else {
            "node_gone"
        };
        return hit(reason, since, None, None);
    }
    if f.item.paused {
        /* a turn your answer started is not stuck */
        if let Some(a) = &s.answered
            && d.settled != Settled::Busy
        {
            return hit(
                "answered_while_paused",
                a.at,
                Some(a.decision_id.clone()),
                Some(a.decision_id.clone()),
            );
        }
        let since = s.last_record.max(activity).max(s.paused_at.unwrap_or(0));
        return hit("paused_silent", since, None, None);
    }
    if closing && settled {
        if let Some(o) = &s.deploy_waiting {
            return hit(
                "deploy_waiting",
                o.since,
                Some(o.label.clone()),
                Some(o.id.clone()),
            );
        }
        if let Some(o) = &s.required_failed {
            return hit(
                "required_failed",
                o.since.max(activity),
                Some(o.label.clone()),
                Some(o.id.clone()),
            );
        }
        if let Some(x) = s.pr_problem.as_ref().filter(|x| x.told) {
            let what = match pr(x.pr) {
                Some(n) => format!("{n} {}", x.problem),
                None => x.problem.clone(),
            };
            return hit("pr_unfixed", activity, Some(what), Some(x.op.clone()));
        }
    }
    if closing && let Some(o) = &s.hooks_open {
        return hit(
            "hooks_silent",
            o.since,
            Some(o.label.clone()),
            Some(o.id.clone()),
        );
    }
    if closing && let Some(o) = &s.build_running {
        return hit(
            "build_long",
            o.since,
            Some(o.label.clone()),
            Some(o.id.clone()),
        );
    }
    match d.code {
        "nobody" => return hit("nobody", now, None, None),
        "stopped" => return hit("stopped", now, None, None),
        "unknown" => return hit("unowned", activity, None, None),
        "follow_up" => return hit("follow_up", now, None, None),
        "running" if session.is_some_and(|x| x.running) => {
            return hit("stall_question", activity, None, None);
        }
        _ => {}
    }
    if d.code == "waiting"
        && let Some(w) = f
            .waits
            .iter()
            .find(|w| w.cycle == f.item.cycle && w.wakes >= p.wait_note_after())
    {
        return hit("long_wait", now, Some(w.what.clone()), Some(w.id.clone()));
    }
    if d.lane == Lane::Running
        && d.settled != Settled::Busy
        && matches!(d.code, "waiting" | "closing" | "quota")
    {
        /* about what it waits on: the wait or the closing operation it is due on */
        let target = match &d.next_step {
            Some(NextStep::Due { id, .. }) => id.clone(),
            _ => None,
        };
        return hit(
            "silent",
            s.last_record.max(activity),
            d.detail.clone(),
            target,
        );
    }
    None
}

/// How many of an item's recorded decisions its Debug view shows.
pub const LOG_SHOWN: usize = 10;

/// The reason in words, in the reader's language.
pub fn label(reason: &str) -> String {
    match reason {
        "merge_declined" => steve_i18n::t!("core.sweep.merge_declined"),
        "node_gone_closing" => steve_i18n::t!("core.sweep.node_gone_closing"),
        "node_gone" => steve_i18n::t!("core.sweep.node_gone"),
        "answered_while_paused" => steve_i18n::t!("core.sweep.answered_while_paused"),
        "paused_silent" => steve_i18n::t!("core.sweep.paused_silent"),
        "deploy_waiting" => steve_i18n::t!("core.sweep.deploy_waiting"),
        "required_failed" => steve_i18n::t!("core.sweep.required_failed"),
        "pr_unfixed" => steve_i18n::t!("core.sweep.pr_unfixed"),
        "hooks_silent" => steve_i18n::t!("core.sweep.hooks_silent"),
        "build_long" => steve_i18n::t!("core.sweep.build_long"),
        "nobody" => steve_i18n::t!("core.sweep.nobody"),
        "stopped" => steve_i18n::t!("core.sweep.stopped"),
        "unowned" => steve_i18n::t!("core.sweep.unowned"),
        "follow_up" => steve_i18n::t!("core.sweep.follow_up"),
        "stall_question" => steve_i18n::t!("core.sweep.stall_question"),
        "long_wait" => steve_i18n::t!("core.sweep.long_wait"),
        "silent" => steve_i18n::t!("core.sweep.silent"),
        _ => reason.to_owned(),
    }
}

impl Tx<'_> {
    /// What the sweep looks at for `item` beyond `facts`.
    pub(crate) fn sweep_signals(&self, item: &Item, facts: &Facts) -> Result<Signals> {
        let store = self.db().store();
        let ops = store.items().operations(&item.id, item.cycle)?;
        let mut s = from_operations(item.revision, &ops);
        if item.status == "pause" {
            let (paused_at, answered) = pause_and_answer(
                store.would_do().paused_at(&item.id)?,
                &self.db().decisions_of_item(&item.id)?,
            );
            s.paused_at = paused_at;
            s.answered = answered;
        }
        s.last_record = s
            .last_record
            .max(item.updated)
            .max(facts.session.as_ref().map_or(0, |x| x.last_activity));
        Ok(s)
    }

    /// The facts, derived state, signals and decision of `item` at `now`.
    fn sweep_plan(
        &self,
        item: &Item,
        now: i64,
        policy: &WorkPolicy,
    ) -> Result<(Derived, Option<WouldDo>)> {
        let facts = self.item_facts(item, now)?;
        let d = derive(&facts, policy);
        let s = self.sweep_signals(item, &facts)?;
        let w = plan(&facts, &d, &s, policy);
        Ok((d, w))
    }

    /// One item's pass: its decision is recorded once it is due, and what no longer holds is
    /// cleared. Carries nothing out ([`EXECUTE`]). Reads first and writes only on a change, so a
    /// pass over items whose decision stays the same writes nothing.
    pub(crate) fn sweep_item(
        &self,
        id: &str,
        now: i64,
        policy: &WorkPolicy,
    ) -> Result<Option<WouldDo>> {
        let log = self.db().store().would_do();
        let open = log.open(id)?;
        let Some(i) = self.db().item(id)? else {
            return Ok(None);
        };
        let w = if i.kind != "task" || i.phase == CLOSED || i.archived {
            None
        } else {
            self.sweep_plan(&i, now, policy)?.1.filter(|w| w.due(now))
        };
        let Some(w) = w else {
            if !open.is_empty() {
                log.clear(id, now)?;
            }
            return Ok(None);
        };
        /* the same decision: of the same turn and scope, for the same reason about the same thing (the
         * same reason about another operation, card or wait is a new decision, not the old one) */
        let same = |r: &WouldDoRow| {
            (r.cycle, r.session_id.as_str(), r.turn_id.as_str())
                == (i.cycle, w.session_id.as_str(), w.turn_id.as_str())
                && r.scope_revision == w.scope_revision
                && r.action == w.action.as_str()
                && r.reason == w.reason
                && r.target == w.target
        };
        if !(open.len() == 1 && same(&open[0])) {
            log.record(
                &NewWouldDo {
                    item_id: id,
                    cycle: i.cycle,
                    session_id: &w.session_id,
                    turn_id: &w.turn_id,
                    scope_revision: w.scope_revision,
                    target: &w.target,
                    action: w.action.as_str(),
                    reason: w.reason,
                    detail: w.detail.as_deref(),
                    row: i64::from(w.row),
                    code: w.code,
                    due_at: w.due_at,
                },
                now,
            )?;
        }
        Ok(Some(w))
    }

    /// The sweep's part of the shadow view: what it would do now, its switch, and the recent log.
    pub(crate) fn sweep_view(
        &self,
        item: &Item,
        facts: &Facts,
        d: &Derived,
        policy: &WorkPolicy,
    ) -> Result<Value> {
        let s = self.sweep_signals(item, facts)?;
        let w = plan(facts, d, &s, policy);
        let now = facts.now;
        let current = w.map(|w| {
            let mut v = serde_json::to_value(&w).unwrap_or(Value::Null);
            v["label"] = json!(label(w.reason));
            v["due"] = json!(w.due(now));
            v["executes"] = json!(EXECUTE.on(w.action));
            v
        });
        let log: Vec<Value> = self
            .db()
            .store()
            .would_do()
            .log(&item.id, LOG_SHOWN)?
            .into_iter()
            .map(|r: WouldDoRow| {
                let mut v = serde_json::to_value(&r).unwrap_or(Value::Null);
                v["label"] = json!(label(&r.reason));
                v
            })
            .collect();
        Ok(json!({"current": current, "log": log, "execute": EXECUTE}))
    }
}

impl Core {
    /// One pass of the sweep over every open task item: each decision that is due is recorded,
    /// nothing is carried out. Returns how many items it would act on. Each item is looked at in its
    /// own critical section, so a pass never holds the core for long.
    pub fn sweep(&self) -> Result<usize> {
        let at = now();
        let (ids, policy) = {
            let t = self.tx();
            /* a release in flight explains an idle item, as for the stall watch */
            if t.holding() || t.st.releasing.is_some() {
                return Ok(0);
            }
            let store = t.db().store();
            (
                store.would_do().open_items()?,
                WorkPolicy::with(&store.item_facts().policy_overrides()?),
            )
        };
        let mut n = 0;
        for id in ids {
            match self.tx().sweep_item(&id, at, &policy) {
                Ok(Some(_)) => n += 1,
                Ok(None) => {}
                Err(e) => tracing::warn!("sweep of {id}: {e}"),
            }
        }
        self.store().store().would_do().clear_gone(at)?;
        Ok(n)
    }
}
