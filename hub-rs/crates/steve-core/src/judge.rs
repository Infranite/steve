//! The completion judge, in shadow (#335): whether an item's work is done once its turn is over,
//! from hard facts first and from a model only for what facts cannot settle. It **stores** its
//! verdict and what that verdict would trigger, and does nothing else: it never closes, reviews,
//! wakes or merges, and who may send, merge or close is unchanged.
//!
//! The steps, each with one advancer:
//! 1. **A candidate** per (item, cycle, current session, logical turn, scope revision), registered
//!    when a turn ends ([`Tx::judge_register`]) or by the sweep for a terminal turn that has none
//!    (a restart between the two loses nothing). It keeps the terminal attempt.
//! 2. **The gate** ([`crate::derive::judge_gate`]) after the debounce: still the current session
//!    and its newest turn, and evaluable — the Hub-visible turn is quiescent. Busy or unknown is
//!    looked at again later; another turn, session, cycle or scope supersedes the candidate.
//! 3. **A fixed input bundle** ([`Bundle`]): the original request's reference, the criteria with
//!    stable ids, kinds and versions, the final text and the end of the candidate's own turn and
//!    attempt (read from its events up to that attempt's end; `unknown` when that end was not kept,
//!    never another turn's), and the deliveries and evidence with their provenance.
//! 4. **Facts first** ([`fact_checks`]), criterion by criterion: a hard condition ([`HARD`]) is met
//!    or unmet only by a verified fact; a read that failed is unknown and stays unknown — the model
//!    never upgrades it ([`parse`]). Deterministic rules of the turn ([`ruled`]) decide what needs no
//!    model; a goal in words or an operation is always the model's, also when every hard condition
//!    is met. The model: one call without tools, outside the core lock, on the owner's "judge" model
//!    setting and by default not the worker's model ([`choose`]); the harness and model that
//!    reported back are recorded (`unknown` when they did not name themselves). Its output must be
//!    one of five verdicts with a status for every criterion id ([`parse`]); anything else is a
//!    failure, retried, given up after the cap.
//! 5. **Compare-and-swap**: the cycle, session, turn, scope, input sequence, evidence and human
//!    actions are read again before the verdict is written, and the cycle, logical turn, scope
//!    revision, input version and evidence version once more inside the write's own transaction
//!    ([`steve_store::item_facts::ItemFacts::candidate_judged`]); any change throws the result away.

use std::collections::HashSet;
use std::time::Duration;

use serde::Serialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use steve_store::item_facts::{Candidate, Judged, NewCandidate, NewJudgement, VERDICTS};

use crate::derive::{Facts, Required, Settled, judge_gate, result_fact};
use crate::gateway::{ThinkTools, Tier};
use crate::models::Choice;
use crate::repo::{Item, Session};
use crate::util::{js_head, now, uid};
use crate::work_policy::WorkPolicy;
use crate::{Core, Result, Tx};

/// The purpose the judge's model calls are booked and configured under (设置 · 模型).
pub const PURPOSE: &str = "judge";

/// What a criterion is checked against.
pub const KINDS: [&str; 5] = ["merge", "build", "deploy", "operated", "free"];

/// The kinds of the hard conditions: settled only by a verified fact about the current result.
/// What facts leave unknown stays unknown, whatever the model says. An actual operation is only
/// ever reported and a goal in words is no fact: those two are the model's to judge, always.
pub const HARD: [&str; 3] = ["merge", "build", "deploy"];

/// What the judge records for what nobody reported: a turn's end that was not kept, a harness or
/// a model that did not name itself.
pub const UNKNOWN: &str = "unknown";

/// Whether a criterion of `kind` is a hard condition ([`HARD`]).
pub fn is_hard(kind: &str) -> bool {
    HARD.contains(&kind)
}

/// One completion criterion: a stable id, the kind that says how it is checked, and the version
/// of the scope it belongs to.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Criterion {
    pub id: String,
    pub kind: &'static str,
    pub version: i64,
    pub text: String,
}

/// How a criterion stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    Met,
    Unmet,
    Unknown,
}

impl Status {
    fn parse(s: &str) -> Option<Status> {
        match s {
            "met" => Some(Status::Met),
            "unmet" => Some(Status::Unmet),
            "unknown" => Some(Status::Unknown),
            _ => None,
        }
    }
}

/// A criterion's status, on what evidence, decided by whom (`facts` or `model`; `none` while open).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Checked {
    pub id: String,
    pub status: Status,
    pub evidence: Vec<String>,
    pub by: &'static str,
    /// The fact's own state (`open`, `running`, `failed`…), when a fact decided it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub state: Option<String>,
}

/// One piece of evidence the judge may cite, by its reference in the bundle.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Evidence {
    #[serde(rename = "ref")]
    pub ref_: String,
    pub kind: String,
    pub state: String,
    /// `verified` (the Hub looked itself) | `reported` (the executor said so) | `unknown`
    pub provenance: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub head: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

/// The original request, by reference.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Request {
    /// `[kind, ref]` of where it came from (a message, a conversation).
    pub source: (String, String),
    pub title: String,
    /// The goal it was created with (its first version), else its title.
    pub text: String,
}

/// Everything the judge decides on, fixed when the candidate is claimed and compared again before
/// the verdict is written.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Bundle {
    pub item: String,
    pub cycle: i64,
    pub revision: i64,
    pub scope_revision: i64,
    pub session: String,
    pub turn: String,
    pub attempt: Option<String>,
    pub request: Request,
    pub criteria: Vec<Criterion>,
    /// The end of what the agent said in this turn after its last tool call, read from the events
    /// of its terminal attempt only (empty when [`Bundle::final_text_source`] is `unknown`).
    pub final_text: String,
    /// `attempt`: read from the candidate's terminal attempt's own events; `unknown`: that attempt's
    /// end was not kept (another attempt's end is the session's last), or its events cannot be told
    /// from another attempt's, so its words are not known.
    pub final_text_source: &'static str,
    /// How the candidate's attempt ended (`end_turn`, `cancelled`, `error`…; `unknown` when its end
    /// was not kept) and its error, if any.
    pub stop_reason: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// The agent delivered (`steve_deliver`) in this turn.
    pub delivered_this_turn: bool,
    pub evidence: Vec<Evidence>,
}

