//! Running a flow run (schedules.md §1): the [`RunExecutor`] trait the scheduler drives runs through,
//! and [`LinearExecutor`], which knows exactly the schedule graph `trigger(time) → action`. S2's
//! `GraphEngine` implements the same trait; the integrator swaps the registration, the data stays.

use std::time::{Duration, Instant};

use async_trait::async_trait;
use serde_json::{Value, json};
use steve_modules::ApiError;

use crate::nodes::action::{self, ActionOutcome};
use crate::runs::{self, NewStep, RunRow};
use crate::{FlowCx, FlowError};

/// A report whose model call failed is retried once, this much later.
pub const RETRY_AFTER_MS: i64 = 10 * 60 * 1000;
/// Retries per run.
pub const MAX_RETRIES: i64 = 1;

/// Where a run stands after [`RunExecutor::drive`] returns.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunStatus {
    Running,
    Waiting,
    Done,
    Failed,
    Skipped,
    Cancelled,
}

impl RunStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            RunStatus::Running => "running",
            RunStatus::Waiting => "waiting",
            RunStatus::Done => "done",
            RunStatus::Failed => "failed",
            RunStatus::Skipped => "skipped",
            RunStatus::Cancelled => "cancelled",
        }
    }

    pub fn parse(s: &str) -> Self {
        match s {
            "running" => RunStatus::Running,
            "waiting" => RunStatus::Waiting,
            "done" => RunStatus::Done,
            "skipped" => RunStatus::Skipped,
            "cancelled" => RunStatus::Cancelled,
            _ => RunStatus::Failed,
        }
    }
}

#[async_trait]
pub trait RunExecutor: Send + Sync {
    /// Runs from `run.cursor` until the run ends, waits or fails; every finished node is already
    /// stored when this returns. A run that is not `running` is left alone.
    async fn drive(&self, cx: &FlowCx, run_id: &str) -> Result<RunStatus, FlowError>;

    /// A new event matched the `wait(event)` node `node_id` of the waiting run `run_id` (by
    /// `match.source` / `match.type`; `match.when` is the executor's to check). S2's engine resumes
    /// the run; the default ignores it (schedule graphs have no `wait`).
    async fn resume_event(
        &self,
        cx: &FlowCx,
        run_id: &str,
        node_id: &str,
        event_id: &str,
    ) -> Result<RunStatus, FlowError> {
        let _ = (cx, node_id, event_id, run_id);
        Ok(RunStatus::Waiting)
    }

    /// [`Self::resume_event`]'s state change inside the transaction that stores the event (`event` is
    /// its `event_json`): `Some(true)` = the run is `running` again and only needs driving after the
    /// commit, `Some(false)` = not woken. `None` (the default): this executor resumes after the commit
    /// through `resume_event`, which a crash in between loses.
    fn wake_in(
        &self,
        tx: &steve_store::Tx,
        run_id: &str,
        node_id: &str,
        event: &Value,
    ) -> Result<Option<bool>, FlowError> {
        let _ = (tx, run_id, node_id, event);
        Ok(None)
    }
}

/// Executor for the two-node schedule graph.
#[derive(Debug, Default, Clone, Copy)]
pub struct LinearExecutor;

/// The graph of the version a run started on (later edits do not change running runs).
fn graph_of(c: &rusqlite::Connection, run: &RunRow) -> Result<Value, FlowError> {
    let g: Option<String> = c
        .query_row(
            "SELECT graph FROM flow_versions WHERE flow_id = ? AND version = ?",
            rusqlite::params![run.flow_id, run.flow_version],
            |r| r.get(0),
        )
        .ok();
    let g = match g {
        Some(g) => g,
        None => c.query_row(
            "SELECT graph FROM flows WHERE id = ?",
            [&run.flow_id],
            |r| r.get(0),
        )?,
    };
    Ok(serde_json::from_str(&g)?)
}

fn label_with_prefix(run: &RunRow, label: &str) -> String {
    let prefix = run
        .context
        .get("label_prefix")
        .and_then(Value::as_str)
        .unwrap_or("");
    format!("{prefix}{label}")
}

