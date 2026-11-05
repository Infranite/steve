//! Decision handlers of flows: `flow_step` (an `approve` node, or the approval the
//! engine asks for before an effect), `signal` (a `notify`: 知道了 / 转成事项). `flow_run` (budget,
//! and `retry`/`drop` after a restart) is [`crate::budget::FlowRunDecisions`]. Handlers run inside
//! the answer's transaction, so the run is only marked `running` there; driving it, and creating
//! items, are queued [`Followup`]s that run after the commit.

use std::sync::Arc;

use serde_json::{Value, json};
use steve_modules::decisions::{DecisionCx, DecisionHandler, choice_result};
use steve_modules::{ApiError, ApiResult};
use steve_store::base::ObjRef;
use steve_store::base::links::{self, NewLink};
use steve_store::{Decision, Tx};
use tokio::sync::{Notify, mpsc};

use crate::engine::{self, GATE_YES};
use crate::nodes::action::NOTIFY_OPTIONS;
use crate::ports::ItemRequest;
use crate::runs;
use crate::{FlowCx, FlowResult, Followup};

/// The run and node of a `flow_step` decision (`<run>:<node>`).
fn step_of(d: &Decision) -> ApiResult<(&str, &str)> {
    d.subject_id
        .split_once(':')
        .ok_or_else(|| ApiError::not_found("运行"))
}

/// `flow_step`: the label you choose is the branch; for an engine approval `执行` runs the step and
/// `不执行` ends the run there.
pub struct FlowStepDecisions {
    followups: mpsc::UnboundedSender<Followup>,
    wake: Arc<Notify>,
}

impl FlowStepDecisions {
    pub fn new(followups: mpsc::UnboundedSender<Followup>, wake: Arc<Notify>) -> Self {
        Self { followups, wake }
    }
}

impl DecisionHandler for FlowStepDecisions {
    fn resolve(&self, tx: &Tx, cx: &DecisionCx, d: &Decision) -> ApiResult<Value> {
        let (run_id, node) = step_of(d)?;
        let mut run = runs::get(tx, run_id)?
            .filter(|r| r.workspace_id == cx.ws)
            .ok_or_else(|| ApiError::not_found("运行"))?;
        let waiting = engine::wait_row(tx, run_id, node)?;
        let decision_of_wait: Option<String> = tx
            .query_row(
                "SELECT decision_id FROM flow_waits WHERE run_id = ? AND node_id = ?",
                rusqlite::params![run_id, node],
                |r| r.get(0),
            )
            .ok()
            .flatten();
        let Some((mode, matcher)) = waiting.filter(|_| decision_of_wait.as_deref() == Some(&d.id))
        else {
            return Err(ApiError::conflict("这一步已经不在等你了"));
        };
        if mode != "decision" {
            return Err(ApiError::conflict("这一步已经不在等你了"));
        }
        engine::release_held_outcome(&mut run);
        let resume = if matcher["kind"] == "gate" {
            json!({ "gate": cx.choice })
        } else {
            run.context["approved_by"] = json!(node);
            json!({ "output": { "choice": cx.choice, "text": cx.text }, "branch": cx.choice })
        };
        if !engine::resume_in(tx, run, node, resume)? {
            return Err(ApiError::conflict("这一步已经不在等你了"));
        }
        let _ = self.followups.send(Followup::Drive {
            run_id: run_id.into(),
            decision_id: d.id.clone(),
        });
        self.wake.notify_one();
        Ok(choice_result(cx))
    }

    /// The run has moved on: an answer cannot be taken back.
    fn before_undo(&self, _tx: &Tx, _d: &Decision) -> ApiResult<()> {
        Err(ApiError::rule("流程已经按你的选择继续了，不能撤销"))
    }

    /// An approval nobody gave in time ends the run.
    fn expire(&self, tx: &Tx, d: &Decision) -> ApiResult<()> {
        let (run_id, _) = step_of(d)?;
        if let Some(r) = runs::get(tx, run_id)? {
            engine::cancel(tx, &r, steve_store::now())?;
        }
        Ok(())
    }
}

/// `signal`: `done` (知道了) changes nothing; `to_item` (转成事项) makes an item from the event.
pub struct SignalDecisions {
    followups: mpsc::UnboundedSender<Followup>,
    wake: Arc<Notify>,
}

impl SignalDecisions {
    pub fn new(followups: mpsc::UnboundedSender<Followup>, wake: Arc<Notify>) -> Self {
        Self { followups, wake }
    }
}

impl DecisionHandler for SignalDecisions {
    fn resolve(&self, tx: &Tx, cx: &DecisionCx, d: &Decision) -> ApiResult<Value> {
        match cx.choice {
            c if c == NOTIFY_OPTIONS[0] => {}
            c if c == NOTIFY_OPTIONS[1] => {
                let exists: bool = tx.query_row(
                    "SELECT EXISTS (SELECT 1 FROM ext_events WHERE id = ? AND workspace_id = ?)",
                    rusqlite::params![d.subject_id, cx.ws],
                    |r| r.get(0),
                )?;
                if !exists {
                    return Err(ApiError::not_found("信号"));
                }
                let _ = self.followups.send(Followup::ToItem {
                    ws: cx.ws.into(),
                    event_id: d.subject_id.clone(),
                    decision_id: d.id.clone(),
                });
                self.wake.notify_one();
            }
            _ => return Err(ApiError::invalid("choice")),
        }
        Ok(choice_result(cx))
    }
}

