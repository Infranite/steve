//! The derived state of an item (#327), in shadow: one pure, first-match function from the item's
//! facts to where it stands — `needs_you`, `running` or `done` — who moves it on, and its next step.
//! It is computed next to today's state ([`crate::work`], `phase`/`status`) and shown read-only in
//! the item API and the debug view; nothing acts on it yet, and it never writes.
//!
//! What it keeps apart:
//! - **Settled** has three values ([`Settled`]): the Hub-visible turn is quiescent and can be
//!   evaluated; it is known to be busy; or nobody can tell (an unreconciled dispatch, a runtime that
//!   is unsure, a Node that is away). Only `evaluable` lets anything be judged; it never proves the
//!   machine has no background work.
//! - **Reviewable, mergeable and closable** are three predicates. A delivery is reviewable once its
//!   commit and worktree are verified and nothing has an unknown effect — a merge is not needed for
//!   that. It is mergeable only with your approval of that very head. A required merge is a
//!   condition of closing, never of review.
//! - **Provenance**: only facts the Hub verified count as hard facts. What the executor reported, or
//!   nobody could tell, never closes anything by itself.
//! - **Late facts**: a fact of another cycle, and a judgement of another session or of a turn that is
//!   not the newest, is ignored: an old turn's answer never closes or moves the new turn. What
//!   closes (a merged pull request, a passed build, a live deploy) must be about the current
//!   result's commit, exactly: a fact of an earlier head of the same cycle is not.
//!
//! The rows, first match wins (numbered as in the design):
//! 1 done · 2 judged done, undoable for a while · 3 a card or something only you can do ·
//! 4 a registered wait (prerequisites, quota, a wait, a closing operation) · 5 a turn runs, input or
//! a dispatch is on its way, or the Hub reconciles · 6 settled, its turn is being judged
//! ([`crate::judge`], in shadow) · 7 settled, the Hub follows up · 8 settled, out of follow-ups ·
//! 9 nobody can bring it back · 10 not started · 11 none of these (a diagnostic).

use serde::Serialize;

use crate::work_policy::WorkPolicy;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Lane {
    NeedsYou,
    Running,
    Done,
}

/// Who moves the item on from here.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Owner {
    You,
    Hub,
    Agent,
    Nobody,
}

/// Whether the Hub-visible turn is quiescent.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Settled {
    Evaluable,
    Busy,
    Unknown,
}

pub const VERIFIED: &str = "verified";

/// The item's own state, as stored today.
#[derive(Debug, Clone, Default)]
pub struct ItemState {
    pub id: String,
    pub cycle: i64,
    /// `ready` | `active` | `closing` | `closed`
    pub phase: String,
    /// You paused it.
    pub paused: bool,
    /// It works in a git worktree on a branch: its delivery is a commit.
    pub code: bool,
    /// The version of what it is asked to do (its goal; 0: none). A change of scope moves it on, and
    /// a judgement of an earlier scope says nothing of this one.
    pub scope_revision: i64,
}

/// What closing needs, by kind (from the project's completion rule and the item's goal today).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub struct Required {
    pub merge: bool,
    pub build: bool,
    pub deploy: bool,
    /// Somebody actually used it: only ever reported by the executor, so never a hard fact.
    pub operate: bool,
}

/// A Node command of the current session whose outcome is not settled.
#[derive(Debug, Clone, Default)]
pub struct Dispatch {
    pub operation_id: String,
    /// `pending` | `accepted` | `active` | `unknown`
    pub state: String,
    pub maybe_sent: bool,
    pub receipt: bool,
    pub retry_at: i64,
}

/// The item's current session (the newest one not ended).
#[derive(Debug, Clone, Default)]
pub struct SessionState {
    pub id: String,
    /// Its newest logical turn on the Node runtime, when it has one.
    pub turn: Option<String>,
    /// A turn runs or is starting.
    pub running: bool,
    /// Its backend cannot tell whether a turn runs.
    pub uncertain: bool,
    pub queued: usize,
    /// Since when its Node is away (`None`: linked, or not on a Node).
    pub offline_since: Option<i64>,
    /// Your Stop holds it.
    pub held_by_you: bool,
    /// The last thing that happened on it.
    pub last_activity: i64,
    pub dispatches: Vec<Dispatch>,
    /// The stall watch looks at it (the item is `run`, not a work unit).
    pub watched: bool,
}

