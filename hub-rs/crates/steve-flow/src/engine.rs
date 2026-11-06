//! The run engine: [`GraphEngine`] walks a run's graph node by node.
//!
//! - After every node, one transaction writes the step (`flow_run_steps`) and moves `cursor` and
//!   `context` on; only then does the next node run, so a restarted Hub continues from `cursor`
//!   ([`recover`]).
//! - A waiting node (`wait`, `approve`, a gated effect, a waiting sub-flow) writes `flow_waits` and
//!   parks the run. Whatever wakes it (a decision, an event, the scheduler's clock, the end of a
//!   child run) stores `context.resume = {node, output, branch | gate | error}` and sets the run
//!   `running` again; the engine then finishes that node from the stored result.
//! - Effectful nodes (sending, paying, changing other systems) run only after an `approve` node on
//!   this run's path, with a grant (`grants`, `subject_type='flow'`), or after an approval the engine
//!   asks for itself (an `approve` decision about the step). Restarts never replay them; dry runs
//!   skip them.
//! - Dry runs (`dry_run=1`) choose `approve` and `bot` branches by `prefer`, pass `wait` at once,
//!   write nothing outside their own run rows and never touch the event's triage result.
//! - After a run stops (ends or waits) the event's result is settled ([`crate::signals::settle`]),
//!   and a finished child run resumes its parent.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use rusqlite::{Connection, OptionalExtension, params};
use serde_json::{Value, json};
use steve_modules::ApiError;
use steve_store::{NewDecision, Tx};

use crate::exec::{MAX_RETRIES, RETRY_AFTER_MS, RunExecutor, RunStatus};
use crate::graph::{Graph, OnFail};
use crate::nodes::approve::SUBJECT as STEP_SUBJECT;
use crate::nodes::{DRY_EFFECT, NodeOutcome, RunCx, WaitSpec, flow::MAX_DEPTH, skipped, wait};
use crate::runs::{self, NewRun, NewStep, RunRow};
use crate::{FlowCx, FlowError, FlowResult, flows, signals};

/// The executor for every flow, schedules included.
#[derive(Debug, Default, Clone, Copy)]
pub struct GraphEngine;

/// Steps one drive goes at most (graphs are acyclic and ≤ 60 nodes; this only guards bugs).
pub const MAX_STEPS: usize = 200;
/// A dry run is stopped after this long.
pub const DRY_RUN_LIMIT: Duration = Duration::from_secs(30);
/// Error of a run interrupted at an effectful node.
pub const INTERRUPTED: &str = crate::scheduler::INTERRUPTED;
/// The options of the approval the engine asks for before an ungated effect.
pub const GATE_YES: &str = "执行";
pub const GATE_NO: &str = "不执行";
/// The third option, offered when the step has a grant ([`crate::nodes::NodeKind::grant`]): runs
/// it now and writes the grant, bound to the run's flow version (an edit of the flow ends it).
pub const GATE_ALWAYS: &str = "此流程当前版本以后都执行";
/// `grants.until` of a flow grant: it ends with the version, not with time.
pub const FLOW_GRANT_UNTIL: i64 = 253_402_300_799_000;
/// `retry` / `drop` of the `fail` decision about an interrupted run.
pub const RETRY: &str = "retry";
pub const DROP: &str = "drop";

type BoxFut<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

#[async_trait]
impl RunExecutor for GraphEngine {
    async fn drive(&self, cx: &FlowCx, run_id: &str) -> Result<RunStatus, FlowError> {
        drive(cx, run_id).await
    }

    async fn resume_event(
        &self,
        cx: &FlowCx,
        run_id: &str,
        node_id: &str,
        event_id: &str,
    ) -> Result<RunStatus, FlowError> {
        let event = cx
            .store()
            .reader(|c| crate::ingest::event_json(c, event_id))?;
        let woke = cx
            .store()
            .transaction(|tx| wake_event_in(tx, run_id, node_id, &event))?;
        if !woke {
            return Ok(RunStatus::Waiting);
        }
        drive(cx, run_id).await
    }

    fn wake_in(
        &self,
        tx: &Tx,
        run_id: &str,
        node_id: &str,
        event: &Value,
    ) -> Result<Option<bool>, FlowError> {
        wake_event_in(tx, run_id, node_id, event).map(Some)
    }
}

/// The waiting run `run_id` resumed at its `wait(event)` node `node_id` by `event` when its `when`
/// holds: `running` again with the event as the node's output. `false` = not woken.
fn wake_event_in(tx: &Tx, run_id: &str, node_id: &str, event: &Value) -> FlowResult<bool> {
    let Some((mode, matcher)) = wait_row(tx, run_id, node_id)? else {
        return Ok(false);
    };
    if mode != "event" {
        return Ok(false);
    }
    let Some(run) = runs::get(tx, run_id)?.filter(|r| r.status == "waiting") else {
        return Ok(false);
    };
    if let Some(w) = matcher["when"].as_str().filter(|w| !w.trim().is_empty()) {
        let input = run.context.get("input").cloned().unwrap_or(Value::Null);
        let steps = run.context.get("steps").cloned().unwrap_or(Value::Null);
        let scope = crate::expr::Scope {
            event: Some(event),
            steps: Some(&steps),
            sender: None,
            input: Some(&input),
        };
        if !crate::expr::eval(w, &scope) {
            return Ok(false);
        }
    }
    resume_in(tx, run, node_id, json!({ "output": event }))
}

tokio::task_local! { static IN_FLOW: bool; }

