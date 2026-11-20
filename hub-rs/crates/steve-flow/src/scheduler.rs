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
/// Reader-localized rejection for projections only; persisted startup errors use the stable code.
/// Detailed decoder/storage messages belong in the log.
pub fn start_rejected() -> String {
    steve_i18n::t!("flow.startup.rejected")
}

/// Keep row decoding failures distinct from a failing storage engine until the source boundary.
#[derive(Debug, thiserror::Error)]
enum StartError {
    #[error("{0}")]
    Row(crate::FlowError),
    #[error("{0}")]
    Storage(steve_store::StoreError),
}
type StartResult<T> = Result<T, StartError>;
impl From<rusqlite::Error> for StartError {
    fn from(e: rusqlite::Error) -> Self {
        match e {
            rusqlite::Error::InvalidColumnType(..)
            | rusqlite::Error::FromSqlConversionFailure(..)
            | rusqlite::Error::IntegralValueOutOfRange(..) => Self::Row(e.into()),
            _ => Self::Storage(e.into()),
        }
    }
}
impl From<steve_store::StoreError> for StartError {
    fn from(e: steve_store::StoreError) -> Self {
        match e {
            steve_store::StoreError::Sqlite(e) => e.into(),
            _ => Self::Storage(e),
        }
    }
}
impl From<crate::FlowError> for StartError {
    fn from(e: crate::FlowError) -> Self {
        match e {
            ApiError::Internal(reason) => Self::Storage(steve_store::StoreError::Hook(reason)),
            _ => Self::Row(e),
        }
    }
}
impl From<StartError> for crate::FlowError {
    fn from(e: StartError) -> Self {
        match e {
            StartError::Row(e) => e,
            StartError::Storage(e) => e.into(),
        }
    }
}
fn source_error(e: crate::FlowError) -> steve_store::StoreError {
    steve_store::StoreError::Hook(e.to_string())
}

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

fn flow_info(c: &Connection, flow_id: &str) -> StartResult<Option<FlowInfo>> {
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
        graph: serde_json::from_str(&graph).map_err(|e| {
            StartError::Row(ApiError::rule(format!("Invalid stored flow graph: {e}")))
        })?,
        overlap: OverlapPolicy::from_db(schedule["overlap"].as_str()),
        budget_usd: schedule["budget_usd"].as_f64().unwrap_or(0.0),
        tz: schedule["tz"].as_str().map(String::from),
    }))
}