/// References a verdict may cite besides the evidence.
const REQUEST_REF: &str = "request";
const FINAL_TEXT_REF: &str = "final_text";

impl Bundle {
    fn refs(&self) -> HashSet<&str> {
        let mut r: HashSet<&str> = self.evidence.iter().map(|e| e.ref_.as_str()).collect();
        r.insert(REQUEST_REF);
        if !self.final_text.is_empty() {
            r.insert(FINAL_TEXT_REF);
        }
        r
    }
}

/// The criteria of an item today: the project's completion rule (a merge), the evidence its goal
/// asks for (a build, a deploy, an actual operation) and the goal in words. Every id is stable; each
/// carries the scope revision (the goal's version) it belongs to.
pub fn criteria(required: Required, goal: Option<(i64, &str)>, title: &str) -> Vec<Criterion> {
    let version = goal.map_or(0, |(v, _)| v);
    let mut out = vec![];
    let mut add = |id: &str, kind: &'static str, text: &str| {
        out.push(Criterion {
            id: id.into(),
            kind,
            version,
            text: text.into(),
        })
    };
    if required.merge {
        add(
            "merge",
            "merge",
            "the change is merged by the project's completion rule",
        );
    }
    if required.build {
        add("build", "build", "the build passes");
    }
    if required.deploy {
        add("deploy", "deploy", "the result runs where it is deployed");
    }
    if required.operate {
        add(
            "operated",
            "operated",
            "it was actually operated and seen to work",
        );
    }
    add("goal", "free", goal.map_or(title, |(_, t)| t));
    out
}

/// The evidence of the facts, in order: the delivery facts (`e1`, `e2`… by their index), then what
/// the executor reported — its delivery's checks (`c1`…) and outputs (`o1`…).
pub fn evidence(
    f: &Facts,
    checks: &[String],
    outputs: &[(String, Option<String>)],
) -> Vec<Evidence> {
    let mut out: Vec<Evidence> = f
        .delivery
        .iter()
        .enumerate()
        .map(|(i, d)| Evidence {
            ref_: format!("e{}", i + 1),
            kind: d.kind.clone(),
            state: d.state.clone(),
            provenance: d.provenance.clone(),
            head: d.head.clone(),
            detail: d.ref_.clone(),
        })
        .collect();
    out.extend(checks.iter().enumerate().map(|(i, c)| Evidence {
        ref_: format!("c{}", i + 1),
        kind: "check".into(),
        state: "reported".into(),
        provenance: "reported".into(),
        head: None,
        detail: Some(c.clone()),
    }));
    out.extend(outputs.iter().enumerate().map(|(i, (title, at))| Evidence {
        ref_: format!("o{}", i + 1),
        kind: "output".into(),
        state: "reported".into(),
        provenance: "reported".into(),
        head: None,
        detail: Some(match at {
            Some(at) => format!("{title} ({at})"),
            None => title.clone(),
        }),
    }));
    out
}

/// Every criterion, one by one, as facts have it: a hard condition ([`HARD`]) met or unmet only by a
/// verified fact about the current result (a read that failed, or nothing to read, is unknown, and
/// stays so). An actual operation is only ever reported by the executor and a goal in words is no
/// fact: both are unknown here and left to the model.
pub fn fact_checks(f: &Facts, criteria: &[Criterion]) -> Vec<Checked> {
    criteria
        .iter()
        .map(|c| {
            let (kind, met, unmet): (&str, &[&str], &[&str]) = match c.kind {
                "merge" => ("pr", &["merged"], &["open", "closed"]),
                "build" => ("build", &["passed"], &["failed", "running"]),
                "deploy" => ("deploy", &["live"], &["failed", "pending"]),
                _ => {
                    return Checked {
                        id: c.id.clone(),
                        status: Status::Unknown,
                        evidence: vec![],
                        by: "none",
                        state: None,
                    };
                }
            };
            let fact = result_fact(f, kind).map(|i| (i, &f.delivery[i]));
            let status = match fact {
                Some((_, d)) if d.provenance == crate::derive::VERIFIED => {
                    if met.contains(&d.state.as_str()) {
                        Status::Met
                    } else if unmet.contains(&d.state.as_str()) {
                        Status::Unmet
                    } else {
                        Status::Unknown
                    }
                }
                _ => Status::Unknown,
            };
            Checked {
                id: c.id.clone(),
                status,
                evidence: fact
                    .map(|(i, _)| vec![format!("e{}", i + 1)])
                    .unwrap_or_default(),
                by: if status == Status::Unknown {
                    "none"
                } else {
                    "facts"
                },
                state: fact.map(|(_, d)| d.state.clone()),
            }
        })
        .collect()
}

/// A fact's state that says the criterion is on its way rather than failed.
const UNDERWAY: [&str; 3] = ["open", "running", "pending"];

