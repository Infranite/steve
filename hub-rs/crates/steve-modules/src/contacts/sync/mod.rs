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
//! 3. Register `sync.run()` as the owned `contacts.sync` job: one worker and a deadline source;
//! 4. Feishu contact events (`contact.department.*`, `contact.user.*`) → `sync.changed(ws, connection_id)`.

pub mod adapter;
pub mod apply;
pub mod repo;

use std::collections::{BTreeMap, HashMap, VecDeque};
use std::sync::{Arc, Mutex};

use chrono::NaiveTime;
use rusqlite::Connection;
use serde::Serialize;
use steve_runtime::tz::Zone;
use steve_store::{Tx, uid};
use tokio::sync::mpsc;

use super::TeamSync;
use crate::{ApiError, ApiResult, Modules};

pub use adapter::{
    Department, DirScope, DirUser, DirectoryAdapter, DirectoryError, FakeDirectory, Snapshot,
};
pub use apply::{EMPTY_SNAPSHOT, SyncReport};

/// Hour of the daily run on each workspace's clock (`settings.tz`, else system).
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
    daily_since: i64,
    /// When the next catch-up of never-synced connections is due.
    next_catch_up: i64,
    /// Last signature handled by explicit `connection_changed` calls.
    seen: HashMap<String, String>,
    /// Last authoritative signature observed by the due source, independently of explicit calls.
    observed: HashMap<String, String>,
    /// The first due query records existing facts without scheduling every old connection.
    observed_ready: bool,
    /// Pending entries inferred from connection facts may extend their own 5s quiet window.
    /// Explicit directory events and row lookup retry deadlines keep their existing deadline.
    inferred_pending: std::collections::BTreeSet<(String, String)>,
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
    wake: Arc<tokio::sync::Notify>,
    tx: mpsc::UnboundedSender<String>,
    rx: Mutex<Option<mpsc::UnboundedReceiver<String>>>,
}

/// The first `hour:00` on the system clock strictly after `now_ms`.
pub fn next_daily(now_ms: i64, hour: u32) -> i64 {
    next_daily_in(now_ms, hour, Zone::system())
}

