//! Org-chart sync from an office IM directory (job
//! `people.directory_sync`): every connection with `directory.read` hands a [`Snapshot`] through its
//! [`DirectoryAdapter`]; [`apply::apply`] turns departments into org-chart teams of the own organization
//! and people into contacts with accounts, members, leads, jobs and `reports_to` links.
//!
//! - [`adapter`]: the [`DirectoryAdapter`] trait, the snapshot shapes and [`FakeDirectory`];
//! - [`DirectoryConnections`]: which connections can read a directory and how to open their adapter;
//!   [`TableConnections`] reads the `connections` table and builds adapters from per-adapter factories;
//! - [`DirectorySync`]: the job queue behind `POST /teams/sync` (it is the [`TeamSync`] port of the team
//!   routes), the daily 06:00 run, and the debounced run after directory change events.
//!
//! Integration (integrator):
//! 1. `let conns = TableConnections::new().with("feishu", "飞书", factory)` (C7 provides the factory);
//! 2. `let sync = DirectorySync::new(modules.clone(), Arc::new(conns))`, then
//!    `ports.sync = Some(sync.clone())` before building the contacts router;
//! 3. `sync.spawn()` at startup (part of `jobs::spawn_all`): one worker and a one-second timer;
//! 4. Feishu contact events (`contact.department.*`, `contact.user.*`) → `sync.changed(ws, connection_id)`.

pub mod adapter;
pub mod apply;
pub mod repo;

use std::collections::{BTreeMap, HashMap, VecDeque};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use chrono::{Local, NaiveTime, TimeZone};
use rusqlite::Connection;
use serde::Serialize;
use steve_store::{Tx, uid};
use tokio::sync::mpsc;

use super::TeamSync;
use crate::{ApiError, ApiResult, Modules};

pub use adapter::{
    Department, DirScope, DirUser, DirectoryAdapter, DirectoryError, FakeDirectory, Snapshot,
};
pub use apply::{EMPTY_SNAPSHOT, SyncReport};

/// Hour of the daily run, local time.
pub const DAILY_HOUR: u32 = 6;
/// Directory change events are coalesced for this long before one sync.
pub const DEBOUNCE_MS: i64 = 60_000;
/// Finished jobs kept for [`DirectorySync::job`].
const KEEP_JOBS: usize = 100;
/// Connections that never synced (just connected, or every read failed — usually a permission
/// not granted yet) are tried again this often, so granting it is enough.
pub const CATCH_UP_MS: i64 = 15 * 60_000;
/// The first catch-up after start: a little after boot, so a Hub that just started is not busy
/// with it at once.
pub const CATCH_UP_FIRST_MS: i64 = 5 * 60_000;
/// `connections.config` key of the last sync's outcome ([`DirectoryState`]).
pub const STATE_KEY: &str = "directory";

/// The scopes a successful full read proves granted (Feishu's names; other providers have none of
/// them, so marking them is a no-op there).
const DIRECTORY_SCOPES: [&str; 3] = [
    "contact:contact.base:readonly",
    "contact:department.base:readonly",
    "contact:user.base:readonly",
];

/// A connection that can read a directory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DirConnection {
    pub id: String,
    pub workspace_id: String,
    /// `connections.adapter` (`feishu`).
    pub adapter: String,
    /// “飞书通讯录”: `GET /teams`'s `sync.label` and the accounts' `source_label`.
    pub label: String,
    /// The user the sync acts for (the connection's creator): new contacts are created by them.
    pub owner: String,
}

/// Which connections read a directory, and their adapters. Built by the integrator.
pub trait DirectoryConnections: Send + Sync {
    /// Directory connections of `ws`, or of every workspace with `None`.
    fn list(&self, c: &Connection, ws: Option<&str>) -> ApiResult<Vec<DirConnection>>;

    /// One directory connection of `ws`; `Ok(None)` when it exists but cannot read a directory, and
    /// `404 没有这个连接` when `ws` has no such connection.
    fn get(&self, c: &Connection, ws: &str, id: &str) -> ApiResult<Option<DirConnection>>;

    /// The adapter reading this connection's directory.
    fn adapter(&self, conn: &DirConnection) -> ApiResult<Arc<dyn DirectoryAdapter>>;

    /// Called in the sync's transaction after a successful sync.
    fn synced(&self, tx: &Tx, conn: &DirConnection, at: i64) -> ApiResult<()> {
        let _ = (tx, conn, at);
        Ok(())
    }
}