tokio::task_local! {
    /// Held by each step's write, inside its blocking task, so a driver that stops waiting (a dry
    /// run's time limit) can wait for the write it left behind before it ends the run.
    static STEP_WRITES: Arc<tokio::sync::Mutex<()>>;
}

/// Drives a run, then settles its event and resumes its parent when it finished.
pub fn drive<'a>(cx: &'a FlowCx, run_id: &'a str) -> BoxFut<'a, FlowResult<RunStatus>> {
    Box::pin(async move {
        let work = async {
            let st = drive_inner(cx, run_id).await?;
            after(cx, run_id).await;
            Ok(st)
        };
        if IN_FLOW.try_with(|v| *v).unwrap_or(false) {
            return work.await;
        }
        let _activity = cx.store().activity("flow").await;
        cx.store().admitted(IN_FLOW.scope(true, work)).await
    })
}

/// After a drive: settle the event (real runs), resume a waiting parent (finished child runs).
async fn after(cx: &FlowCx, run_id: &str) {
    let Ok(Some(run)) = cx.store().reader(|c| runs::get(c, run_id)) else {
        return;
    };
    if !run.dry_run
        && let Some(evt) = &run.event_id
    {
        match cx.store().transaction(|tx| signals::settle(tx, evt)) {
            Ok(Some(ws)) => cx.signals.signal(cx.store(), &ws, evt),
            Ok(None) => {}
            Err(e) => tracing::warn!(run = run_id, "settling the event failed: {e}"),
        }
    }
    let ended = matches!(run.status.as_str(), "done" | "failed" | "cancelled");
    if let (true, Some(parent)) = (ended, &run.parent_run) {
        let resumed = cx.store().transaction(|tx| -> FlowResult<bool> {
            let node: Option<String> = tx
                .query_row(
                    "SELECT node_id FROM flow_waits WHERE run_id = ? AND mode = 'flow' AND json_extract(match, '$.child') = ?",
                    params![parent, run.id],
                    |r| r.get(0),
                )
                .optional()?;
            let (Some(node), Some(p)) = (node, runs::get(tx, parent)?) else {
                return Ok(false);
            };
            if p.status != "waiting" {
                return Ok(false);
            }
            let res = if run.status == "done" {
                json!({ "output": run.context.get("input").cloned().unwrap_or(Value::Null) })
            } else {
                let name = flows::name_of(tx, &run.flow_id)?;
                json!({ "error": format!("子流程「{name}」没有完成：{}", run.error.clone().unwrap_or_else(|| run.status.clone())) })
            };
            resume_in(tx, p, &node, res)
        });
        match resumed {
            Ok(true) => {
                if let Err(e) = drive(cx, parent).await {
                    tracing::warn!(run = %parent, "parent run: {e}");
                }
            }
            Ok(false) => {}
            Err(e) => tracing::warn!(run = %parent, "resuming the parent failed: {e}"),
        }
    }
}

/// The `(mode, match)` of a wait row.
pub(crate) fn wait_row(
    c: &Connection,
    run_id: &str,
    node_id: &str,
) -> FlowResult<Option<(String, Value)>> {
    Ok(c.query_row(
        "SELECT mode, match FROM flow_waits WHERE run_id = ? AND node_id = ?",
        params![run_id, node_id],
        |r| {
            let m: Option<String> = r.get(1)?;
            Ok((
                r.get::<_, String>(0)?,
                m.and_then(|m| serde_json::from_str(&m).ok())
                    .unwrap_or(Value::Null),
            ))
        },
    )
    .optional()?)
}

/// Ends the wait of `run` at `node` with `resume` (`{output, branch?}`, `{gate}` or `{error}`):
/// deletes the wait row, stores `context.resume`, sets the run `running`. `false` when the run was
/// not waiting there (someone else woke it first).
pub fn resume_in(tx: &Tx, mut run: RunRow, node: &str, resume: Value) -> FlowResult<bool> {
    if run.status != "waiting" || run.cursor.as_deref() != Some(node) {
        return Ok(false);
    }
    let n = tx.execute(
        "DELETE FROM flow_waits WHERE run_id = ? AND node_id = ?",
        params![run.id, node],
    )?;
    if n == 0 {
        return Ok(false);
    }
    let mut r = resume;
    r["node"] = json!(node);
    run.context["resume"] = r;
    run.status = "running".into();
    runs::save(tx, &run)?;
    Ok(true)
}

/// The graph a run walks: the dry run's working copy, else the version it started on.
fn graph_of(c: &Connection, run: &RunRow) -> FlowResult<Graph> {
    if run.dry_run
        && let Some(g) = run.context.get("graph").filter(|g| g.is_object())
    {
        return Graph::from_value(g).map_err(ApiError::rule);
    }
    let g = match flows::version_graph(c, &run.flow_id, run.flow_version)? {
        Some(g) => g,
        None => flows::get(c, &run.flow_id)?
            .map(|f| f.graph)
            .ok_or_else(|| ApiError::not_found("流程"))?,
    };
    Graph::from_value(&g).map_err(ApiError::rule)
}

fn sender_of(c: &Connection, ev: &Value) -> Value {
    let Some(party) = ev["actor_party_id"].as_str() else {
        return Value::Null;
    };
    let name: Option<String> = c
        .query_row("SELECT name FROM parties WHERE id = ?", [party], |r| {
            r.get(0)
        })
        .optional()
        .ok()
        .flatten();
    json!({ "name": name.unwrap_or_default(), "party_id": party, "account_id": ev["actor_account_id"] })
}

