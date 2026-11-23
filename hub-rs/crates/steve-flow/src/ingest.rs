//! The receive pipeline `ingest(ws, conn, RawEvent)`, the poll job
//! `flow.poll` and the push entry the `/hooks` route (S6) calls.
//!
//! 1. paused source, type turned off, scope `ignore` → dropped, only counted;
//! 2. `dedupe_key = <connection_id>:<external_id>`; a repeat returns the existing id;
//! 3. the sender is identified on the connection owner's behalf;
//! 4. visibility: a connection of someone's “我” → `private` to them, otherwise `workspace`;
//! 5. scope `record` (or [`RawEvent::record_only`]) → `recorded`, no flow;
//! 6. every enabled event trigger that matches opens a run (driven in the background); none →
//!    `recorded` “没有流程，只记录”;
//! 7. waiting `wait(event)` nodes that match are handed to the executor;
//! 8. SSE `signal` (coalesced per workspace, [`crate::signals::SignalDebounce`]).

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use serde_json::{Value, json};
use steve_modules::ApiError;
use steve_modules::accounts::{Hints, identify::identify};
use steve_runtime::due::{DueError, DueSource, RowDeadlines, due_loop};
use steve_runtime::policy::{Backoff, Jitter, Limiter, RateLimit};
use steve_store::{Contacts, W, uid};

use crate::adapter::{
    ActorRef, AdapterError, DropReason, EventSink, IngestMode, Ingested, PushRequest, RawEvent,
};
use crate::sources::{self, ScopeMode};
use crate::{FlowCx, FlowResult, signals};
use steve_store::{
    Connections, FlowSources, FlowTriggers, FlowWaits, NewRun, NewSignal, RunRows, SignalRows,
};

/// Summaries are cut to this many characters.
pub const SUMMARY_MAX: usize = 200;
/// `outcome_label` of an event from a `record` scope.
pub const LABEL_RECORD: &str = "只记录";
/// `outcome_label` when no flow takes the event.
pub const LABEL_NO_FLOW: &str = "没有流程，只记录";

/// Dropped events of the last 24 hours, per connection and type (job `flow.stats`; memory only, so
/// the source page's “24 小时 n 条” forgets them on restart).
#[derive(Default)]
pub struct DropStats(Mutex<VecDeque<(i64, String, String)>>);

/// Entries kept at most (a flood of dropped events must not grow memory without bound).
const DROP_CAP: usize = 100_000;

impl DropStats {
    fn lock(&self) -> std::sync::MutexGuard<'_, VecDeque<(i64, String, String)>> {
        self.0.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub fn add(&self, at: i64, conn: &str, type_key: &str) {
        let mut q = self.lock();
        while q.front().is_some_and(|(t, ..)| *t < at - sources::DAY_MS) || q.len() >= DROP_CAP {
            q.pop_front();
        }
        q.push_back((at, conn.into(), type_key.into()));
    }

    /// Dropped events of these connections and type since `since`.
    pub fn count(&self, conns: &[String], type_key: &str, since: i64) -> i64 {
        self.lock()
            .iter()
            .filter(|(t, c, k)| *t >= since && k == type_key && conns.contains(c))
            .count() as i64
    }
}

/// The connection facts the pipeline needs.
struct ConnInfo {
    adapter: String,
    created_by: String,
    /// The user whose “我” owns the account (a personal connection).
    owner_user: Option<String>,
}

fn conn_info(w: &mut W<'_>, ws: &str, id: &str) -> FlowResult<ConnInfo> {
    let row = Connections(w)
        .info(ws, id)?
        .ok_or_else(|| ApiError::not_found("连接"))?;
    let (_, owner_user) = Contacts::new(w)
        .account_owners([row.account.as_str()])?
        .remove(&row.account)
        .ok_or_else(|| ApiError::not_found("连接"))?;
    Ok(ConnInfo {
        adapter: row.adapter,
        created_by: row.created_by,
        owner_user,
    })
}

fn cut(s: &str, max: usize) -> String {
    let s = s.trim();
    match s.char_indices().nth(max) {
        Some((i, _)) => s[..i].to_string(),
        None => s.to_string(),
    }
}

