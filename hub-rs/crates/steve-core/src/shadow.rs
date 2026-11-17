//! The derived state in shadow (#327): the facts of an item gathered from the database and the
//! runtime ([`Tx::item_facts`]), run through [`crate::derive::derive`], and shown next to today's
//! state with whether the two agree. Read-only: nothing here writes, sends or decides.
//!
//! Delivery facts come from their own table and, until something writes it, from what the release
//! of the current cycle already knows (its pull request, its build, its deploy), each with the
//! provenance the Hub can vouch for. Your approval of a merge today is not tied to a head, so it is
//! shown as such and does not make anything mergeable.

use serde_json::{Value, json};
use steve_store::item_facts::ItemFacts;

use crate::cycle::{ACTIVE, CLOSED, CLOSING, OP_BUILD, OP_RELEASE};
use crate::derive::{
    Card, Delivered, Derived, Dispatch, Facts, ItemState, Judged, Lane, OpenOperation, Required,
    SessionState, VERIFIED, Wait, derive,
};
use crate::lifecycle::stop_card;
use crate::proof::OP_DEPLOY;
use crate::repo::Item;
use crate::work::{Work, held};
use crate::work_policy::WorkPolicy;
use crate::{Core, Result, Tx};

/// Where today's state puts the item, in the derived state's lanes (`None`: today cannot say).
fn legacy_lane(item: &Item, work: &Work) -> Option<Lane> {
    if item.phase == CLOSED {
        return Some(Lane::Done);
    }
    match work {
        Work::Running(_) | Work::Queued(_) | Work::Blocked(_) | Work::Until(_) => {
            Some(Lane::Running)
        }
        Work::User(_) | Work::Paused(_) | Work::Halted(_) | Work::Failed(_) => Some(Lane::NeedsYou),
        Work::Idle if item.phase == CLOSING => Some(Lane::Running),
        Work::Idle => None,
    }
}

/// What the release of the cycle already knows, as delivery facts.
fn release_facts(cycle: i64, ops: &[steve_store::ItemOperation]) -> Vec<Delivered> {
    let mut out = vec![];
    for o in ops {
        let ev = &o.evidence;
        let fact = |kind: &str, state: &str, provenance: &str, ref_: Option<String>| Delivered {
            cycle,
            kind: kind.into(),
            head: ev["head"].as_str().map(str::to_owned),
            ref_,
            state: state.into(),
            provenance: provenance.into(),
            observed_at: o.updated,
        };
        match o.kind.as_str() {
            OP_RELEASE => {
                if let Some(pr) = ev["pr"].as_u64() {
                    let state = if ev["pr_merged"] == json!(true) {
                        "merged"
                    } else {
                        match ev["pr_state"].as_str() {
                            Some("OPEN") => "open",
                            Some("CLOSED") => "closed",
                            Some("MERGED") => "merged",
                            _ => "unknown",
                        }
                    };
                    /* the Hub read it off the host itself */
                    let provenance = if state == "unknown" {
                        "unknown"
                    } else {
                        VERIFIED
                    };
                    out.push(fact("pr", state, provenance, Some(pr.to_string())));
                }
                if ev["pr_merge_ok"] == json!(true) || ev["pr_merge_declined"] == json!(true) {
                    let state = if ev["pr_merge_ok"] == json!(true) {
                        "approved"
                    } else {
                        "declined"
                    };
                    /* not tied to a head today: never your approval of a particular head */
                    let mut a = fact(
                        "approval",
                        state,
                        VERIFIED,
                        ev["pr_merge_by"].as_str().map(str::to_owned),
                    );
                    a.head = None;
                    out.push(a);
                }
            }
            OP_BUILD => {
                let (state, provenance) = match o.status.as_str() {
                    "succeeded" => ("passed", VERIFIED),
                    "failed" => ("failed", VERIFIED),
                    "unknown" => ("unknown", "unknown"),
                    _ => ("running", VERIFIED),
                };
                out.push(fact("build", state, provenance, None));
            }
            OP_DEPLOY => {
                let (state, provenance) = match o.status.as_str() {
                    "succeeded" => ("live", VERIFIED),
                    "failed" => ("failed", VERIFIED),
                    "unknown" => ("unknown", "unknown"),
                    _ => ("pending", VERIFIED),
                };
                out.push(fact("deploy", state, provenance, None));
            }
            _ => {}
        }
    }
    out
}

