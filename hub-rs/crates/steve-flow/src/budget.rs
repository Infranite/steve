//! Per-run budgets. A run's cost is the report's model cost, or for `item*` runs the
//! sum of the `sessions.cost` of the items linked `from` it. Over the limit: the item is paused and a
//! `fail` decision about the `flow_run` asks “continue once” (the limit becomes the larger of twice
//! the limit and twice what was spent, so one answer covers a run that overshot by far) or “stop”.
//! One decision per limit: only exceeding the raised limit opens another.

use std::sync::Arc;

use serde_json::{Value, json};
use steve_modules::decisions::{DecisionCx, DecisionHandler, DecisionOutcome, choice_result};
use steve_modules::{ApiError, ApiResult};
use steve_store::base::ObjRef;
use steve_store::base::links::Links;
use steve_store::{Decision, DecisionRows, FlowRows, NewDecision, RunRows, SessionEvidence, W};
use tokio::sync::{Notify, mpsc};

use crate::runs::{self, RunRow};
use crate::{FlowCx, FlowResult, Followup};

/// `subject_type` of decisions about a run (S2 adds its `retry`/`drop` choices to the same handler).
pub const SUBJECT: &str = "flow_run";
pub const CONTINUE_ONCE: &str = "continue_once";
pub const STOP: &str = "stop";

/// `context.budget_limit`: the run's limit in USD (0 = none); set from the schedule at start.
const LIMIT: &str = "budget_limit";
/// `context.budget_asked`: the limit a decision was already opened for.
const ASKED: &str = "budget_asked";

fn limit_of(r: &RunRow) -> f64 {
    r.context.get(LIMIT).and_then(Value::as_f64).unwrap_or(0.0)
}

/// `$0.50`.
pub fn usd(x: f64) -> String {
    format!("${x:.2}")
}

/// `steve_flow::on_session_cost(session)`: steve-core calls it after every `sessions.cost` update.
/// Recomputes the cost of the run the session's item came from and, over budget, pauses the item and
/// opens the decision. Returns the decision opened, if any. Errors are logged, never raised: cost
/// accounting must not break a session.
pub async fn on_session_cost(cx: &FlowCx, session_id: &str) -> Option<String> {
    match account(cx, session_id) {
        Ok(Some((ws, item, dec))) => {
            if let Err(e) = cx.core.pause_item(&ws, &item).await {
                tracing::warn!(item = %item, "budget pause failed: {e}");
            }
            Some(dec)
        }
        Ok(None) => None,
        Err(e) => {
            tracing::error!(session = %session_id, "budget accounting: {e}");
            None
        }
    }
}

fn account(cx: &FlowCx, session_id: &str) -> FlowResult<Option<(String, String, String)>> {
    cx.store().write(|w| {
        let Some((ws, item)) = SessionEvidence(w).item_scope(session_id)? else {
            return Ok(None);
        };
        let mut opened = None;
        let runs = Links(w).destinations(&ws, &ObjRef::new("item", &item), "from", "flow_run")?;
        for run_id in runs {
            let Some(mut run) = RunRows(w).get(&run_id)? else {
                continue;
            };
            let items: Vec<_> = Links(w)
                .sources(&ws, "item", "from", "flow_run", [run_id.as_str()])?
                .into_iter()
                .map(|(_, item, _)| item)
                .collect();
            let cost = SessionEvidence(w).cost_for_items(&ws, &items)?;
            if (cost - run.cost).abs() < f64::EPSILON {
                continue;
            }
            run.cost = cost;
            let limit = limit_of(&run);
            let asked = run.context.get(ASKED).and_then(Value::as_f64);
            if limit > 0.0 && cost > limit && asked != Some(limit) {
                let name = FlowRows(w)
                    .get(&run.flow_id)?
                    .filter(|flow| flow.kind == "schedule")
                    .map_or_else(String::new, |flow| flow.name);
                let d = DecisionRows(w).open(&NewDecision {
                    workspace_id: ws.clone(),
                    kind: "fail".into(),
                    subject_type: SUBJECT.into(),
                    subject_id: run.id.clone(),
                    item_id: Some(item.clone()),
                    session_id: None,
                    question: format!(
                        "「{name}」这次花了 {}，超过预算 {}，已暂停 {item}",
                        usd(cost),
                        usd(limit)
                    ),
                    draft: None,
                    options: Some(vec![CONTINUE_ONCE.into(), STOP.into()]),
                    requested_by: "steve".into(),
                    question_template: None,
                    source: Some(steve_store::DecisionSource::Revision {
                        revision: limit.to_string(),
                    }),
                })?;
                run.context[ASKED] = json!(limit);
                opened = Some((ws.clone(), item.clone(), d.id));
            }
            RunRows(w).save(&run)?;
        }
        Ok(opened)
    })
}

/// The limit after `continue_once`: `max(limit × 2, cost × 2)`.
pub fn raised_limit(limit: f64, cost: f64) -> f64 {
    (limit * 2.0).max(cost * 2.0)
}

/// The `flow_run` decision handler. `continue_once` raises the run's limit ([`raised_limit`]) and queues the resume
/// (steve-core can only be called after this transaction commits); `stop` leaves the item paused.
/// `retry` (after a restart interrupted an effectful step, [`crate::engine::recover`]) continues the
/// run from that step; `drop` leaves it failed.
pub struct FlowRunDecisions {
    followups: mpsc::UnboundedSender<Followup>,
    wake: Arc<Notify>,
}

impl FlowRunDecisions {
    pub fn new(followups: mpsc::UnboundedSender<Followup>, wake: Arc<Notify>) -> Self {
        Self { followups, wake }
    }
}

impl DecisionHandler for FlowRunDecisions {
    fn resolve(&self, tx: &mut W<'_>, cx: &DecisionCx, d: &Decision) -> ApiResult<DecisionOutcome> {
        let mut run = steve_store::RunRows(tx)
            .get(&d.subject_id)?
            .filter(|r| r.workspace_id == cx.ws)
            .ok_or_else(|| ApiError::not_found("运行"))?;
        match cx.choice {
            CONTINUE_ONCE => {
                let limit = limit_of(&run);
                run.context[LIMIT] = json!(raised_limit(limit, run.cost));
                runs::save(tx, &run)?;
                if let Some(item) = &d.item_id {
                    let _ = self.followups.send(Followup::Resume {
                        ws: cx.ws.into(),
                        item_id: item.clone(),
                        decision_id: d.id.clone(),
                    });
                    self.wake.notify_one();
                }
            }
            STOP => {}
            crate::engine::RETRY => {
                crate::decisions::retry(tx, &mut run)?;
                let _ = self.followups.send(Followup::Drive {
                    run_id: run.id.clone(),
                    decision_id: d.id.clone(),
                });
                self.wake.notify_one();
            }
            crate::engine::DROP => {}
            _ => return Err(ApiError::invalid("choice")),
        }
        Ok(choice_result(cx).into())
    }
}

/// Runs a follow-up once its decision is committed (a rolled-back answer leaves the decision open,
/// and nothing happens).
pub(crate) async fn followup(cx: &FlowCx, f: Followup) {
    #[allow(clippy::single_match)]
    match f {
        Followup::Resume {
            ws,
            item_id,
            decision_id,
        } => {
            if !crate::decisions::committed(cx, &decision_id).await {
                return;
            }
            if let Err(e) = cx.core.resume_item(&ws, &item_id).await {
                tracing::warn!(item = %item_id, "budget resume failed: {e}");
            }
        }
        _ => {}
    }
}