/// Identifies the sender in a savepoint: a failure leaves no half-created contact and does not stop
/// the event.
fn identify_actor(
    tx: &mut W<'_>,
    ws: &str,
    on_behalf: Option<&str>,
    actor: Option<&ActorRef>,
) -> (Option<String>, Option<String>) {
    match actor {
        None => (None, None),
        Some(ActorRef::Known {
            account_id,
            party_id,
        }) => (Some(account_id.clone()), Some(party_id.clone())),
        Some(ActorRef::Handle {
            system,
            handle,
            display_name,
            signature_org,
        }) => {
            let Some(user) = on_behalf else {
                return (None, None);
            };
            let hints = Hints {
                display_name: display_name.clone(),
                signature_org: signature_org.clone(),
                ..Hints::default()
            };
            match tx.scope(|w| identify(w, ws, user, system, handle, &hints)) {
                Ok(id) => (Some(id.account_id), Some(id.party_id)),
                Err(e) => {
                    tracing::warn!(ws, system, "flow ingest: sender not identified: {e}");
                    (None, None)
                }
            }
        }
    }
}

/// The event object flows see (`{{event.…}}`, run `context.event`).
pub fn event_json(c: &mut impl steve_store::Read, id: &str) -> steve_store::Result<Value> {
    let event = SignalRows(c)
        .event(id)?
        .ok_or(steve_store::StoreError::NotFound("信号"))?;
    Ok(serde_json::to_value(event)?)
}

/// Empty = no restriction.
fn allows(list: &[String], v: Option<&str>) -> bool {
    list.is_empty() || v.is_some_and(|v| list.iter().any(|x| x == v))
}

/// A trigger that takes the event: `(flow_id, flow_version, node_id, trigger kind)`.
type Hit = (String, i64, String, String);

/// The enabled event triggers matching `ev`, one per flow.
fn matching_triggers(
    tx: &mut W<'_>,
    ws: &str,
    conn: Option<&str>,
    ev: &RawEvent,
) -> FlowResult<Vec<Hit>> {
    let mut out: Vec<Hit> = Vec::new();
    let rows = FlowTriggers(tx).for_source(ws, &ev.source)?;
    for row in rows {
        let type_ok = row.types.is_empty() || ev.types().any(|t| row.types.iter().any(|x| x == t));
        if type_ok
            && allows(&row.connections, conn)
            && allows(&row.scopes, ev.scope_ext_id.as_deref())
            && allows(&row.spaces, ev.space_id.as_deref())
            && !out.iter().any(|hit| hit.0 == row.flow)
            && !ev.skip_flows.contains(&row.flow)
        {
            out.push((row.flow, row.version, row.node, row.kind));
        }
    }
    Ok(out)
}

/// Event waits whose source and type match; the executor checks their condition.
fn matching_waits(tx: &mut W<'_>, ws: &str, ev: &RawEvent) -> FlowResult<Vec<(String, String)>> {
    let rows = FlowWaits(tx).matching_event(ws)?;
    let types: Vec<&str> = ev.types().collect();
    Ok(rows
        .into_iter()
        .filter(|row| wait_takes(row.matcher.as_deref(), &ev.source, &types))
        .map(|row| (row.run, row.node))
        .collect())
}

/// Does a `wait(event)` with this stored `match` take an event of `source` and `types`? An empty or
/// missing `source` / `type` takes any; `type` is one type or a list (`match.when` is the
/// executor's to evaluate).
pub(crate) fn wait_takes(m: Option<&str>, source: &str, types: &[&str]) -> bool {
    let m: Value = m
        .and_then(|m| serde_json::from_str(m).ok())
        .unwrap_or(Value::Null);
    let source_ok = m["source"]
        .as_str()
        .is_none_or(|s| s.is_empty() || s == source);
    let type_ok = match &m["type"] {
        Value::String(t) => t.is_empty() || types.contains(&t.as_str()),
        Value::Array(ts) => {
            ts.is_empty()
                || ts
                    .iter()
                    .any(|t| t.as_str().is_some_and(|t| types.contains(&t)))
        }
        _ => true,
    };
    source_ok && type_ok
}

/// What the transaction decided, for the work after the commit.
pub(crate) struct Stored {
    pub(crate) id: String,
    pub(crate) runs: Vec<String>,
    /// Matched waits the executor resumes after the commit (`resume_event`).
    pub(crate) waits: Vec<(String, String)>,
    /// Runs woken in the transaction: only driven after the commit.
    woken: Vec<String>,
}

