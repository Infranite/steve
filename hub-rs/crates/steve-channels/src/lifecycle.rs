//! What runs for each connection, kept in step with the connections as they change, with no Hub
//! restart: a channel-side adapter registers a [`ConnectionHook`] for its `connections.adapter`, and
//! the [`ConnectionRuntime`] runs it once per connection the hook [wants](ConnectionHook::wants).
//!
//! - The runtime taps the `connection` events first, then lists the connections, so a change
//!   between the two is not lost.
//! - Each connection has its own supervisor task, driven by the latest wanted state: a slow stop of
//!   one connection never holds up another.
//! - A change of the hook's [fingerprint](ConnectionHook::fingerprint) (a new credential, another
//!   paired connection) restarts the run: the old run's `stop` is cancelled and awaited (aborted
//!   after 30 s) before the new one starts. The old run's [`ConnSink`] refuses from then on.
//! - A run that returns `Ok` is done: it starts again only once the hook stopped wanting the
//!   connection and wants it again, or the fingerprint changed. An `Err` or a panic is retried with
//!   a backoff (5 s doubling to 5 min) while the connection is wanted, and reported to the
//!   connection's health (`ProviderError::Auth` → `reauth`, anything else a failure). What a run
//!   that was stopped returns is dropped, and so is the failure of a run the connection has moved
//!   past before its event arrived (a new credential, not wanted any more): the connection is read
//!   again and the run follows what it is now.
//! - An event on a connection also re-evaluates the connections paired with it
//!   (`config.app_connection`, [`PAIR_KEY`]).
//! - Before a run, the connection gets its `channel_settings` in the adapter's declared mode.
//!
//! The types an adapter implements and receives ([`ConnectionHook`], [`ConnState`], [`ConnRun`],
//! [`ConnSink`]) are part of the public extension API (`ext`); the runtime itself is the Hub's.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use rusqlite::{OptionalExtension, params};
use serde_json::Value;
use steve_flow::AdapterError;
use steve_modules::accounts::connections::{ConnCx, Health, PAIR_KEY, Paired};
use tokio::sync::{mpsc, watch};
use tokio::task::JoinHandle;
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

use crate::channel::{InboundMessage, SpaceEvent};
use crate::ext::ChannelDecl;
use crate::inbound::Inbound;
use crate::membership::SpaceChange;
use crate::queries::ConnQueries;
use crate::spaces;
use crate::{ChannelError, ChannelResult, Channels, MessageSink};

/// One connection as a [`ConnectionHook`] sees it.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct ConnState {
    pub ws: String,
    pub id: String,
    pub adapter: String,
    /// `connections.status`: `ok`, `error`, `paused`, `reauth`, `revoked`.
    pub status: String,
    /// The capabilities turned on.
    pub caps: Vec<String>,
    /// The vault item holding its credential (an id, never the secret).
    pub credential: Option<String>,
    /// `connections.config` (no secrets).
    pub config: Value,
    /// The connection it is paired with, for a provider with a pairing.
    pub paired: Option<Paired>,
    /// `ok` or `error`, the receive capability on (when the adapter declares one), and the pairing
    /// working when the provider requires one.
    pub live: bool,
}

/// What one run of a hook gets.
#[non_exhaustive]
pub struct ConnRun {
    pub cx: ConnCx,
    pub state: ConnState,
    /// Cancelled when the run is to end (stopped, restarted, Hub shutdown).
    pub stop: CancellationToken,
    /// Bound to this connection and this run; refuses once `stop` is cancelled.
    pub inbound: ConnSink,
    pub queries: ConnQueries,
}

/// The inbound entry point of one run of one connection.
#[derive(Clone)]
pub struct ConnSink {
    channels: Channels,
    ws: String,
    conn: String,
    /// The run's: cancelled, this sink is of a run that ended.
    run: CancellationToken,
}

impl std::fmt::Debug for ConnSink {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ConnSink")
            .field("ws", &self.ws)
            .field("conn", &self.conn)
            .field("stopped", &self.is_stopped())
            .finish()
    }
}

impl ConnSink {
    pub(crate) fn new(channels: Channels, ws: &str, conn: &str, run: CancellationToken) -> Self {
        Self {
            channels,
            ws: ws.to_string(),
            conn: conn.to_string(),
            run,
        }
    }