#[derive(Debug, Clone, Default)]
pub struct Card {
    pub id: String,
    /// A card saying the agent stopped (it answers a stop, it is not a question of the work).
    pub stop: bool,
    pub question: String,
}

#[derive(Debug, Clone, Default)]
pub struct Wait {
    pub id: String,
    pub cycle: i64,
    pub on: String,
    pub what: String,
    pub next_check: i64,
    pub wakes: i64,
    pub cap: Option<i64>,
}

#[derive(Debug, Clone, Default)]
pub struct Judged {
    pub id: String,
    pub cycle: i64,
    pub session_id: String,
    pub turn_id: String,
    /// The scope it judged ([`ItemState::scope_revision`] then).
    pub scope_revision: i64,
    pub verdict: String,
    pub created: i64,
}

/// The judge candidate of the current turn that is not judged yet ([`crate::judge`]).
#[derive(Debug, Clone, Default)]
pub struct Judging {
    pub id: String,
    pub cycle: i64,
    pub session_id: String,
    pub turn_id: String,
    pub next_check: i64,
    pub failures: i64,
}

#[derive(Debug, Clone, Default)]
pub struct Delivered {
    pub cycle: i64,
    pub kind: String,
    pub head: Option<String>,
    pub ref_: Option<String>,
    pub state: String,
    pub provenance: String,
    pub observed_at: i64,
}

impl Delivered {
    fn verified(&self) -> bool {
        self.provenance == VERIFIED
    }
}

/// A required operation of the current cycle that is still open (a release, its build, a deploy).
#[derive(Debug, Clone, Default)]
pub struct OpenOperation {
    pub id: String,
    pub kind: String,
    pub label: String,
}

/// Everything the derived state is computed from. Gathered by [`crate::Tx`] from the database and
/// the runtime; built by hand in tests.
#[derive(Debug, Clone, Default)]
pub struct Facts {
    pub now: i64,
    pub item: ItemState,
    pub required: Required,
    pub session: Option<SessionState>,
    /// Open cards of the item.
    pub cards: Vec<Card>,
    pub waits: Vec<Wait>,
    pub judgements: Vec<Judged>,
    /// The current turn's judge candidate while it waits to be judged (pending or being judged).
    pub judging: Option<Judging>,
    pub delivery: Vec<Delivered>,
    /// Operations whose effect nobody knows (they must be reconciled before anything closes).
    pub unknown_effects: Vec<String>,
    pub closing_ops: Vec<OpenOperation>,
    pub dependency_wait: bool,
    pub quota_until: Option<i64>,
    /// Its work graph is at work (`Some(why)`), when it has one.
    pub graph_busy: Option<String>,
    /// You declined to merge its pull request.
    pub merge_declined: bool,
    pub continues_spent: u32,
    pub continues_cap: u32,
    /// When the stall watch looks at an idle session again after its last activity.
    pub stall_after: i64,
}

/// What moves the item on next.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum NextStep {
    /// An open card somebody may answer.
    Card { decision_id: String },
    /// Something persisted that comes due: at a time, or on the event it is registered for.
    Due {
        kind: Due,
        #[serde(skip_serializing_if = "Option::is_none")]
        id: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        at: Option<i64>,
    },
    /// A turn that runs and that the Hub can reconcile.
    Turn {
        session_id: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        turn_id: Option<String>,
    },
    /// A control of the item only you use (resume what you paused, continue what you stopped).
    Control { action: &'static str },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Due {
    /// A registered wait's next look.
    Wait,
    /// The model's quota is back.
    Quota,
    /// The prerequisites close (registered, event-driven).
    Dependency,
    /// Queued input is sent once the session can take it.
    Queued,
    /// A Node command not sent yet: its next try.
    Dispatch,
    /// A Node command that may have reached the Node without its receipt: reconciled under the same
    /// operation id, never sent again under a new one.
    Reconcile,
    /// The Node is away: when the item becomes yours unless it is back.
    NodeBack,
    /// The stall watch's next look at a settled session (Steve's continue, or a card).
    StallWatch,
    /// A judged close becomes final.
    Grace,
    /// The settled turn's judge candidate is looked at (judged, or retried after a failure).
    Judge,
    /// The work graph's own scheduling.
    Workflow,
    /// A required operation of the closing (a release, a build, a deploy) is followed.
    Closing,
}