pub(crate) enum Step {
    Done(Ingested),
    Stored(Stored),
}

pub(crate) fn check(ev: &RawEvent) -> FlowResult<()> {
    let mut c = steve_modules::Check::new();
    c.len("source", &ev.source, 1, 40)
        .len("type_key", &ev.type_key, 1, 80)
        .len("external_id", &ev.external_id, 1, 500);
    c.done()
}

/// Takes one event in. Called by adapters through the poll job and the hooks route, and by other modules through
/// [`EventSink`]. `conn` must be a connection of `ws` (`404 没有这个连接` otherwise).
pub async fn ingest(
    cx: &Arc<FlowCx>,
    ws: &str,
    conn: Option<&str>,
    ev: RawEvent,
) -> FlowResult<Ingested> {
    check(&ev)?;
    let now = cx.now();
    // on the blocking pool: the write may wait for the writer (a retention pass, a sync). The
    // commit and the work after it are one task: a caller that stops waiting (the poll timeout, a
    // dropped hook request) cannot leave a stored event whose runs are never driven.
    let (cx, ws, conn) = (cx.clone(), ws.to_string(), conn.map(String::from));
    tokio::spawn(async move {
        let step = {
            let (cx, ws) = (cx.clone(), ws.clone());
            cx.clone()
                .blocking(move |s| s.write(|tx| store_in(tx, &cx, &ws, conn.as_deref(), &ev, now)))
                .await?
        };
        Ok(after_store(&cx, &ws, step))
    })
    .await
    .map_err(|e| ApiError::Internal(format!("task failed: {e}")))?
}

/// An event staged with its connector receipt. Drive it only after that transaction commits;
/// the persisted runs are recovered if the process exits between commit and dispatch.
pub struct IngestCommit(Step);

impl IngestCommit {
    pub fn outcome(&self) -> Ingested {
        match &self.0 {
            Step::Done(out) => out.clone(),
            Step::Stored(s) => Ingested::Stored {
                id: s.id.clone(),
                runs: s.runs.clone(),
            },
        }
    }

    pub fn dispatch(self, cx: &Arc<FlowCx>, ws: &str) {
        after_store(cx, ws, self.0);
    }
}

pub fn ingest_in(
    tx: &mut W<'_>,
    cx: &Arc<FlowCx>,
    ws: &str,
    conn: &str,
    ev: &RawEvent,
    now: i64,
) -> FlowResult<IngestCommit> {
    check(ev)?;
    Ok(IngestCommit(store_in(tx, cx, ws, Some(conn), ev, now)?))
}