    /// The run it belongs to has been stopped: everything is refused.
    pub fn is_stopped(&self) -> bool {
        self.run.is_cancelled()
    }

    fn open(&self) -> ChannelResult<()> {
        if self.is_stopped() {
            return Err(ChannelError::Conflict(steve_i18n::t!(
                "channels.error.run_stopped"
            )));
        }
        Ok(())
    }

    /// A message for the inbound pipeline (stored, deduplicated, answered by the rules).
    pub async fn message(&self, msg: InboundMessage) -> ChannelResult<Inbound> {
        self.open()?;
        self.channels.on_message(&self.ws, &self.conn, msg).await
    }

    /// A change of a space or its members.
    pub async fn space_event(&self, ev: SpaceEvent) -> ChannelResult<Option<SpaceChange>> {
        self.open()?;
        self.channels.on_space_event(&self.ws, &self.conn, ev).await
    }
}

/// As a [`MessageSink`] for code written against it: only for its own workspace and connection.
#[async_trait]
impl MessageSink for ConnSink {
    async fn inbound(&self, ws: &str, conn: &str, msg: InboundMessage) -> ChannelResult<Inbound> {
        if ws != self.ws || conn != self.conn {
            return Err(ChannelError::not_found("连接"));
        }
        self.message(msg).await
    }

    async fn space_event(
        &self,
        ws: &str,
        conn: &str,
        ev: SpaceEvent,
    ) -> ChannelResult<Option<SpaceChange>> {
        if ws != self.ws || conn != self.conn {
            return Err(ChannelError::not_found("连接"));
        }
        ConnSink::space_event(self, ev).await
    }
}

/// What runs for each connection of one adapter (a long connection, a poller, a stream reader).
#[async_trait]
pub trait ConnectionHook: Send + Sync + 'static {
    /// Whether it runs for this connection now. Default: [`ConnState::live`].
    fn wants(&self, s: &ConnState) -> bool {
        s.live
    }

    /// Default: the credential item and the paired connection's id. Never logged.
    /// A change of this value restarts the run (the old run's `stop` is cancelled first). Override it
    /// to include config fields, so a config or status change restarts the run as well.
    fn fingerprint(&self, s: &ConnState) -> String {
        default_fingerprint(s)
    }

    /// Runs until `run.stop` is cancelled, or until there is nothing more to do.
    /// `Ok`: finished, started again only after `wants` went false → true or the fingerprint changed.
    /// `Err` or a panic: retried with a backoff (5 s doubling to 5 min) while wanted, reported to the
    /// connection's health (`ProviderError::Auth` → reauth, otherwise a failure). What a stopped run
    /// returns is dropped. Once connected, the run reports `Health::Ok` itself (`run.cx.report`).
    async fn run(&self, run: ConnRun) -> Result<(), AdapterError>;
}

/// [`ConnectionHook::fingerprint`]'s default.
pub fn default_fingerprint(s: &ConnState) -> String {
    format!(
        "{}\n{}",
        s.credential.as_deref().unwrap_or_default(),
        s.paired.as_ref().map(|p| p.id.as_str()).unwrap_or_default()
    )
}

// ---------------------------------------------------------------------------------------------
// The runtime.

/// The runtime's waits. [`Timing::default`] is what the Hub runs with.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Timing {
    /// The first wait after a failed run; doubles up to `backoff_max`.
    pub backoff_min: Duration,
    pub backoff_max: Duration,
    /// How long a restart waits for the old run to end before aborting it.
    pub stop_grace: Duration,
    /// The same at Hub shutdown.
    pub shutdown_grace: Duration,
}

impl Default for Timing {
    fn default() -> Self {
        Self {
            backoff_min: Duration::from_secs(5),
            backoff_max: Duration::from_secs(5 * 60),
            stop_grace: Duration::from_secs(30),
            shutdown_grace: Duration::from_secs(5),
        }
    }
}

struct Registered {
    decl: Option<ChannelDecl>,
    hook: Arc<dyn ConnectionHook>,
}

/// Builds a [`ConnectionRuntime`].
pub struct RuntimeBuilder {
    channels: Channels,
    hooks: HashMap<String, Registered>,
    timing: Timing,
}