impl Tx<'_> {
    /// The facts the derived state of `item` is computed from, now.
    pub(crate) fn item_facts(&self, item: &Item, now: i64) -> Result<Facts> {
        let store = self.db().store();
        let facts: ItemFacts<'_> = store.item_facts();
        let ops = store.items().operations(&item.id, item.cycle)?;
        let mut delivery: Vec<Delivered> = facts
            .deliveries(&item.id, item.cycle)?
            .into_iter()
            .map(|d| Delivered {
                cycle: d.cycle,
                kind: d.kind,
                head: d.head,
                ref_: d.ref_,
                state: d.state,
                provenance: d.provenance,
                observed_at: d.observed_at,
            })
            .collect();
        delivery.extend(release_facts(item.cycle, &ops));
        let unknown_effects = ops
            .iter()
            .filter(|o| o.status == "unknown")
            .map(|o| o.id.clone())
            .collect();
        let closing_ops = if item.phase == CLOSING {
            ops.iter()
                .filter(|o| o.required && matches!(o.status.as_str(), "pending" | "running"))
                .map(|o| OpenOperation {
                    id: o.id.clone(),
                    kind: o.kind.clone(),
                    label: o.label.clone(),
                })
                .collect()
        } else {
            vec![]
        };
        let needs = self.needs_of(item)?;
        let required = Required {
            merge: self.done_rule(item)?.is_some(),
            build: needs.build,
            deploy: needs.deploy,
            operate: needs.operate,
        };
        let session = match self.active_session(&item.id)? {
            Some(s) => {
                let stop = store.sessions().stop(&s.id)?;
                let offline_since = match self.rt().node_offline(&s.id) {
                    Some(_) => Some(facts.node_last_seen(&s.id)?.unwrap_or(s.updated)),
                    None => None,
                };
                Some(SessionState {
                    turn: facts.latest_turn(&s.id)?.map(|t| t.turn_id),
                    running: self.busy(&s.id)
                        || matches!(s.status.as_str(), "running" | "starting"),
                    uncertain: self.rt().uncertain(&s.id),
                    queued: self.db().queued(&s.id)?.len(),
                    offline_since,
                    held_by_you: stop.as_ref().is_some_and(held),
                    last_activity: self.db().last_event_ts(&s.id)?.unwrap_or(0).max(s.updated),
                    dispatches: facts
                        .open_dispatches(&s.id)?
                        .into_iter()
                        .map(|d| Dispatch {
                            operation_id: d.operation_id,
                            state: d.state,
                            maybe_sent: d.maybe_sent,
                            receipt: d.receipt,
                            retry_at: d.retry_at,
                        })
                        .collect(),
                    watched: item.status == "run"
                        && item.phase == ACTIVE
                        && !item.archived
                        && !self.workflow_child(item)?,
                    id: s.id,
                })
            }
            None => None,
        };
        let cards = self
            .db()
            .decisions_of_item(&item.id)?
            .into_iter()
            .filter(|d| d.status == "open")
            .map(|d| Card {
                stop: stop_card(&d),
                id: d.id,
                question: d.question,
            })
            .collect();
        let graph_busy = match store.workflows().get(&item.id)? {
            Some(g) if !g.single_session() => self
                .graph_work(&g)?
                .filter(|w| matches!(w, Work::Running(_) | Work::Queued(_)))
                .and_then(|w| w.text().map(str::to_owned)),
            _ => None,
        };
        let merge_declined = ops
            .iter()
            .any(|o| o.kind == OP_RELEASE && o.evidence["pr_merge_declined"] == json!(true));
        Ok(Facts {
            now,
            item: ItemState {
                id: item.id.clone(),
                cycle: item.cycle,
                phase: item.phase.clone(),
                paused: item.status == "pause",
                code: item.branch.as_deref().is_some_and(|b| !b.is_empty()),
            },
            required,
            session,
            cards,
            waits: facts
                .waits_open(&item.id, item.cycle)?
                .into_iter()
                .map(|w| Wait {
                    id: w.id,
                    cycle: w.cycle,
                    on: w.waits_on,
                    what: w.what,
                    next_check: w.next_check,
                    wakes: w.wakes,
                    cap: w.cap,
                })
                .collect(),
            judgements: facts
                .judgements(&item.id, item.cycle)?
                .into_iter()
                .map(|j| Judged {
                    id: j.id,
                    cycle: j.cycle,
                    session_id: j.session_id,
                    turn_id: j.turn_id,
                    verdict: j.verdict,
                    created: j.created,
                })
                .collect(),
            delivery,
            unknown_effects,
            closing_ops,
            dependency_wait: self.dependency_waiting(&item.id)?,
            quota_until: self.st.quota_waits.get(&item.id).map(|w| w.until),
            graph_busy,
            merge_declined,
            continues_spent: self.takeover_count(&item.id)?,
            continues_cap: self.cfg().takeover_continues,
            stall_after: crate::stall::STALL_AFTER_MS,
        })
    }

    /// The derived state of a task item next to today's, for the item API and the debug view.
    /// `None` for a conversation item (it has no lifecycle to derive).
    pub(crate) fn shadow_view(&self, item: &Item) -> Result<Option<Value>> {
        if item.kind != "task" {
            return Ok(None);
        }
        let now = crate::util::now();
        let policy = WorkPolicy::with(&self.db().store().item_facts().policy_overrides()?);
        let facts = self.item_facts(item, now)?;
        let d = derive(&facts, &policy);
        let work = self.work(item)?;
        let today = legacy_lane(item, &work);
        Ok(Some(view(&d, item, &work, today, &policy)))
    }
}