/// Waits until the decision's answer is settled; `true` when it was resolved. Handlers queue their
/// follow-up inside the answer's transaction, which holds the store's writer until it commits or
/// rolls back, so reading the decision under the writer sees how it ended (a rolled-back answer
/// leaves it `open`).
pub(crate) async fn committed(cx: &FlowCx, decision_id: &str) -> bool {
    let id = decision_id.to_string();
    cx.blocking(move |s| {
        s.transaction(|tx| -> FlowResult<_> { Ok(steve_store::base::decisions::get(tx, &id)?) })
    })
    .await
    .ok()
    .flatten()
    .is_some_and(|d| d.status == "resolved")
}

/// Runs one follow-up after its decision committed.
pub(crate) async fn followup(cx: &FlowCx, f: Followup) {
    match f {
        Followup::Resume { .. } => crate::budget::followup(cx, f).await,
        Followup::Drive {
            run_id,
            decision_id,
        } => {
            if !committed(cx, &decision_id).await {
                return;
            }
            if let Err(e) = cx.exec.drive(cx, &run_id).await {
                tracing::warn!(run = %run_id, "resumed run: {e}");
            }
        }
        Followup::ToItem {
            ws,
            event_id,
            decision_id,
        } => {
            if !committed(cx, &decision_id).await {
                return;
            }
            if let Err(e) = to_item(cx, &ws, &event_id).await {
                tracing::warn!(event = %event_id, "转成事项 failed: {e}");
            }
        }
    }
}

/// “转成事项”: an item from the event's summary and text, linked `from` it and `raised_by` the
/// sender; the event's outcome becomes `item`.
pub async fn to_item(cx: &FlowCx, ws: &str, event_id: &str) -> FlowResult<String> {
    let ev = cx
        .store()
        .reader(|c| crate::ingest::event_json(c, event_id))?;
    let bot = cx.store().reader(|c| -> FlowResult<String> {
        let triage = match ev["connection_id"].as_str() {
            Some(conn) => crate::sources::settings(c, ws, conn)?.triage_bot,
            None => None,
        };
        let usable = |b: &str| -> FlowResult<bool> {
            Ok(crate::schedules::bot_of(c, ws, b)?.is_some_and(|(_, ok)| ok))
        };
        if let Some(b) = triage.filter(|b| usable(b).unwrap_or(false)) {
            return Ok(b);
        }
        rusqlite::OptionalExtension::optional(c.query_row(
            "SELECT id FROM bots WHERE main = 1 AND deleted = 0 ORDER BY updated LIMIT 1",
            [],
            |r| r.get::<_, String>(0),
        ))?
        .ok_or_else(|| ApiError::rule("没有主 Bot"))
    })?;
    let summary = ev["summary"].as_str().unwrap_or("").to_string();
    let text = ev["payload"]["text"].as_str().unwrap_or("");
    let goal = if text.is_empty() {
        summary.clone()
    } else {
        format!(
            "{summary}\n\n{}",
            text.chars().take(4000).collect::<String>()
        )
    };
    let label = crate::signals::source_label(ev["source"].as_str().unwrap_or("")).to_string();
    let item = cx
        .core
        .create_item(ItemRequest {
            ws: ws.into(),
            title: summary.chars().take(80).collect(),
            goal,
            bot,
            project: None,
            tag_ids: Vec::new(),
            source: (label, "sig".into()),
            propose: false,
            accept_auto: false,
            run_id: String::new(),
        })
        .await
        .map_err(|e| ApiError::rule(format!("建事项失败：{e}")))?;
    cx.store().transaction(|tx| -> FlowResult<()> {
        links::add(
            tx,
            ws,
            &NewLink::new(ObjRef::new("item", &item), "from", ObjRef::new("event", event_id), "sig"),
        )?;
        if let Some(p) = ev["actor_party_id"].as_str() {
            links::add(
                tx,
                ws,
                &NewLink::new(ObjRef::new("item", &item), "raised_by", ObjRef::new("party", p), "sig"),
            )?;
        }
        tx.execute(
            "UPDATE ext_events SET outcome = 'item', outcome_label = ?, work_item_id = ? WHERE id = ?",
            rusqlite::params![format!("转成了事项 {item}"), item, event_id],
        )?;
        Ok(())
    })?;
    cx.signals.signal(cx.store(), ws, event_id);
    Ok(item)
}

/// `retry` of an interrupted run: it goes on from the step it stopped at (your answer is the
/// approval for that step).
pub(crate) fn retry(tx: &Tx, run: &mut runs::RunRow) -> FlowResult<()> {
    if run.status != "failed" || run.cursor.is_none() {
        return Err(ApiError::conflict("这次运行已经不能重试了"));
    }
    let cur = run.cursor.clone().unwrap_or_default();
    run.context["approved"][&cur] = json!(true);
    if run
        .context
        .get("resume")
        .is_some_and(|r| r["node"] == json!(cur))
    {
        run.context["resume"]["gate"] = json!(GATE_YES);
    }
    run.status = "running".into();
    run.error = None;
    run.ended = None;
    run.outcome = None;
    run.outcome_label = String::new();
    runs::save(tx, run)
}
