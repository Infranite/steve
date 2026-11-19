//! `flow.scheduler`: claims due time triggers, applies the missed-run and overlap
//! rules, starts runs, and picks up due retries. Claiming is one conditional `UPDATE` per trigger
//! (`… WHERE next_at = :old`), so two loops never start the same planned run twice.

use std::sync::Arc;
use std::time::Duration;

use rusqlite::{Connection, OptionalExtension, params};
use serde_json::{Value, json};
use steve_modules::ApiError;
use steve_store::Tx;

use crate::cron::zone;
use crate::exec::RunStatus;
use crate::policy::{
    ClaimAction, MissedPolicy, OVERLAP_SKIP_LABEL, OverlapPolicy, TriggerKind, overlap_blocks,
    plan_claim,
};
use crate::runs::{self, NewRun, NewStep};
use crate::{FlowCx, FlowResult};
use steve_runtime::schedule::CronExpr;

/// Due rows claimed per pass.
pub const BATCH: usize = 200;
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
    /// A schedule's time zone (the skip line on its open item says the planned time in it).
    tz: Option<String>,
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
        graph: serde_json::from_str(&graph)
            .map_err(|e| ApiError::rule(format!("Invalid stored flow graph: {e}")))?,
        overlap: OverlapPolicy::from_db(schedule["overlap"].as_str()),
        budget_usd: schedule["budget_usd"].as_f64().unwrap_or(0.0),
        tz: schedule["tz"].as_str().map(String::from),
    }))
}

/// The item a run of `flow_id` made that is not closed yet, newest first (#223): one open run per
/// schedule. Found by the run's own record or by the `from` link its item carries, so it holds
/// across restarts; an archived or removed item counts as over.
fn open_item(c: &Connection, ws: &str, flow_id: &str) -> FlowResult<Option<String>> {
    Ok(c.query_row(
        "SELECT x.item FROM (
             SELECT r.work_item_id AS item, r.started AS at FROM flow_runs r
              WHERE r.workspace_id = ?1 AND r.flow_id = ?2 AND r.work_item_id IS NOT NULL AND r.dry_run = 0
             UNION ALL
             SELECT l.subject_id, r.started FROM links l JOIN flow_runs r ON r.id = l.object_id
              WHERE l.workspace_id = ?1 AND l.subject_type = 'item' AND l.predicate = 'from'
                AND l.object_type = 'flow_run' AND r.flow_id = ?2 AND r.dry_run = 0
         ) x JOIN items i ON i.id = x.item AND i.workspace_id = ?1
          WHERE coalesce(i.phase, '') <> 'closed' AND i.archived = 0
          ORDER BY x.at DESC LIMIT 1",
        params![ws, flow_id],
        |r| r.get(0),
    )
    .optional()?)
}