fn view(d: &Derived, item: &Item, work: &Work, today: Option<Lane>, policy: &WorkPolicy) -> Value {
    let mut v = serde_json::to_value(d).unwrap_or(Value::Null);
    v["label"] = json!(label(d));
    v["shadow"] = json!(true);
    v["today"] = json!({
        "phase": item.phase, "status": item.status, "work": work.code(), "why": work.text(),
        "lane": today,
    });
    v["agrees"] = json!(today == Some(d.lane));
    if !policy.ignored.is_empty() {
        v["policy_ignored"] = json!(policy.ignored);
    }
    v
}

/// The row in words, in the reader's language.
pub fn label(d: &Derived) -> String {
    let detail = d.detail.clone().unwrap_or_default();
    match d.code {
        "done" => steve_i18n::t!("core.derive.done"),
        "judged_done" => steve_i18n::t!("core.derive.judged_done"),
        "card" => steve_i18n::t!("core.derive.card", question = detail),
        "paused" => steve_i18n::t!("core.derive.paused"),
        "your_stop" => steve_i18n::t!("core.derive.your_stop"),
        "merge_declined" => steve_i18n::t!("core.derive.merge_declined"),
        "confirm_done" => steve_i18n::t!("core.derive.confirm_done"),
        "waits_on_you" => steve_i18n::t!("core.derive.waits_on_you", what = detail),
        "dependency" => steve_i18n::t!("core.derive.dependency"),
        "quota" => steve_i18n::t!("core.derive.quota"),
        "waiting" => steve_i18n::t!("core.derive.waiting", what = detail),
        "closing" => steve_i18n::t!("core.derive.closing", what = detail),
        "running" => steve_i18n::t!("core.derive.running"),
        "dispatching" => steve_i18n::t!("core.derive.dispatching"),
        "reconciling" => steve_i18n::t!("core.derive.reconciling"),
        "node_away" => steve_i18n::t!("core.derive.node_away"),
        "queued" => steve_i18n::t!("core.derive.queued"),
        "workflow" => steve_i18n::t!("core.derive.workflow", what = detail),
        "not_started" => steve_i18n::t!("core.derive.not_started"),
        "nobody" => steve_i18n::t!("core.derive.nobody"),
        "follow_up" => steve_i18n::t!("core.derive.follow_up"),
        "stopped" => steve_i18n::t!("core.derive.stopped"),
        _ => steve_i18n::t!("core.derive.unknown"),
    }
}

impl Core {
    /// See [`Tx::shadow_view`].
    pub fn shadow_view(&self, item: &str) -> Result<Option<Value>> {
        let t = self.tx();
        let Some(i) = t.db().item(item)? else {
            return Ok(None);
        };
        t.shadow_view(&i)
    }
}