impl RuntimeBuilder {
    /// The hook of `adapter`, with the adapter's declaration when it is a channel side (its mode
    /// and receive capability). The last one registered for an adapter wins; the Hub's assembly
    /// refuses a second one before.
    pub fn hook(
        mut self,
        adapter: &str,
        decl: Option<ChannelDecl>,
        hook: Arc<dyn ConnectionHook>,
    ) -> Self {
        self.hooks
            .insert(adapter.to_string(), Registered { decl, hook });
        self
    }

    pub fn timing(mut self, t: Timing) -> Self {
        self.timing = t;
        self
    }

    pub fn build(self) -> ConnectionRuntime {
        ConnectionRuntime {
            inner: Arc::new(Inner {
                channels: self.channels,
                hooks: self.hooks,
                timing: self.timing,
                sups: Mutex::default(),
                tasks: Mutex::default(),
                running: Mutex::default(),
                seen: Mutex::default(),
                shutdown: CancellationToken::new(),
            }),
        }
    }
}

/// The connection runtime: one supervisor per connection whose adapter registered a hook.
#[derive(Clone)]
pub struct ConnectionRuntime {
    inner: Arc<Inner>,
}

struct Inner {
    channels: Channels,
    hooks: HashMap<String, Registered>,
    timing: Timing,
    /// Connection id → the wanted state its supervisor follows.
    sups: Mutex<HashMap<String, watch::Sender<Option<Desired>>>>,
    /// Every supervisor (and the event loop), for shutdown.
    tasks: Mutex<Vec<JoinHandle<()>>>,
    /// Connection ids with a run going.
    running: Mutex<HashSet<String>>,
    /// Each connection's change signature when last seen (names are looked up again on a change).
    seen: Mutex<HashMap<String, String>>,
    shutdown: CancellationToken,
}

#[derive(Clone)]
struct Desired {
    fp: String,
    state: ConnState,
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

impl ConnectionRuntime {
    pub fn builder(channels: Channels) -> RuntimeBuilder {
        RuntimeBuilder {
            channels,
            hooks: HashMap::new(),
            timing: Timing::default(),
        }
    }

    /// The adapters with a hook, sorted.
    pub fn adapters(&self) -> Vec<String> {
        let mut v: Vec<String> = self.inner.hooks.keys().cloned().collect();
        v.sort();
        v
    }

    /// Connection ids with a run going, sorted.
    pub fn running(&self) -> Vec<String> {
        let mut v: Vec<String> = lock(&self.inner.running).iter().cloned().collect();
        v.sort();
        v
    }

    /// Starts every wanted run, then follows `events` (the Hub bus's `connection` events, tapped
    /// before this call) until they end or the runtime is shut down. Returns at once; the work is
    /// a task of its own.
    pub fn start(&self, mut events: mpsc::UnboundedReceiver<Value>) -> JoinHandle<()> {
        let me = self.clone();
        tokio::spawn(async move {
            me.start_all();
            // names of the people met: kicked runs and the periodic one
            drop(me.inner.channels.spawn_names());
            loop {
                let ev = tokio::select! {
                    biased;
                    _ = me.inner.shutdown.cancelled() => return,
                    ev = events.recv() => match ev {
                        Some(ev) => ev,
                        None => return,
                    },
                };
                if ev.get("type").and_then(Value::as_str) != Some("connection") {
                    continue;
                }
                let (Some(id), Some(ws)) = (
                    ev.get("id").and_then(Value::as_str),
                    ev.get("ws").and_then(Value::as_str),
                ) else {
                    continue;
                };
                me.on_event(ws, id);
            }
        })
    }

    /// Every connection of an adapter with a hook, brought in step.
    fn start_all(&self) {
        let ids = self
            .inner
            .channels
            .store()
            .reader(|c| -> ChannelResult<Vec<(String, String)>> {
                let mut st =
                    c.prepare("SELECT id, adapter FROM connections ORDER BY created, id")?;
                let rows = st
                    .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
                    .collect::<rusqlite::Result<Vec<_>>>()?;
                Ok(rows)
            });
        match ids {
            Ok(ids) => {
                for (id, adapter) in ids {
                    if self.inner.hooks.contains_key(&adapter) {
                        self.evaluate(&id);
                    }
                }
            }
            Err(e) => tracing::warn!("connection runtime: listing connections failed: {e}"),
        }
    }

