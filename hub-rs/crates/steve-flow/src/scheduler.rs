//! `flow.scheduler`: claims due time triggers, applies the missed-run and overlap
//! rules, starts runs, and picks up due retries. Claiming is one conditional `UPDATE` per trigger
//! (`… WHERE next_at = :old`), so two loops never start the same planned run twice.

use std::sync::Arc;
use std::time::Duration;

use serde_json::{Value, json};
use steve_modules::ApiError;
use steve_store::base::links::Links;
use steve_store::{
    FlowRows, FlowTriggers, FlowWaits, ItemReads, R, Read, RunRows, TimelineRows, W,
};

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
/// Metric task label for ordinary cron schedule triggers.
const PERIODIC_TASK_SCHEDULE: &str = "flow.schedule";
/// Metric task label for wall-clock cron triggers suppressed on queue-driven digest schedules.
const PERIODIC_TASK_DIGEST_CRON: &str = "digest.cron";
/// Error of a run that was in the middle of its action when the Hub stopped.
pub const INTERRUPTED: &str = "Hub 重启时这一步可能没有完成";
/// Reader-localized rejection for projections only; persisted startup errors use the stable code.
/// Detailed decoder/storage messages belong in the log.
pub fn start_rejected() -> String {
    steve_i18n::t!("flow.startup.rejected")
}