/// The item a run of `flow_id` made that is not closed yet, newest first (#223): one open run per
/// schedule. Found by the run's own record or by the `from` link its item carries, so it holds
/// across restarts; an archived or removed item counts as over.
fn open_item(c: &Connection, ws: &str, flow_id: &str) -> StartResult<Option<String>> {
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
fn start(tx: &Tx, f: &FlowInfo, s: &Start, now: i64) -> StartResult<(String, bool)> {
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
        let id = runs::insert_stored(
            tx,
            &NewRun {
                ws: &f.ws,
                flow_id: &f.id,
                flow_version: f.version,
                trigger_kind: s.kind.as_str(),
                status: "skipped",
                error: None,
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
        let id = runs::insert_stored(
            tx,
            &NewRun {
                ws: &f.ws,
                flow_id: &f.id,
                flow_version: f.version,
                trigger_kind: s.kind.as_str(),
                status: "skipped",
                error: None,
                outcome_label: OVERLAP_SKIP_LABEL,
                cursor: None,
                context: &context,
                now,
            },
        )?;
        return Ok((id, false));
    }
    let cursor = after_trigger(&f.graph, s.trigger_node);
    let id = runs::insert_stored(
        tx,
        &NewRun {
            ws: &f.ws,
            flow_id: &f.id,
            flow_version: f.version,
            trigger_kind: s.kind.as_str(),
            status: "running",
            error: None,
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

fn due(c: &Connection, now: i64) -> FlowResult<Vec<i64>> {
    let mut st = c.prepare_cached("SELECT t.rowid FROM flow_triggers t JOIN flows f ON f.id=t.flow_id WHERE t.kind='time' AND t.enabled=1 AND f.enabled=1 AND f.deleted=0 AND t.next_at IS NOT NULL AND (typeof(t.next_at)<>'integer' OR t.next_at<=?1) ORDER BY t.next_at,t.id LIMIT ?2")?;
    Ok(st
        .query_map(params![now, BATCH as i64], |r| r.get(0))?
        .collect::<rusqlite::Result<_>>()?)
}
fn load_due(tx: &Tx, rowid: i64, now: i64) -> FlowResult<Option<Due>> {
    Ok(tx.query_row("SELECT t.id,t.flow_id,t.node_id,t.cron,t.tz,t.next_at,t.missed FROM flow_triggers t JOIN flows f ON f.id=t.flow_id WHERE t.rowid=?1 AND t.kind='time' AND t.enabled=1 AND f.enabled=1 AND f.deleted=0 AND t.next_at IS NOT NULL AND (typeof(t.next_at)<>'integer' OR t.next_at<=?2)", params![rowid,now], |r| Ok(Due {
        id:r.get(0)?, flow_id:r.get(1)?, node_id:r.get(2)?, cron:r.get::<_,Option<String>>(3)?.unwrap_or_default(), tz:r.get::<_,Option<String>>(4)?.unwrap_or_default(), next_at:r.get(5)?, missed:MissedPolicy::from_db(r.get::<_,Option<String>>(6)?.as_deref()),
    })).optional()?)
}

/// Claims one trigger. `None` = someone else claimed it first (or it is broken and was disabled).
fn claim(tx: &Tx, d: &Due, now: i64) -> FlowResult<Option<(String, bool)>> {
    let (Ok(expr), Ok(tz)) = (CronExpr::parse(&d.cron), zone(&d.tz)) else {
        tracing::error!(trigger = %d.id, cron = %d.cron, tz = %d.tz, "unusable time trigger, disabled");
        tx.execute("UPDATE flow_triggers SET enabled = 0 WHERE id = ?", [&d.id])?;
        return Ok(None);
    };
    let plan = plan_claim(&expr, tz, d.next_at, now, d.missed);
    let f = match flow_info(tx, &d.flow_id).map_err(crate::FlowError::from) {
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
                    error: None,
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
        tracing::error!(%error, "due source storage failed");
        self.error.get_or_insert(error);
    }
}

pub(crate) fn claim_triggers(cx: &FlowCx, now: i64) -> FlowResult<Claims> {
    let mut out = Claims::default();
    for rowid in cx.store().reader(|c| due(c, now))? {
        let key = crate::claim::Key::new("flow_triggers", "id", rowid);
        let result = crate::claim::row(
            cx.store(),
            &key,
            now,
            |tx| match load_due(tx, rowid, now)? {
                Some(d) => claim(tx, &d, now),
                None => Ok(None),
            },
            |tx, _| {
                tx.execute("UPDATE flow_triggers SET enabled=0 WHERE rowid=?", [rowid])?;
                Ok(None)
            },
        );
        match result {
            Ok(Some((id, true))) => out.ids.push(id),
            Ok(_) => {}
            Err(e) => out.failed(e),
        }
    }
    Ok(out)
}

pub(crate) fn claim_retries(cx: &FlowCx, now: i64) -> FlowResult<Claims> {
    let mut out = Claims::default();
    for rowid in cx.store().reader(|c| runs::due_retries(c, now))? {
        let key = crate::claim::Key::new("flow_runs", "id", rowid);
        let result = crate::claim::row(
            cx.store(),
            &key,
            now,
            |tx| {
                let row:Option<(String,i64)>=tx.query_row("SELECT id,retry_at FROM flow_runs WHERE rowid=?1 AND status='waiting' AND retry_at IS NOT NULL AND (typeof(retry_at)<>'integer' OR retry_at<=?2)",params![rowid,now],|r|Ok((r.get(0)?,r.get(1)?))).optional()?;
                let Some((id, at)) = row else { return Ok(None) };
                Ok(runs::claim_retry(tx, &id, at, now)?.then_some(id))
            },
            |tx, error| {
                let id: String =
                    tx.query_row("SELECT id FROM flow_runs WHERE rowid=?", [rowid], |r| {
                        r.get(0)
                    })?;
                Ok(
                    runs::fail_due(tx, &id, now, &format!("Retry claim failed: {error}"))?
                        .then_some(id),
                )
            },
        );
        match result {
            Ok(Some(id)) => out.ids.push(id),
            Ok(None) => {}
            Err(e) => out.failed(e),
        }
    }
    Ok(out)
}

/// `POST /schedules/:id/run`: a manual run now. Leaves `next_at` alone; the overlap rule applies.
/// Returns the run id and whether it was started (`false` = written as skipped).
pub fn start_manual(cx: &FlowCx, ws: &str, flow_id: &str) -> FlowResult<(String, bool)> {
    cx.store()
        .transaction(|tx| start_manual_in(tx, ws, flow_id, cx.now()).map_err(Into::into))
}

fn start_manual_in(tx: &Tx, ws: &str, flow_id: &str, now: i64) -> StartResult<(String, bool)> {
    let f = flow_info(tx, flow_id)?
        .filter(|f| f.ws == ws)
        .ok_or_else(|| StartError::Row(ApiError::not_found("定时任务")))?;
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
}

/// Schedule.updated is an existing strictly increasing edit revision, not wall-clock precision.
/// Notification contents and ordinary run history never change this configuration identity.
pub fn startup_revision(c: &Connection, ws: &str, id: &str) -> FlowResult<Option<Value>> {
    let Some(s) = crate::schedules::get(c, ws, id)? else {
        return Ok(None);
    };
    let bot_revision: Option<i64> = c
        .query_row(
            "SELECT revision FROM bots WHERE id=? AND deleted=0",
            [&s.spec.bot],
            |r| r.get(0),
        )
        .optional()?;
    Ok(Some(
        json!({"workspace":ws,"flow":id,"flow_version":s.version,"flow_updated":s.updated,
        "bot":s.spec.bot,"bot_revision":bot_revision}),
    ))
}

/// Queue-driven starts share ordinary manual execution. Recheck authorization and configuration
/// under the same outer transaction; reject row data locally and preserve genuine storage faults.
pub fn start_from_source(
    cx: &FlowCx,
    ws: &str,
    id: &str,
    source: &str,
    limit: i64,
    now: i64,
    ready: impl FnOnce(&Connection) -> FlowResult<bool>,
) -> steve_store::Result<Option<(String, bool)>> {
    cx.store().transaction(|tx| {
        if !ready(tx).map_err(source_error)? {
            return Ok(None);
        }
        let Some(revision) = startup_revision(tx, ws, id).map_err(source_error)? else {
            return Ok(None);
        };
        let state = runs::startup_state(tx, ws, id, source, &revision).map_err(source_error)?;
        if state.is_some_and(|s| s.state == runs::StartupStatus::Dead) {
            return Ok(None);
        }
        // Store's nested transaction is a savepoint: no partial attempt/events survive rejection.
        match cx
            .store()
            .transaction(|attempt| start_manual_in(attempt, ws, id, now))
        {
            Ok((run, go)) => {
                if go {
                    runs::record_started(tx, &run, source, &revision, limit)
                        .map_err(source_error)?;
                }
                Ok(Some((run, go)))
            }
            Err(StartError::Row(error)) => {
                runs::record_start_rejection(
                    tx,
                    &runs::FailedStart {
                        ws,
                        flow_id: id,
                        flow_version: revision["flow_version"].as_i64().expect("flow revision"),
                        source,
                        error: runs::START_REJECTED_CODE,
                        now,
                        revision: &revision,
                        limit,
                    },
                    &error.to_string(),
                )
                .map_err(source_error)?;
                tracing::warn!(
                    workspace = ws,
                    flow = id,
                    source,
                    "schedule startup rejected: {error}"
                );
                Ok(None)
            }
            Err(StartError::Storage(error)) => Err(error),
        }
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

/// One-time recovery. Due work belongs exclusively to the independent sources below.
pub struct Scheduler {
    cx: Arc<FlowCx>,
}

impl Scheduler {
    pub fn new(cx: Arc<FlowCx>) -> Self {
        Self { cx }
    }

    /// Boot: recover interrupted runs (schedules here, flows in [`crate::engine::recover`], whose
    /// safe runs continue), open the gate for the independent due sources. Returns only the recovered runs started.
    pub async fn boot(&self) -> FlowResult<Vec<tokio::task::JoinHandle<FlowResult<RunStatus>>>> {
        let _activity = self.cx.store().activity("scheduler_boot").await;
        let n = recover(&self.cx)?;
        if n > 0 {
            tracing::warn!("{n} schedule runs were interrupted by a restart");
        }
        let started: Vec<_> = crate::engine::recover(&self.cx)?
            .into_iter()
            .map(|id| spawn_drive(self.cx.clone(), id))
            .collect();
        // only once both recoveries went through (and their runs are spawned): the gate of what
        // starts runs on its own ([`FlowCx::recovered`])
        self.cx.mark_recovered();
        Ok(started)
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

/// Time triggers own the one-time recovery phase before opening the other sources' gate.
pub struct Triggers {
    cx: Arc<FlowCx>,
}
/// Persisted wait deadlines, independently woken and claimed.
pub struct Waits {
    cx: Arc<FlowCx>,
}
/// Persisted action retries, independently woken and claimed.
pub struct Retries {
    cx: Arc<FlowCx>,
}

async fn run_ready<S: steve_runtime::due::DueSource>(
    source: S,
    cx: Arc<FlowCx>,
    wake: Arc<tokio::sync::Notify>,
) {
    cx.recovered().await;
    steve_runtime::due::due_loop(Arc::new(source), cx.modules.clock.clone(), wake, backoff()).await;
}
fn next(
    cx: Arc<FlowCx>,
    sql: &'static str,
) -> impl std::future::Future<Output = Result<Option<i64>, steve_runtime::due::DueError>> + Send {
    async move {
        let now = cx.now();
        cx.blocking(move |s| s.reader(|c| Ok(c.query_row(sql, [now], |r| r.get(0))?)))
            .await
            .map_err(|e| steve_runtime::due::DueError(e.to_string()))
    }
}
fn process(
    cx: Arc<FlowCx>,
    name: &'static str,
    now: i64,
    claim: fn(&FlowCx, i64) -> FlowResult<Claims>,
) -> impl std::future::Future<Output = Result<(), steve_runtime::due::DueError>> + Send {
    async move {
        let _activity = cx
            .store()
            .try_activity(name)
            .ok_or_else(|| steve_runtime::due::DueError("store is in maintenance".into()))?;
        cx.store()
            .admitted(async {
                let work = cx.clone();
                let claims = cx
                    .blocking(move |_| claim(&work, now))
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
impl Triggers {
    pub fn new(cx: Arc<FlowCx>) -> Self {
        Self { cx }
    }
    pub async fn run(self) {
        let cx = self.cx.clone();
        let scheduler = Scheduler::new(cx.clone());
        let mut attempt = 0;
        while let Err(error) = scheduler.boot().await {
            tracing::error!(%error,"flow recovery failed; retrying");
            cx.modules
                .clock
                .sleep(backoff().sample_delay(attempt))
                .await;
            attempt = attempt.saturating_add(1);
        }
        let wake = cx.trigger_wake();
        run_ready(self, cx, wake).await;
    }
}
impl Waits {
    pub fn new(cx: Arc<FlowCx>) -> Self {
        Self { cx }
    }
    pub async fn run(self) {
        let cx = self.cx.clone();
        let wake = cx.wait_wake.clone();
        run_ready(self, cx, wake).await;
    }
}
impl Retries {
    pub fn new(cx: Arc<FlowCx>) -> Self {
        Self { cx }
    }
    pub async fn run(self) {
        let cx = self.cx.clone();
        let wake = cx.retry_wake.clone();
        run_ready(self, cx, wake).await;
    }
}
impl steve_runtime::due::DueSource for Triggers {
    fn name(&self) -> &'static str {
        "flow.triggers"
    }
    fn next_due(
        &self,
    ) -> impl std::future::Future<Output = Result<Option<i64>, steve_runtime::due::DueError>> + Send
    {
        next(
            self.cx.clone(),
            "SELECT min(CASE WHEN typeof(t.next_at)='integer' THEN t.next_at ELSE ?1 END) FROM flow_triggers t JOIN flows f ON f.id=t.flow_id WHERE t.kind='time' AND t.enabled=1 AND f.enabled=1 AND f.deleted=0 AND t.next_at IS NOT NULL",
        )
    }
    fn run_due(
        &self,
        now: i64,
    ) -> impl std::future::Future<Output = Result<(), steve_runtime::due::DueError>> + Send {
        process(self.cx.clone(), "flow.triggers", now, claim_triggers)
    }
}
impl steve_runtime::due::DueSource for Waits {
    fn name(&self) -> &'static str {
        "flow.waits"
    }
    fn next_due(
        &self,
    ) -> impl std::future::Future<Output = Result<Option<i64>, steve_runtime::due::DueError>> + Send
    {
        next(
            self.cx.clone(),
            "SELECT min(CASE WHEN typeof(w.until)='integer' THEN w.until ELSE ?1 END) FROM flow_waits w JOIN flow_runs r ON r.id=w.run_id WHERE r.status='waiting' AND w.until IS NOT NULL AND w.mode IN ('delay','until','event')",
        )
    }
    fn run_due(
        &self,
        now: i64,
    ) -> impl std::future::Future<Output = Result<(), steve_runtime::due::DueError>> + Send {
        process(
            self.cx.clone(),
            "flow.waits",
            now,
            crate::engine::claim_waits_at,
        )
    }
}
impl steve_runtime::due::DueSource for Retries {
    fn name(&self) -> &'static str {
        "flow.retries"
    }
    fn next_due(
        &self,
    ) -> impl std::future::Future<Output = Result<Option<i64>, steve_runtime::due::DueError>> + Send
    {
        next(
            self.cx.clone(),
            "SELECT min(CASE WHEN typeof(retry_at)='integer' THEN retry_at ELSE ?1 END) FROM flow_runs WHERE status='waiting' AND retry_at IS NOT NULL",
        )
    }
    fn run_due(
        &self,
        now: i64,
    ) -> impl std::future::Future<Output = Result<(), steve_runtime::due::DueError>> + Send {
        process(self.cx.clone(), "flow.retries", now, claim_retries)
    }
}