impl Due {
    /// Comes due at a time (as opposed to on a registered event).
    pub fn timed(self) -> bool {
        matches!(
            self,
            Due::Wait
                | Due::Quota
                | Due::Dispatch
                | Due::Reconcile
                | Due::NodeBack
                | Due::StallWatch
                | Due::Grace
                | Due::Judge
        )
    }
}

/// One of the three predicates: whether it holds, for which head, and what is missing.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct Predicate {
    pub holds: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub head: Option<String>,
    pub missing: Vec<&'static str>,
}

impl Predicate {
    fn of(head: Option<String>, missing: Vec<&'static str>) -> Self {
        Self {
            holds: missing.is_empty(),
            head,
            missing,
        }
    }
}

/// Closable, and on what.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct Closable {
    #[serde(flatten)]
    pub predicate: Predicate,
    /// `hard_facts` (everything required is verified) or `judgement` (a judge said done).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub by: Option<&'static str>,
    /// A judged close takes effect then (undoable until).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub after: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Derived {
    /// The row of the design that matched.
    pub row: u8,
    pub code: &'static str,
    pub lane: Lane,
    pub owner: Owner,
    /// What the row names (a card's question, what a wait is for, an operation).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    pub next_step: Option<NextStep>,
    pub next_at: Option<i64>,
    pub settled: Settled,
    pub reviewable: Predicate,
    pub mergeable: Predicate,
    pub closable: Closable,
}

/// The newest verified fact of `kind` in the current cycle about the current result: about its
/// commit when one is verified, else one tied to no head (as [`closable`] reads them). Its index in
/// [`Facts::delivery`], for the judge's evidence references.
pub fn result_fact(f: &Facts, kind: &str) -> Option<usize> {
    let head = latest(f, "commit", None)
        .filter(|d| d.verified())
        .and_then(|d| d.head.clone());
    f.delivery
        .iter()
        .enumerate()
        .filter(|(_, d)| d.cycle == f.item.cycle && d.kind == kind && d.head == head)
        .max_by_key(|(_, d)| d.observed_at)
        .map(|(i, _)| i)
}

/// Whether the turn of the current session can be judged now, and why not: [`Settled::Evaluable`]
/// only when the Hub-visible turn is quiescent and nothing else is going on that a judge would
/// answer over — no running turn, retry, dispatch without a receipt, queued input, Node away or
/// unsure runtime (as [`derive`] reads them), no unknown side effect, no card, wait, prerequisite,
/// quota wait, pause, Stop of yours or busy work graph. It never says the machine has no background
/// work; it says the Hub sees nothing more coming for this turn.
pub fn judge_gate(f: &Facts, policy: &WorkPolicy) -> (Settled, &'static str) {
    let (settled, busy) = settled(f, policy);
    if let Some((code, _)) = busy {
        return (settled, code);
    }
    if settled != Settled::Evaluable {
        return (settled, "node_away");
    }
    let Some(s) = &f.session else {
        return (Settled::Unknown, "no_session");
    };
    if s.turn.is_none() {
        return (Settled::Unknown, "no_turn");
    }
    if !f.unknown_effects.is_empty() {
        return (Settled::Unknown, "unknown_effects");
    }
    let open_wait = f
        .waits
        .iter()
        .any(|w| w.cycle == f.item.cycle && w.cap.is_none_or(|c| w.wakes < c));
    let busy = if f.item.paused {
        "paused"
    } else if s.held_by_you {
        "your_stop"
    } else if !f.cards.is_empty() {
        "card"
    } else if open_wait {
        "waiting"
    } else if f.dependency_wait {
        "dependency"
    } else if f.quota_until.is_some_and(|u| u > f.now) {
        "quota"
    } else if f.graph_busy.is_some() {
        "workflow"
    } else {
        return (Settled::Evaluable, "settled");
    };
    (Settled::Busy, busy)
}