    /// One `connection` event: the connection, the ones paired with it, and the names.
    fn on_event(&self, ws: &str, id: &str) {
        self.evaluate(id);
        for d in self.dependents(ws, id) {
            self.evaluate(&d);
        }
        // an identity verified, re-authorized, a permission granted: look the nameless up again.
        // Not on health changes (every failed call is one, and a lookup that fails would kick
        // the next) nor other connections' events.
        if self.really_changed(id)
            && let Ok(Some(s)) = self.state(id)
            && s.status == "ok"
            && self.inner.channels.profile_adapter(&s.adapter).is_some()
        {
            self.inner.channels.kick_names(&s.ws, true);
        }
    }

    /// Connections of `ws` paired with `id`.
    fn dependents(&self, ws: &str, id: &str) -> Vec<String> {
        let sql = format!(
            "SELECT id FROM connections WHERE workspace_id = ? AND json_extract(config, '$.{PAIR_KEY}') = ? ORDER BY created, id"
        );
        self.inner
            .channels
            .store()
            .reader(|c| -> ChannelResult<Vec<String>> {
                let mut st = c.prepare(&sql)?;
                let rows = st
                    .query_map(params![ws, id], |r| r.get(0))?
                    .collect::<rusqlite::Result<Vec<_>>>()?;
                Ok(rows)
            })
            .unwrap_or_else(|e| {
                tracing::warn!(conn = id, "connection runtime: paired connections: {e}");
                Vec::new()
            })
    }

    /// Has connection `id` changed since last seen beyond its health counters?
    fn really_changed(&self, id: &str) -> bool {
        let sig = self
            .inner
            .channels
            .store()
            .reader(|c| steve_modules::accounts::connections::repo::change_signature(c, id))
            .ok()
            .flatten();
        let mut seen = lock(&self.inner.seen);
        match sig {
            Some(s) => seen.insert(id.to_string(), s.clone()).as_ref() != Some(&s),
            None => seen.remove(id).is_some(),
        }
    }

    /// The connection as its hook sees it; `None` when it is gone.
    pub fn state(&self, id: &str) -> ChannelResult<Option<ConnState>> {
        let inner = &self.inner;
        let row = inner.channels.store().reader(|c| -> ChannelResult<_> {
            Ok(c.query_row(
                "SELECT workspace_id, adapter, status, caps, credential_item, config FROM connections WHERE id = ?",
                params![id],
                |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, String>(1)?,
                        r.get::<_, String>(2)?,
                        r.get::<_, String>(3)?,
                        r.get::<_, Option<String>>(4)?,
                        r.get::<_, String>(5)?,
                    ))
                },
            )
            .optional()?)
        })?;
        let Some((ws, adapter, status, caps, credential, config)) = row else {
            return Ok(None);
        };
        let caps: Vec<String> = serde_json::from_str(&caps).unwrap_or_default();
        let config: Value = serde_json::from_str(&config).unwrap_or(Value::Null);
        let pairing = inner
            .channels
            .connections()
            .provider(&adapter)
            .and_then(|p| p.pairing());
        let paired = inner
            .channels
            .connections()
            .paired(&ws, &adapter, &config)
            .map_err(|e| ChannelError::Internal(e.to_string()))?;
        let receive_cap = inner
            .hooks
            .get(&adapter)
            .and_then(|r| r.decl.as_ref())
            .and_then(|d| d.receive_cap.clone());
        let live = matches!(status.as_str(), "ok" | "error")
            && receive_cap.is_none_or(|cap| caps.contains(&cap))
            && pairing.is_none_or(|p| !p.required || paired.as_ref().is_some_and(|x| x.ok));
        Ok(Some(ConnState {
            ws,
            id: id.to_string(),
            adapter,
            status,
            caps,
            credential,
            config,
            paired,
            live,
        }))
    }

    /// Hands connection `id`'s wanted state to its supervisor (started when first wanted).
    fn evaluate(&self, id: &str) {
        if self.inner.shutdown.is_cancelled() {
            return;
        }
        let state = match self.state(id) {
            Ok(s) => s,
            Err(e) => {
                tracing::warn!(
                    conn = id,
                    "connection runtime: reading the connection failed: {e}"
                );
                return;
            }
        };
        let Some(state) = state else {
            // gone: its supervisor stops the run and ends
            lock(&self.inner.sups).remove(id);
            return;
        };
        let Some(reg) = self.inner.hooks.get(&state.adapter) else {
            return;
        };
        let desired = reg.hook.wants(&state).then(|| Desired {
            fp: reg.hook.fingerprint(&state),
            state,
        });
        let mut sups = lock(&self.inner.sups);
        match sups.get(id) {
            Some(tx) => {
                tx.send_replace(desired);
            }
            None if desired.is_some() => {
                let (tx, rx) = watch::channel(desired);
                sups.insert(id.to_string(), tx);
                let task = tokio::spawn(supervise(self.inner.clone(), id.to_string(), rx));
                let mut tasks = lock(&self.inner.tasks);
                tasks.retain(|t| !t.is_finished());
                tasks.push(task);
            }
            None => {}
        }
    }

    /// Stops every run (each gets [`Timing::shutdown_grace`], then is aborted) and the event loop.
    pub async fn shutdown(&self) {
        self.inner.shutdown.cancel();
        lock(&self.inner.sups).clear();
        let tasks: Vec<JoinHandle<()>> = std::mem::take(&mut *lock(&self.inner.tasks));
        for t in tasks {
            let _ = t.await;
        }
    }
}