/// Is there a live grant for this effect? Only a grant for the run's flow version counts.
fn granted(c: &Connection, run: &RunRow, g: &crate::nodes::Grant, now: i64) -> FlowResult<bool> {
    Ok(c.query_row(
        "SELECT EXISTS (SELECT 1 FROM grants WHERE workspace_id = ? AND subject_type = 'flow' AND subject_id = ?
            AND capability = ? AND resource_type = ? AND resource_id IN (?, '*') AND revoked IS NULL AND until > ?
            AND json_extract(scope, '$.flow_version') = ?)",
        params![run.workspace_id, run.flow_id, g.capability, g.resource_type, g.resource_id, now, run.flow_version],
        |r| r.get(0),
    )?)
}

/// The grants an approval may write when you answer [`GATE_ALWAYS`]: `matcher.grants`.
pub fn gate_matcher(grants: &[crate::nodes::Grant]) -> Value {
    if grants.is_empty() {
        return json!({ "kind": "gate" });
    }
    let gs: Vec<Value> = grants
        .iter()
        .map(|g| json!({ "capability": g.capability, "resource_type": g.resource_type, "resource_id": g.resource_id }))
        .collect();
    json!({ "kind": "gate", "grants": gs })
}

/// The options of an approval the engine asks for: [`GATE_ALWAYS`] only when it can write a grant.
pub fn gate_options(grantable: bool) -> Vec<String> {
    let mut o = vec![GATE_YES.to_string()];
    if grantable {
        o.push(GATE_ALWAYS.to_string());
    }
    o.push(GATE_NO.to_string());
    o
}

/// Writes the grants of `matcher.grants` for the run's flow, bound to its version
/// (`scope = {"flow_version": n}`).
pub(crate) fn write_gate_grants(
    tx: &Tx,
    run: &RunRow,
    matcher: &Value,
    actor: &str,
    decision_id: &str,
    now: i64,
) -> FlowResult<usize> {
    let Some(gs) = matcher["grants"].as_array() else {
        return Ok(0);
    };
    let scope = json!({ "flow_version": run.flow_version }).to_string();
    let mut n = 0;
    for g in gs {
        let (Some(cap), Some(rt), Some(rid)) = (
            g["capability"].as_str(),
            g["resource_type"].as_str(),
            g["resource_id"].as_str(),
        ) else {
            continue;
        };
        tx.execute(
            "INSERT INTO grants (id, workspace_id, subject_type, subject_id, capability, resource_type, resource_id,
               scope, until, created_by, decision_id, revoked, created)
             VALUES (?, ?, 'flow', ?, ?, ?, ?, ?, ?, ?, ?, NULL, ?)",
            params![
                steve_store::uid("gr"),
                run.workspace_id,
                run.flow_id,
                cap,
                rt,
                rid,
                scope,
                FLOW_GRANT_UNTIL,
                actor,
                decision_id,
                now
            ],
        )?;
        n += 1;
    }
    Ok(n)
}

/// Approved by you for this run: an `approve` node on the path, or a per-step approval.
fn approved(run: &RunRow, node: &str) -> bool {
    run.context.get("approved_by").is_some_and(|v| !v.is_null())
        || run.context["approved"][node] == json!(true)
}

/// How the walk goes on after a node.
enum Step {
    Advance {
        output: Value,
        branch: Option<String>,
        to: Option<String>,
        status: &'static str,
    },
    Wait(Box<WaitSpec>),
    Retry(String),
    Fail(String),
    /// You did not approve the effect: the run ends here.
    Refused,
}

fn label_with_prefix(run: &RunRow, label: &str) -> String {
    let prefix = run
        .context
        .get("label_prefix")
        .and_then(Value::as_str)
        .unwrap_or("");
    format!("{prefix}{label}")
}

/// Turns a node outcome into a step along `g`.
fn step_of(g: &Graph, node: &crate::graph::Node, o: NodeOutcome) -> Step {
    match o {
        NodeOutcome::Next { output, branch } => {
            let to = g.next(&node.id, branch.as_deref()).map(|e| e.to.clone());
            Step::Advance {
                output,
                branch,
                to,
                status: "ok",
            }
        }
        NodeOutcome::Wait(w) => Step::Wait(w),
        NodeOutcome::Retry(e) => Step::Retry(e),
        NodeOutcome::Fail(e) => match node.on_fail() {
            OnFail::Stop => Step::Fail(e),
            OnFail::Continue => Step::Advance {
                output: json!({ "error": e }),
                branch: None,
                to: g.next(&node.id, None).map(|x| x.to.clone()),
                status: "failed",
            },
            OnFail::Branch(l) => Step::Advance {
                output: json!({ "error": e }),
                to: g.next(&node.id, Some(&l)).map(|x| x.to.clone()),
                branch: Some(l),
                status: "failed",
            },
        },
    }
}