#[async_trait]
impl RunExecutor for LinearExecutor {
    async fn drive(&self, cx: &FlowCx, run_id: &str) -> Result<RunStatus, FlowError> {
        let _activity = cx.store().activity("schedule").await;
        cx.store()
            .admitted(async {
                let store = &cx.modules.store;

                let run = store
                    .reader(|c| runs::get(c, run_id))?
                    .ok_or_else(|| ApiError::not_found("运行"))?;
                if run.status != "running" {
                    return Ok(RunStatus::parse(&run.status));
                }
                let graph = store.reader(|c| graph_of(c, &run))?;
                let node = run.cursor.as_deref().and_then(|cur| {
                    graph["nodes"]
                        .as_array()?
                        .iter()
                        .find(|n| n["id"] == cur)
                        .cloned()
                });
                let started = cx.now();
                let t0 = Instant::now();
                let outcome = match &node {
                    None => ActionOutcome::Done {
                        output: json!({}),
                        outcome: "done",
                        label: String::new(),
                        item_id: None,
                        cost: 0.0,
                    },
                    Some(n) if n["kind"] == "action" => action::run(cx, &run, &n["config"]).await,
                    Some(n) => ActionOutcome::Fail {
                        error: format!(
                            "定时任务不支持「{}」节点",
                            n["kind"].as_str().unwrap_or("?")
                        ),
                        cost: 0.0,
                    },
                };
                let ms = t0.elapsed().as_secs_f64() * 1000.0;
                let node_id = node
                    .as_ref()
                    .and_then(|n| n["id"].as_str())
                    .unwrap_or("")
                    .to_string();
                let input = json!({ "at": run.at() });
                let now = cx.now();
                let status = store.transaction(|tx| -> Result<RunStatus, FlowError> {
                    let mut r =
                        runs::get(tx, run_id)?.ok_or_else(|| ApiError::not_found("运行"))?;
                    let still_ours = r.status == "running";
                    let (step_status, output, error) = match &outcome {
                        ActionOutcome::Done {
                            output,
                            outcome,
                            label,
                            item_id,
                            cost,
                        } => {
                            r.cost += cost;
                            if still_ours {
                                r.status = "done".into();
                                r.outcome = Some((*outcome).into());
                                r.outcome_label = label_with_prefix(&r, label);
                                r.work_item_id = item_id.clone().or(r.work_item_id.take());
                                r.cursor = None;
                                r.error = None;
                                r.ended = Some(now);
                            }
                            ("ok", output.clone(), None)
                        }
                        ActionOutcome::Retry { error, cost } => {
                            r.cost += cost;
                            if still_ours {
                                if r.attempts < MAX_RETRIES {
                                    r.status = "waiting".into();
                                    r.retry_at = Some(now + RETRY_AFTER_MS);
                                    r.attempts += 1;
                                } else {
                                    r.status = "failed".into();
                                    r.outcome_label = label_with_prefix(&r, error);
                                    r.ended = Some(now);
                                }
                                r.error = Some(error.clone());
                            }
                            ("failed", Value::Null, Some(error.as_str()))
                        }
                        ActionOutcome::Fail { error, cost } => {
                            r.cost += cost;
                            if still_ours {
                                r.status = "failed".into();
                                r.outcome_label = label_with_prefix(&r, error);
                                r.error = Some(error.clone());
                                r.ended = Some(now);
                            }
                            ("failed", Value::Null, Some(error.as_str()))
                        }
                    };
                    if !node_id.is_empty() {
                        runs::add_step(
                            tx,
                            &r.workspace_id,
                            run_id,
                            &NewStep {
                                node_id: &node_id,
                                status: step_status,
                                input: &input,
                                output: &output,
                                ms,
                                error,
                                started,
                            },
                        )?;
                    }
                    runs::save(tx, &r)?;
                    Ok(RunStatus::parse(&r.status))
                })?;
                if status == RunStatus::Waiting {
                    cx.wake();
                }
                Ok(status)
            })
            .await
    }
}

/// Default action timeout of the report's model call.
pub const REPORT_TIMEOUT: Duration = Duration::from_secs(90);