/// The first `hour:00` on `zone`'s clock strictly after `now_ms`, using runtime DST rules.
pub fn next_daily_in(now_ms: i64, hour: u32, zone: Zone) -> i64 {
    let Some(mut day) = zone.local(now_ms).ok().map(|t| t.date()) else {
        return now_ms.saturating_add(86_400_000);
    };
    let at = NaiveTime::from_hms_opt(hour, 0, 0).unwrap_or_default();
    for _ in 0..3 {
        if let Ok(t) = zone.instant(day.and_time(at))
            && t > now_ms
        {
            return t;
        }
        day = day.succ_opt().unwrap_or(day);
    }
    now_ms.saturating_add(86_400_000)
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

impl DirectorySync {
    pub fn new(m: Modules, conns: Arc<dyn DirectoryConnections>) -> Arc<Self> {
        let (tx, rx) = mpsc::unbounded_channel();
        let daily_since = m.clock.now();
        let catch_up = m.clock.now() + CATCH_UP_FIRST_MS;
        Arc::new(Self {
            m,
            conns,
            state: Mutex::new(State {
                daily_since,
                next_catch_up: catch_up,
                ..Default::default()
            }),
            wake: Arc::new(tokio::sync::Notify::new()),
            tx,
            rx: Mutex::new(Some(rx)),
        })
    }

    /// Runs the worker and timer once. Dropping this future drops both loops.
    pub async fn run(self: Arc<Self>) {
        let Some(mut rx) = lock(&self.rx).take() else {
            return;
        };
        let worker = async {
            while let Some(id) = rx.recv().await {
                self.run_job(&id).await;
            }
        };
        let timer = steve_runtime::due::due_loop(
            Arc::new(DirectoryDue(self.clone())),
            self.m.clock.clone(),
            self.wake.clone(),
            steve_runtime::policy::Backoff {
                base: std::time::Duration::from_secs(1),
                max: std::time::Duration::from_secs(60),
                factor: 2,
                jitter: steve_runtime::policy::Jitter::None,
                attempts: None,
                reset_after: None,
            },
        );
        tokio::join!(worker, timer);
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

    /// Isolate unreadable per-connection metadata just as the sync's due-row lookup does.
    fn connection_fact(
        &self,
        c: &Connection,
        ws: &str,
        id: &str,
    ) -> ApiResult<(Option<String>, bool)> {
        let signature = crate::accounts::connections::repo::change_signature(c, id)?;
        let eligible = match self.conns.get(c, ws, id) {
            Ok(row) => row.is_some(),
            Err(ApiError::NotFound(_)) => false,
            Err(error) => return Err(error),
        };
        Ok((signature, eligible))
    }

    /// Recompute connection changes from authoritative rows, never from a commit hint's payload.
    fn observe_connections(&self) -> ApiResult<()> {
        let facts = self.m.store.reader(|c| -> ApiResult<_> {
            let mut st = c.prepare("SELECT workspace_id,id FROM connections ORDER BY id")?;
            let rows = st.query_map([], |r| Ok((r.get::<_, String>(0)?,r.get::<_, String>(1)?)))?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            let mut facts = Vec::with_capacity(rows.len());
            for (ws, id) in rows {
                match self.connection_fact(c, &ws, &id) {
                    Ok((signature, eligible)) => facts.push((ws, id, signature, Some(eligible))),
                    Err(error) => {
                        tracing::warn!(connection = %id, %error, "directory connection facts unavailable");
                        facts.push((ws, id, None, None));
                    }
                }
            }
            Ok(facts)
        })?;
        let mut st = lock(&self.state);
        let live: std::collections::BTreeSet<_> =
            facts.iter().map(|(_, id, _, _)| id.as_str()).collect();
        st.observed.retain(|id, _| live.contains(id.as_str()));
        st.seen.retain(|id, _| live.contains(id.as_str()));
        st.inferred_pending
            .retain(|(_, id)| live.contains(id.as_str()));
        let seed = !st.observed_ready;
        st.observed_ready = true;
        let now = self.m.clock.now();
        for (ws, id, signature, eligible) in facts {
            let Some(eligible) = eligible else { continue };
            let changed = match signature {
                Some(signature) => {
                    st.observed.insert(id.clone(), signature.clone()).as_ref() != Some(&signature)
                }
                None => st.observed.remove(&id).is_some(),
            };
            if !seed && eligible && changed {
                let key = (ws, id);
                if !st.pending.contains_key(&key) || st.inferred_pending.contains(&key) {
                    st.pending.insert(key.clone(), now.saturating_add(5_000));
                    st.inferred_pending.insert(key);
                }
            }
        }
        Ok(())
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
            let mut st = lock(&self.state);
            let key = (ws.into(), connection_id.into());
            st.inferred_pending.remove(&key);
            st.pending.insert(key, due);
            drop(st);
            self.wake.notify_one();
        }
    }

    /// Own the physical-table subscription for contacts.sync. Hints only request a due requery;
    /// the source itself checks connection signatures and workspace clocks from storage.
    /// Register while constructing the Job future, so commits before its first poll are buffered.
    pub fn watch(self: Arc<Self>) -> impl std::future::Future<Output = ()> + Send {
        let committed = self
            .m
            .store
            .commit_wake(&["connections", "workspaces", "settings"]);
        async move {
            loop {
                committed.notified().await;
                self.wake.notify_one();
            }
        }
    }

    /// A directory reported a change: one sync of that connection after [`DEBOUNCE_MS`] of quiet.
    pub fn changed(&self, ws: &str, connection_id: &str) {
        let due = self.m.clock.now() + DEBOUNCE_MS;
        let mut st = lock(&self.state);
        let key = (ws.into(), connection_id.into());
        st.inferred_pending.remove(&key);
        st.pending.insert(key, due);
        drop(st);
        self.wake.notify_one();
    }

    /// Queues what is due at `now`: the daily run of every directory connection, and debounced change
    /// syncs. Returns the queued job ids.
    pub fn tick(&self, now: i64) -> ApiResult<Vec<String>> {
        // Re-read workspace clocks so changing settings.tz takes effect on the next tick.
        let since = lock(&self.state).daily_since;
        let daily = self.m.store.reader(|c| -> ApiResult<_> {
            let mut stmt = c.prepare("SELECT id FROM workspaces")?;
            let workspaces = stmt.query_map([], |r| r.get::<_, String>(0))?;
            let mut daily = std::collections::BTreeSet::new();
            for ws in workspaces {
                let ws = ws?;
                let tz = crate::zone::workspace_tz(c, &ws)?;
                let zone = Zone::parse(tz.name()).expect("workspace IANA zone");
                if next_daily_in(since, DAILY_HOUR, zone) <= now {
                    daily.insert(ws);
                }
            }
            Ok(daily)
        })?;
        let (catch_up, due) = {
            let st = lock(&self.state);
            let mut due: Vec<_> = st
                .pending
                .iter()
                .filter(|(_, at)| **at <= now)
                .map(|(k, at)| (k.clone(), *at))
                .collect();
            due.sort_by(|(a, at), (b, bt)| (at, a).cmp(&(bt, b)));
            due.truncate(DIRECTORY_BATCH);
            (now >= st.next_catch_up, due)
        };
        if daily.is_empty() && !catch_up && due.is_empty() {
            return Ok(Vec::new());
        }
        let mut out = Vec::new();
        let (all, events, failed) = self.m.store.reader(|c| {
            let mut all = Vec::new();
            if !daily.is_empty() || catch_up {
                for x in self.conns.list(c, None)? {
                    if daily.contains(&x.workspace_id)
                        || (catch_up && repo::never_synced(c, &x.id)? && !self.queued_for(&x.id))
                    {
                        all.push(x);
                    }
                }
            }
            let mut events = Vec::new();
            let mut failed = std::collections::BTreeSet::new();
            for ((ws, id), _) in &due {
                match self.conns.get(c, ws, id) {
                    Ok(Some(conn)) => events.push(conn),
                    Ok(None) | Err(ApiError::NotFound(_)) => {}
                    Err(e) => {
                        // A single connection must not hold up other event syncs. Its own
                        // deadline advances; the source remains healthy.
                        tracing::warn!(connection = %id, error = %e, "directory sync timer");
                        failed.insert((ws.clone(), id.clone()));
                    }
                }
            }
            Ok::<_, ApiError>((all, events, failed))
        })?;
        // Commit the timer state only after successful source reads. Preserve events changed
        // concurrently with the read, and retain individual lookup failures with a later deadline.
        {
            let mut st = lock(&self.state);
            st.daily_since = now;
            if catch_up {
                st.next_catch_up = now.saturating_add(CATCH_UP_MS);
            }
            for (key, at) in due {
                if st.pending.get(&key) != Some(&at) {
                    continue;
                }
                st.inferred_pending.remove(&key);
                if failed.contains(&key) {
                    st.pending.insert(key, now.saturating_add(CATCH_UP_MS));
                } else {
                    st.pending.remove(&key);
                }
            }
        }
        let mut by_ws: BTreeMap<String, Vec<DirConnection>> = BTreeMap::new();
        for c in all {
            by_ws.entry(c.workspace_id.clone()).or_default().push(c);
        }
        for (ws, conns) in by_ws {
            out.push(self.enqueue(
                &ws,
                if daily.contains(&ws) {
                    "daily"
                } else {
                    "catch_up"
                },
                conns,
            ));
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

const DIRECTORY_BATCH: usize = 64;
struct DirectoryDue(Arc<DirectorySync>);
impl steve_runtime::due::DueSource for DirectoryDue {
    fn name(&self) -> &'static str {
        "people.directory_sync"
    }
    async fn next_due(&self) -> Result<Option<i64>, steve_runtime::due::DueError> {
        let sync = self.0.clone();
        self.0
            .m
            .blocking(move |m| {
                sync.observe_connections()?;
                let (since, mut next) = {
                    let st = lock(&sync.state);
                    (
                        st.daily_since,
                        st.pending
                            .values()
                            .copied()
                            .fold(st.next_catch_up, i64::min),
                    )
                };
                m.store.reader(|c| -> ApiResult<_> {
                    let mut stmt = c.prepare("SELECT id FROM workspaces")?;
                    for ws in stmt.query_map([], |r| r.get::<_, String>(0))? {
                        let tz = crate::zone::workspace_tz(c, &ws?)?;
                        next = next.min(next_daily_in(
                            since,
                            DAILY_HOUR,
                            Zone::parse(tz.name()).expect("workspace IANA zone"),
                        ));
                    }
                    Ok(Some(next))
                })
            })
            .await
            .map_err(|e| steve_runtime::due::DueError(e.to_string()))
    }
    async fn run_due(&self, now: i64) -> Result<(), steve_runtime::due::DueError> {
        let sync = self.0.clone();
        self.0
            .m
            .blocking(move |_| sync.tick(now))
            .await
            .map(|_| ())
            .map_err(|e| steve_runtime::due::DueError(e.to_string()))
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

#[cfg(test)]
mod due_contract_tests {
    use super::*;
    use crate::testkit::{ME, TestEnv, WS};
    use steve_runtime::due::{DueSource, contract};

    struct Connections(TableConnections);
    impl DirectoryConnections for Connections {
        fn list(&self, c: &Connection, ws: Option<&str>) -> ApiResult<Vec<DirConnection>> {
            self.0.list(c, ws)
        }
        fn get(&self, c: &Connection, ws: &str, id: &str) -> ApiResult<Option<DirConnection>> {
            // A real per-connection lookup failure, not a fake DueSource or terminal marker.
            if id.starts_with("poison") {
                return Err(ApiError::rule("directory metadata unavailable"));
            }
            self.0.get(c, ws, id)
        }
        fn adapter(&self, conn: &DirConnection) -> ApiResult<Arc<dyn DirectoryAdapter>> {
            self.0.adapter(conn)
        }
    }
    struct Fixture {
        env: TestEnv,
        source: DirectoryDue,
    }
    impl Fixture {
        fn new() -> Self {
            let env = TestEnv::new(crate::accounts::register);
            env.clock.set(1000);
            let table = TableConnections::new().with(
                "feishu",
                "Test",
                Arc::new(|_| Ok(Arc::new(FakeDirectory::new("feishu", Snapshot::default())))),
            );
            let sync = DirectorySync::new(env.modules.clone(), Arc::new(Connections(table)));
            Self {
                env,
                source: DirectoryDue(sync),
            }
        }
    }
    impl contract::Fixture for Fixture {
        type Source = DirectoryDue;
        fn source(&self) -> &Self::Source {
            &self.source
        }
        fn batch(&self) -> usize {
            DIRECTORY_BATCH
        }
        fn dead_letters(&self) -> bool {
            false
        }
        fn dead(&self) -> Vec<String> {
            vec![]
        }
        fn add(&self, at: i64, poison: bool) -> String {
            let id = format!("{}-{}", if poison { "poison" } else { "ok" }, uid("due"));
            self.env.store().transaction(|tx| -> ApiResult<_> {
                let party = crate::contacts::service::ensure_self_org(tx, WS, ME)?;
                let account = crate::accounts::repo::insert(tx, WS, &crate::accounts::repo::NewAccount {
                    system_id: "feishu_app".into(), family: "feishu_app".into(), party_id: party.id,
                    kind: "app_bot".into(), handle: id.clone(), handle_display: id.clone(),
                    display_name: id.clone(), verified: true, confidence: 1.0,
                    source: "test".into(), source_label: "test".into(),
                })?;
                tx.execute("INSERT INTO connections (id, workspace_id, account_id, adapter, auth, caps, created_by, created, updated, last_sync)
                    VALUES (?1, ?2, ?3, 'feishu', 'credential', ?5, ?4, 1, 1, 1)",
                    rusqlite::params![id, WS, account.id, ME, serde_json::json!(["directory.read"]).to_string()])?;
                Ok(())
            }).unwrap();
            lock(&self.source.0.state)
                .pending
                .insert((WS.into(), id.clone()), at);
            id
        }
        fn done(&self) -> Vec<String> {
            lock(&self.source.0.state)
                .jobs
                .iter()
                .flat_map(|j| j.connections.clone())
                .collect()
        }
    }
    #[tokio::test]
    async fn r7b_directory_timers_meet_the_continuing_contract() {
        contract::check(Fixture::new, 1000, 3).await;
    }
    #[tokio::test]
    async fn r7b_directory_uses_workspace_deadlines_and_event_wakes() {
        let f = Fixture::new();
        let sync = &f.source.0;
        f.env
            .store()
            .with_conn(|c| {
                c.execute(
                    "INSERT OR REPLACE INTO settings(workspace_id,key,value,updated) SELECT id,'tz',?1,1000 FROM workspaces",
                    [serde_json::json!("UTC").to_string()],
                )?;
                Ok(())
            })
            .unwrap();
        let daily = next_daily_in(1000, DAILY_HOUR, Zone::parse("UTC").unwrap());
        lock(&sync.state).next_catch_up = daily + 1;
        assert_eq!(f.source.next_due().await.unwrap(), Some(daily));
        let id = contract::Fixture::add(&f, daily + 1000, false);
        sync.changed(WS, &id);
        assert_eq!(f.source.next_due().await.unwrap(), Some(1000 + DEBOUNCE_MS));
        assert!(
            tokio::time::timeout(std::time::Duration::ZERO, sync.wake.notified())
                .await
                .is_ok()
        );
        f.source.run_due(1000 + DEBOUNCE_MS - 1).await.unwrap();
        assert!(contract::Fixture::done(&f).is_empty());
        f.source.run_due(1000 + DEBOUNCE_MS).await.unwrap();
        assert!(contract::Fixture::done(&f).contains(&id));
        assert_eq!(f.source.next_due().await.unwrap(), Some(daily));
    }
}

#[cfg(test)]
mod commit_wake_tests;