/// The stored part of [`ingest`], in the caller's transaction: dedupe, the `ext_events` row, the runs it
/// opens (`running` at their trigger, so `engine::recover` drives them if the Hub stops before
/// [`after_store`]), the waits it matches. `ev` passed [`check`].
pub(crate) fn store_in(
    tx: &mut W<'_>,
    cx: &Arc<FlowCx>,
    ws: &str,
    conn: Option<&str>,
    ev: &RawEvent,
    now: i64,
) -> FlowResult<Step> {
    {
        // a known event first: a repeated delivery (the outbox's at-least-once) gets the event it
        // stored, also after the source was paused or the type turned off meanwhile
        let dedupe = format!("{}:{}", conn.unwrap_or(&ev.source), ev.external_id);
        if let Some(id) = SignalRows(tx).duplicate(ws, &dedupe)? {
            return Ok(Step::Done(Ingested::Duplicate { id }));
        }

        let info = conn.map(|id| conn_info(tx, ws, id)).transpose()?;
        let mut recorded: Option<String> = ev.record_only.clone();
        if let (Some(id), Some(info)) = (conn, &info) {
            let set = sources::settings(tx, ws, id)?;
            let drop = if set.paused {
                Some(DropReason::Paused)
            } else if !set.type_on(
                &ev.type_key,
                cx.events.default_on(&info.adapter, &ev.type_key),
            ) {
                Some(DropReason::TypeOff)
            } else {
                match set.scope_mode(ev.scope_ext_id.as_deref()) {
                    ScopeMode::Ignore => Some(DropReason::ScopeIgnored),
                    ScopeMode::Record => {
                        recorded.get_or_insert_with(|| LABEL_RECORD.into());
                        None
                    }
                    ScopeMode::Receive => None,
                }
            };
            if let Some(r) = drop {
                cx.drops.add(now, id, &ev.type_key);
                return Ok(Step::Done(Ingested::Dropped(r)));
            }
        }

        let id = store_event_row(tx, ws, conn, ev, info.as_ref(), &dedupe, now)?;

        let mut runs_opened = Vec::new();
        if recorded.is_none() {
            let hits = matching_triggers(tx, ws, conn, ev)?;
            if hits.is_empty() {
                recorded = Some(LABEL_NO_FLOW.into());
            } else {
                let event = event_json(tx, &id)?;
                for (flow_id, version, node_id, kind) in hits {
                    let run = {
                        let mut runs = RunRows(tx);
                        let run = runs.insert(&NewRun {
                            ws,
                            flow_id: &flow_id,
                            flow_version: version,
                            trigger_kind: &kind,
                            status: "running",
                            error: None,
                            outcome_label: "",
                            cursor: Some(&node_id),
                            context: &json!({ "event": event, "steps": {} }),
                            now,
                        })?;
                        runs.set_origin(&run, Some(&id), false, None, 0)?;
                        run
                    };
                    runs_opened.push(run);
                }
            }
        }
        if let Some(label) = &recorded {
            SignalRows(tx).recorded(&id, label)?;
        }
        // waits woken in this transaction (the run is `running` again, recovered on a restart);
        // an executor that can't resumes after the commit
        let mut waits = Vec::new();
        let mut woken = Vec::new();
        let matched = if ev.wake {
            matching_waits(tx, ws, ev)?
        } else {
            Vec::new()
        };
        if !matched.is_empty() {
            let event = event_json(tx, &id)?;
            for (run, node) in matched {
                match cx.exec.wake_in(tx, &run, &node, &event)? {
                    Some(true) => woken.push(run),
                    Some(false) => {}
                    None => waits.push((run, node)),
                }
            }
        }
        Ok(Step::Stored(Stored {
            id,
            runs: runs_opened,
            waits,
            woken,
        }))
    }
}

/// Snapshot event identity, visibility and payload in the caller's transaction.
fn store_event_row(
    tx: &mut W<'_>,
    ws: &str,
    conn: Option<&str>,
    ev: &RawEvent,
    info: Option<&ConnInfo>,
    dedupe: &str,
    now: i64,
) -> FlowResult<String> {
    let on_behalf = info.map(|i| i.created_by.as_str());
    let (actor_account, actor_party) = identify_actor(tx, ws, on_behalf, ev.actor.as_ref());
    let personal = info.is_some_and(|info| info.owner_user.is_some());
    let id = uid("evt");
    SignalRows(tx).insert(&NewSignal {
        id: &id,
        ws,
        connection: conn,
        source: &ev.source,
        kind: &ev.type_key,
        external_id: &ev.external_id,
        dedupe,
        thread: ev.thread_key.as_deref(),
        group: ev.group_key.as_deref(),
        scope: ev.scope_ext_id.as_deref(),
        space: ev.space_id.as_deref(),
        message: ev.message_id.as_deref(),
        actor_account: actor_account.as_deref(),
        actor_party: actor_party.as_deref(),
        occurred: ev.occurred,
        received: now,
        summary: &cut(&ev.summary, SUMMARY_MAX),
        payload: &ev.payload,
        personal,
    })?;
    Ok(id)
}

/// After the commit of [`store_in`]: the signal, the runs driven, the waits resumed.
pub(crate) fn after_store(cx: &Arc<FlowCx>, ws: &str, step: Step) -> Ingested {
    let s = match step {
        Step::Done(out) => return out,
        Step::Stored(s) => s,
    };
    cx.signals.signal(cx.store(), ws, &s.id);
    for run in &s.runs {
        spawn_run(cx.clone(), run.clone(), s.id.clone());
    }
    for run in &s.woken {
        let cx = cx.clone();
        let run = run.clone();
        tokio::spawn(async move {
            if let Err(e) = crate::engine::drive(&cx, &run).await {
                tracing::warn!(run, "flow wait resume failed: {e}");
            }
        });
    }
    for (run, node) in &s.waits {
        let cx = cx.clone();
        let (run, node, evt) = (run.clone(), node.clone(), s.id.clone());
        tokio::spawn(async move {
            if let Err(e) = cx.exec.resume_event(&cx, &run, &node, &evt).await {
                tracing::warn!(run, "flow wait resume failed: {e}");
            }
        });
    }
    Ingested::Stored {
        id: s.id,
        runs: s.runs,
    }
}