async fn drive_inner(cx: &FlowCx, run_id: &str) -> FlowResult<RunStatus> {
    // the graph and the sender are fixed for a run (its version is pinned, a dry run's graph and
    // the event are in its context): loaded once, on the first step
    let mut loaded: Option<(Graph, Value)> = None;
    for _ in 0..MAX_STEPS {
        let run = cx
            .store()
            .reader(|c| runs::get(c, run_id))?
            .ok_or_else(|| ApiError::not_found("运行"))?;
        if run.status != "running" {
            return Ok(RunStatus::parse(&run.status));
        }
        let (graph, sender) = match &mut loaded {
            Some(x) => &*x,
            slot @ None => {
                let r = cx.store().reader(|c| -> FlowResult<_> {
                    let g = graph_of(c, &run)?;
                    let s = sender_of(c, run.context.get("event").unwrap_or(&Value::Null));
                    Ok((g, s))
                });
                match r {
                    Ok(x) => &*slot.insert(x),
                    Err(e) => {
                        end_failed(cx, &run, &e.to_string())?;
                        return Ok(RunStatus::Failed);
                    }
                }
            }
        };
        let Some(cursor) = run.cursor.clone() else {
            finish(cx, &run)?;
            return Ok(RunStatus::Done);
        };
        let Some(node) = graph.node(&cursor) else {
            end_failed(cx, &run, &format!("流程里没有节点「{cursor}」"))?;
            return Ok(RunStatus::Failed);
        };
        let Some(kind) = cx.nodes.get(&node.kind).cloned() else {
            end_failed(cx, &run, &format!("不认识的节点种类：{}", node.kind))?;
            return Ok(RunStatus::Failed);
        };
        let input = run
            .context
            .get("input")
            .cloned()
            .filter(|_| node.kind != "trigger")
            .unwrap_or(Value::Null);
        let started = cx.now();
        let t0 = Instant::now();
        let effectful = kind.effectful(&node.config);
        let resume = run
            .context
            .get("resume")
            .filter(|r| r["node"] == json!(cursor))
            .cloned();
        let mut rcx = RunCx {
            flow: cx,
            run: run.clone(),
            graph,
            node,
            dry_run: run.dry_run,
            sender: sender.clone(),
            effects: Default::default(),
        };
        let mut gate_ok = approved(&run, &cursor);
        let step = match resume {
            Some(r) if r["gate"] == GATE_NO => Step::Refused,
            Some(r) if r.get("error").is_some_and(|e| !e.is_null()) => step_of(
                graph,
                node,
                NodeOutcome::Fail(r["error"].as_str().unwrap_or("").to_string()),
            ),
            Some(r) if r["gate"] == GATE_YES => {
                gate_ok = true;
                rcx.run.context["approved"][&cursor] = json!(true);
                step_of(graph, node, kind.run(&mut rcx, &node.config, &input).await)
            }
            Some(r) => step_of(
                graph,
                node,
                NodeOutcome::Next {
                    output: r.get("output").cloned().unwrap_or(Value::Null),
                    branch: r["branch"].as_str().map(String::from),
                },
            ),
            None if run.dry_run => {
                let prefer: Vec<String> = run.context["prefer"]
                    .as_array()
                    .map(|a| {
                        a.iter()
                            .filter_map(|x| x.as_str().map(String::from))
                            .collect()
                    })
                    .unwrap_or_default();
                if effectful {
                    step_of(graph, node, NodeOutcome::next(skipped(DRY_EFFECT)))
                } else if node.kind == "approve" || node.kind == "bot" {
                    let e = graph.preferred(&cursor, &prefer);
                    let mut out = skipped("试运行时按预设的分支走");
                    out["branch"] = json!(e.map(|e| e.label.clone()));
                    Step::Advance {
                        output: out,
                        branch: e.map(|e| e.label.clone()),
                        to: e.map(|e| e.to.clone()),
                        status: "ok",
                    }
                } else if node.kind == "wait" {
                    let l = wait::pass_label(graph, node);
                    let to = graph
                        .outs(&cursor)
                        .find(|e| Some(&e.label) == l.as_ref())
                        .map(|e| e.to.clone());
                    Step::Advance {
                        output: skipped("试运行时不等待"),
                        branch: l,
                        to,
                        status: "ok",
                    }
                } else {
                    step_of(
                        graph,
                        node,
                        kind.dry_run(&mut rcx, &node.config, &input).await,
                    )
                }
            }
            None if effectful && !gate_ok => {
                let grant = kind.grant(&node.config);
                let now = cx.now();
                let ok = match &grant {
                    Some(g) => cx.store().reader(|c| granted(c, &run, g, now))?,
                    None => false,
                };
                if ok {
                    step_of(graph, node, kind.run(&mut rcx, &node.config, &input).await)
                } else {
                    let flow_name = cx.store().reader(|c| flows::name_of(c, &run.flow_id))?;
                    rcx.effects.outcome = Some("need".into());
                    rcx.effects.label = Some(format!("等你批准：{}", node.name()));
                    let note = kind.gate_note(&rcx, &node.config);
                    Step::Wait(Box::new(gate_wait(
                        &run,
                        &flow_name,
                        node,
                        &rcx.render_value(&node.config, &input),
                        note.as_deref(),
                        grant.as_ref(),
                    )))
                }
            }
            None => step_of(graph, node, kind.run(&mut rcx, &node.config, &input).await),
        };
        let ms = t0.elapsed().as_secs_f64() * 1000.0;
        // a named branch with no edge while the node has others: the run ends here, and says so
        let unrouted: Option<String> = match &step {
            Step::Advance {
                branch: Some(b),
                to: None,
                ..
            } if graph.outs(&cursor).next().is_some() => {
                tracing::info!(run = %run_id, node = %cursor, branch = %b, "flow branch without edge");
                Some(b.clone())
            }
            _ => None,
        };
        let effects = std::mem::take(&mut rcx.effects);
        let gate_mark = rcx.run.context["approved"][&cursor] == json!(true);
        let now = cx.now();
        let node_name = node.name().to_string();
        let run_id_owned = run_id.to_string();
        let held = match STEP_WRITES.try_with(Arc::clone) {
            Ok(m) => Some(m.lock_owned().await),
            Err(_) => None,
        };
        // on the blocking pool: the step's write may wait for the writer
        let status = cx
            .blocking(move |s| s.transaction(|tx| -> FlowResult<RunStatus> {
            let _held = held;
            let run_id = run_id_owned.as_str();
            let mut r = runs::get(tx, run_id)?.ok_or_else(|| ApiError::not_found("运行"))?;
            let ours = r.status == "running";
            if let Some(o) = r.context.as_object_mut() {
                o.remove("resume");
            }
            if gate_mark {
                r.context["approved"][&cursor] = json!(true);
            }
            r.cost += effects.cost;
            if let Some(o) = &effects.outcome {
                r.outcome = Some(o.clone());
            }
            if let Some(l) = &effects.label {
                r.outcome_label = label_with_prefix(&r, l);
            }
            if effects.item_id.is_some() {
                r.work_item_id = effects.item_id.clone();
            }
            for (k, v) in [
                ("decision_id", &effects.decision_id),
                ("reason", &effects.reason),
                ("handled_by", &effects.handled_by),
            ] {
                if let Some(v) = v {
                    r.context[k] = json!(v);
                }
            }
            let ws = r.workspace_id.clone();
            let add = |tx: &Tx, st: &str, out: &Value, branch: Option<&str>, err: Option<&str>| -> FlowResult<()> {
                let seq = runs::add_step(
                    tx,
                    &ws,
                    run_id,
                    &NewStep {
                        node_id: &cursor,
                        status: st,
                        input: &input,
                        output: out,
                        ms,
                        error: err,
                        started,
                    },
                )?;
                if let Some(b) = branch {
                    tx.execute(
                        "UPDATE flow_run_steps SET branch = ? WHERE run_id = ? AND seq = ?",
                        params![b, run_id, seq],
                    )?;
                }
                Ok(())
            };
            match &step {
                Step::Advance {
                    output,
                    branch,
                    to,
                    status,
                } => {
                    let err = output["error"].as_str().filter(|_| *status == "failed");
                    add(tx, status, output, branch.as_deref(), err)?;
                    if let Some(b) = &unrouted
                        && r.outcome_label.is_empty()
                        && effects.label.is_none()
                    {
                        r.outcome_label = label_with_prefix(&r, &format!("分支「{b}」没有连线，到「{node_name}」为止"));
                    }
                    r.context["steps"][&cursor] = output.clone();
                    r.context["input"] = output.clone();
                    if ours {
                        r.cursor = to.clone();
                        r.attempts = 0;
                        r.retry_at = None;
                        if to.is_none() {
                            r.status = "done".into();
                            r.ended = Some(now);
                            r.error = None;
                        }
                    }
                }
                // a run that ended while the node ran gets the step, not a decision or a wait
                Step::Wait(w) if !ours => {
                    add(tx, "waiting", &w.output, None, None)?;
                }
                Step::Wait(w) => {
                    let mut out = w.output.clone();
                    let mut decision_id: Option<String> = None;
                    if let Some(d) = &w.decision {
                        let mut d = d.clone();
                        d.subject_id = format!("{run_id}:{cursor}");
                        let opened = steve_store::base::decisions::open(tx, &d)?;
                        out["decision_id"] = json!(opened.id);
                        decision_id = Some(opened.id.clone());
                        r.context["decision_id"] = json!(opened.id);
                        if effects.outcome.is_some() {
                            r.context["held_outcome"] = json!(true);
                        }
                    }
                    add(tx, "waiting", &out, None, None)?;
                    tx.execute(
                        "INSERT OR REPLACE INTO flow_waits (run_id, node_id, workspace_id, mode, until, match, decision_id, created) VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
                        params![run_id, cursor, r.workspace_id, w.mode, w.until, w.matcher.to_string(), decision_id, now],
                    )?;
                    r.status = "waiting".into();
                }
                Step::Retry(e) => {
                    add(tx, "failed", &Value::Null, None, Some(e))?;
                    if ours {
                        if r.attempts < MAX_RETRIES {
                            r.status = "waiting".into();
                            r.retry_at = Some(now + RETRY_AFTER_MS);
                            r.attempts += 1;
                        } else {
                            r.status = "failed".into();
                            r.outcome_label = label_with_prefix(&r, e);
                            r.ended = Some(now);
                        }
                        r.error = Some(e.clone());
                    }
                }
                Step::Fail(e) => {
                    add(tx, "failed", &Value::Null, None, Some(e))?;
                    if ours {
                        r.status = "failed".into();
                        r.outcome_label = label_with_prefix(&r, e);
                        r.error = Some(e.clone());
                        r.ended = Some(now);
                    }
                }
                Step::Refused => {
                    add(tx, "skipped", &json!({ "refused": true }), Some(GATE_NO), None)?;
                    if ours {
                        r.status = "done".into();
                        r.cursor = None;
                        r.outcome = None;
                        r.outcome_label = format!("你没有批准「{node_name}」，没有执行");
                        r.ended = Some(now);
                    }
                }
            }
            runs::save(tx, &r)?;
            Ok(RunStatus::parse(&r.status))
        }))
            .await?;
        match status {
            RunStatus::Running => continue,
            RunStatus::Waiting => {
                cx.wake();
                return Ok(status);
            }
            s => return Ok(s),
        }
    }
    let run = cx.store().reader(|c| runs::get(c, run_id))?;
    if let Some(run) = run {
        end_failed(cx, &run, "流程走了太多步，已停止")?;
    }
    Ok(RunStatus::Failed)
}

