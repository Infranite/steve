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

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use rusqlite::{OptionalExtension, params};
use serde_json::{Value, json};
use steve_modules::ApiError;
use steve_modules::accounts::{Hints, identify};
use steve_store::{Tx, uid};

use crate::adapter::{
    ActorRef, AdapterError, DropReason, EventSink, IngestMode, Ingested, PushRequest, RawEvent,
};
use crate::runs::{self, NewRun};
use crate::sources::{self, ScopeMode};
use crate::{FlowCx, FlowResult, signals};

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

fn conn_info(tx: &Tx, ws: &str, id: &str) -> FlowResult<ConnInfo> {
    tx.query_row(
        "SELECT c.adapter, c.created_by, p.user_id FROM connections c JOIN accounts a ON a.id = c.account_id
           LEFT JOIN parties p ON p.id = a.party_id WHERE c.id = ? AND c.workspace_id = ?",
        params![id, ws],
        |r| {
            Ok(ConnInfo {
                adapter: r.get(0)?,
                created_by: r.get(1)?,
                owner_user: r.get(2)?,
            })
        },
    )
    .optional()?
    .ok_or_else(|| ApiError::not_found("连接"))
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
    tx: &Tx,
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
            if tx.execute_batch("SAVEPOINT flow_identify").is_err() {
                return (None, None);
            }
            let hints = Hints {
                display_name: display_name.clone(),
                signature_org: signature_org.clone(),
                ..Hints::default()
            };
            match identify(tx, ws, user, system, handle, &hints) {
                Ok(id) => {
                    let _ = tx.execute_batch("RELEASE flow_identify");
                    (Some(id.account_id), Some(id.party_id))
                }
                Err(e) => {
                    tracing::warn!(ws, system, "flow ingest: sender not identified: {e}");
                    let _ = tx.execute_batch("ROLLBACK TO flow_identify; RELEASE flow_identify");
                    (None, None)
                }
            }
        }
    }
}

/// The event object flows see (`{{event.…}}`, run `context.event`).
pub fn event_json(c: &rusqlite::Connection, id: &str) -> FlowResult<Value> {
    Ok(c.query_row(
        "SELECT id, source, type, connection_id, external_id, thread_key, group_key, scope_ext_id, space_id, message_id,
                actor_account_id, actor_party_id, occurred, received, summary, payload FROM ext_events WHERE id = ?",
        [id],
        |r| {
            let payload: Option<String> = r.get(15)?;
            Ok(json!({
                "id": r.get::<_, String>(0)?,
                "source": r.get::<_, String>(1)?,
                "type": r.get::<_, String>(2)?,
                "connection_id": r.get::<_, Option<String>>(3)?,
                "external_id": r.get::<_, Option<String>>(4)?,
                "thread_key": r.get::<_, Option<String>>(5)?,
                "group_key": r.get::<_, Option<String>>(6)?,
                "scope_ext_id": r.get::<_, Option<String>>(7)?,
                "space_id": r.get::<_, Option<String>>(8)?,
                "message_id": r.get::<_, Option<String>>(9)?,
                "actor_account_id": r.get::<_, Option<String>>(10)?,
                "actor_party_id": r.get::<_, Option<String>>(11)?,
                "occurred": r.get::<_, i64>(12)?,
                "received": r.get::<_, i64>(13)?,
                "summary": r.get::<_, String>(14)?,
                "payload": payload.and_then(|p| serde_json::from_str::<Value>(&p).ok()).unwrap_or(Value::Null),
            }))
        },
    )?)
}

fn json_list(s: &str) -> Vec<String> {
    serde_json::from_str(s).unwrap_or_default()
}

/// Empty = no restriction.
fn allows(list: &[String], v: Option<&str>) -> bool {
    list.is_empty() || v.is_some_and(|v| list.iter().any(|x| x == v))
}

/// A trigger that takes the event: `(flow_id, flow_version, node_id, trigger kind)`.
type Hit = (String, i64, String, String);