/// Drives a run in the background, then settles its event's outcome.
fn spawn_run(cx: Arc<FlowCx>, run: String, event: String) {
    tokio::spawn(async move {
        if let Err(e) = cx.exec.drive(&cx, &run).await {
            tracing::warn!(run, "flow run failed: {e}");
        }
        let ws = cx.store().write(|tx| signals::settle(tx, &event));
        match ws {
            Ok(Some(ws)) => cx.signals.signal(cx.store(), &ws, &event),
            Ok(None) => {}
            Err(e) => tracing::warn!(event, "settling the event failed: {e}"),
        }
    });
}

#[async_trait]
impl EventSink for Arc<FlowCx> {
    async fn ingest(&self, ws: &str, conn: Option<&str>, ev: RawEvent) -> FlowResult<Ingested> {
        ingest(self, ws, conn, ev).await
    }
}

/// The registered adapter of a connection of `ws`.
fn adapter_of(
    cx: &FlowCx,
    ws: &str,
    conn: &str,
) -> Result<Option<Arc<dyn crate::EventAdapter>>, AdapterError> {
    let a = cx
        .store()
        .read(|r| Connections(r).adapter_in(ws, conn))
        .map_err(|e| AdapterError::Malformed(e.to_string()))?;
    Ok(a.and_then(|a| cx.events.get(&a).cloned()))
}

/// Polls one connection once: `poll(cursor)` → `ingest` each → store the new cursor and
/// `last_sync`. A malformed event is skipped; one that can't be stored stops the pass before the cursor moves: the next
/// poll reads the batch again from the old cursor, and what was stored before it comes back as a
/// duplicate (same connection and `external_id`). Polling goes on while the source is paused, so
/// nothing piles up to be caught up on resume. Returns the events stored.
pub async fn poll_connection(
    cx: &Arc<FlowCx>,
    ws: &str,
    conn: &str,
) -> Result<usize, AdapterError> {
    let adapter = adapter_of(cx, ws, conn)?
        .ok_or_else(|| AdapterError::Malformed("没有这个事件源".into()))?;
    let ccx = cx.events.cx(ws, conn)?;
    let cursor = cx
        .store()
        .read(|r| FlowSources(r).cursor(ws, conn))
        .map_err(|e| AdapterError::Malformed(e.to_string()))?;
    let batch = match adapter.poll(&ccx, cursor.as_deref()).await {
        Ok(b) => b,
        Err(e) => {
            if let AdapterError::Provider(p) = &e {
                ccx.report(p.into()).await;
            }
            return Err(e);
        }
    };
    let mut stored = 0;
    for ev in batch.events {
        // malformed never becomes valid: skipped, or it would hold the source at this cursor forever
        if let Err(e) = check(&ev) {
            tracing::warn!(conn, external_id = %ev.external_id, "flow.poll: malformed event skipped: {e}");
            continue;
        }
        match ingest(cx, ws, Some(conn), ev).await {
            Ok(Ingested::Stored { .. }) => stored += 1,
            Ok(_) => {}
            Err(e) => {
                tracing::warn!(conn, "flow.poll: event not ingested, cursor kept: {e}");
                return Err(AdapterError::Malformed(format!(
                    "事件没有存下，下次从原位置重读：{e}"
                )));
            }
        }
    }
    let now = cx.now();
    let (ws, conn, cursor) = (ws.to_string(), conn.to_string(), batch.cursor);
    cx.blocking(move |s| {
        s.write(|w| -> FlowResult<()> {
            if let Some(cursor) = &cursor {
                FlowSources(w).save_cursor(&ws, &conn, cursor, now)?;
            }
            Connections(w).mark_synced(&ws, &conn, now)?;
            Ok(())
        })
    })
    .await
    .map_err(|e| AdapterError::Malformed(e.to_string()))?;
    Ok(stored)
}