/// The approval the engine asks for before an effect nobody approved.
fn gate_wait(
    run: &RunRow,
    flow_name: &str,
    node: &crate::graph::Node,
    cfg: &Value,
    note: Option<&str>,
    grant: Option<&crate::nodes::Grant>,
) -> WaitSpec {
    let grants: Vec<crate::nodes::Grant> = grant.into_iter().cloned().collect();
    let draft: String = serde_json::to_string_pretty(cfg)
        .unwrap_or_default()
        .chars()
        .take(2000)
        .collect();
    WaitSpec {
        mode: "decision",
        until: None,
        matcher: gate_matcher(&grants),
        decision: Some(NewDecision {
            workspace_id: run.workspace_id.clone(),
            kind: "approve".into(),
            subject_type: STEP_SUBJECT.into(),
            subject_id: String::new(),
            item_id: run.work_item_id.clone(),
            session_id: None,
            question: format!(
                "「{flow_name}」要执行「{}」，这是对外动作。要执行吗？{}",
                node.name(),
                note.unwrap_or("")
            ),
            draft: Some(draft),
            options: Some(gate_options(!grants.is_empty())),
            requested_by: "steve".into(),
        }),
        output: json!({ "waiting": "approval" }),
    }
}

fn finish(cx: &FlowCx, run: &RunRow) -> FlowResult<()> {
    let now = cx.now();
    cx.store().transaction(|tx| {
        let Some(mut r) = runs::get(tx, &run.id)?.filter(|r| r.status == "running") else {
            return Ok(());
        };
        r.status = "done".into();
        r.ended = Some(now);
        runs::save(tx, &r)
    })
}