/// `上一次还没结束，跳过 <time> 的这一次`: the open item's timeline line and the skipped run's label.
pub fn open_item_skip_label(at: i64, tz: Option<&str>) -> String {
    let tz = tz
        .and_then(|t| zone(t).ok())
        .unwrap_or_else(steve_runtime::tz::Zone::system);
    format!(
        "{OVERLAP_SKIP_LABEL} {} 的这一次",
        crate::schedules::local(at, tz, "%m-%d %H:%M")
    )
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
    // the item the last run made is still open (#223): this one is skipped, said on that item
    if f.overlap == OverlapPolicy::Skip
        && let Some(item) = open_item(tx, &f.ws, &f.id)?
    {
        let label = open_item_skip_label(s.at, f.tz.as_deref());
        if let Some(ctx) = context.as_object_mut() {
            ctx.insert("open_item".into(), json!(item));
        }
        let id = runs::insert(
            tx,
            &NewRun {
                ws: &f.ws,
                flow_id: &f.id,
                flow_version: f.version,
                trigger_kind: s.kind.as_str(),
                status: "skipped",
                outcome_label: &label,
                cursor: None,
                context: &context,
                now,
            },
        )?;
        steve_store::timeline_add_in(tx, &item, "steve", "sys", &label, None)?;
        return Ok((id, false));
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
        "SELECT t.id, t.flow_id, t.node_id, t.cron, t.tz, t.next_at, t.missed FROM flow_triggers t JOIN flows f ON f.id = t.flow_id WHERE t.kind = 'time' AND t.enabled = 1 AND f.enabled = 1 AND f.deleted = 0 AND t.next_at IS NOT NULL AND t.next_at <= ? ORDER BY t.next_at, t.id LIMIT ?",
    )?;
    let rows = st
        .query_map([now, BATCH as i64], |r| {
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
    let (Ok(expr), Ok(tz)) = (CronExpr::parse(&d.cron), zone(&d.tz)) else {
        tracing::error!(trigger = %d.id, cron = %d.cron, tz = %d.tz, "unusable time trigger, disabled");
        tx.execute("UPDATE flow_triggers SET enabled = 0 WHERE id = ?", [&d.id])?;
        return Ok(None);
    };
    let plan = plan_claim(&expr, tz, d.next_at, now, d.missed);
    let f = match flow_info(tx, &d.flow_id) {
        Ok(Some(f)) => f,
        Ok(None) => return Ok(None),
        Err(e) if e.status() < 500 => {
            tracing::error!(trigger = %d.id, error = %e, "invalid flow; time trigger disabled");
            tx.execute("UPDATE flow_triggers SET enabled = 0 WHERE id = ?", [&d.id])?;
            return Ok(None);
        }
        Err(e) => return Err(e),
    };
    let n = tx.execute(
        "UPDATE flow_triggers SET next_at = ? WHERE id = ? AND next_at = ?",
        params![plan.next_at_ms, d.id, d.next_at],
    )?;
    if n != 1 {
        return Ok(None);
    }
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

/// Committed claims survive an error in another row; the source drives them before backing off.
#[derive(Default)]
pub(crate) struct Claims {
    pub ids: Vec<String>,
    pub error: Option<crate::FlowError>,
}
impl Claims {
    pub fn failed(&mut self, error: crate::FlowError) {
        tracing::error!(%error, "due row claim failed");
        self.error.get_or_insert(error);
    }
}

fn claim_triggers(cx: &FlowCx, now: i64) -> FlowResult<Claims> {
    let mut out = Claims::default();
    for d in cx.store().reader(|c| due(c, now))? {
        match cx.store().transaction(|tx| claim(tx, &d, now)) {
            Ok(Some((id, true))) => out.ids.push(id),
            Ok(_) => {}
            Err(e) => out.failed(e),
        }
    }
    Ok(out)
}

fn claim_retries(cx: &FlowCx, now: i64) -> FlowResult<Claims> {
    let mut out = Claims::default();
    for (id, retry_at) in cx.store().reader(|c| runs::due_retries(c, now))? {
        match cx
            .store()
            .transaction(|tx| runs::claim_retry(tx, &id, retry_at, now))
        {
            Ok(true) => out.ids.push(id),
            Ok(false) => {}
            Err(e) => out.failed(e),
        }
    }
    Ok(out)
}

/// One best-effort pass for explicit callers and boot; all claims use the same source functions.
/// Background timing belongs to the three independent due_loop tasks below.
pub fn claim_due(cx: &FlowCx) -> FlowResult<Vec<String>> {
    let now = cx.now();
    let mut out = Vec::new();
    for claims in [
        claim_triggers(cx, now),
        crate::engine::claim_waits_at(cx, now),
        claim_retries(cx, now),
    ] {
        match claims {
            Ok(claims) => out.extend(claims.ids),
            Err(e) => tracing::error!(%e, "due source failed"),
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
}

/// Source-local retry policy, including the one-time recovery phase.
pub(crate) fn backoff() -> steve_runtime::policy::Backoff {
    steve_runtime::policy::Backoff {
        base: Duration::from_secs(1),
        max: Duration::from_secs(60),
        factor: 2,
        jitter: steve_runtime::policy::Jitter::None,
        attempts: None,
        reset_after: None,
    }
}

#[derive(Clone, Copy)]
enum Kind {
    Triggers,
    Waits,
    Retries,
}

/// One table of runnable flow work. Each instance owns its own due_loop and wake permit.
pub struct RunSource {
    cx: Arc<FlowCx>,
    kind: Kind,
}
impl RunSource {
    pub fn triggers(cx: Arc<FlowCx>) -> Self {
        Self {
            cx,
            kind: Kind::Triggers,
        }
    }
    pub fn waits(cx: Arc<FlowCx>) -> Self {
        Self {
            cx,
            kind: Kind::Waits,
        }
    }
    pub fn retries(cx: Arc<FlowCx>) -> Self {
        Self {
            cx,
            kind: Kind::Retries,
        }
    }
    pub fn run(self) -> impl std::future::Future<Output = ()> + Send + 'static {
        async move {
            let clock = self.cx.modules.clock.clone();
            if matches!(self.kind, Kind::Triggers) {
                let scheduler = Scheduler::new(self.cx.clone());
                let policy = backoff();
                let mut attempt = 0u32;
                // This is the one-time recovery gate, not another due-work polling loop.
                while let Err(error) = scheduler.boot().await {
                    tracing::error!(%error, "flow recovery failed; retrying");
                    clock.sleep(policy.sample_delay(attempt)).await;
                    attempt = attempt.saturating_add(1);
                }
            } else {
                self.cx.recovered().await;
            }
            let wake = match self.kind {
                Kind::Triggers => self.cx.trigger_wake(),
                Kind::Waits => self.cx.wait_wake.clone(),
                Kind::Retries => self.cx.retry_wake.clone(),
            };
            steve_runtime::due::due_loop(Arc::new(self), clock, wake, backoff()).await;
        }
    }
}
impl steve_runtime::due::DueSource for RunSource {
    fn name(&self) -> &'static str {
        match self.kind {
            Kind::Triggers => "flow.triggers",
            Kind::Waits => "flow.waits",
            Kind::Retries => "flow.retries",
        }
    }
    fn next_due(
        &self,
    ) -> impl std::future::Future<Output = Result<Option<i64>, steve_runtime::due::DueError>> + Send
    {
        let (cx, kind) = (self.cx.clone(), self.kind);
        async move {
            if matches!(kind, Kind::Triggers) && cx.has_followups().await {
                return Ok(Some(cx.now()));
            }
            let sql = match kind {
                Kind::Triggers => {
                    "SELECT min(t.next_at) FROM flow_triggers t JOIN flows f ON f.id=t.flow_id WHERE t.kind='time' AND t.enabled=1 AND f.enabled=1 AND f.deleted=0"
                }
                Kind::Waits => {
                    "SELECT min(w.until) FROM flow_waits w JOIN flow_runs r ON r.id=w.run_id WHERE r.status='waiting' AND w.mode IN ('delay','until','event')"
                }
                Kind::Retries => "SELECT min(retry_at) FROM flow_runs WHERE status='waiting'",
            };
            cx.blocking(move |s| s.reader(|c| Ok(c.query_row(sql, [], |r| r.get(0))?)))
                .await
                .map_err(|e| steve_runtime::due::DueError(e.to_string()))
        }
    }
    fn run_due(
        &self,
        now: i64,
    ) -> impl std::future::Future<Output = Result<(), steve_runtime::due::DueError>> + Send {
        // Own the context: the public loop future can cross crate and Tokio task boundaries.
        let (cx, kind, name) = (self.cx.clone(), self.kind, self.name());
        async move {
            let _activity = cx
                .store()
                .try_activity(name)
                .ok_or_else(|| steve_runtime::due::DueError("store is in maintenance".into()))?;
            cx.store()
                .admitted(async {
                    if matches!(kind, Kind::Triggers) {
                        cx.spawn_followups().await;
                    }
                    let work = cx.clone();
                    let claims = cx
                        .blocking(move |_| match kind {
                            Kind::Triggers => claim_triggers(&work, now),
                            Kind::Waits => crate::engine::claim_waits_at(&work, now),
                            Kind::Retries => claim_retries(&work, now),
                        })
                        .await
                        .map_err(|e| steve_runtime::due::DueError(e.to_string()))?;
                    for id in claims.ids {
                        spawn_drive(cx.clone(), id);
                    }
                    claims
                        .error
                        .map_or(Ok(()), |e| Err(steve_runtime::due::DueError(e.to_string())))
                })
                .await
        }
    }
}