/// One run going.
struct Current {
    fp: String,
    stop: CancellationToken,
    task: JoinHandle<Result<(), AdapterError>>,
    cx: ConnCx,
    /// The credential item the run started with.
    credential: Option<String>,
    started: Instant,
}

/// What one supervisor does for connection `id`: follow the wanted state in `rx`.
async fn supervise(inner: Arc<Inner>, id: String, mut rx: watch::Receiver<Option<Desired>>) {
    let t = inner.timing;
    let mut current: Option<Current> = None;
    // finished with `Ok` under this fingerprint: not started again for it
    let mut done: Option<String> = None;
    // failed under this fingerprint: started again at that time
    let mut retry: Option<(String, Instant)> = None;
    let mut backoff = t.backoff_min;
    loop {
        let desired = rx.borrow_and_update().clone();
        match desired {
            None => {
                if let Some(c) = current.take() {
                    stop_run(&inner, &id, c, t.stop_grace).await;
                }
                done = None;
                retry = None;
                backoff = t.backoff_min;
            }
            Some(d) => {
                if current.as_ref().is_some_and(|c| c.fp != d.fp) {
                    let c = current.take().expect("checked");
                    stop_run(&inner, &id, c, t.stop_grace).await;
                }
                if done.as_ref().is_some_and(|f| *f != d.fp) {
                    done = None;
                }
                if retry.as_ref().is_some_and(|(f, _)| *f != d.fp) {
                    retry = None;
                    backoff = t.backoff_min;
                }
                let due = retry.as_ref().is_none_or(|(_, at)| Instant::now() >= *at);
                if current.is_none() && done.is_none() && due && !inner.shutdown.is_cancelled() {
                    retry = None;
                    match start_run(&inner, &id, d.fp.clone(), d.state) {
                        Some(c) => current = Some(c),
                        None => {
                            retry = Some((d.fp, Instant::now() + backoff));
                            backoff = (backoff * 2).min(t.backoff_max);
                        }
                    }
                }
            }
        }
        let retry_at = retry.as_ref().map(|(_, at)| *at);
        tokio::select! {
            biased;
            _ = inner.shutdown.cancelled() => {
                if let Some(c) = current.take() {
                    stop_run(&inner, &id, c, t.shutdown_grace).await;
                }
                return;
            }
            changed = rx.changed() => {
                if changed.is_err() {
                    // the connection is gone (or the runtime shut down)
                    if let Some(c) = current.take() {
                        stop_run(&inner, &id, c, t.stop_grace).await;
                    }
                    return;
                }
            }
            res = async { (&mut current.as_mut().expect("guarded").task).await }, if current.is_some() => {
                let c = current.take().expect("guarded");
                lock(&inner.running).remove(&id);
                // a run that lasted past the longest wait starts its backoff over
                if c.started.elapsed() >= t.backoff_max {
                    backoff = t.backoff_min;
                }
                let failed = match res {
                    Ok(Ok(())) => None,
                    Ok(Err(e)) => Some(match &e {
                        AdapterError::Provider(p) => Health::from(p),
                        e => Health::Failed(e.to_string()),
                    }),
                    Err(e) if e.is_panic() => Some(Health::Failed(steve_i18n::stored(|| {
                        steve_i18n::t!("channels.error.run_panicked")
                    }))),
                    // aborted by us: nothing to say
                    Err(_) => None,
                };
                match failed {
                    None => done = Some(c.fp),
                    Some(_) if superseded(&inner, &id, &c) => {
                        // the connection changed under the run (a credential replaced and the old
                        // one destroyed) before its event got here: not the connection's failure.
                        // Follow what it is now.
                        tracing::info!(conn = id, "connection runtime: run of a changed connection ended");
                        ConnectionRuntime { inner: inner.clone() }.evaluate(&id);
                    }
                    Some(h) => {
                        tracing::warn!(conn = id, "connection runtime: run failed: {h:?}");
                        c.cx.report(h).await;
                        retry = Some((c.fp, Instant::now() + backoff));
                        backoff = (backoff * 2).min(t.backoff_max);
                    }
                }
            }
            _ = sleep_until(retry_at), if retry_at.is_some() => {}
        }
    }
}