fn end_failed(cx: &FlowCx, run: &RunRow, error: &str) -> FlowResult<()> {
    let now = cx.now();
    cx.store().transaction(|tx| {
        let Some(mut r) = runs::get(tx, &run.id)?.filter(|r| r.status == "running") else {
            return Ok(());
        };
        r.status = "failed".into();
        r.error = Some(error.into());
        r.outcome_label = label_with_prefix(&r, error);
        r.ended = Some(now);
        runs::save(tx, &r)
    })
}

/// Opens a run of `flow` at its trigger node. `context` gets `steps: {}`.
#[allow(clippy::too_many_arguments)]
pub fn open_run(
    tx: &Tx,
    f: &flows::FlowRow,
    trigger_kind: &str,
    mut context: Value,
    event_id: Option<&str>,
    dry_run: bool,
    parent: Option<&str>,
    depth: i64,
    now: i64,
) -> FlowResult<String> {
    let graph = if dry_run {
        context
            .get("graph")
            .filter(|g| g.is_object())
            .cloned()
            .unwrap_or_else(|| f.graph.clone())
    } else {
        f.graph.clone()
    };
    let trigger = Graph::from_value(&graph)
        .ok()
        .and_then(|g| g.trigger().map(|t| t.id.clone()))
        .ok_or_else(|| ApiError::rule(steve_i18n::t!("flow.graph.one_trigger")))?;
    if context.get("steps").is_none() {
        context["steps"] = json!({});
    }
    let id = runs::insert(
        tx,
        &NewRun {
            ws: &f.workspace_id,
            flow_id: &f.id,
            flow_version: f.version,
            trigger_kind,
            status: "running",
            outcome_label: "",
            cursor: Some(&trigger),
            context: &context,
            now,
        },
    )?;
    runs::set_origin(tx, &id, event_id, dry_run, parent, depth)?;
    Ok(id)
}

/// The `flow` node: runs the child inline; a waiting child parks the parent.
pub async fn call_child(cx: &mut RunCx<'_>, flow_id: &str, input: Value) -> NodeOutcome {
    if cx.run.depth + 1 > MAX_DEPTH {
        return NodeOutcome::Fail(format!("子流程嵌套太深（最多 {MAX_DEPTH} 层）"));
    }
    if flow_id == cx.run.flow_id {
        return NodeOutcome::Fail("流程不能调用它自己".into());
    }
    let ws = cx.ws().to_string();
    let now = cx.now();
    let (parent_id, depth, dry, event_id) = (
        cx.run.id.clone(),
        cx.run.depth + 1,
        cx.dry_run,
        cx.run.event_id.clone(),
    );
    let event = cx.event().clone();
    let opened = cx
        .flow
        .store()
        .transaction(|tx| -> FlowResult<(String, String)> {
            let f = flows::own(tx, &ws, flow_id).map_err(|_| ApiError::rule("没有这个子流程"))?;
            let id = open_run(
                tx,
                &f,
                "flow",
                json!({ "event": event, "input": input }),
                event_id.as_deref(),
                dry,
                Some(&parent_id),
                depth,
                now,
            )?;
            Ok((id, f.name))
        });
    let (child, name) = match opened {
        Ok(x) => x,
        Err(e) => return NodeOutcome::Fail(e.to_string()),
    };
    let st = match drive_inner(cx.flow, &child).await {
        Ok(s) => s,
        Err(e) => return NodeOutcome::Fail(format!("子流程「{name}」出错：{e}")),
    };
    let row = cx
        .flow
        .store()
        .reader(|c| runs::get(c, &child))
        .ok()
        .flatten();
    match (st, row) {
        (RunStatus::Done, Some(r)) => {
            NodeOutcome::next(r.context.get("input").cloned().unwrap_or(Value::Null))
        }
        (RunStatus::Waiting, _) => NodeOutcome::Wait(Box::new(WaitSpec {
            mode: "flow",
            until: None,
            matcher: json!({ "child": child }),
            decision: None,
            output: json!({ "waiting": "flow", "run_id": child }),
        })),
        (_, r) => NodeOutcome::Fail(format!(
            "子流程「{name}」没有完成：{}",
            r.and_then(|r| r.error).unwrap_or_default()
        )),
    }
}