/// The enabled event triggers matching `ev`, one per flow.
fn matching_triggers(tx: &Tx, ws: &str, conn: Option<&str>, ev: &RawEvent) -> FlowResult<Vec<Hit>> {
    let mut st = tx.prepare_cached(
        "SELECT t.flow_id, f.version, t.node_id, t.kind, t.types, t.connections, t.scopes, t.spaces
           FROM flow_triggers t JOIN flows f ON f.id = t.flow_id
          WHERE t.workspace_id = ? AND t.kind IN ('event', 'message', 'item') AND t.enabled = 1
            AND f.enabled = 1 AND f.deleted = 0 AND (t.source IS NULL OR t.source = '' OR t.source = ?)
          ORDER BY f.created, t.flow_id",
    )?;
    let rows = st
        .query_map(params![ws, ev.source], |r| {
            Ok((
                (r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?),
                [r.get::<_, String>(4)?, r.get(5)?, r.get(6)?, r.get(7)?],
            ))
        })?
        .collect::<rusqlite::Result<Vec<(Hit, [String; 4])>>>()?;
    let mut out: Vec<Hit> = Vec::new();
    for (hit, [types, conns, scopes, spaces]) in rows {
        let types = json_list(&types);
        let type_ok = types.is_empty() || ev.types().any(|t| types.iter().any(|x| x == t));
        if type_ok
            && allows(&json_list(&conns), conn)
            && allows(&json_list(&scopes), ev.scope_ext_id.as_deref())
            && allows(&json_list(&spaces), ev.space_id.as_deref())
            && !out.iter().any(|h| h.0 == hit.0)
            && !ev.skip_flows.contains(&hit.0)
        {
            out.push(hit);
        }
    }
    Ok(out)
}

/// `flow_waits(mode=event)` of waiting runs whose `match.source` / `match.type` take the event
/// (`match.when` is the executor's to evaluate): `(run_id, node_id)`.
fn matching_waits(tx: &Tx, ws: &str, ev: &RawEvent) -> FlowResult<Vec<(String, String)>> {
    let mut st = tx.prepare_cached(
        "SELECT w.run_id, w.node_id, w.match FROM flow_waits w JOIN flow_runs r ON r.id = w.run_id
          WHERE w.workspace_id = ? AND w.mode = 'event' AND r.status = 'waiting' ORDER BY w.created",
    )?;
    let rows = st
        .query_map([ws], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, Option<String>>(2)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let types: Vec<&str> = ev.types().collect();
    Ok(rows
        .into_iter()
        .filter(|(_, _, m)| wait_takes(m.as_deref(), &ev.source, &types))
        .map(|(r, n, _)| (r, n))
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

/// §2.2. Called by adapters through the poll job and the hooks route, and by other modules through
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
                .blocking(move |s| {
                    s.transaction(|tx| store_in(tx, &cx, &ws, conn.as_deref(), &ev, now))
                })
                .await?
        };
        Ok(after_store(&cx, &ws, step))
    })
    .await
    .map_err(|e| ApiError::Internal(format!("task failed: {e}")))?
}