/// The newest delivery fact of `kind` in the current cycle, about `head` when given.
fn latest<'a>(f: &'a Facts, kind: &str, head: Option<&str>) -> Option<&'a Delivered> {
    f.delivery
        .iter()
        .filter(|d| d.cycle == f.item.cycle && d.kind == kind)
        .filter(|d| head.is_none_or(|h| d.head.as_deref() == Some(h)))
        .max_by_key(|d| d.observed_at)
}

/// How settled the current session is, and what it waits for when it is not.
fn settled(f: &Facts, policy: &WorkPolicy) -> (Settled, Option<(&'static str, NextStep)>) {
    /* no session: there is no Hub-visible turn to be at rest, so nobody can tell (never settled) */
    let Some(s) = &f.session else {
        return (Settled::Unknown, None);
    };
    let turn = || NextStep::Turn {
        session_id: s.id.clone(),
        turn_id: s.turn.clone(),
    };
    if s.running {
        return (Settled::Busy, Some(("running", turn())));
    }
    for d in &s.dispatches {
        match d.state.as_str() {
            "accepted" | "active" => return (Settled::Busy, Some(("running", turn()))),
            "pending" if !d.maybe_sent => {
                return (
                    Settled::Busy,
                    Some((
                        "dispatching",
                        NextStep::Due {
                            kind: Due::Dispatch,
                            id: Some(d.operation_id.clone()),
                            at: Some(d.retry_at.max(f.now)),
                        },
                    )),
                );
            }
            /* it may have reached the Node and nothing came back: the same operation is asked for
             * again, never a new one */
            _ => {
                return (
                    Settled::Unknown,
                    Some((
                        "reconciling",
                        NextStep::Due {
                            kind: Due::Reconcile,
                            id: Some(d.operation_id.clone()),
                            at: Some(d.retry_at.max(f.now)),
                        },
                    )),
                );
            }
        }
    }
    if let Some(since) = s.offline_since {
        let back_by = since + policy.nobody_offline();
        if f.now >= back_by {
            return (Settled::Unknown, None);
        }
        return (
            Settled::Unknown,
            Some((
                "node_away",
                NextStep::Due {
                    kind: Due::NodeBack,
                    id: None,
                    at: Some(back_by),
                },
            )),
        );
    }
    if s.uncertain {
        return (Settled::Unknown, Some(("reconciling", turn())));
    }
    if s.queued > 0 {
        return (
            Settled::Busy,
            Some((
                "queued",
                NextStep::Due {
                    kind: Due::Queued,
                    id: None,
                    at: None,
                },
            )),
        );
    }
    (Settled::Evaluable, None)
}

/// The judgement that speaks for now: of this cycle, of the current session, of its newest turn and
/// of the current scope, and only while that turn is settled. Anything else is a late answer to an
/// old question (a judgement of the goal before you changed it included).
fn current_judgement<'a>(f: &'a Facts, settled: Settled) -> Option<&'a Judged> {
    let s = f.session.as_ref()?;
    let turn = s.turn.as_deref()?;
    if settled != Settled::Evaluable {
        return None;
    }
    f.judgements
        .iter()
        .filter(|j| {
            j.cycle == f.item.cycle
                && j.session_id == s.id
                && j.turn_id == turn
                && j.scope_revision == f.item.scope_revision
        })
        .max_by_key(|j| j.created)
}