/// `POST /flows/:id/dry-run`: a dry run on `event` (or `{at}`), with `prefer` and an optional
/// working copy of the graph. Driven inline for at most 30 s. Returns the run id.
/// `event_id`: the stored event the dry run was started from (`None` for a sample); it is kept on
/// the run so the event's runs and the flow's history can show it, but never settles the event.
pub async fn dry_run(
    cx: &FlowCx,
    f: &flows::FlowRow,
    event: Value,
    event_id: Option<&str>,
    prefer: Vec<String>,
    graph: Option<Value>,
) -> FlowResult<String> {
    let now = cx.now();
    let mut ctx = json!({ "prefer": prefer, "at": now });
    if !event.is_null() {
        ctx["event"] = event;
    }
    if let Some(g) = graph {
        ctx["graph"] = g;
    }
    let id = cx
        .store()
        .transaction(|tx| open_run(tx, f, "dry_run", ctx, event_id, true, None, 0, now))?;
    drive_dry(cx, &id, DRY_RUN_LIMIT).await?;
    // a dry run never stays waiting (waits pass, approvals are simulated); a waiting sub-flow can't
    cx.store().transaction(|tx| -> FlowResult<()> {
        if let Some(mut r) =
            runs::get(tx, &id)?.filter(|r| r.status == "waiting" || r.status == "running")
        {
            tx.execute("DELETE FROM flow_waits WHERE run_id = ?", [&id])?;
            r.status = "done".into();
            r.ended = Some(cx.now());
            runs::save(tx, &r)?;
        }
        Ok(())
    })?;
    Ok(id)
}

/// Drives dry run `id` for at most `limit`, then ends it as failed if it is still running. A step
/// write the time limit cut off still commits; it is waited for first, so it cannot land on the
/// run after it was ended.
async fn drive_dry(cx: &FlowCx, id: &str, limit: Duration) -> FlowResult<()> {
    let writes = Arc::new(tokio::sync::Mutex::new(()));
    let driven = STEP_WRITES.scope(writes.clone(), drive_inner(cx, id));
    match tokio::time::timeout(limit, driven).await {
        Ok(r) => {
            r?;
        }
        Err(_) => {
            drop(writes.lock().await);
            if let Some(run) = cx.store().reader(|c| runs::get(c, id))? {
                end_failed(cx, &run, "试运行超过 30 秒，已停止")?;
            }
        }
    }
    Ok(())
}

/// `POST /runs/:id/cancel`: a running or waiting run → `cancelled`; its waits go. `false` when it
/// had already ended.
pub fn cancel(tx: &Tx, run: &RunRow, now: i64) -> FlowResult<bool> {
    if run.status != "running" && run.status != "waiting" {
        return Ok(false);
    }
    let mut r = run.clone();
    tx.execute("DELETE FROM flow_waits WHERE run_id = ?", [&r.id])?;
    r.status = "cancelled".into();
    r.ended = Some(now);
    r.retry_at = None;
    runs::save(tx, &r)?;
    if let Some(e) = &r.event_id
        && !r.dry_run
    {
        signals::settle(tx, e)?;
    }
    Ok(true)
}

