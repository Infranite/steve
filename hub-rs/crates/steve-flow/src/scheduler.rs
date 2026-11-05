//! `flow.scheduler`: claims due time triggers, applies the missed-run and overlap
//! rules, starts runs, and picks up due retries. Claiming is one conditional `UPDATE` per trigger
//! (`… WHERE next_at = :old`), so two loops never start the same planned run twice.

use std::sync::Arc;
use std::time::Duration;

use rusqlite::{Connection, OptionalExtension, params};
use serde_json::{Value, json};
use steve_modules::ApiError;
use steve_store::Tx;

use crate::cron::{CronExpr, parse_tz};
use crate::exec::RunStatus;
use crate::policy::{
    ClaimAction, MissedPolicy, OVERLAP_SKIP_LABEL, OverlapPolicy, TriggerKind, overlap_blocks,
    plan_claim,
};
use crate::runs::{self, NewRun, NewStep};
use crate::{FlowCx, FlowResult};

/// Longest sleep between two rounds.
pub const IDLE_MS: i64 = 15_000;
/// Triggers due this soon are claimed in the current round.
pub const EARLY_MS: i64 = 1_000;
/// How often old schedule runs are pruned.
pub const RETENTION_EVERY_MS: i64 = 3_600_000;
/// Error of a run that was in the middle of its action when the Hub stopped.
pub const INTERRUPTED: &str = "Hub 重启时这一步可能没有完成";

/// What the scheduler needs to know about a flow to start a run of it.
#[derive(Debug, Clone)]
struct FlowInfo {
    id: String,
    ws: String,
    version: i64,
    graph: Value,
    overlap: OverlapPolicy,
    budget_usd: f64,
}

fn flow_info(c: &Connection, flow_id: &str) -> FlowResult<Option<FlowInfo>> {
    let row: Option<(String, String, i64, String, Option<String>)> = c
        .query_row(
            "SELECT id, workspace_id, version, graph, schedule FROM flows WHERE id = ? AND deleted = 0",
            [flow_id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
        )
        .optional()?;
    let Some((id, ws, version, graph, schedule)) = row else {
        return Ok(None);
    };
    let schedule: Value = schedule
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or(Value::Null);
    Ok(Some(FlowInfo {
        id,
        ws,
        version,
        graph: serde_json::from_str(&graph)?,
        overlap: OverlapPolicy::from_db(schedule["overlap"].as_str()),
        budget_usd: schedule["budget_usd"].as_f64().unwrap_or(0.0),
    }))
}

/// The node after the trigger (`flow_runs.cursor` of a new run).
fn after_trigger(graph: &Value, trigger: &str) -> Option<String> {
    graph["edges"]
        .as_array()?
        .iter()
        .find(|e| e["from"] == trigger)
        .and_then(|e| e["to"].as_str())
        .map(String::from)
}

/// How a run comes about.
struct Start<'a> {
    kind: TriggerKind,
    /// Planned time (`context.at`).
    at: i64,
    /// Extra context (`late_ms`, `missed`, `label_prefix`).
    extra: Value,
    trigger_node: &'a str,
}

/// Inserts the run row (and the trigger step), or a `skipped` row when the previous run is still
/// going and the overlap rule says skip. Returns the run id and whether it should be driven.
fn start(tx: &Tx, f: &FlowInfo, s: &Start, now: i64) -> FlowResult<(String, bool)> {
    let mut context =
        json!({ "at": s.at, "trigger": s.kind.as_str(), "budget_limit": f.budget_usd });
    if let (Some(ctx), Some(extra)) = (context.as_object_mut(), s.extra.as_object()) {
        ctx.extend(extra.clone());
    }
    let active = runs::active_exists(tx, &f.ws, &f.id)?;
    if overlap_blocks(f.overlap, active.then_some("running")) {
        let id = runs::insert(
            tx,
            &NewRun {
                ws: &f.ws,
                flow_id: &f.id,
                flow_version: f.version,
                trigger_kind: s.kind.as_str(),
                status: "skipped",
                outcome_label: OVERLAP_SKIP_LABEL,
                cursor: None,
                context: &context,
                now,
            },
        )?;
        return Ok((id, false));
    }
    let cursor = after_trigger(&f.graph, s.trigger_node);
    let id = runs::insert(
        tx,
        &NewRun {
            ws: &f.ws,
            flow_id: &f.id,
            flow_version: f.version,
            trigger_kind: s.kind.as_str(),
            status: "running",
            outcome_label: "",
            cursor: cursor.as_deref(),
            context: &context,
            now,
        },
    )?;
    runs::add_step(
        tx,
        &f.ws,
        &id,
        &NewStep {
            node_id: s.trigger_node,
            status: "ok",
            input: &Value::Null,
            output: &context,
            ms: 0.0,
            error: None,
            started: now,
        },
    )?;
    Ok((id, true))
}