fn reviewable(f: &Facts, settled: Settled) -> Predicate {
    let mut missing = vec![];
    if !f.item.code {
        return Predicate::of(None, vec!["not_code"]);
    }
    let commit = latest(f, "commit", None).filter(|d| d.verified());
    let head = commit.and_then(|d| d.head.clone());
    if head.is_none() {
        missing.push("commit");
    }
    /* the worktree's results are all in the commit under review */
    let tree = latest(f, "tree", head.as_deref());
    if !tree.is_some_and(|t| t.verified() && t.state == "committed") {
        missing.push("worktree_in_commit");
    }
    if !f.unknown_effects.is_empty() {
        missing.push("unknown_effects");
    }
    if settled != Settled::Evaluable {
        missing.push("not_settled");
    }
    Predicate::of(head, missing)
}

fn mergeable(f: &Facts, review: &Predicate) -> Predicate {
    let mut missing = vec![];
    if !review.holds {
        missing.push("reviewable");
    }
    let head = review.head.as_deref();
    if !latest(f, "pr", head).is_some_and(|p| p.verified() && p.state == "open") {
        missing.push("pr_open");
    }
    let checks: Vec<_> = {
        let mut latest_per: Vec<&Delivered> = vec![];
        for d in f
            .delivery
            .iter()
            .filter(|d| d.cycle == f.item.cycle && d.kind == "ci" && d.head.as_deref() == head)
        {
            match latest_per.iter_mut().find(|x| x.ref_ == d.ref_) {
                Some(x) if x.observed_at <= d.observed_at => *x = d,
                Some(_) => {}
                None => latest_per.push(d),
            }
        }
        latest_per
    };
    if checks.is_empty() || !checks.iter().all(|c| c.verified() && c.state == "passed") {
        missing.push("ci_passed");
    }
    /* your approval of this very head: approving another head, or one not tied to a head, is not it */
    if head.is_none()
        || !latest(f, "approval", head).is_some_and(|a| a.verified() && a.state == "approved")
    {
        missing.push("approval_of_head");
    }
    if latest(f, "review", head).is_some_and(|r| r.state == "blocking") {
        missing.push("review_blocking");
    }
    if f.merge_declined {
        missing.push("merge_declined");
    }
    Predicate::of(review.head.clone(), missing)
}

fn closable(f: &Facts, settled: Settled, judged: Option<&Judged>, policy: &WorkPolicy) -> Closable {
    let mut missing = vec![];
    if settled != Settled::Evaluable {
        missing.push("not_settled");
    }
    if !f.unknown_effects.is_empty() {
        missing.push("unknown_effects");
    }
    if !f.cards.is_empty() {
        missing.push("open_card");
    }
    let r = f.required;
    let head = latest(f, "commit", None)
        .filter(|d| d.verified())
        .and_then(|d| d.head.clone());
    /* what closes is about this result: of its commit when one is known, and while none is, only a
     * fact tied to no head (a fact of some head cannot be told to be this result's) */
    let fact = |kind: &str, state: &str| {
        f.delivery
            .iter()
            .filter(|d| d.cycle == f.item.cycle && d.kind == kind && d.head == head)
            .max_by_key(|d| d.observed_at)
            .is_some_and(|d| d.verified() && d.state == state)
    };
    if r.merge && !fact("pr", "merged") {
        missing.push("merged");
    }
    if r.build && !fact("build", "passed") {
        missing.push("build_passed");
    }
    if r.deploy && !fact("deploy", "live") {
        missing.push("deployed");
    }
    let hard_only = (r.merge || r.build || r.deploy) && !r.operate;
    if hard_only || !missing.is_empty() {
        let by = (hard_only && missing.is_empty()).then_some("hard_facts");
        return Closable {
            predicate: Predicate::of(head, missing),
            by,
            after: None,
        };
    }
    /* what is left (an operation somebody used, or a goal in words) only a judge can say */
    match judged {
        Some(j) if j.verdict == "done" => Closable {
            predicate: Predicate::of(head, vec![]),
            by: Some("judgement"),
            after: Some(j.created + policy.close_grace()),
        },
        _ => Closable {
            predicate: Predicate::of(head, vec!["judged_done"]),
            by: None,
            after: None,
        },
    }
}

struct Row {
    row: u8,
    code: &'static str,
    lane: Lane,
    owner: Owner,
    detail: Option<String>,
    next: Option<NextStep>,
}