/// Has connection `id` moved past run `c` (gone, not wanted, another fingerprint or credential)?
/// Read from the database, ahead of the `connection` event that will say so.
fn superseded(inner: &Arc<Inner>, id: &str, c: &Current) -> bool {
    let rt = ConnectionRuntime {
        inner: inner.clone(),
    };
    match rt.state(id) {
        Ok(Some(s)) => match inner.hooks.get(&s.adapter) {
            Some(reg) => {
                s.credential != c.credential
                    || !reg.hook.wants(&s)
                    || reg.hook.fingerprint(&s) != c.fp
            }
            None => true,
        },
        Ok(None) => true,
        // unreadable: report as usual
        Err(_) => false,
    }
}

async fn sleep_until(at: Option<Instant>) {
    match at {
        Some(at) => tokio::time::sleep_until(at).await,
        None => std::future::pending().await,
    }
}

/// Starts one run of connection `id`; `None` when its context could not be made (gone meanwhile).
fn start_run(inner: &Arc<Inner>, id: &str, fp: String, state: ConnState) -> Option<Current> {
    let reg = inner.hooks.get(&state.adapter)?;
    let cx = match inner.channels.connections().cx(&state.ws, id) {
        Ok(cx) => cx,
        Err(e) => {
            tracing::warn!(conn = id, "connection runtime: no context: {e}");
            return None;
        }
    };
    if let Some(decl) = &reg.decl {
        let now = inner.channels.now();
        if let Err(e) = inner
            .channels
            .store()
            .transaction(|tx| spaces::ensure_channel_settings(tx, &state.ws, id, decl.mode, now))
        {
            tracing::warn!(
                conn = id,
                "connection runtime: channel settings not created: {e}"
            );
        }
    }
    let stop = CancellationToken::new();
    let credential = state.credential.clone();
    let run = ConnRun {
        cx: cx.clone(),
        inbound: ConnSink::new(inner.channels.clone(), &state.ws, id, stop.clone()),
        queries: ConnQueries::bound(inner.channels.clone(), &state.ws, id),
        state,
        stop: stop.clone(),
    };
    let hook = reg.hook.clone();
    let task = tokio::spawn(async move { hook.run(run).await });
    lock(&inner.running).insert(id.to_string());
    tracing::info!(conn = id, "connection runtime: started");
    Some(Current {
        fp,
        stop,
        task,
        cx,
        credential,
        started: Instant::now(),
    })
}

/// Cancels a run and waits for it, aborting it after `grace`, or after [`Timing::shutdown_grace`]
/// once the runtime shuts down meanwhile. Its result is dropped.
async fn stop_run(inner: &Inner, id: &str, mut c: Current, grace: Duration) {
    c.stop.cancel();
    let mut deadline = Instant::now() + grace;
    let mut capped = false;
    let stopped = loop {
        tokio::select! {
            r = tokio::time::timeout_at(deadline, &mut c.task) => break r.is_ok(),
            _ = inner.shutdown.cancelled(), if !capped => {
                capped = true;
                deadline = deadline.min(Instant::now() + inner.timing.shutdown_grace);
            }
        }
    };
    if !stopped {
        tracing::warn!(
            conn = id,
            "connection runtime: run did not stop in time, aborted"
        );
        c.task.abort();
        let _ = c.task.await;
    }
    lock(&inner.running).remove(id);
    tracing::info!(conn = id, "connection runtime: stopped");
}