/// A verdict that needs no model: `Some((verdict, why, decided_by))`.
/// - a turn cancelled (not by you: your Stop holds the gate) says nothing of the work: unknown;
/// - a turn that failed did not finish it: not done;
/// - every criterion a hard condition, each settled by verified facts: done, waiting (one is
///   underway) or not done. A criterion the model judges (a goal in words, an operation) is never
///   skipped because the hard ones are settled: with one, this rule does not apply;
/// - nothing said (or its words not known) and nothing delivered in the turn: unknown;
/// - the judge's model switched off (`judge_model` = 0): unknown.
pub fn ruled(
    b: &Bundle,
    checked: &[Checked],
    model_on: bool,
) -> Option<(&'static str, &'static str, &'static str)> {
    if b.stop_reason == "cancelled" {
        return Some(("unknown", "cancelled", "rule"));
    }
    if b.stop_reason == "error" || b.error.as_deref().is_some_and(|e| !e.is_empty()) {
        return Some(("not_done", "turn_failed", "rule"));
    }
    let status = |c: &Criterion| {
        checked
            .iter()
            .find(|k| k.id == c.id)
            .filter(|k| k.by == "facts")
    };
    let by_facts: Option<Vec<&Checked>> = b
        .criteria
        .iter()
        .map(|c| status(c).filter(|_| is_hard(c.kind)))
        .collect();
    if let Some(hard) = by_facts.filter(|h| !h.is_empty()) {
        if hard.iter().all(|k| k.status == Status::Met) {
            return Some(("done", "hard_facts_met", "facts"));
        }
        let underway = hard.iter().all(|k| {
            k.status == Status::Met || k.state.as_deref().is_some_and(|s| UNDERWAY.contains(&s))
        });
        return Some(if underway {
            ("waiting", "hard_facts_underway", "facts")
        } else {
            ("not_done", "hard_facts_failed", "facts")
        });
    }
    if b.final_text.is_empty() && !b.delivered_this_turn {
        return Some((
            "unknown",
            if b.final_text_source == UNKNOWN {
                "reply_unknown"
            } else {
                "no_reply"
            },
            "rule",
        ));
    }
    if !model_on {
        return Some(("unknown", "model_off", "rule"));
    }
    None
}

/// The judge's instructions: the stable head of its prompt.
const INSTRUCTIONS: &str = r#"You judge whether a piece of delegated work is done. You only judge; you do not do the work.
You get one JSON bundle: the original request, the completion criteria (each with an id), the agent's final text of its last turn ("final_text_source" "unknown": it could not be read), how that turn ended ("unknown": not known), and evidence. Each piece of evidence has a "ref" and a "provenance": "verified" means the system checked it itself; "reported" means the agent only said so; "unknown" means nobody could tell.

Rules:
- Judge the original request, not the last question. Finishing one exchange is not finishing the task.
- Only what the evidence and the final text show counts. If you are not sure, it is not done.
- A criterion listed in "checked" is a hard condition the system checked itself against verified facts: repeat its status exactly. "unknown" there means nobody could verify it: it stays "unknown", whatever the final text or reported evidence says.
- Judge every other criterion yourself, each one, also when the hard conditions are met.
- "met" needs at least one reference: an evidence "ref", "final_text" or "request".
- verdict "done" only when every criterion is "met". "waiting" when the work waits on something outside the agent (a review, a build, another person). "needs_you" when only the owner can decide or answer. "not_done" when work is missing. "unknown" when you cannot tell.

Reply with one JSON object and nothing else:
{"verdict":"done|not_done|waiting|needs_you|unknown","criteria":[{"id":"<criterion id>","status":"met|unmet|unknown","evidence":["<ref>"]}],"reason":"<one sentence>"}
List every criterion id exactly once."#;

/// The judge's prompt: its instructions, then the bundle and what facts already settled.
pub fn prompt(b: &Bundle, checked: &[Checked]) -> String {
    /* every hard condition, unknown ones too: the model repeats them, it never settles them */
    let settled: Vec<Value> = checked
        .iter()
        .filter(|k| b.criteria.iter().any(|c| c.id == k.id && is_hard(c.kind)))
        .map(|k| json!({"id": k.id, "status": k.status, "evidence": k.evidence}))
        .collect();
    let body = json!({
        "request": b.request,
        "criteria": b.criteria,
        "checked": settled,
        "final_text": b.final_text,
        "final_text_source": b.final_text_source,
        "turn": {"stop_reason": b.stop_reason, "delivered": b.delivered_this_turn},
        "evidence": b.evidence,
    });
    crate::gateway::with_static(INSTRUCTIONS, &body.to_string())
}

/// What the model concluded, validated.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ModelVerdict {
    pub verdict: String,
    pub criteria: Vec<Checked>,
    pub reason: String,
}