/// Builds the adapter of one connection (it reads its credentials through the vault when it runs).
pub type AdapterFactory =
    Arc<dyn Fn(&DirConnection) -> ApiResult<Arc<dyn DirectoryAdapter>> + Send + Sync>;

/// [`DirectoryConnections`] over the `connections` table: healthy connections with `directory.read`
/// among their enabled capabilities, whose adapter has a registered factory.
#[derive(Default, Clone)]
pub struct TableConnections {
    /// adapter → (display name, factory)
    adapters: BTreeMap<String, (String, AdapterFactory)>,
}

impl TableConnections {
    pub fn new() -> Self {
        Self::default()
    }

    /// Registers the directory of connections with `connections.adapter = adapter`; `name` (“飞书”)
    /// makes the label “飞书通讯录”.
    pub fn with(mut self, adapter: &str, name: &str, factory: AdapterFactory) -> Self {
        self.adapters.insert(adapter.into(), (name.into(), factory));
        self
    }

    fn conn(&self, row: repo::ConnectionRow) -> Option<DirConnection> {
        let (name, _) = self.adapters.get(&row.adapter)?;
        Some(DirConnection {
            label: format!("{name}通讯录"),
            id: row.id,
            workspace_id: row.workspace_id,
            adapter: row.adapter,
            owner: row.created_by,
        })
    }
}

impl DirectoryConnections for TableConnections {
    fn list(&self, c: &Connection, ws: Option<&str>) -> ApiResult<Vec<DirConnection>> {
        Ok(repo::directory_connections(c, ws)?
            .into_iter()
            .filter_map(|r| self.conn(r))
            .collect())
    }

    fn get(&self, c: &Connection, ws: &str, id: &str) -> ApiResult<Option<DirConnection>> {
        match repo::directory_connection(c, ws, id)? {
            Some(r) => Ok(self.conn(r)),
            None if repo::connection_exists(c, ws, id)? => Ok(None),
            None => Err(ApiError::not_found("连接")),
        }
    }

    fn adapter(&self, conn: &DirConnection) -> ApiResult<Arc<dyn DirectoryAdapter>> {
        match self.adapters.get(&conn.adapter) {
            Some((_, f)) => f(conn),
            None => Err(ApiError::Internal(format!(
                "no directory adapter for {}",
                conn.adapter
            ))),
        }
    }

    fn synced(&self, tx: &Tx, conn: &DirConnection, at: i64) -> ApiResult<()> {
        repo::set_last_sync(tx, &conn.id, at)?;
        Ok(())
    }
}

/// `queued` → `running` → `done` | `failed`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum JobState {
    Queued,
    Running,
    Done,
    Failed,
}

/// One queued sync of one or more connections of a workspace.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Job {
    pub id: String,
    pub ws: String,
    /// `manual` (`POST /teams/sync`), `daily`, `event`.
    pub reason: String,
    pub connections: Vec<String>,
    pub state: JobState,
    /// Per connection, in order.
    pub reports: Vec<(String, SyncReport)>,
    /// Per failed connection: `(connection id, message)`.
    pub errors: Vec<(String, String)>,
    pub queued: i64,
    pub finished: Option<i64>,
}

#[derive(Default)]
struct State {
    jobs: VecDeque<Job>,
    /// The connections of each job, by job id (dropped when it finishes).
    targets: HashMap<String, Vec<DirConnection>>,
    /// `(ws, connection)` → when its debounced sync is due.
    pending: BTreeMap<(String, String), i64>,
    next_daily: i64,
    /// When the next catch-up of never-synced connections is due.
    next_catch_up: i64,
    /// Each connection's [`crate::accounts::connections::repo::change_signature`] when last seen.
    seen: HashMap<String, String>,
}

/// The outcome of a connection's last sync, kept in `connections.config.directory` for the 组织架构
/// header: when, what the app may see, and why it failed.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, serde::Deserialize)]
pub struct DirectoryState {
    pub at: i64,
    pub ok: bool,
    #[serde(default)]
    pub error: Option<String>,
    #[serde(default)]
    pub scope: Option<DirScope>,
    /// Departments read (the visible tree).
    #[serde(default)]
    pub departments: usize,
    #[serde(default)]
    pub users: usize,
    /// Provider permissions found missing.
    #[serde(default)]
    pub missing_scopes: Vec<String>,
}