/// The stored part of [`ingest`], in the caller's transaction: dedupe, the `ext_events` row, the runs it
/// opens (`running` at their trigger, so `engine::recover` drives them if the Hub stops before
/// [`after_store`]), the waits it matches. `ev` passed [`check`].
pub(crate) fn store_in(
    tx: &steve_store::Tx,
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
        if let Some(id) = tx
            .query_row(
                "SELECT id FROM ext_events WHERE workspace_id = ? AND dedupe_key = ?",
                params![ws, dedupe],
                |r| r.get::<_, String>(0),
            )
            .optional()?
        {
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

        let on_behalf = info.as_ref().map(|i| i.created_by.as_str());
        let (actor_account, actor_party) = identify_actor(tx, ws, on_behalf, ev.actor.as_ref());
        let visibility = match &info {
            Some(i) if i.owner_user.is_some() => "private",
            _ => "workspace",
        };
        let id = uid("evt");
        let payload = (!ev.payload.is_null()).then(|| ev.payload.to_string());
        tx.execute(
            "INSERT INTO ext_events (id, workspace_id, connection_id, source, type, external_id, dedupe_key, thread_key, group_key,
                scope_ext_id, space_id, message_id, actor_account_id, actor_party_id, occurred, received, summary, payload, visibility)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
            params![
                id,
                ws,
                conn,
                ev.source,
                ev.type_key,
                ev.external_id,
                dedupe,
                ev.thread_key,
                ev.group_key,
                ev.scope_ext_id,
                ev.space_id,
                ev.message_id,
                actor_account,
                actor_party,
                ev.occurred,
                now,
                cut(&ev.summary, SUMMARY_MAX),
                payload,
                visibility
            ],
        )?;

        let mut runs_opened = Vec::new();
        if recorded.is_none() {
            let hits = matching_triggers(tx, ws, conn, ev)?;
            if hits.is_empty() {
                recorded = Some(LABEL_NO_FLOW.into());
            } else {
                let event = event_json(tx, &id)?;
                for (flow_id, version, node_id, kind) in hits {
                    let run = runs::insert(
                        tx,
                        &NewRun {
                            ws,
                            flow_id: &flow_id,
                            flow_version: version,
                            trigger_kind: &kind,
                            status: "running",
                            outcome_label: "",
                            cursor: Some(&node_id),
                            context: &json!({ "event": event, "steps": {} }),
                            now,
                        },
                    )?;
                    tx.execute(
                        "UPDATE flow_runs SET event_id = ? WHERE id = ?",
                        params![id, run],
                    )?;
                    runs_opened.push(run);
                }
            }
        }
        if let Some(label) = &recorded {
            tx.execute(
                "UPDATE ext_events SET outcome = 'recorded', outcome_label = ? WHERE id = ?",
                params![label, id],
            )?;
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
        let ws = cx.store().transaction(|tx| signals::settle(tx, &event));
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
        .reader(|c| -> FlowResult<Option<String>> {
            Ok(c.query_row(
                "SELECT adapter FROM connections WHERE id = ? AND workspace_id = ?",
                params![conn, ws],
                |r| r.get::<_, String>(0),
            )
            .optional()?)
        })
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
        .reader(|c| sources::settings(c, ws, conn))
        .map_err(|e| AdapterError::Malformed(e.to_string()))?
        .cursor;
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
        s.transaction(|tx| -> FlowResult<()> {
            if let Some(c) = &cursor {
                sources::save_cursor(tx, &ws, &conn, c, now)?;
            }
            tx.execute(
                "UPDATE connections SET last_sync = ? WHERE id = ?",
                params![now, conn],
            )?;
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
/// How often the poller looks for due connections.
pub const POLL_TICK: Duration = Duration::from_secs(15);
/// Connections polled at the same time.
pub const POLL_PARALLEL: usize = 4;
/// A poll (provider call and its ingests) that takes longer counts as failed.
pub const POLL_TIMEOUT: Duration = Duration::from_secs(5 * 60);

/// The `flow.poll` job: every [`IngestMode::Poll`] connection with the `events` capability, at its
/// adapter's interval; failures double the wait up to [`MAX_BACKOFF`].
pub struct Poller {
    cx: Arc<FlowCx>,
    /// connection → (next due, consecutive failures)
    due: HashMap<String, (i64, u32)>,
}

impl Poller {
    pub fn new(cx: Arc<FlowCx>) -> Self {
        Self {
            cx,
            due: HashMap::new(),
        }
    }

    /// One round: polls the connections that are due. Returns how many were polled.
    pub async fn tick(&mut self) -> usize {
        let conns = match self.cx.store().reader(|c| -> FlowResult<Vec<(String, String, String, String)>> {
            let mut st = c.prepare_cached(
                "SELECT id, workspace_id, adapter, caps FROM connections WHERE status IN ('ok', 'error') ORDER BY created, id",
            )?;
            let rows = st
                .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(rows)
        }) {
            Ok(c) => c,
            Err(e) => {
                tracing::warn!("flow.poll: {e}");
                return 0;
            }
        };
        let now = self.cx.now();
        // a connection that is gone is no longer tracked
        self.due.retain(|id, _| conns.iter().any(|c| &c.0 == id));
        let mut todo = Vec::new();
        for (id, ws, adapter, caps) in conns {
            let Some(a) = self.cx.events.get(&adapter) else {
                continue;
            };
            let IngestMode::Poll { every } = a.mode() else {
                continue;
            };
            if !json_list(&caps).iter().any(|c| c == "events") {
                continue;
            }
            let (next, fails) = self.due.get(&id).copied().unwrap_or((0, 0));
            if next > now {
                continue;
            }
            todo.push((id, ws, every, fails));
        }
        let polled = todo.len();
        // a few at a time, so one slow upstream does not hold up the others; a connection is in
        // the list once, so its own polls stay one after another
        let mut todo = todo.into_iter();
        let mut running = tokio::task::JoinSet::new();
        loop {
            while running.len() < POLL_PARALLEL {
                let Some((id, ws, every, fails)) = todo.next() else {
                    break;
                };
                let cx = self.cx.clone();
                running.spawn(async move {
                    let r = tokio::time::timeout(POLL_TIMEOUT, poll_connection(&cx, &ws, &id))
                        .await
                        .unwrap_or_else(|_| Err(AdapterError::Malformed("拉取超时".into())));
                    (id, every, fails, r)
                });
            }
            let Some(done) = running.join_next().await else {
                break;
            };
            let (id, every, fails, r) = match done {
                Ok(x) => x,
                // a panicked poll leaves its connection due; it is tried again next round
                Err(e) => {
                    tracing::error!("flow.poll task: {e}");
                    continue;
                }
            };
            match r {
                Ok(_) => {
                    self.due.insert(id, (now + every.as_millis() as i64, 0));
                }
                Err(e) => {
                    tracing::warn!(conn = id, "flow.poll failed: {e}");
                    let back = every.saturating_mul(2u32.saturating_pow(fails + 1));
                    let wait = back.min(MAX_BACKOFF);
                    self.due
                        .insert(id, (now + wait.as_millis() as i64, fails + 1));
                }
            }
        }
        polled
    }

    /// Runs forever.
    pub async fn run(mut self) {
        loop {
            self.tick().await;
            tokio::time::sleep(POLL_TICK).await;
        }
    }
}
