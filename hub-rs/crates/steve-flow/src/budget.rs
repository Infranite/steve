//! Per-run budgets (schedules.md §6). A run's cost is the report's model cost, or for `item*` runs the
//! sum of the `sessions.cost` of the items linked `from` it. Over the limit: the item is paused and a
//! `fail` decision about the `flow_run` asks “continue once” (the limit doubles) or “stop”. One
//! decision per limit: only exceeding the doubled limit opens another.

use std::sync::Arc;

use rusqlite::{Connection, OptionalExtension, params};
use serde_json::{Value, json};
use steve_modules::decisions::{DecisionCx, DecisionHandler, choice_result};
use steve_modules::{ApiError, ApiResult};
use steve_store::{Decision, NewDecision, Tx};
use tokio::sync::{Notify, mpsc};

use crate::runs::{self, RunRow};
use crate::schedules;
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

/// The items linked `from` a run.
fn items_of(c: &Connection, ws: &str, run_id: &str) -> FlowResult<Vec<String>> {
    let mut st = c.prepare_cached(
        "SELECT subject_id FROM links WHERE workspace_id = ? AND subject_type = 'item' AND predicate = 'from' AND object_type = 'flow_run' AND object_id = ?",
    )?;
    let rows = st
        .query_map(params![ws, run_id], |r| r.get(0))?
        .collect::<rusqlite::Result<Vec<String>>>()?;
    Ok(rows)
}

/// The runs an item came from.
fn runs_of_item(c: &Connection, ws: &str, item_id: &str) -> FlowResult<Vec<String>> {
    let mut st = c.prepare_cached(
        "SELECT object_id FROM links WHERE workspace_id = ? AND subject_type = 'item' AND subject_id = ? AND predicate = 'from' AND object_type = 'flow_run'",
    )?;
    let rows = st
        .query_map(params![ws, item_id], |r| r.get(0))?
        .collect::<rusqlite::Result<Vec<String>>>()?;
    Ok(rows)
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
    cx.store().transaction(|tx| {
        let Some((ws, item)) = tx
            .query_row(
                "SELECT workspace_id, item_id FROM sessions WHERE id = ? AND item_id IS NOT NULL",
                [session_id],
                |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)),
            )
            .optional()?
        else {
            return Ok(None);
        };
        let mut opened = None;
        for run_id in runs_of_item(tx, &ws, &item)? {
            let Some(mut run) = runs::get(tx, &run_id)? else {
                continue;
            };
            let items = items_of(tx, &ws, &run_id)?;
            let mut cost = 0.0;
            for i in &items {
                cost += tx.query_row(
                    "SELECT coalesce(sum(cost), 0) FROM sessions WHERE item_id = ?",
                    [i],
                    |r| r.get::<_, f64>(0),
                )?;
            }
            if (cost - run.cost).abs() < f64::EPSILON {
                continue;
            }
            run.cost = cost;
            let limit = limit_of(&run);
            let asked = run.context.get(ASKED).and_then(Value::as_f64);
            if limit > 0.0 && cost > limit && asked != Some(limit) {
                let name = schedules::get_any(tx, &run.flow_id)?
                    .map(|s| s.name)
                    .unwrap_or_default();
                let d = steve_store::base::decisions::open(
                    tx,
                    &NewDecision {
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
                    },
                )?;
                run.context[ASKED] = json!(limit);
                opened = Some((ws.clone(), item.clone(), d.id));
            }
            runs::save(tx, &run)?;
        }
        Ok(opened)
    })
}

/// The `flow_run` decision handler. `continue_once` doubles the run's limit and queues the resume
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
    fn resolve(&self, tx: &Tx, cx: &DecisionCx, d: &Decision) -> ApiResult<Value> {
        let mut run = runs::get(tx, &d.subject_id)?.ok_or_else(|| ApiError::not_found("运行"))?;
        match cx.choice {
            CONTINUE_ONCE => {
                let limit = limit_of(&run);
                run.context[LIMIT] = json!(limit * 2.0);
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
        Ok(choice_result(cx))
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