/// The model's reply, strictly: one of the five verdicts, every criterion id exactly once with a
/// valid status, references that exist in the bundle, a met criterion with at least one, a hard
/// condition exactly as facts have it (an unknown one stays unknown: the model never upgrades it),
/// and `done` only when every criterion is met. `Err` says why it was refused.
pub fn parse(
    raw: &str,
    b: &Bundle,
    checked: &[Checked],
) -> std::result::Result<ModelVerdict, String> {
    let v = match crate::gateway::first_json(raw) {
        Ok(Some(v)) => v,
        Ok(None) => return Err("no_json".into()),
        Err(_) => return Err("bad_json".into()),
    };
    let verdict = v["verdict"].as_str().unwrap_or_default();
    if !VERDICTS.contains(&verdict) {
        return Err(format!("bad_verdict:{}", js_head(verdict, 40)));
    }
    let Some(list) = v["criteria"].as_array() else {
        return Err("no_criteria".into());
    };
    let refs = b.refs();
    let mut out: Vec<Checked> = vec![];
    for c in list {
        let id = c["id"].as_str().unwrap_or_default();
        if !b.criteria.iter().any(|k| k.id == id) {
            return Err(format!("unknown_criterion:{}", js_head(id, 40)));
        }
        if out.iter().any(|k| k.id == id) {
            return Err(format!("criterion_twice:{id}"));
        }
        let Some(status) = c["status"].as_str().and_then(Status::parse) else {
            return Err(format!("bad_status:{id}"));
        };
        let evidence: Vec<String> = match &c["evidence"] {
            Value::Null => vec![],
            Value::Array(a) => a
                .iter()
                .map(|r| r.as_str().map(str::to_owned).ok_or(format!("bad_ref:{id}")))
                .collect::<std::result::Result<_, _>>()?,
            _ => return Err(format!("bad_refs:{id}")),
        };
        if let Some(r) = evidence.iter().find(|r| !refs.contains(r.as_str())) {
            return Err(format!("unknown_ref:{}", js_head(r, 40)));
        }
        if status == Status::Met && evidence.is_empty() {
            return Err(format!("met_without_evidence:{id}"));
        }
        let hard = b.criteria.iter().any(|k| k.id == id && is_hard(k.kind));
        if hard
            && checked
                .iter()
                .find(|k| k.id == id)
                .is_none_or(|k| k.status != status)
        {
            return Err(format!("contradicts_facts:{id}"));
        }
        out.push(Checked {
            id: id.into(),
            status,
            evidence,
            by: "model",
            state: None,
        });
    }
    if let Some(missing) = b
        .criteria
        .iter()
        .find(|k| !out.iter().any(|c| c.id == k.id))
    {
        return Err(format!("missing_criterion:{}", missing.id));
    }
    if verdict == "done" && out.iter().any(|c| c.status != Status::Met) {
        return Err("done_with_open_criteria".into());
    }
    /* a hard condition stays as facts have it, with their evidence */
    for c in &mut out {
        if b.criteria.iter().any(|k| k.id == c.id && is_hard(k.kind))
            && let Some(k) = checked.iter().find(|k| k.id == c.id)
        {
            *c = k.clone();
        }
    }
    Ok(ModelVerdict {
        verdict: verdict.into(),
        criteria: out,
        reason: v["reason"]
            .as_str()
            .map(|r| js_head(r, 400).to_string())
            .unwrap_or_default(),
    })
}

/// What the verdict would trigger once the judge acts (it does not: in shadow this is only
/// stored). `done`: closed at once on verified hard facts; else a code item goes to its independent
/// review; a reopened item (a later cycle) is never closed by a model, it is yours to confirm; else
/// it closes after the undo window. `not_done`: Steve continues while it has continues left, then
/// it is yours. `unknown`: Steve follows up while it can, then yours. `waiting`: the wait is
/// followed. `needs_you`: a card.
pub fn would(
    verdict: &str,
    by_facts: bool,
    code: bool,
    cycle: i64,
    continues_left: bool,
) -> &'static str {
    match verdict {
        "done" if by_facts => "close",
        "done" if code => "review",
        "done" if cycle > 1 => "confirm",
        "done" => "close_after_grace",
        "not_done" if continues_left => "continue",
        "unknown" if continues_left => "follow_up",
        "waiting" => "wait",
        _ => "needs_you",
    }
}

/// Who ran the worker's turns.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Worker {
    pub harness: String,
    /// `None`: the harness's own default.
    pub model: Option<String>,
}

/// The judge's harness and models. An owner's own setting for the judge (`custom`) is taken as it
/// is. Without one, the purpose's resolution (Steve's model) minus the worker's model on the same
/// harness; when that leaves nothing, the harness's quick model or another model it offers that is
/// not the worker's. When there is no other, the resolution as it is (the judgement then records
/// that it was not independent).
pub fn choose(
    resolved: Choice,
    custom: bool,
    worker: &Worker,
    offered: &[String],
    quick: Option<String>,
) -> Choice {
    // the same adapter as the worker's (an id, not a name check)
    if custom || !resolved.harness.eq(&worker.harness) {
        return resolved;
    }
    let Some(mine) = worker.model.as_deref() else {
        /* the worker runs the harness's default: a named model is another one only when it is
         * not that default, which is not known here; the resolution stands */
        return resolved;
    };
    let models: Vec<String> = resolved
        .models
        .iter()
        .filter(|m| *m != mine)
        .cloned()
        .collect();
    if !models.is_empty() {
        return Choice { models, ..resolved };
    }
    let other = quick
        .filter(|q| q != mine)
        .or_else(|| offered.iter().find(|m| *m != mine).cloned());
    match other {
        Some(m) => Choice {
            models: vec![m],
            ..resolved
        },
        None => resolved,
    }
}

/// Whether the model that answered was another than the worker's: `yes`, `no`, or `unknown` (a
/// harness default on the worker's side, or a judge's harness or model that did not report itself:
/// its name is not known).
pub fn independent(harness: &str, model: &str, worker: &Worker) -> &'static str {
    let known = |v: &str| !v.is_empty() && v != UNKNOWN;
    match worker.model.as_deref() {
        Some(w) if known(model) && known(harness) => {
            if harness.eq(&worker.harness) && model == w {
                "no"
            } else {
                "yes"
            }
        }
        _ => "unknown",
    }
}