/// The `people.directory_sync` job: a queue with one worker, so syncs never overlap.
pub struct DirectorySync {
    m: Modules,
    conns: Arc<dyn DirectoryConnections>,
    state: Mutex<State>,
    tx: mpsc::UnboundedSender<String>,
    rx: Mutex<Option<mpsc::UnboundedReceiver<String>>>,
}

/// The first `hour:00` local time strictly after `now_ms` (a DST gap moves it an hour later).
pub fn next_daily(now_ms: i64, hour: u32) -> i64 {
    let Some(now) = Local.timestamp_millis_opt(now_ms).single() else {
        return now_ms + 86_400_000;
    };
    let at = NaiveTime::from_hms_opt(hour, 0, 0).unwrap_or_default();
    let mut day = now.date_naive();
    for _ in 0..3 {
        let t = day.and_time(at);
        let local = Local.from_local_datetime(&t).earliest().or_else(|| {
            Local
                .from_local_datetime(&(t + chrono::Duration::hours(1)))
                .earliest()
        });
        if let Some(l) = local
            && l.timestamp_millis() > now_ms
        {
            return l.timestamp_millis();
        }
        day = day.succ_opt().unwrap_or(day);
    }
    now_ms + 86_400_000
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

impl DirectorySync {
    pub fn new(m: Modules, conns: Arc<dyn DirectoryConnections>) -> Arc<Self> {
        let (tx, rx) = mpsc::unbounded_channel();
        let next = next_daily(m.clock.now(), DAILY_HOUR);
        let catch_up = m.clock.now() + CATCH_UP_FIRST_MS;
        Arc::new(Self {
            m,
            conns,
            state: Mutex::new(State {
                next_daily: next,
                next_catch_up: catch_up,
                ..Default::default()
            }),
            tx,
            rx: Mutex::new(Some(rx)),
        })
    }

    /// Starts the worker and the timer (once; later calls return no handles).
    pub fn spawn(self: &Arc<Self>) -> Vec<tokio::task::JoinHandle<()>> {
        let Some(mut rx) = lock(&self.rx).take() else {
            return Vec::new();
        };
        let worker = {
            let me = self.clone();
            tokio::spawn(async move {
                while let Some(id) = rx.recv().await {
                    me.run_job(&id).await;
                }
            })
        };
        let timer = {
            let me = self.clone();
            tokio::spawn(async move {
                loop {
                    tokio::time::sleep(Duration::from_secs(1)).await;
                    if let Err(e) = me.tick(me.m.clock.now()) {
                        tracing::warn!(error = %e, "directory sync timer");
                    }
                }
            })
        };
        vec![worker, timer]
    }

    /// A job's state (the last [`KEEP_JOBS`] jobs).
    pub fn job(&self, id: &str) -> Option<Job> {
        lock(&self.state).jobs.iter().find(|j| j.id == id).cloned()
    }

    /// Queues a sync of these connections; a job still queued for the same connections is reused.
    pub fn enqueue(&self, ws: &str, reason: &str, conns: Vec<DirConnection>) -> String {
        let ids: Vec<String> = conns.iter().map(|c| c.id.clone()).collect();
        let mut st = lock(&self.state);
        if let Some(j) = st
            .jobs
            .iter()
            .find(|j| j.state == JobState::Queued && j.ws == ws && j.connections == ids)
        {
            return j.id.clone();
        }
        let id = uid("job");
        st.jobs.push_back(Job {
            id: id.clone(),
            ws: ws.into(),
            reason: reason.into(),
            connections: ids,
            state: JobState::Queued,
            reports: Vec::new(),
            errors: Vec::new(),
            queued: self.m.clock.now(),
            finished: None,
        });
        st.targets.insert(id.clone(), conns);
        while st.jobs.len() > KEEP_JOBS {
            match st.jobs.iter().position(|j| j.finished.is_some()) {
                Some(i) => {
                    st.jobs.remove(i);
                }
                None => break,
            }
        }
        drop(st);
        if self.tx.send(id.clone()).is_err() {
            tracing::warn!(job = %id, "directory sync worker is gone");
        }
        id
    }

    /// Is a sync of this connection queued or running?
    fn queued_for(&self, connection_id: &str) -> bool {
        let st = lock(&self.state);
        st.jobs.iter().any(|j| {
            matches!(j.state, JobState::Queued | JobState::Running)
                && j.connections.iter().any(|c| c == connection_id)
        })
    }

    /// A connection was connected, re-verified or changed its permissions: one sync soon (after
    /// the same quiet period as directory events), when it can read a directory.
    pub fn connection_changed(&self, ws: &str, connection_id: &str) {
        let (ok, sig) = self
            .m
            .store
            .reader(|c| -> ApiResult<_> {
                Ok((
                    self.conns.get(c, ws, connection_id)?.is_some(),
                    crate::accounts::connections::repo::change_signature(c, connection_id)?,
                ))
            })
            .unwrap_or((false, None));
        // health counters change on every failed call: only a real change counts
        let changed = {
            let mut st = lock(&self.state);
            match sig {
                Some(s) => {
                    st.seen
                        .insert(connection_id.to_string(), s.clone())
                        .as_ref()
                        != Some(&s)
                }
                None => st.seen.remove(connection_id).is_some(),
            }
        };
        if ok && changed {
            let due = self.m.clock.now() + 5_000;
            lock(&self.state)
                .pending
                .insert((ws.into(), connection_id.into()), due);
        }
    }

    /// Follows `{type:"connection", id, ws}` events (steve-hub's `Bus::tap(&["connection"])`) with
    /// [`DirectorySync::connection_changed`] until the sender goes away.
    pub fn spawn_watch(
        self: &Arc<Self>,
        mut rx: mpsc::UnboundedReceiver<serde_json::Value>,
    ) -> tokio::task::JoinHandle<()> {
        let me = self.clone();
        tokio::spawn(async move {
            while let Some(ev) = rx.recv().await {
                if ev.get("type").and_then(serde_json::Value::as_str) != Some("connection") {
                    continue;
                }
                if let (Some(id), Some(ws)) = (
                    ev.get("id").and_then(serde_json::Value::as_str),
                    ev.get("ws").and_then(serde_json::Value::as_str),
                ) {
                    me.connection_changed(ws, id);
                }
            }
        })
    }

    /// A directory reported a change: one sync of that connection after [`DEBOUNCE_MS`] of quiet.
    pub fn changed(&self, ws: &str, connection_id: &str) {
        let due = self.m.clock.now() + DEBOUNCE_MS;
        lock(&self.state)
            .pending
            .insert((ws.into(), connection_id.into()), due);
    }

    /// Queues what is due at `now`: the daily run of every directory connection, and debounced change
    /// syncs. Returns the queued job ids.
    pub fn tick(&self, now: i64) -> ApiResult<Vec<String>> {
        let (daily, catch_up, due) = {
            let mut st = lock(&self.state);
            let daily = now >= st.next_daily;
            if daily {
                st.next_daily = next_daily(now, DAILY_HOUR);
            }
            let catch_up = !daily && now >= st.next_catch_up;
            if catch_up || daily {
                st.next_catch_up = now + CATCH_UP_MS;
            }
            let due: Vec<(String, String)> = st
                .pending
                .iter()
                .filter(|(_, at)| **at <= now)
                .map(|(k, _)| k.clone())
                .collect();
            for k in &due {
                st.pending.remove(k);
            }
            (daily, catch_up, due)
        };
        if !daily && !catch_up && due.is_empty() {
            return Ok(Vec::new());
        }
        let mut out = Vec::new();
        let (all, events) = self.m.store.reader(|c| {
            let all = if daily {
                self.conns.list(c, None)?
            } else if catch_up {
                let mut v = Vec::new();
                for x in self.conns.list(c, None)? {
                    if repo::never_synced(c, &x.id)? && !self.queued_for(&x.id) {
                        v.push(x);
                    }
                }
                v
            } else {
                Vec::new()
            };
            let mut events = Vec::new();
            for (ws, id) in &due {
                match self.conns.get(c, ws, id) {
                    Ok(Some(conn)) => events.push(conn),
                    Ok(None) | Err(ApiError::NotFound(_)) => {}
                    Err(e) => return Err(e),
                }
            }
            Ok::<_, ApiError>((all, events))
        })?;
        let mut by_ws: BTreeMap<String, Vec<DirConnection>> = BTreeMap::new();
        for c in all {
            by_ws.entry(c.workspace_id.clone()).or_default().push(c);
        }
        for (ws, conns) in by_ws {
            out.push(self.enqueue(&ws, if daily { "daily" } else { "catch_up" }, conns));
        }
        for c in events {
            let ws = c.workspace_id.clone();
            out.push(self.enqueue(&ws, "event", vec![c]));
        }
        Ok(out)
    }

    /// Runs a queued job now (the worker calls this; tests may too).
    pub async fn run_job(&self, id: &str) {
        let conns = {
            let mut st = lock(&self.state);
            let Some(conns) = st.targets.remove(id) else {
                return;
            };
            if let Some(j) = st.jobs.iter_mut().find(|j| j.id == id) {
                j.state = JobState::Running;
            }
            conns
        };
        let mut reports = Vec::new();
        let mut errors = Vec::new();
        for c in &conns {
            match self.sync_connection(c).await {
                Ok(r) => reports.push((c.id.clone(), r)),
                Err(e) => {
                    tracing::warn!(connection = %c.id, error = %e, "directory sync failed");
                    errors.push((c.id.clone(), e.to_string()));
                }
            }
        }
        let mut st = lock(&self.state);
        if let Some(j) = st.jobs.iter_mut().find(|j| j.id == id) {
            j.state = if errors.is_empty() {
                JobState::Done
            } else {
                JobState::Failed
            };
            j.reports = reports;
            j.errors = errors;
            j.finished = Some(self.m.clock.now());
        }
    }

    /// Reads one connection's directory and applies it in one transaction. A failed read changes
    /// nothing.
    pub async fn sync_connection(&self, conn: &DirConnection) -> ApiResult<SyncReport> {
        let adapter = self.conns.adapter(conn)?;
        let read = adapter.snapshot().await;
        let missing = adapter.missing_scopes();
        let mut state = DirectoryState {
            at: self.m.clock.now(),
            scope: adapter.scope(),
            missing_scopes: missing.iter().map(|(s, _)| s.clone()).collect(),
            ..DirectoryState::default()
        };
        let snapshot = match read {
            Ok(s) => s,
            Err(e) => {
                state.error = Some(e.to_string());
                self.note(conn, &state, &missing);
                return Err(ApiError::rule(e.to_string()));
            }
        };
        state.departments = snapshot.departments.len();
        state.users = snapshot.users.len();
        let system = adapter.system().to_string();
        let c2 = conn.clone();
        let conns = self.conns.clone();
        let r = self
            .m
            .blocking(move |m| {
                let now = m.clock.now();
                m.store.transaction(|tx| {
                    let r = apply::apply(tx, &c2, &system, &snapshot, now)?;
                    conns.synced(tx, &c2, now)?;
                    Ok::<_, ApiError>(r)
                })
            })
            .await;
        match &r {
            Ok(_) => state.ok = true,
            Err(e) => state.error = Some(e.to_string()),
        }
        self.note(conn, &state, &missing);
        r
    }

    /// Keeps the outcome on the connection and the missing permissions on its capability rows.
    fn note(&self, conn: &DirConnection, state: &DirectoryState, missing: &[(String, String)]) {
        let id = conn.id.clone();
        let value = serde_json::to_value(state).unwrap_or_default();
        let missing: Vec<(&str, &str)> = missing
            .iter()
            .map(|(s, p)| (s.as_str(), p.as_str()))
            .collect();
        let granted: Vec<&str> = if state.ok {
            DIRECTORY_SCOPES.to_vec()
        } else {
            Vec::new()
        };
        let now = self.m.clock.now();
        let r = self.m.store.transaction(|tx| -> ApiResult<()> {
            crate::accounts::connections::repo::set_config_key(tx, &id, STATE_KEY, &value)?;
            crate::accounts::connections::repo::note_permissions(tx, &id, &missing, &granted, now)?;
            Ok(())
        });
        if let Err(e) = r {
            tracing::warn!(connection = %conn.id, error = %e, "directory sync: state not kept");
        }
    }
}

impl TeamSync for DirectorySync {
    fn label(&self, c: &Connection, ws: &str, connection_id: &str) -> ApiResult<Option<String>> {
        match self.conns.get(c, ws, connection_id) {
            Ok(x) => Ok(x.map(|c| c.label)),
            Err(ApiError::NotFound(_)) => Ok(None),
            Err(e) => Err(e),
        }
    }

    fn schedule(
        &self,
        c: &Connection,
        ws: &str,
        _actor: &str,
        connection_id: Option<&str>,
    ) -> ApiResult<Option<String>> {
        let conns = match connection_id {
            Some(id) => self.conns.get(c, ws, id)?.into_iter().collect(),
            None => self.conns.list(c, Some(ws))?,
        };
        if conns.is_empty() {
            return Ok(None);
        }
        Ok(Some(self.enqueue(ws, "manual", conns)))
    }
}