/// A webhook for `conn` (`POST /hooks/:connection_id`, S6): the adapter verifies and parses, each
/// event is ingested. A bad signature returns before anything is written.
pub async fn push(
    cx: &Arc<FlowCx>,
    ws: &str,
    conn: &str,
    req: &PushRequest,
) -> Result<Vec<Ingested>, AdapterError> {
    let adapter = adapter_of(cx, ws, conn)?
        .filter(|a| a.mode() == IngestMode::Push)
        .ok_or(AdapterError::BadSignature)?;
    let ccx = cx.events.cx(ws, conn)?;
    let events = adapter.parse_push(&ccx, req).await?;
    let mut out = Vec::new();
    for ev in events {
        match ingest(cx, ws, Some(conn), ev).await {
            Ok(r) => out.push(r),
            Err(e) => tracing::warn!(conn, "hook: event not ingested: {e}"),
        }
    }
    Ok(out)
}

/// Longest wait after failures (back off to 30 minutes).
pub const MAX_BACKOFF: Duration = Duration::from_secs(30 * 60);
/// The pause after a completed background round, also the source-query retry interval.
pub const POLL_TICK: Duration = Duration::from_secs(15);
/// Connections polled at the same time.
pub const POLL_PARALLEL: usize = 4;
/// A poll (provider call and its ingests) that takes longer counts as failed.
pub const POLL_TIMEOUT: Duration = Duration::from_secs(5 * 60);

fn retry_wait(every: Duration, failures: u32) -> Duration {
    Backoff {
        base: every,
        // Preserve the old saturated u32 multiplier even for sub-millisecond intervals.
        max: MAX_BACKOFF.min(every.saturating_mul(u32::MAX)),
        factor: 2,
        jitter: Jitter::None,
        attempts: None,
        reset_after: None,
    }
    .sample_delay(failures.saturating_add(1))
}

fn cadence_rate() -> RateLimit {
    RateLimit::Window {
        len: POLL_TICK,
        max: 1,
        sliding: true,
    }
}

fn millis(duration: Duration) -> i64 {
    i64::try_from(duration.as_millis()).unwrap_or(i64::MAX)
}

async fn poll_bounded(
    cx: &Arc<FlowCx>,
    ws: &str,
    id: &str,
    timeout: Duration,
) -> Result<usize, AdapterError> {
    tokio::time::timeout(timeout, poll_connection(cx, ws, id))
        .await
        .unwrap_or_else(|_| Err(AdapterError::Malformed("拉取超时".into())))
}

/// Each live Poll connection owns a row deadline; a round retains the former completion gap.
pub struct Poller {
    cx: Arc<FlowCx>,
    due: RowDeadlines,
    failures: Mutex<HashMap<String, u32>>,
    serial: tokio::sync::Mutex<()>,
    cadence: Limiter<()>,
}

struct PollRow {
    id: String,
    ws: String,
    every: Duration,
    at: Option<i64>,
}

impl Poller {
    pub fn new(cx: Arc<FlowCx>) -> Self {
        Self {
            cx,
            due: RowDeadlines::default(),
            failures: Mutex::default(),
            serial: tokio::sync::Mutex::new(()),
            cadence: Limiter::new(cadence_rate()),
        }
    }