/// A hex sha-256, for fingerprints.
fn digest(text: &str) -> String {
    Sha256::digest(text.as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// A judging pass's work for the model, taken under the lock and carried out of it.
pub(crate) struct Prepared {
    pub(crate) candidate: Candidate,
    pub(crate) claim: String,
    pub(crate) workspace: String,
    pub(crate) bundle: Bundle,
    pub(crate) checked: Vec<Checked>,
    pub(crate) fingerprint: String,
    pub(crate) input_seq: i64,
    /// The evidence version ([`steve_store::item_facts::ItemFacts::evidence_version`]) the bundle
    /// was read on: the write compares it again in its own transaction.
    pub(crate) evidence: String,
    pub(crate) choice: Choice,
    pub(crate) worker: Worker,
    pub(crate) timeout: Duration,
}

/// What the model call gave: its text, and the harness and model that actually answered, as they
/// reported themselves ([`UNKNOWN`] when they did not).
pub(crate) struct Called {
    pub(crate) result: std::result::Result<String, String>,
    pub(crate) harness: String,
    pub(crate) model: String,
}

/// Where a candidate stands against the item now.
enum Current {
    /// It no longer stands for the current turn: why.
    Gone(&'static str),
    Still(Box<Item>, Session),
}

impl Tx<'_> {
    /// The scope revision of an item: the version of its goal (0: none), which a change of scope
    /// moves on.
    pub(crate) fn scope_revision(&self, item: &str) -> Result<i64> {
        Ok(self.db().goals(item)?.last().map_or(0, |g| g.version))
    }

    /// Registers the judge candidate of a turn that ended on `session` (its `attempt`): only for a
    /// task item that is worked on or closing, whose current session it is, and not a unit of a
    /// work graph (its graph judges it). Best effort: a failure is logged, never the turn end's.
    pub(crate) fn judge_register(&self, session: &Session, attempt: Option<&str>, by: &str) {
        if let Err(e) = self.judge_register_in(session, attempt, by) {
            tracing::warn!(session = %session.id, "judge candidate not registered: {e}");
        }
    }

    fn judge_register_in(
        &self,
        session: &Session,
        attempt: Option<&str>,
        by: &str,
    ) -> Result<bool> {
        let Some(item_id) = session.item_id.as_deref() else {
            return Ok(false);
        };
        let Some(item) = self.db().item(item_id)? else {
            return Ok(false);
        };
        if item.kind != "task"
            || !matches!(
                item.phase.as_str(),
                crate::cycle::ACTIVE | crate::cycle::CLOSING
            )
            || self.workflow_child(&item)?
            || self
                .active_session(item_id)?
                .is_none_or(|s| s.id != session.id)
        {
            return Ok(false);
        }
        let facts = self.db().store().item_facts();
        let turn = match attempt {
            Some(a) => facts.turn_of_attempt(a)?,
            None => None,
        };
        let turn = match turn {
            Some(t) => t,
            /* the attempt is not on the Node runtime's ledger: the newest finished turn */
            None => match facts.terminal_turn(&session.id)? {
                Some(t) => t.turn_id,
                None => return Ok(false),
            },
        };
        let policy = self.work_policy()?;
        Ok(facts.register_candidate(&NewCandidate {
            item_id,
            cycle: item.cycle,
            session_id: &session.id,
            turn_id: &turn,
            scope_revision: self.scope_revision(item_id)?,
            terminal_attempt: attempt,
            registered_by: by,
            next_check: now() + policy.settle_debounce(),
        })?)
    }

    pub(crate) fn work_policy(&self) -> Result<WorkPolicy> {
        Ok(WorkPolicy::with(
            &self.db().store().item_facts().policy_overrides()?,
        ))
    }

    /// The sweep: a terminal turn of an open item's current session that has no candidate gets one
    /// (its turn end was lost to a restart, or came before this build). Returns how many.
    pub(crate) fn judge_sweep(&self) -> Result<usize> {
        let facts = self.db().store().item_facts();
        let mut n = 0;
        for id in facts.open_task_items()? {
            let Some(s) = self.active_session(&id)? else {
                continue;
            };
            let Some(t) = facts.terminal_turn(&s.id)? else {
                continue;
            };
            let Some(item) = self.db().item(&id)? else {
                continue;
            };
            let scope = self.scope_revision(&id)?;
            if facts
                .candidate_of(&id, item.cycle, &s.id, &t.turn_id, scope)?
                .is_some()
            {
                continue;
            }
            if self.judge_register_in(&s, t.attempt.as_deref(), "sweep")? {
                n += 1;
            }
        }
        Ok(n)
    }

    /// Whether candidate `c` still stands for the item's current turn.
    fn judge_current(&self, c: &Candidate) -> Result<Current> {
        let Some(item) = self.db().item(&c.item_id)? else {
            return Ok(Current::Gone("item_gone"));
        };
        if item.phase == crate::cycle::CLOSED || item.cycle != c.cycle {
            return Ok(Current::Gone("cycle"));
        }
        let Some(s) = self
            .active_session(&item.id)?
            .filter(|s| s.id == c.session_id)
        else {
            return Ok(Current::Gone("session"));
        };
        let latest = self.db().store().item_facts().latest_turn(&s.id)?;
        if latest.is_none_or(|t| t.turn_id != c.turn_id) {
            return Ok(Current::Gone("turn"));
        }
        if self.scope_revision(&item.id)? != c.scope_revision {
            return Ok(Current::Gone("scope"));
        }
        Ok(Current::Still(Box::new(item), s))
    }

    /// The bundle of candidate `c` now, what facts settle of it, the fingerprint of everything the
    /// verdict rests on (the bundle and what you did: cards, the item's state), and the session's
    /// input sequence.
    fn judge_inputs(
        &self,
        c: &Candidate,
        item: &Item,
        s: &Session,
        f: &Facts,
        policy: &WorkPolicy,
    ) -> Result<(Bundle, Vec<Checked>, String, i64)> {
        let store = self.db().store();
        let goals = self.db().goals(&item.id)?;
        let goal = goals.last().map(|g| (g.version, g.text.as_str()));
        let criteria = criteria(f.required, goal, &item.title);
        let closing: Option<crate::cycle::Closing> = item
            .closing
            .clone()
            .and_then(|v| serde_json::from_value(v).ok());
        let checks = closing
            .as_ref()
            .map(|c| c.evidence.clone())
            .unwrap_or_default();
        let outputs: Vec<(String, Option<String>)> = store
            .artifacts()
            .list(&item.id)?
            .into_iter()
            .map(|a| (a.title, a.location))
            .collect();
        let evidence = evidence(f, &checks, &outputs);
        let checked = fact_checks(f, &criteria);
        let turn = store.item_facts().terminal_turn(&s.id)?;
        let since = turn.as_ref().map_or(i64::MAX, |t| t.created);
        let delivered_this_turn = self.db().timeline(&item.id)?.iter().any(|t| {
            t.session_id.as_deref() == Some(s.id.as_str())
                && t.text == crate::takeover::DELIVERED
                && t.ts >= since
        });
        /* how this candidate's attempt ended, and what it said: never another turn's or attempt's.
         * The session's last end is kept for its last attempt only; the words are the attempt's own
         * events (a retry's earlier attempt left its own), as their Node stream attributes them */
        let stop = store
            .sessions()
            .stop(&s.id)?
            .filter(|k| c.terminal_attempt.is_some() && k.attempt == c.terminal_attempt);
        let chars = policy.judge_text_chars();
        let start = match (&stop, c.terminal_attempt.as_deref()) {
            (Some(k), Some(a)) => {
                let until = k.seq.unwrap_or(0);
                store
                    .item_facts()
                    .attempt_start(&s.id, a, until)?
                    .map(|since| (until, since))
            }
            _ => None,
        };
        let (final_text, final_text_source) = match start {
            Some((until, since)) => (
                tail(&self.db().final_reply_of(&s.id, Some(until), since)?, chars),
                "attempt",
            ),
            None => (String::new(), UNKNOWN),
        };
        let first = goals.first().map_or(item.title.clone(), |g| g.text.clone());
        let bundle = Bundle {
            item: item.id.clone(),
            cycle: item.cycle,
            revision: item.revision,
            scope_revision: c.scope_revision,
            session: s.id.clone(),
            turn: c.turn_id.clone(),
            attempt: c.terminal_attempt.clone(),
            request: Request {
                source: item.source.clone(),
                title: item.title.clone(),
                text: js_head(&first, chars).to_string(),
            },
            criteria,
            final_text,
            final_text_source,
            stop_reason: stop
                .as_ref()
                .and_then(|k| k.stop_reason.clone())
                .filter(|r| !r.is_empty())
                .unwrap_or_else(|| UNKNOWN.into()),
            error: stop.and_then(|k| k.error),
            delivered_this_turn,
            evidence,
        };
        /* what you did meanwhile counts too: a card answered, the item paused, moved or reopened */
        let cards: Vec<(String, String)> = self
            .db()
            .decisions_of_item(&item.id)?
            .into_iter()
            .map(|d| (d.id, d.status))
            .collect();
        let human = json!({
            "status": item.status, "phase": item.phase, "revision": item.revision,
            "closing": item.closing, "cards": cards,
        });
        let fingerprint = digest(&format!(
            "{}\n{}",
            serde_json::to_string(&bundle).unwrap_or_default(),
            human
        ));
        let input_seq = store.item_facts().last_event_seq(&s.id)?;
        Ok((bundle, checked, fingerprint, input_seq))
    }

    /// One judging pass under the lock: the sweep, then the due candidates — superseded, put off
    /// (busy or unknown), decided by facts and rules, or claimed for the model (returned).
    pub(crate) fn judge_prepare(&self, at: i64) -> Result<Vec<Prepared>> {
        self.judge_sweep()?;
        /* what the sweep registered just now is due by its own clock */
        let at = at.max(now());
        let policy = self.work_policy()?;
        let facts = self.db().store().item_facts();
        let stale = at - policy.judge_claim_stale();
        let mut out = vec![];
        for mut c in facts.candidates_due(at, stale, policy.judge_looks_per_pass())? {
            /* the model calls of one pass are capped; the rest wait for the next pass */
            if out.len() >= policy.judge_per_pass() {
                break;
            }
            /* a pass that claimed it and never came back: its claim is taken back first */
            if c.state == "judging"
                && let Some(claim) = c.claim.clone()
            {
                facts.candidate_release(&c.id, &claim, Some("claim_stale"), false, at)?;
                c.state = "pending".into();
            }
            let (item, s) = match self.judge_current(&c)? {
                Current::Gone(why) => {
                    facts.candidate_supersede(&c.id, why)?;
                    continue;
                }
                Current::Still(item, s) => (item, s),
            };
            let f = self.item_facts(&item, at)?;
            let (settled, why) = judge_gate(&f, &policy);
            if settled != Settled::Evaluable {
                let reading = match settled {
                    Settled::Busy => "busy",
                    _ => "unknown",
                };
                facts.candidate_settled(&c.id, reading, why, at + policy.judge_recheck())?;
                continue;
            }
            let evidence = facts.evidence_version(&c.item_id, c.cycle)?;
            let (bundle, checked, fingerprint, input_seq) =
                self.judge_inputs(&c, &item, &s, &f, &policy)?;
            let claim = uid("claim");
            if !facts.candidate_claim(&c.id, &claim, input_seq, &fingerprint, stale)? {
                continue;
            }
            if let Some((verdict, why, by)) = ruled(&bundle, &checked, policy.judge_model()) {
                let w = would(
                    verdict,
                    by == "facts",
                    f.item.code,
                    item.cycle,
                    f.continues_spent < f.continues_cap,
                );
                let basis = json!({"reason": why, "criteria": checked, "bundle": bundle});
                let written = facts.candidate_judged(
                    &c.id,
                    &claim,
                    &evidence,
                    &NewJudgement {
                        item_id: &c.item_id,
                        cycle: c.cycle,
                        session_id: &c.session_id,
                        turn_id: &c.turn_id,
                        scope_revision: c.scope_revision,
                        verdict,
                        basis,
                        candidate_id: Some(&c.id),
                        decided_by: Some(by),
                        would: Some(w),
                        fingerprint: Some(&fingerprint),
                        ..Default::default()
                    },
                )?;
                if let Judged::Stale(why) = written {
                    self.judge_stale(&c, &claim, why, &policy, at)?;
                }
                continue;
            }
            let worker = Worker {
                harness: s.harness.clone(),
                model: s.model.clone().filter(|m| !m.is_empty()),
            };
            let models = self.c.models();
            let resolved = models.for_call(&item.workspace_id, PURPOSE, Tier::Smart);
            let custom = models.custom(&item.workspace_id, PURPOSE).is_some();
            let h = models.harness(&resolved.harness);
            let offered: Vec<String> = h
                .as_ref()
                .map(|h| h.models.iter().map(|m| m.id.clone()).collect())
                .unwrap_or_default();
            let quick = h.as_ref().and_then(|h| h.fast_model());
            let choice = choose(resolved, custom, &worker, &offered, quick);
            out.push(Prepared {
                candidate: c,
                claim,
                workspace: item.workspace_id.clone(),
                bundle,
                checked,
                fingerprint,
                input_seq,
                evidence,
                choice,
                worker,
                timeout: Duration::from_millis(u64::try_from(policy.judge_timeout()).unwrap_or(0)),
            });
        }
        Ok(out)
    }

    /// Writes what the model said for `p`, compare-and-swap: the candidate must still stand for the
    /// current turn and its inputs must be the same as when it was claimed, else the result is
    /// thrown away (superseded, or judged again on the new inputs). A failed call or an invalid
    /// reply counts as a failure. Returns the judgement's id when one was written.
    pub(crate) fn judge_commit(&self, p: Prepared, called: Called) -> Result<Option<String>> {
        let at = now();
        let policy = self.work_policy()?;
        let facts = self.db().store().item_facts();
        let c = &p.candidate;
        let (item, s) = match self.judge_current(c)? {
            Current::Gone(why) => {
                facts.candidate_supersede(&c.id, why)?;
                return Ok(None);
            }
            Current::Still(item, s) => (item, s),
        };
        let f = self.item_facts(&item, at)?;
        let (settled, why) = judge_gate(&f, &policy);
        if settled != Settled::Evaluable {
            /* something started meanwhile: the verdict is not for what is there now */
            facts.candidate_release(&c.id, &p.claim, None, false, at + policy.judge_recheck())?;
            let reading = if settled == Settled::Busy {
                "busy"
            } else {
                "unknown"
            };
            facts.candidate_settled(&c.id, reading, why, at + policy.judge_recheck())?;
            return Ok(None);
        }
        let (_, _, fingerprint, input_seq) = self.judge_inputs(c, &item, &s, &f, &policy)?;
        if fingerprint != p.fingerprint || input_seq != p.input_seq {
            /* judged again on the new inputs; counted, so inputs that never hold still cannot
             * keep the model busy */
            let failures = c.failures + 1;
            facts.candidate_release(
                &c.id,
                &p.claim,
                Some("inputs_changed"),
                failures >= policy.judge_failures(),
                at + policy.settle_debounce(),
            )?;
            return Ok(None);
        }
        let parsed = called.result.and_then(|raw| {
            parse(&raw, &p.bundle, &p.checked).map_err(|e| format!("invalid_output:{e}"))
        });
        let v = match parsed {
            Ok(v) => v,
            Err(e) => {
                let failures = c.failures + 1;
                let give_up = failures >= policy.judge_failures();
                facts.candidate_release(
                    &c.id,
                    &p.claim,
                    Some(js_head(&e, 300)),
                    give_up,
                    at + policy.judge_retry_after(failures),
                )?;
                return Ok(None);
            }
        };
        let w = would(
            &v.verdict,
            false,
            f.item.code,
            item.cycle,
            f.continues_spent < f.continues_cap,
        );
        let ind = independent(&called.harness, &called.model, &p.worker);
        let basis = json!({
            "reason": v.reason, "criteria": v.criteria, "bundle": p.bundle,
            "choice": p.choice, "failures": c.failures,
        });
        let written = facts.candidate_judged(
            &c.id,
            &p.claim,
            &p.evidence,
            &NewJudgement {
                item_id: &c.item_id,
                cycle: c.cycle,
                session_id: &c.session_id,
                turn_id: &c.turn_id,
                scope_revision: c.scope_revision,
                verdict: &v.verdict,
                basis,
                harness: Some(&called.harness),
                model: Some(&called.model),
                candidate_id: Some(&c.id),
                decided_by: Some("model"),
                would: Some(w),
                independent: Some(ind),
                fingerprint: Some(&p.fingerprint),
            },
        )?;
        Ok(match written {
            Judged::Written(id) => Some(id),
            Judged::Stale(why) => {
                self.judge_stale(c, &p.claim, why, &policy, at)?;
                None
            }
            Judged::Lost | Judged::Already => None,
        })
    }

    /// A verdict the write's own transaction found stale ([`Judged::Stale`]) was thrown away: a
    /// cycle, turn or scope that moved on supersedes the candidate; inputs or evidence that changed
    /// have it judged again on the new ones, counted as a failure (inputs that never hold cannot
    /// keep the model busy).
    fn judge_stale(
        &self,
        c: &Candidate,
        claim: &str,
        why: &'static str,
        policy: &WorkPolicy,
        at: i64,
    ) -> Result<()> {
        let facts = self.db().store().item_facts();
        match why {
            "inputs" | "evidence" => {
                let failures = c.failures + 1;
                facts.candidate_release(
                    &c.id,
                    claim,
                    Some(if why == "inputs" {
                        "inputs_changed"
                    } else {
                        "evidence_changed"
                    }),
                    failures >= policy.judge_failures(),
                    at + policy.settle_debounce(),
                )?;
            }
            _ => {
                facts.candidate_supersede(&c.id, why)?;
            }
        }
        Ok(())
    }

    /// The judge's part of the shadow view: the newest candidate of the cycle and its judgement.
    pub(crate) fn judge_view(&self, item: &Item, f: &Facts) -> Result<Value> {
        let facts = self.db().store().item_facts();
        let Some(c) = facts.candidates(&item.id, item.cycle)?.into_iter().next() else {
            return Ok(Value::Null);
        };
        let current = f
            .session
            .as_ref()
            .is_some_and(|s| s.id == c.session_id && s.turn.as_deref() == Some(c.turn_id.as_str()));
        let judgement = match &c.judgement_id {
            Some(id) => facts
                .judgements(&item.id, item.cycle)?
                .into_iter()
                .find(|j| &j.id == id)
                .map(|j| {
                    json!({
                        "id": j.id, "verdict": j.verdict, "would": j.would,
                        "decided_by": j.decided_by, "harness": j.harness, "model": j.model,
                        "independent": j.independent, "reason": j.basis["reason"],
                        "criteria": j.basis["criteria"], "created": j.created,
                    })
                }),
            None => None,
        };
        Ok(json!({
            "candidate": {
                "id": c.id, "state": c.state, "settled": c.settled, "why": c.settled_reason,
                "turn_id": c.turn_id, "session_id": c.session_id,
                "scope_revision": c.scope_revision, "registered_by": c.registered_by,
                "failures": c.failures, "last_error": c.last_error, "next_check": c.next_check,
            },
            "current": current,
            "judgement": judgement,
        }))
    }
}

/// Who answered a call, as they reported it: the harness the answer came back from and the model
/// that harness named itself ([`crate::gateway::Printed::reported_model`]) — never the choice that
/// was asked for. [`UNKNOWN`] for what did not report: no answer, a gateway that does not say its
/// harness, a harness that names no model.
pub(crate) fn answered_by(a: &crate::gateway::Attempt) -> (String, String) {
    let answered = a.result.as_ref().ok();
    let harness = Some(&a.harness)
        .filter(|h| answered.is_some() && !h.is_empty())
        .cloned()
        .unwrap_or_else(|| UNKNOWN.into());
    let model = answered
        .and_then(|r| r.reported_model.clone())
        .filter(|m| !m.is_empty())
        .unwrap_or_else(|| UNKNOWN.into());
    (harness, model)
}

/// The last `n` characters of `text` (all of it when shorter).
fn tail(text: &str, n: usize) -> String {
    let chars: Vec<char> = text.chars().collect();
    chars[chars.len().saturating_sub(n)..].iter().collect()
}

/// The shortest pause between two judging passes, whatever `judge_every` is set to: an override
/// of 0 must not make the watch spin on the lock.
const PASS_GAP_FLOOR: Duration = Duration::from_secs(1);

impl Core {
    /// The judge's own watch, apart from the stall watch: its model calls (up to `judge_per_pass`
    /// of `judge_timeout` each) never hold up stall cards, Release builds or pull request checks.
    /// One pass at a time, `judge_every` after the last one ended, so passes never overlap and a
    /// slow pass is not followed by a burst. Runs forever.
    pub async fn run_judge_watch(self) {
        loop {
            if let Err(e) = self.check_judges().await {
                tracing::warn!("judge pass {e}");
            }
            let c = self.clone();
            let every = match tokio::task::spawn_blocking(move || c.tx().work_policy()).await {
                Ok(Ok(p)) => p.judge_every(),
                _ => WorkPolicy::default().judge_every(),
            };
            let every = Duration::from_millis(u64::try_from(every).unwrap_or(0));
            tokio::time::sleep(every.max(PASS_GAP_FLOOR)).await;
        }
    }

    /// One judging pass (from [`Core::run_judge_watch`]): the facts and rules under the lock, the
    /// model calls outside it, one at a time, each verdict written compare-and-swap. Shadow: it
    /// stores verdicts and acts on none. Returns the judgements written.
    pub async fn check_judges(&self) -> Result<usize> {
        let c = self.clone();
        let prepared = tokio::task::spawn_blocking(move || c.tx().judge_prepare(now()))
            .await
            .map_err(|e| crate::error::msg(format!("judge pass: {e}")))??;
        let mut written = 0;
        for p in prepared {
            let called = self.judge_call(&p).await;
            let c = self.clone();
            let r = tokio::task::spawn_blocking(move || c.tx().judge_commit(p, called))
                .await
                .map_err(|e| crate::error::msg(format!("judge commit: {e}")))?;
            match r {
                Ok(Some(_)) => written += 1,
                Ok(None) => {}
                Err(e) => tracing::warn!("judge commit: {e}"),
            }
        }
        Ok(written)
    }

    /// The model call of `p`: no tools, on its own choice, booked under the judge's purpose and its
    /// item. Never under the core lock.
    pub(crate) async fn judge_call(&self, p: &Prepared) -> Called {
        let prompt = prompt(&p.bundle, &p.checked);
        let gw = self.gw_arc();
        let a = crate::stats::tagged(
            PURPOSE,
            &p.workspace,
            crate::stats::for_item(
                &p.bundle.item,
                crate::gateway::with_choice(
                    p.choice.clone(),
                    gw.attempt(&prompt, Tier::Smart, p.timeout, &ThinkTools::None, None),
                ),
            ),
        )
        .await;
        let (harness, model) = answered_by(&a);
        Called {
            result: a
                .result
                .map(|r| r.text)
                .map_err(|e| js_head(&e.to_string(), 300).to_string()),
            harness,
            model,
        }
    }
}