/// Waits due by the clock (`delay`, `until`, event timeouts): resumes them and returns the runs to
/// drive. Called by every scheduler round. A wait that cannot be resumed is logged and skipped, so
/// the ones resumed before it are still driven.
pub fn claim_due_waits(cx: &FlowCx) -> Vec<String> {
    let now = cx.now();
    let due = cx.store().reader(|c| -> FlowResult<Vec<(String, String, String, Value)>> {
        let mut st = c.prepare_cached(
            "SELECT w.run_id, w.node_id, w.mode, w.match FROM flow_waits w JOIN flow_runs r ON r.id = w.run_id
              WHERE w.until IS NOT NULL AND w.until <= ? AND w.mode IN ('delay', 'until', 'event') AND r.status = 'waiting'
              ORDER BY w.until",
        )?;
        let rows = st
            .query_map([now], |r| {
                let m: Option<String> = r.get(3)?;
                Ok((
                    r.get(0)?,
                    r.get(1)?,
                    r.get(2)?,
                    m.and_then(|m| serde_json::from_str(&m).ok()).unwrap_or(Value::Null),
                ))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    });
    let due = due.unwrap_or_else(|e| {
        tracing::error!("due waits: {e}");
        Vec::new()
    });
    let mut out = Vec::new();
    for (run_id, node, mode, matcher) in due {
        let ok = cx.store().transaction(|tx| -> FlowResult<bool> {
            let Some(run) = runs::get(tx, &run_id)? else {
                return Ok(false);
            };
            let res = if mode == "event" {
                json!({ "output": { "timeout": true }, "branch": wait::timeout_branch(&matcher, &Value::Null) })
            } else {
                json!({ "output": { "waited": mode, "at": now } })
            };
            resume_in(tx, run, &node, res)
        });
        match ok {
            Ok(true) => out.push(run_id),
            Ok(false) => {}
            Err(e) => tracing::error!(run = %run_id, node = %node, "wait not resumed: {e}"),
        }
    }
    out
}

/// `flow.recover` for flows (schedule runs are [`crate::scheduler::recover`]'s): a run left
/// `running` at an effectful node is not replayed but failed, with the event `need` and a `fail`
/// decision (`retry` / `drop`); others continue. Unfinished dry runs are dropped. Returns the runs
/// to drive.
pub fn recover(cx: &FlowCx) -> FlowResult<Vec<String>> {
    let now = cx.now();
    cx.store().transaction(|tx| {
        let ids: Vec<String> = {
            let mut st = tx.prepare(
                "SELECT r.id FROM flow_runs r JOIN flows f ON f.id = r.flow_id WHERE f.kind != 'schedule' AND r.status = 'running' ORDER BY r.started",
            )?;
            st.query_map([], |r| r.get(0))?
                .collect::<rusqlite::Result<Vec<_>>>()?
        };
        let mut go = Vec::new();
        for id in ids {
            let Some(mut r) = runs::get(tx, &id)? else {
                continue;
            };
            if r.dry_run {
                r.status = "failed".into();
                r.error = Some("Hub 重启，试运行没有完成".into());
                r.ended = Some(now);
                runs::save(tx, &r)?;
                continue;
            }
            let graph = graph_of(tx, &r).ok();
            let node = r
                .cursor
                .as_deref()
                .and_then(|c| graph.as_ref().and_then(|g| g.node(c)).cloned());
            let resumed_here = r.context.get("resume").is_some_and(|x| x["node"] == json!(r.cursor));
            let effect = node.as_ref().is_some_and(|n| cx.nodes.effectful(n))
                && !(resumed_here && r.context["resume"]["gate"] == GATE_NO);
            if !effect {
                go.push(id);
                continue;
            }
            let node_name = node.map(|n| n.name().to_string()).unwrap_or_default();
            let flow_name = flows::name_of(tx, &r.flow_id)?;
            let d = steve_store::base::decisions::open(
                tx,
                &NewDecision {
                    workspace_id: r.workspace_id.clone(),
                    kind: "fail".into(),
                    subject_type: crate::budget::SUBJECT.into(),
                    subject_id: r.id.clone(),
                    item_id: r.work_item_id.clone(),
                    session_id: None,
                    question: format!(
                        "「{flow_name}」停在「{node_name}」这一步时 Hub 重启了，这一步可能没有完成。要重试吗？"
                    ),
                    draft: None,
                    options: Some(vec![RETRY.into(), DROP.into()]),
                    requested_by: "steve".into(),
                },
            )?;
            r.status = "failed".into();
            r.error = Some(INTERRUPTED.into());
            r.outcome = Some("need".into());
            r.outcome_label = INTERRUPTED.into();
            r.context["decision_id"] = json!(d.id);
            r.ended = Some(now);
            runs::save(tx, &r)?;
            if let Some(e) = &r.event_id {
                signals::settle(tx, e)?;
            }
        }
        Ok(go)
    })
}

/// The waiting-for-you label an `approve` node puts on its run, removed when you answer.
pub fn release_held_outcome(r: &mut RunRow) {
    if r.context
        .get("held_outcome")
        .is_some_and(|v| v == &json!(true))
    {
        r.outcome = None;
        r.outcome_label = String::new();
        if let Some(o) = r.context.as_object_mut() {
            o.remove("held_outcome");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testkit::FlowEnv;
    use steve_modules::testkit::{ME, WS, call, caller};

    fn steps(e: &FlowEnv, run: &str) -> i64 {
        e.flow
            .store()
            .reader(|c| -> FlowResult<i64> {
                Ok(c.query_row(
                    "SELECT count(*) FROM flow_run_steps WHERE run_id = ?",
                    [run],
                    |r| r.get(0),
                )?)
            })
            .unwrap()
    }

    /// The blocking pool is one thread and busy, so the first step's write starts only after the
    /// limit, while the writer itself is free for the end of the run.
    #[test]
    fn a_dry_run_cut_off_mid_write_waits_for_the_write_before_it_ends() {
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .max_blocking_threads(1)
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(cut_off_dry_run());
    }

    async fn cut_off_dry_run() {
        let e = FlowEnv::new();
        let graph = json!({
            "nodes": [
                {"id": "t", "kind": "trigger", "config": {"source": "mail", "types": ["mail.new"]}},
                {"id": "a", "kind": "action", "config": {"action": "ignore", "reason": "x"}}
            ],
            "edges": [{"from": "t", "to": "a"}]
        });
        let body = json!({ "name": "f", "graph": graph });
        let (st, v) = call(
            &e.router(),
            Some(&caller(WS, ME)),
            "POST",
            "/flows",
            Some(&body),
        )
        .await;
        assert_eq!(st, 200, "{v}");
        let f = e
            .flow
            .store()
            .reader(|c| flows::own(c, WS, v["id"].as_str().unwrap()))
            .unwrap();
        let now = e.flow.now();
        let ctx = json!({ "prefer": [], "at": now });
        let id = e
            .flow
            .store()
            .transaction(|tx| open_run(tx, &f, "dry_run", ctx, None, true, None, 0, now))
            .unwrap();
        let busy = tokio::task::spawn_blocking(|| std::thread::sleep(Duration::from_millis(300)));
        drive_dry(&e.flow, &id, Duration::from_millis(50))
            .await
            .unwrap();
        // the cut-off step landed before the run was ended, and nothing lands after
        let at_end = steps(&e, &id);
        busy.await.unwrap();
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert_eq!(at_end, 1);
        assert_eq!(steps(&e, &id), at_end);
        let run = e
            .flow
            .store()
            .reader(|c| runs::get(c, &id))
            .unwrap()
            .unwrap();
        assert_eq!(run.status, "failed");
        assert_eq!(run.cursor.as_deref(), Some("a"));
    }
}