/// A due time trigger as read before claiming.
#[derive(Debug, Clone)]
struct Due {
    id: String,
    flow_id: String,
    node_id: String,
    cron: String,
    tz: String,
    next_at: i64,
    missed: MissedPolicy,
}

fn due(c: &Connection, now: i64) -> FlowResult<Vec<Due>> {
    let mut st = c.prepare_cached(
        "SELECT t.id, t.flow_id, t.node_id, t.cron, t.tz, t.next_at, t.missed FROM flow_triggers t JOIN flows f ON f.id = t.flow_id WHERE t.kind = 'time' AND t.enabled = 1 AND f.enabled = 1 AND f.deleted = 0 AND t.next_at IS NOT NULL AND t.next_at <= ? ORDER BY t.next_at",
    )?;
    let rows = st
        .query_map([now + EARLY_MS], |r| {
            Ok(Due {
                id: r.get(0)?,
                flow_id: r.get(1)?,
                node_id: r.get(2)?,
                cron: r.get::<_, Option<String>>(3)?.unwrap_or_default(),
                tz: r.get::<_, Option<String>>(4)?.unwrap_or_default(),
                next_at: r.get(5)?,
                missed: MissedPolicy::from_db(r.get::<_, Option<String>>(6)?.as_deref()),
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

/// Claims one trigger. `None` = someone else claimed it first (or it is broken and was disabled).
fn claim(tx: &Tx, d: &Due, now: i64) -> FlowResult<Option<(String, bool)>> {
    let (Ok(expr), Ok(tz)) = (CronExpr::parse(&d.cron), parse_tz(&d.tz)) else {
        tracing::error!(trigger = %d.id, cron = %d.cron, tz = %d.tz, "unusable time trigger, disabled");
        tx.execute("UPDATE flow_triggers SET enabled = 0 WHERE id = ?", [&d.id])?;
        return Ok(None);
    };
    let plan = plan_claim(&expr, tz, d.next_at, now, d.missed);
    let n = tx.execute(
        "UPDATE flow_triggers SET next_at = ? WHERE id = ? AND next_at = ?",
        params![plan.next_at_ms, d.id, d.next_at],
    )?;
    if n != 1 {
        return Ok(None);
    }
    let Some(f) = flow_info(tx, &d.flow_id)? else {
        return Ok(None);
    };
    let started = match plan.action {
        ClaimAction::Skip { missed } => {
            let label = plan.action.label().unwrap_or_default();
            let ctx = json!({ "at": plan.at_ms, "trigger": "time", "missed": missed });
            let id = runs::insert(
                tx,
                &NewRun {
                    ws: &f.ws,
                    flow_id: &f.id,
                    flow_version: f.version,
                    trigger_kind: plan.action.trigger_kind().as_str(),
                    status: "skipped",
                    outcome_label: &label,
                    cursor: None,
                    context: &ctx,
                    now,
                },
            )?;
            (id, false)
        }
        ClaimAction::CatchUp { late_ms, missed } => start(
            tx,
            &f,
            &Start {
                kind: TriggerKind::CatchUp,
                at: plan.at_ms,
                extra: json!({ "late_ms": late_ms, "missed": missed, "label_prefix": plan.action.label() }),
                trigger_node: &d.node_id,
            },
            now,
        )?,
        ClaimAction::Run => start(
            tx,
            &f,
            &Start {
                kind: TriggerKind::Time,
                at: plan.at_ms,
                extra: json!({}),
                trigger_node: &d.node_id,
            },
            now,
        )?,
    };
    Ok(Some(started))
}

/// One round: claims every due trigger, every wait whose time came, and every due retry. Returns
/// the runs to drive. Each claim commits on its own, so a claim that fails is logged and skipped:
/// the runs already claimed are still returned (nothing else would drive them before a restart),
/// and one broken row does not hold up the others. Never fails; the `Result` is the callers'.
pub fn claim_due(cx: &FlowCx) -> FlowResult<Vec<String>> {
    let now = cx.now();
    let mut out = Vec::new();
    let triggers = cx.store().reader(|c| due(c, now)).unwrap_or_else(|e| {
        tracing::error!("due triggers: {e}");
        Vec::new()
    });
    for d in triggers {
        match cx.store().transaction(|tx| claim(tx, &d, now)) {
            Ok(Some((id, true))) => out.push(id),
            Ok(_) => {}
            Err(e) => tracing::error!(trigger = %d.id, flow = %d.flow_id, "claim failed: {e}"),
        }
    }
    out.extend(crate::engine::claim_due_waits(cx));
    let retries = cx
        .store()
        .reader(|c| runs::due_retries(c, now))
        .unwrap_or_else(|e| {
            tracing::error!("due retries: {e}");
            Vec::new()
        });
    for (id, retry_at) in retries {
        match cx
            .store()
            .transaction(|tx| runs::claim_retry(tx, &id, retry_at))
        {
            Ok(true) => out.push(id),
            Ok(false) => {}
            Err(e) => tracing::error!(run = %id, "retry claim failed: {e}"),
        }
    }
    Ok(out)
}

/// `POST /schedules/:id/run`: a manual run now. Leaves `next_at` alone; the overlap rule applies.
/// Returns the run id and whether it was started (`false` = written as skipped).
pub fn start_manual(cx: &FlowCx, ws: &str, flow_id: &str) -> FlowResult<(String, bool)> {
    let now = cx.now();
    cx.store().transaction(|tx| {
        let f = flow_info(tx, flow_id)?
            .filter(|f| f.ws == ws)
            .ok_or_else(|| ApiError::not_found("定时任务"))?;
        let trigger: String = tx
            .query_row(
                "SELECT node_id FROM flow_triggers WHERE flow_id = ? AND kind = 'time' LIMIT 1",
                [flow_id],
                |r| r.get(0),
            )
            .optional()?
            .unwrap_or_else(|| "t".into());
        start(
            tx,
            &f,
            &Start {
                kind: TriggerKind::Manual,
                at: now,
                extra: json!({}),
                trigger_node: &trigger,
            },
            now,
        )
    })
}

/// Drives one run with the registered executor, logging failures.
pub async fn drive(cx: &FlowCx, run_id: &str) -> FlowResult<RunStatus> {
    let r = cx.exec.drive(cx, run_id).await;
    if let Err(e) = &r {
        tracing::error!(run = %run_id, "run failed: {e}");
    }
    r
}

/// Drives a run in its own task (a report's model call must not hold up the loop).
pub fn spawn_drive(
    cx: Arc<FlowCx>,
    run_id: String,
) -> tokio::task::JoinHandle<FlowResult<RunStatus>> {
    tokio::spawn(async move { drive(&cx, &run_id).await })
}

/// `flow.recover` for schedule runs: a run still `running` at boot was inside its action (an item
/// may or may not have been created, a report may or may not have been posted), so it is not
/// replayed but marked failed. Returns how many.
pub fn recover(cx: &FlowCx) -> FlowResult<usize> {
    let now = cx.now();
    cx.store().transaction(|tx| {
        let ids: Vec<String> = {
            let mut st = tx.prepare(
                "SELECT r.id FROM flow_runs r JOIN flows f ON f.id = r.flow_id WHERE f.kind = 'schedule' AND r.status = 'running'",
            )?;
            st.query_map([], |r| r.get(0))?
                .collect::<rusqlite::Result<Vec<_>>>()?
        };
        for id in &ids {
            if let Some(mut r) = runs::get(tx, id)? {
                r.status = "failed".into();
                r.error = Some(INTERRUPTED.into());
                r.outcome_label = INTERRUPTED.into();
                r.ended = Some(now);
                runs::save(tx, &r)?;
            }
        }
        Ok(ids.len())
    })
}

/// The earliest `next_at` of an enabled time trigger.
fn earliest(c: &Connection) -> FlowResult<Option<i64>> {
    Ok(c.query_row(
        "SELECT min(t.next_at) FROM flow_triggers t JOIN flows f ON f.id = t.flow_id WHERE t.kind = 'time' AND t.enabled = 1 AND f.enabled = 1 AND f.deleted = 0",
        [],
        |r| r.get(0),
    )?)
}

/// The longest wait between two failed boots.
pub const BOOT_RETRY_MAX: Duration = Duration::from_secs(60);

/// The `flow.scheduler` task.
pub struct Scheduler {
    cx: Arc<FlowCx>,
}

impl Scheduler {
    pub fn new(cx: Arc<FlowCx>) -> Self {
        Self { cx }
    }

    /// Boot: recover interrupted runs (schedules here, flows in [`crate::engine::recover`], whose
    /// safe runs continue), then a first round (which applies the missed-run policy to
    /// everything that came due while the Hub was down). Returns the runs started.
    pub async fn boot(&self) -> FlowResult<Vec<tokio::task::JoinHandle<FlowResult<RunStatus>>>> {
        let _activity = self.cx.store().activity("scheduler_boot").await;
        let n = recover(&self.cx)?;
        if n > 0 {
            tracing::warn!("{n} schedule runs were interrupted by a restart");
        }
        let mut started: Vec<_> = crate::engine::recover(&self.cx)?
            .into_iter()
            .map(|id| spawn_drive(self.cx.clone(), id))
            .collect();
        // only once both recoveries went through (and their runs are spawned): the gate of what
        // starts runs on its own ([`FlowCx::recovered`])
        self.cx.mark_recovered();
        // past this point a failure must not boot again (the recovered runs are driven already)
        match self.round().await {
            Ok(more) => started.extend(more),
            Err(e) => tracing::error!("scheduler round: {e}"),
        }
        Ok(started)
    }

    /// One round: follow-ups, claims, retries; the runs are spawned.
    pub async fn round(&self) -> FlowResult<Vec<tokio::task::JoinHandle<FlowResult<RunStatus>>>> {
        let Some(_activity) = self.cx.store().try_activity("scheduler") else {
            return Ok(vec![]);
        };
        self.cx
            .store()
            .admitted(async {
                // spawned, not awaited: a resumed run must not hold up the claims
                self.cx.spawn_followups().await;
                let cx = self.cx.clone();
                let ids = self.cx.blocking(move |_| claim_due(&cx)).await?;
                Ok(ids
                    .into_iter()
                    .map(|id| spawn_drive(self.cx.clone(), id))
                    .collect())
            })
            .await
    }

    /// Runs forever: sleeps until `min(earliest next_at, now + 15 s)` or a wake-up.
    pub async fn run(self) {
        // a failed boot (the store busy, a broken row) is tried again with a back-off; nothing runs
        // on its own before it went through
        let mut wait = Duration::from_secs(1);
        while let Err(e) = self.boot().await {
            tracing::error!("scheduler boot failed, again in {}s: {e}", wait.as_secs());
            tokio::time::sleep(wait).await;
            wait = (wait * 2).min(BOOT_RETRY_MAX);
        }
        let mut pruned_at = i64::MIN;
        loop {
            let now = self.cx.now();
            if now.saturating_sub(pruned_at) >= RETENTION_EVERY_MS {
                pruned_at = now;
                let pruned = self
                    .cx
                    .blocking(move |s| s.transaction(|tx| runs::prune(tx, now)))
                    .await;
                match pruned {
                    Ok(n) if n > 0 => tracing::info!("pruned {n} old schedule runs"),
                    Ok(_) => {}
                    Err(e) => tracing::error!("schedule run retention: {e}"),
                }
            }
            let next = self
                .cx
                .store()
                .reader(earliest)
                .unwrap_or(None)
                .unwrap_or(i64::MAX);
            let wait = next.min(now + IDLE_MS).saturating_sub(now).max(0);
            tokio::select! {
                _ = tokio::time::sleep(Duration::from_millis(wait as u64)) => {}
                _ = self.cx.notified().notified() => {}
            }
            if let Err(e) = self.round().await {
                tracing::error!("scheduler round: {e}");
            }
        }
    }
}