fn row(
    row: u8,
    code: &'static str,
    lane: Lane,
    owner: Owner,
    detail: Option<String>,
    next: Option<NextStep>,
) -> Row {
    Row {
        row,
        code,
        lane,
        owner,
        detail,
        next,
    }
}

/// The derived state of an item, first match wins. Pure: the same facts give the same answer.
pub fn derive(f: &Facts, policy: &WorkPolicy) -> Derived {
    let (settled, busy) = settled(f, policy);
    let judged = current_judgement(f, settled);
    let review = reviewable(f, settled);
    let merge = mergeable(f, &review);
    let close = closable(f, settled, judged, policy);
    let r = rows(f, settled, busy, judged, &close);
    let next_at = match &r.next {
        Some(NextStep::Due { at, .. }) => *at,
        _ => None,
    };
    Derived {
        row: r.row,
        code: r.code,
        lane: r.lane,
        owner: r.owner,
        detail: r.detail,
        next_step: r.next,
        next_at,
        settled,
        reviewable: review,
        mergeable: merge,
        closable: close,
    }
}

fn rows(
    f: &Facts,
    settled: Settled,
    busy: Option<(&'static str, NextStep)>,
    judged: Option<&Judged>,
    close: &Closable,
) -> Row {
    use Lane::*;
    use Owner::*;
    let due = |kind, id: Option<String>, at| Some(NextStep::Due { kind, id, at });
    /* 1 */
    if f.item.phase == crate::cycle::CLOSED {
        return row(1, "done", Done, Nobody, None, None);
    }
    /* 2: a judged close, undoable until it takes effect */
    if close.by == Some("judgement")
        && let Some(after) = close.after
    {
        return row(
            2,
            "judged_done",
            Running,
            Hub,
            None,
            due(Due::Grace, judged.map(|j| j.id.clone()), Some(after)),
        );
    }
    /* 3: only what you can move on yourself */
    if let Some(c) = f.cards.iter().find(|c| !c.stop).or(f.cards.first()) {
        return row(
            3,
            "card",
            NeedsYou,
            You,
            Some(c.question.clone()),
            Some(NextStep::Card {
                decision_id: c.id.clone(),
            }),
        );
    }
    if f.item.paused {
        return row(
            3,
            "paused",
            NeedsYou,
            You,
            None,
            Some(NextStep::Control { action: "resume" }),
        );
    }
    if let Some(s) = &f.session
        && s.held_by_you
        && settled == Settled::Evaluable
    {
        return row(
            3,
            "your_stop",
            NeedsYou,
            You,
            None,
            Some(NextStep::Control { action: "continue" }),
        );
    }
    if f.merge_declined {
        return row(3, "merge_declined", NeedsYou, You, None, None);
    }
    if judged.is_some_and(|j| j.verdict == "done") && settled == Settled::Evaluable {
        /* judged done, but not every condition of an automatic close holds: confirm it */
        return row(3, "confirm_done", NeedsYou, You, None, None);
    }
    let waits: Vec<_> = f
        .waits
        .iter()
        .filter(|w| w.cycle == f.item.cycle && w.cap.is_none_or(|c| w.wakes < c))
        .collect();
    if let Some(w) = waits.iter().find(|w| w.on == "you") {
        /* a wait on you is a card or nothing: the Hub never wakes it by itself */
        return row(3, "waits_on_you", NeedsYou, You, Some(w.what.clone()), None);
    }
    /* 4: registered waits */
    if f.dependency_wait {
        return row(
            4,
            "dependency",
            Running,
            Hub,
            None,
            due(Due::Dependency, None, None),
        );
    }
    if let Some(until) = f.quota_until.filter(|u| *u > f.now) {
        return row(
            4,
            "quota",
            Running,
            Hub,
            None,
            due(Due::Quota, None, Some(until)),
        );
    }
    if let Some(w) = waits.first() {
        return row(
            4,
            "waiting",
            Running,
            Hub,
            Some(w.what.clone()),
            due(Due::Wait, Some(w.id.clone()), Some(w.next_check)),
        );
    }
    /* the Hub follows its closing operations once no turn is in the way: a settled one, or none at all
     * (no session is not a settled turn, but nothing of it can change the result either) */
    if (settled == Settled::Evaluable || f.session.is_none())
        && let Some(o) = f.closing_ops.first()
    {
        return row(
            4,
            "closing",
            Running,
            Hub,
            Some(o.label.clone()),
            due(Due::Closing, Some(o.id.clone()), None),
        );
    }
    /* 5: something is on its way */
    if let Some((code, next)) = busy {
        let owner = if matches!(next, NextStep::Turn { .. }) && code == "running" {
            Agent
        } else {
            Hub
        };
        return row(5, code, Running, owner, None, Some(next));
    }
    if let Some(why) = &f.graph_busy {
        return row(
            5,
            "workflow",
            Running,
            Agent,
            Some(why.clone()),
            due(Due::Workflow, None, None),
        );
    }
    /* 6: settled, and the current turn's judge candidate is due (in shadow: it is only recorded) */
    if settled == Settled::Evaluable
        && let (Some(j), Some(s)) = (&f.judging, &f.session)
        && j.cycle == f.item.cycle
        && j.session_id == s.id
        && s.turn.as_deref() == Some(j.turn_id.as_str())
    {
        return row(
            6,
            "judging",
            Running,
            Hub,
            None,
            due(
                Due::Judge,
                Some(j.id.clone()),
                Some(j.next_check.max(f.now)),
            ),
        );
    }
    if f.item.phase == crate::cycle::READY {
        return row(
            10,
            "not_started",
            NeedsYou,
            You,
            None,
            Some(NextStep::Control { action: "start" }),
        );
    }
    /* 9: nothing can bring it back */
    let Some(s) = &f.session else {
        return row(9, "nobody", NeedsYou, You, None, None);
    };
    if settled == Settled::Unknown {
        return row(9, "nobody", NeedsYou, You, None, None);
    }
    /* 7 / 8: settled, nothing delivered or asked */
    if s.watched && f.continues_spent < f.continues_cap {
        return row(
            7,
            "follow_up",
            Running,
            Hub,
            None,
            due(
                Due::StallWatch,
                None,
                Some((s.last_activity + f.stall_after).max(f.now)),
            ),
        );
    }
    if s.watched {
        return row(8, "stopped", NeedsYou, You, None, None);
    }
    row(11, "unknown", Running, Nobody, None, None)
}

/// Whether `d` names a next step that can actually move the item on, given the same facts: an
/// answerable open card, a persisted due action (timed, or registered for an event that is there),
/// a running turn the Hub can reconcile, or a control of yours. `Err` says what is missing.
pub fn check_next_step(f: &Facts, d: &Derived) -> Result<(), String> {
    if d.lane == Lane::Done {
        return Ok(());
    }
    let Some(next) = &d.next_step else {
        return Err(format!("row {} ({}) has no next step", d.row, d.code));
    };
    match next {
        NextStep::Card { decision_id } => f
            .cards
            .iter()
            .any(|c| &c.id == decision_id)
            .then_some(())
            .ok_or_else(|| format!("card {decision_id} is not open")),
        NextStep::Due { kind, at, .. } if kind.timed() => at
            .map(|_| ())
            .ok_or_else(|| format!("{kind:?} has no time")),
        NextStep::Due { kind, .. } => {
            let registered = match kind {
                Due::Dependency => f.dependency_wait,
                Due::Queued => f.session.as_ref().is_some_and(|s| s.queued > 0),
                Due::Workflow => f.graph_busy.is_some(),
                Due::Closing => !f.closing_ops.is_empty(),
                _ => false,
            };
            registered
                .then_some(())
                .ok_or_else(|| format!("{kind:?} is not registered"))
        }
        NextStep::Turn { session_id, .. } => f
            .session
            .as_ref()
            .filter(|s| &s.id == session_id && d.settled != Settled::Evaluable)
            .map(|_| ())
            .ok_or_else(|| format!("turn of {session_id} is not running")),
        NextStep::Control { .. } => (d.owner == Owner::You)
            .then_some(())
            .ok_or_else(|| "a control that is not yours".to_string()),
    }
}