    fn failure_counts(&self) -> std::sync::MutexGuard<'_, HashMap<String, u32>> {
        self.failures.lock().unwrap_or_else(|e| e.into_inner())
    }

    async fn rows(&self) -> Result<Vec<PollRow>, DueError> {
        let cx = self.cx.clone();
        let conns = cx
            .blocking(|store| Ok(store.read(|r| Connections(r).poll_connections())?))
            .await
            .map_err(|e| DueError(e.to_string()))?;
        {
            let live: HashSet<_> = conns.iter().map(|c| c.id.as_str()).collect();
            self.due.retain_live(live.iter().copied());
            self.failure_counts()
                .retain(|id, _| live.contains(id.as_str()));
        }
        let mut rows = Vec::new();
        for conn in conns {
            let Some(adapter) = self.cx.events.get(&conn.adapter) else {
                continue;
            };
            let IngestMode::Poll { every } = adapter.mode() else {
                continue;
            };
            if !conn.caps.iter().any(|cap| cap == "events") {
                continue;
            }
            let at = self.due.not_before(&conn.id);
            rows.push(PollRow {
                id: conn.id,
                ws: conn.ws,
                every,
                at,
            });
        }
        Ok(rows)
    }

    /// One manual round retains the existing tick API, without the background round throttle.
    pub async fn tick(&mut self) -> usize {
        match self.round_at(None, false).await {
            Ok(n) => n,
            Err(e) => {
                tracing::warn!("flow.poll: {e}");
                0
            }
        }
    }

    async fn round_at(&self, at: Option<i64>, paced: bool) -> Result<usize, DueError> {
        let _serial = self.serial.lock().await;
        if paced && self.cadence.peek(&()).is_err() {
            return Ok(0);
        }
        let mut rows = self.rows().await?;
        let now = at.unwrap_or_else(|| self.cx.now());
        // A fresh row is due immediately; using None avoids assigning a later query timestamp.
        rows.retain(|row| row.at.unwrap_or(0) <= now);
        rows.sort_by(|a, b| (a.at.unwrap_or(now), &a.id).cmp(&(b.at.unwrap_or(now), &b.id)));
        let polled = rows.len();
        let mut todo = rows.into_iter();
        let mut running = tokio::task::JoinSet::new();
        loop {
            while running.len() < POLL_PARALLEL {
                let Some(row) = todo.next() else { break };
                let fails = self.failure_counts().get(&row.id).copied().unwrap_or(0);
                let cx = self.cx.clone();
                running.spawn(async move {
                    let result = poll_bounded(&cx, &row.ws, &row.id, POLL_TIMEOUT).await;
                    (row.id, row.every, fails, result)
                });
            }
            let Some(done) = running.join_next().await else {
                break;
            };
            let (id, every, fails, result) = match done {
                Ok(done) => done,
                Err(e) => {
                    tracing::error!("flow.poll task: {e}");
                    continue;
                }
            };
            match result {
                Ok(_) => {
                    self.due
                        .defer(id.clone(), now.saturating_add(millis(every)));
                    self.failure_counts().remove(&id);
                }
                Err(e) => {
                    tracing::warn!(conn = id, "flow.poll failed: {e}");
                    self.due.defer(
                        id.clone(),
                        now.saturating_add(millis(retry_wait(every, fails))),
                    );
                    self.failure_counts().insert(id, fails.saturating_add(1));
                }
            }
        }
        // This is a source-wide completion gap, not another per-connection deadline table.
        if paced && polled > 0 {
            self.cadence.hit(&());
        }
        Ok(polled)
    }

    pub async fn run(self) {
        let clock = self.cx.modules.clock.clone();
        self.run_on_clock(clock).await;
    }

    async fn run_on_clock(self, clock: Arc<dyn steve_runtime::Clock>) {
        let wake = self.cx.store().commit_wake(&["connections"]);
        due_loop(
            Arc::new(self),
            clock,
            wake,
            Backoff {
                base: POLL_TICK,
                max: POLL_TICK,
                factor: 1,
                jitter: Jitter::None,
                attempts: None,
                reset_after: None,
            },
        )
        .await;
    }
}

impl DueSource for Poller {
    fn name(&self) -> &'static str {
        "flow.ingest"
    }

    async fn next_due(&self) -> Result<Option<i64>, DueError> {
        let rows = self.rows().await?;
        let now = self.cx.now();
        let Some(at) = rows.iter().map(|row| row.at.unwrap_or(now.max(0))).min() else {
            return Ok(None);
        };
        let at = match self.cadence.peek(&()) {
            Ok(()) => at,
            Err(steve_runtime::policy::Wait(wait)) => {
                // Round up: a sub-millisecond remaining quota must not become an already-due time.
                let ms = i64::try_from(wait.as_nanos().div_ceil(1_000_000)).unwrap_or(i64::MAX);
                at.max(now.saturating_add(ms))
            }
        };
        Ok(Some(at))
    }

    async fn run_due(&self, now: i64) -> Result<(), DueError> {
        self.round_at(Some(now), true).await.map(|_| ())
    }
}

#[cfg(test)]
#[path = "../tests/ingest_due/mod.rs"]
mod poll_tests;