// Debug retains typed NotFound/Invalid identity without their locale-dependent Display text.
fn rejection_detail(error: &crate::FlowError) -> String {
    format!("{error:?}")
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
impl From<steve_store::StoreError> for StartError {
    fn from(e: steve_store::StoreError) -> Self {
        Self::Storage(e)
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

fn flow_info(r: &mut impl Read, flow_id: &str) -> StartResult<Option<FlowInfo>> {
    let Some(row) = FlowRows(r)
        .get(flow_id)
        .map_err(invalid_schedule)?
        .filter(|row| !row.deleted)
    else {
        return Ok(None);
    };
    let schedule = FlowRows(r)
        .schedule_config(flow_id)
        .map_err(invalid_schedule)?
        .unwrap_or(Value::Null);
    Ok(Some(FlowInfo {
        id: row.id,
        ws: row.workspace_id,
        version: row.version,
        graph: row.graph,
        overlap: OverlapPolicy::from_db(schedule["overlap"].as_str()),
        budget_usd: schedule["budget_usd"].as_f64().unwrap_or(0.0),
        tz: schedule["tz"].as_str().map(String::from),
    }))
}

fn invalid_schedule(error: steve_store::StoreError) -> StartError {
    match error {
        steve_store::StoreError::Json(_) | steve_store::StoreError::Rejected(_) => {
            StartError::Row(ApiError::rule(format!("Invalid schedule data: {error}")))
        }
        _ => StartError::Storage(error),
    }
}

/// The newest run's open item, found through the run's result or through its source links.
fn open_item(r: &mut impl Read, ws: &str, flow_id: &str) -> StartResult<Option<String>> {
    const PAGE: u64 = 100;
    let mut offset = 0;
    loop {
        let runs = RunRows(r).item_origins(ws, flow_id, offset, PAGE)?;
        if runs.is_empty() {
            return Ok(None);
        }
        let links = Links(r).sources(
            ws,
            "item",
            "from",
            "flow_run",
            runs.iter().map(|(id, _)| id.as_str()),
        )?;
        let mut by_run = std::collections::HashMap::<String, Vec<String>>::new();
        for (run, item, _) in links {
            by_run.entry(run).or_default().push(item);
        }
        let candidates: Vec<_> = runs
            .into_iter()
            .flat_map(|(id, item)| {
                item.into_iter()
                    .chain(by_run.remove(&id).unwrap_or_default())
            })
            .collect();
        for batch in candidates.chunks(500) {
            let open = ItemReads(r).open_ids(ws, batch.iter().map(String::as_str))?;
            if let Some(item) = batch.iter().find(|id| open.contains(*id)) {
                return Ok(Some(item.clone()));
            }
        }
        offset += PAGE;
    }
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
fn start(w: &mut W<'_>, f: &FlowInfo, s: &Start, now: i64) -> StartResult<(String, bool)> {
    let mut context =
        json!({ "at": s.at, "trigger": s.kind.as_str(), "budget_limit": f.budget_usd });
    if let (Some(ctx), Some(extra)) = (context.as_object_mut(), s.extra.as_object()) {
        ctx.extend(extra.clone());
    }
    let is_periodic = matches!(s.kind, TriggerKind::Time | TriggerKind::CatchUp);
    // the item the last run made is still open (#223): this one is skipped, said on that item
    if f.overlap == OverlapPolicy::Skip
        && let Some(item) = open_item(w, &f.ws, &f.id)?
    {
        let label = open_item_skip_label(s.at, f.tz.as_deref());
        if let Some(ctx) = context.as_object_mut() {
            ctx.insert("open_item".into(), json!(item));
        }
        let id = RunRows(w).insert(&NewRun {
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
        })?;
        TimelineRows(w).add(&f.ws, &item, "steve", "sys", &label)?;
        if is_periodic {
            steve_runtime::jobs::record_periodic_skipped(
                PERIODIC_TASK_SCHEDULE,
                steve_runtime::jobs::SkipReason::Overlap,
            );
        }
        return Ok((id, false));
    }
    let active = RunRows(w).active(&f.ws, &f.id)?;
    if overlap_blocks(f.overlap, active.then_some("running")) {
        let id = RunRows(w).insert(&NewRun {
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
        })?;
        if is_periodic {
            steve_runtime::jobs::record_periodic_skipped(
                PERIODIC_TASK_SCHEDULE,
                steve_runtime::jobs::SkipReason::Overlap,
            );
        }
        return Ok((id, false));
    }
    let cursor = after_trigger(&f.graph, s.trigger_node);
    let id = RunRows(w).insert(&NewRun {
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
    })?;
    RunRows(w).add_step(
        &f.ws,
        &id,
        &NewStep {
            branch: None,
            node_id: s.trigger_node,
            status: "ok",
            input: &Value::Null,
            output: &context,
            ms: 0.0,
            error: None,
            started: now,
        },
    )?;
    if is_periodic {
        steve_runtime::jobs::record_periodic_triggered(PERIODIC_TASK_SCHEDULE);
    }
    Ok((id, true))
}

/// Claims one trigger. `None` = someone else claimed it first (or it is broken and was disabled).
fn claim(
    w: &mut W<'_>,
    d: &steve_store::DueTrigger,
    now: i64,
) -> FlowResult<Option<(String, bool)>> {
    let cron = d.cron.as_deref().unwrap_or_default();
    let tz = d.tz.as_deref().unwrap_or_default();
    let (Ok(expr), Ok(tz)) = (CronExpr::parse(cron), zone(tz)) else {
        tracing::error!(trigger = %d.id, cron, tz, "unusable time trigger, disabled");
        FlowTriggers(w).disable(&d.id)?;
        return Ok(None);
    };
    let plan = plan_claim(
        &expr,
        tz,
        d.next_at,
        now,
        MissedPolicy::from_db(d.missed.as_deref()),
    );
    let f = match flow_info(w, &d.flow_id).map_err(crate::FlowError::from) {
        Ok(Some(f)) => f,
        Ok(None) => return Ok(None),
        Err(e) if e.status() < 500 => {
            tracing::error!(trigger = %d.id, error = %e, "invalid flow; time trigger disabled");
            FlowTriggers(w).disable(&d.id)?;
            return Ok(None);
        }
        Err(e) => return Err(e),
    };
    // Digest Bot schedules are driven exclusively by the durable digest packet queue (`digest.bot`);
    // suppress and disable any residual wall-clock cron trigger so idle workspaces never start sessions.
    if w.digest_packets().schedule(&f.ws)?.as_deref() == Some(f.id.as_str()) {
        FlowTriggers(w).disable(&d.id)?;
        steve_runtime::jobs::record_periodic_skipped(
            PERIODIC_TASK_DIGEST_CRON,
            steve_runtime::jobs::SkipReason::QueueDriven,
        );
        steve_runtime::jobs::record_model_calls_avoided(
            PERIODIC_TASK_DIGEST_CRON,
            steve_runtime::jobs::AvoidReason::QueueDriven,
            1,
        );
        return Ok(None);
    }
    if !FlowTriggers(w).advance(&d.id, d.next_at, plan.next_at_ms)? {
        return Ok(None);
    }
    let started = match plan.action {
        ClaimAction::Skip { missed } => {
            let label = plan.action.label().unwrap_or_default();
            let ctx = json!({ "at": plan.at_ms, "trigger": "time", "missed": missed });
            let id = RunRows(w).insert(&NewRun {
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
            })?;
            steve_runtime::jobs::record_periodic_skipped(
                PERIODIC_TASK_SCHEDULE,
                steve_runtime::jobs::SkipReason::NotDue,
            );
            (id, false)
        }
        ClaimAction::CatchUp { late_ms, missed } => start(
            w,
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
            w,
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
    let due = cx
        .store()
        .read(|r| FlowTriggers(r).due(now, BATCH as u64))?;
    for locator in due {
        let result = crate::claim::row(
            cx.store(),
            "flow_triggers",
            now,
            |w| FlowTriggers(w).scope_at(locator),
            |w| match FlowTriggers(w).due_at(locator, now)? {
                Some(row) => claim(w, &row, now),
                None => Ok(None),
            },
            |w, _| {
                FlowTriggers(w).disable_at(locator)?;
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
    let due = cx
        .store()
        .read(|r| steve_store::RunRows(r).due_retries(now, BATCH as u64))?;
    for (id, ws) in due {
        let result = cx.store().write(|w| -> FlowResult<_> {
            let attempted = w
                .scope(|w| steve_store::RunRows(w).claim_retry(&id, now, super::exec::MAX_RETRIES));
            let changed = match attempted {
                Ok(changed) => changed,
                Err(error) if error.is_storage_failure() => return Err(error.into()),
                Err(error) => {
                    let changed = steve_store::RunRows(w).fail_due(
                        &id,
                        now,
                        &format!("Retry claim failed: {error}"),
                    )?;
                    crate::claim::record_failure(w, "flow_runs", &ws, &id, &error, now);
                    changed
                }
            };
            Ok(changed.then_some(id))
        });
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
        .write(|w| start_manual_in(w, ws, flow_id, cx.now()).map_err(Into::into))
}

fn start_manual_in(
    w: &mut W<'_>,
    ws: &str,
    flow_id: &str,
    now: i64,
) -> StartResult<(String, bool)> {
    let f = flow_info(w, flow_id)?
        .filter(|f| f.ws == ws)
        .ok_or_else(|| StartError::Row(ApiError::not_found("定时任务")))?;
    let trigger = FlowTriggers(w)
        .time_node(flow_id)?
        .unwrap_or_else(|| "t".into());
    start(
        w,
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
pub fn startup_revision(r: &mut impl Read, ws: &str, id: &str) -> FlowResult<Option<Value>> {
    let Some(schedule) = steve_store::Schedules(r).live(ws, id)? else {
        return Ok(None);
    };
    let spec: crate::schedules::Spec = serde_json::from_value(schedule.spec)?;
    let bot_revision = steve_store::BotRows(r).revision(&spec.bot)?;
    Ok(Some(json!({
        "workspace": ws, "flow": id, "flow_version": schedule.version, "flow_updated": schedule.updated,
        "bot": spec.bot, "bot_revision": bot_revision,
    })))
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
    ready: impl FnOnce(&mut W<'_>) -> FlowResult<bool>,
) -> steve_store::Result<Option<(String, bool)>> {
    cx.store().write(|w| {
        if !ready(w).map_err(source_error)? {
            return Ok(None);
        }
        let Some(revision) = startup_revision(w, ws, id).map_err(source_error)? else {
            return Ok(None);
        };
        let state = runs::startup_state(w, ws, id, source, &revision).map_err(source_error)?;
        if state.is_some_and(|s| s.state == runs::StartupStatus::Dead) {
            return Ok(None);
        }
        // Store's nested transaction is a savepoint: no partial attempt/events survive rejection.
        match w.scope(|w| start_manual_in(w, ws, id, now)) {
            Ok((run, go)) => {
                if go {
                    runs::record_started(w, &run, source, &revision, limit)
                        .map_err(source_error)?;
                }
                Ok(Some((run, go)))
            }
            Err(StartError::Row(error)) => {
                runs::record_start_rejection(
                    w,
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
                    &rejection_detail(&error),
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
    cx.store().write(|w| {
        let ids = RunRows(w).interrupted(true)?;
        for id in &ids {
            if let Some(mut r) = RunRows(w).get(id)? {
                r.status = "failed".into();
                r.error = Some(INTERRUPTED.into());
                r.outcome_label = INTERRUPTED.into();
                r.ended = Some(now);
                RunRows(w).save(&r)?;
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
    deadline: impl FnOnce(&mut R<'_>, i64) -> steve_store::Result<Option<i64>> + Send + 'static,
) -> impl std::future::Future<Output = Result<Option<i64>, steve_runtime::due::DueError>> + Send {
    async move {
        let now = cx.now();
        cx.blocking(move |s| Ok(s.read(|r| deadline(r, now))?))
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
        next(self.cx.clone(), |r, now| FlowTriggers(r).next_deadline(now))
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
        next(self.cx.clone(), |r, now| FlowWaits(r).next_deadline(now))
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
        next(self.cx.clone(), |r, now| RunRows(r).next_retry(now))
    }
    fn run_due(
        &self,
        now: i64,
    ) -> impl std::future::Future<Output = Result<(), steve_runtime::due::DueError>> + Send {
        process(self.cx.clone(), "flow.retries", now, claim_retries)
    }
}

#[cfg(test)]
mod rejection_identity_tests {
    use super::*;
    #[test]
    fn r4g_fix_typed_rejection_identity_is_independent_of_locale_display() {
        for error in [ApiError::not_found("定时任务"), ApiError::invalid("field")] {
            let zh = steve_i18n::with(steve_i18n::ZH_CN, || error.to_string());
            let en = steve_i18n::with(steve_i18n::EN, || error.to_string());
            assert_ne!(zh, en);
            let zh = steve_i18n::with(steve_i18n::ZH_CN, || {
                steve_store::sha(&rejection_detail(&error))
            });
            let en = steve_i18n::with(steve_i18n::EN, || {
                steve_store::sha(&rejection_detail(&error))
            });
            assert_eq!(zh, en);
        }
    }
}
