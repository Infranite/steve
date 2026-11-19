//! SQLite storage for the Steve Hub: the Rust port of hub/src/db.ts and hub/src/repo.ts.
//!
//! [`Store::open`] runs the db.ts migrations verbatim; the repositories (`store.items()`, `store.bots()`, …)
//! mirror the repo.ts objects one to one and publish a [`BusEvent`] after every write.
//!
//! Connections: one write connection behind a `std::sync::Mutex`, plus an r2d2 pool of read-only
//! connections (one per core, 4 to 16). SQLite allows a single writer anyway and every write here is a few statements, so a
//! mutex costs nothing and keeps the TS guarantee that writes (and their events) happen in one order;
//! Offline administration uses one lazy reader instead.
//! WAL lets the pooled readers serve `/api/state` and event paging in parallel without waiting on it.
//! A write followed by a read in the same operation runs on the write connection, so it sees its own data.
//!
//! The API is synchronous like better-sqlite3; calls take microseconds,
//! async callers can run them inline or in `spawn_blocking` for the big reads.

steve_i18n::init!();

pub mod base;
pub mod bot_dispatch;
mod bus;
pub mod caps;
pub mod compact;
pub mod db;
pub mod diagnostics;
mod event_payload;
pub mod maintenance;
pub mod outbox;
mod repo;
pub mod seeded;
mod tx;
mod types;
pub mod workflow_gate;

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{SystemTime, UNIX_EPOCH};

use r2d2_sqlite::SqliteConnectionManager;
use rusqlite::{Connection, OpenFlags};
use sha2::{Digest, Sha256};

pub use bus::{FnPublisher, NoPublisher, Publisher};
pub use repo::*;
pub use steve_types::BusEvent;
pub use tx::Tx;
pub use types::*;

/// The default workspace id (`WS` in repo.ts).
pub const WS: &str = "default";

/// Number of pooled read connections: one per core, at least 4 and at most 16. WAL readers do not
/// block each other or the writer, so a request waits on the pool only when this many reads are running.
fn readers() -> u32 {
    std::thread::available_parallelism()
        .map_or(4, |n| n.get() as u32)
        .clamp(4, 16)
}

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error(transparent)]
    Sqlite(#[from] rusqlite::Error),
    #[error(transparent)]
    Pool(#[from] r2d2::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    /// The TS `throw new Error("没有这个…")` cases; the message is user-facing.
    #[error("{0}")]
    NotFound(&'static str),
    /// A hook registered by a module (e.g. [`Store::on_item_deleted`]) refused; the write rolls back.
    #[error("{0}")]
    Hook(String),
    /// A migration left the database inconsistent; it was rolled back.
    #[error("{0}")]
    Migration(String),
    /// The database needs a newer build: opening it is refused, nothing was written.
    #[error(
        "数据库（schema v{schema}）需要至少认识 schema v{min_reader} 的版本才能打开，这个版本只认识到 v{ours}；没有打开，也没有改动它"
    )]
    Incompatible {
        schema: i64,
        min_reader: i64,
        ours: i64,
    },
    /// Offline administration must never initialize or upgrade a database.
    #[error("离线管理需要本版本的数据库头部；请先启动或升级 Hub：{0}")]
    AdminSchema(String),
    /// The write contradicts the item's lifecycle (a terminal status outside the lifecycle, a status
    /// change on a closed item); the message is user-facing.
    #[error("{0}")]
    Conflict(String),
}

/// Runs inside the transaction that deletes an item: `(tx, workspace_id, item_id)`.
pub type ItemDeletedHook = dyn Fn(&Tx, &str, &str) -> std::result::Result<(), String> + Send + Sync;

pub type Result<T, E = StoreError> = std::result::Result<T, E>;

/// A failed session open requires an explicit human retry. Used by both its durable stop and
/// its decision card, so background queue flushing and YOLO cannot restart it.
pub const SESSION_START_FAILURE: &str = "session_start";

/// Unix milliseconds, like `Date.now()`.
pub fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// `uid(p)`: prefix, underscore, first 16 hex chars of a random UUID v4.
pub fn uid(prefix: &str) -> String {
    let hex = uuid::Uuid::new_v4().simple().to_string();
    format!("{prefix}_{}", &hex[..16])
}

/// sha256 as lowercase hex; auth and MCP tokens are stored only in this form.
pub fn sha(s: &str) -> String {
    Sha256::digest(s.as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// Shared handle to the database. Cheap to clone.
#[derive(Clone)]
pub struct Store {
    inner: Arc<Inner>,
}

struct Inner {
    maintenance: Arc<maintenance::Gate>,
    writer: Mutex<Connection>,
    readers: r2d2::Pool<SqliteConnectionManager>,
    bus: Box<dyn Publisher>,
    path: PathBuf,
    item_deleted: std::sync::OnceLock<Box<ItemDeletedHook>>,
    /// Notified after a commit that emitted an outbox event ([`outbox`]).
    outbox_wake: Arc<tokio::sync::Notify>,
    commit_wakes: Arc<Mutex<CommitWakes>>,
    diagnostics: std::sync::OnceLock<diagnostics::Recorder>,
}

#[derive(Default)]
struct CommitWakes {
    hooked: bool,
    listeners: Vec<(Vec<&'static str>, std::sync::Weak<tokio::sync::Notify>)>,
    changed: std::collections::BTreeSet<String>,
}

/// The write connection plus the publisher, handed to write closures so events go out under the lock.
pub(crate) struct W<'a> {
    pub c: &'a Connection,
    store: &'a Store,
}

impl W<'_> {
    pub fn publish(&self, e: impl Into<BusEvent>) {
        self.store.emit_event(e.into())
    }

    /// A write transaction on the write connection (or a nested `SAVEPOINT` inside [`Store::atomic`]).
    pub fn tx(&self) -> rusqlite::Result<tx::WriteTx<'_>> {
        tx::WriteTx::begin(self.c)
    }
}

/// `BEGIN IMMEDIATE`: takes the write lock at the start. The writer Mutex orders this process's
/// writes, but another process (the upgrade supervisor, the next release during a switch) can open the
/// same file; a DEFERRED transaction that read first and then writes after that process committed
/// fails at once with SQLITE_BUSY_SNAPSHOT, while BEGIN IMMEDIATE waits out the busy timeout.
/// Read-only transactions on the pooled readers stay DEFERRED (IMMEDIATE fails there).
pub(crate) fn immediate(c: &Connection) -> rusqlite::Result<rusqlite::Transaction<'_>> {
    rusqlite::Transaction::new_unchecked(c, rusqlite::TransactionBehavior::Immediate)
}

impl Store {
    /// Opens `path` (creating it), sets the pragmas, migrates, and starts the read pool.
    /// The TS Hub's path is `STEVE_HOME/data/steve.sqlite3`; the caller decides.
    pub fn open(path: impl AsRef<Path>, bus: impl Publisher + 'static) -> Result<Self> {
        Self::open_observed(path.as_ref(), bus, None, false)
    }

    /// Opens an existing database at exactly this build's schema head for offline administration.
    /// The caller must hold the Hub instance lock. Repository writes remain available, but opening
    /// never migrates, optimizes, initializes workspaces or starts diagnostic recording. One lazy
    /// read connection is enough for a CLI. Transitional adapter, replaced by the storage engine.
    pub fn open_admin(path: impl AsRef<Path>) -> Result<Self> {
        Self::open_observed(path.as_ref(), NoPublisher, None, true)
    }

    /// Like [`Self::open`], with a callback for actual migration work (also within long statements).
    /// The caller owns reporting; ordinary opens install no progress handler.
    pub fn open_with_migration_progress(
        path: impl AsRef<Path>,
        bus: impl Publisher + 'static,
        observer: db::MigrationObserver,
    ) -> Result<Self> {
        Self::open_observed(path.as_ref(), bus, Some(observer), false)
    }

    fn open_observed(
        path: &Path,
        bus: impl Publisher + 'static,
        observer: Option<db::MigrationObserver>,
        admin: bool,
    ) -> Result<Self> {
        let path = path.to_path_buf();
        let writer = if admin {
            db::open_admin_writer(&path)?
        } else {
            db::open_writer(&path, observer)?
        };
        let manager = SqliteConnectionManager::file(&path)
            .with_flags(OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX)
            .with_init(|c| db::pragmas(c));
        // a checkout test (an empty statement per `get`) finds nothing on a local file
        let readers = r2d2::Pool::builder()
            .max_size(if admin { 1 } else { readers() })
            .min_idle(if admin { Some(0) } else { None })
            .test_on_check_out(false)
            .build(manager)?;
        let maintenance = Arc::new(maintenance::Gate::load(&writer)?);
        let diagnostic_until = if admin {
            0
        } else {
            diagnostics::active_until(&writer).unwrap_or(0)
        };
        let store = Self {
            inner: Arc::new(Inner {
                maintenance,
                writer: Mutex::new(writer),
                readers,
                bus: Box::new(bus),
                path,
                item_deleted: std::sync::OnceLock::new(),
                outbox_wake: Arc::new(tokio::sync::Notify::new()),
                commit_wakes: Arc::default(),
                diagnostics: std::sync::OnceLock::new(),
            }),
        };
        if !admin
            && let Some(recorder) =
                diagnostics::Recorder::start(Arc::downgrade(&store.inner), diagnostic_until)
        {
            let _ = store.inner.diagnostics.set(recorder);
        }
        Ok(store)
    }

    /// Sets what else goes when an item is deleted (the knowledge module's document and index rows),
    /// in the same transaction as the delete. Set once; `false` if already set.
    pub fn on_item_deleted(
        &self,
        f: impl Fn(&Tx, &str, &str) -> std::result::Result<(), String> + Send + Sync + 'static,
    ) -> bool {
        self.inner.item_deleted.set(Box::new(f)).is_ok()
    }

    pub(crate) fn item_deleted_hook(&self) -> Option<&ItemDeletedHook> {
        self.inner.item_deleted.get().map(|b| b.as_ref())
    }

    /// Connect only to a DueSource's Notify. This content-free hint must never identify a row
    /// or control a business-logic branch. Hints may coalesce or be spurious; in-process watched
    /// row changes through the authoritative Store (or its clones) wake after the outermost commit.
    /// Independently opened Stores and other processes are not observed. Correctness must still
    /// rely on due_loop's 60-second fallback.
    /// DomainEvent carries business facts; it must not be a second wake path for a due loop.
    ///
    /// SQLite update_hook observes trigger writes, but not truncate-optimized unconditional
    /// DELETEs, REPLACE's implicit deleted rows, or WITHOUT ROWID tables. Use explicit row writes
    /// for promptly making work due; these SQLite exceptions also rely on the fallback.
    /// A rolled-back savepoint can leave a harmless extra hint when its outer write commits.
    /// Hooks install lazily for live subscribers and detach before the next writer lease
    /// without subscribers. Subscribing inside a write scope also hints at its outer commit.
    pub fn commit_wake(&self, tables: &[&'static str]) -> Arc<tokio::sync::Notify> {
        let wake = Arc::new(tokio::sync::Notify::new());
        let register = |c: &Connection| {
            self.inner
                .commit_wakes
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .listeners
                .push((tables.to_vec(), Arc::downgrade(&wake)));
            self.sync_commit_hooks(c);
        };
        if let Some(c) = self.active_conn() {
            register(c);
            // Earlier statements may predate hook installation. A conservative hint is safe.
            self.inner
                .commit_wakes
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .changed
                .extend(tables.iter().map(|t| (*t).to_owned()));
        } else {
            let c = self.lock();
            register(&c);
        }
        wake
    }

    // Registration and hook installation share the unique writer lease, so a new
    // subscription cannot race an already-running transaction on another thread.
    fn sync_commit_hooks(&self, c: &Connection) {
        let mut state = self
            .inner
            .commit_wakes
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        state.listeners.retain(|(_, w)| w.strong_count() > 0);
        let needed = !state.listeners.is_empty();
        if needed == state.hooked {
            return;
        }
        if needed {
            let wakes = self.inner.commit_wakes.clone();
            c.update_hook(Some(move |_, _: &str, table: &str, _| {
                let mut wakes = wakes.lock().unwrap_or_else(|e| e.into_inner());
                if wakes
                    .listeners
                    .iter()
                    .any(|(tables, _)| tables.contains(&table))
                {
                    wakes.changed.insert(table.to_owned());
                }
            }))
            .expect("writer lease permits installing update hook");
            let wakes = self.inner.commit_wakes.clone();
            c.rollback_hook(Some(move || {
                wakes
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .changed
                    .clear();
            }))
            .expect("writer lease permits installing rollback hook");
        } else {
            c.update_hook(None::<fn(rusqlite::hooks::Action, &str, &str, i64)>)
                .expect("remove update hook");
            c.rollback_hook(None::<fn()>).expect("remove rollback hook");
            state.changed.clear();
        }
        state.hooked = needed;
    }

    fn notify_committed(&self) {
        let mut wakes = self
            .inner
            .commit_wakes
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let changed = std::mem::take(&mut wakes.changed);
        wakes.listeners.retain(|(tables, w)| {
            if let Some(wake) = w.upgrade() {
                if tables.iter().any(|t| changed.contains(*t)) {
                    wake.notify_one();
                }
                true
            } else {
                false
            }
        });
    }

    /// Wakes the outbox dispatcher: notified (one permit) after each commit that emitted an event.
    pub fn outbox_wake(&self) -> Arc<tokio::sync::Notify> {
        self.inner.outbox_wake.clone()
    }

    pub fn path(&self) -> &Path {
        &self.inner.path
    }

    pub(crate) fn inner_id(&self) -> usize {
        Arc::as_ptr(&self.inner) as usize
    }

    /// Publishes an event that is not tied to a store write (`steve.thinking`, imports finishing).
    pub fn publish(&self, e: impl Into<BusEvent>) {
        self.emit_event(e.into())
    }

    #[track_caller]
    pub(crate) fn lock(&self) -> WriteGuard<'_> {
        let timing = LockTiming::start("store", std::panic::Location::caller());
        // A panic mid-write leaves SQLite consistent (the transaction rolls back), so poisoning is ignored.
        let conn = self.inner.writer.lock().unwrap_or_else(|e| e.into_inner());
        instrumentation::store_acquired();
        self.sync_commit_hooks(&conn);
        WriteGuard {
            conn,
            timing: timing.map(LockTiming::acquired),
        }
    }

    #[track_caller]
    pub(crate) fn write<T>(&self, f: impl FnOnce(&W) -> Result<T>) -> Result<T> {
        if let Some(c) = self.active_conn() {
            return f(&W { c, store: self });
        }
        let c = self.lock();
        let result = f(&W { c: &c, store: self });
        if c.is_autocommit() {
            self.notify_committed();
        }
        result
    }

    pub(crate) fn read<T>(&self, f: impl FnOnce(&Connection) -> Result<T>) -> Result<T> {
        if let Some(c) = self.active_conn() {
            return f(c);
        }
        let c = self.inner.readers.get()?;
        instrumentation::store_acquired();
        f(&c)
    }

    /// Refreshes the query planner's statistics where they went stale (`PRAGMA optimize`, bounded), on
    /// the write connection. Cheap when nothing changed; meant for an hourly housekeeping pass.
    #[track_caller]
    pub fn optimize(&self) -> Result<()> {
        if let Some(c) = self.active_conn() {
            return Ok(db::optimize(c, false)?);
        }
        Ok(db::optimize(&self.lock(), false)?)
    }

    /// Runs `f` on the write connection under the write lock, for one-off maintenance and tests.
    /// Prefer a repository method; this exists so callers never need a second connection.
    #[track_caller]
    pub fn with_conn<T>(&self, f: impl FnOnce(&Connection) -> Result<T>) -> Result<T> {
        if let Some(c) = self.active_conn() {
            return f(c);
        }
        let c = self.lock();
        let result = f(&c);
        if c.is_autocommit() {
            self.notify_committed();
        }
        result
    }
}

/// Runs a multi-statement read under a single snapshot. When `c` is already inside an active
/// transaction (such as a read inside [`Store::atomic`]), reuses that transaction instead of
/// starting a nested `BEGIN DEFERRED`.
pub(crate) fn read_snapshot<T>(
    c: &Connection,
    f: impl FnOnce(&Connection) -> Result<T>,
) -> Result<T> {
    if !c.is_autocommit() {
        return f(c);
    }
    let tx = rusqlite::Transaction::new_unchecked(c, rusqlite::TransactionBehavior::Deferred)?;
    let out = f(&tx)?;
    tx.commit()?;
    Ok(out)
}

// Shared lock-boundary mechanics; recorder installation belongs to the embedding application.
pub mod instrumentation {
    /// Whether the Hub installed its in-memory recorder. Off: one flag load, no clock or TLS.
    static LOCK_METRICS: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

    pub fn enable_lock_metrics() {
        // Publish the already-installed recorder before any worker caches its first handle.
        LOCK_METRICS.store(true, std::sync::atomic::Ordering::Release);
    }

    tokio::task_local! {
        static REQUEST_CALLS: std::sync::Arc<std::sync::atomic::AtomicU64>;
    }

    /// One HTTP request's successful store connection acquisitions, shared with carried closures.
    #[derive(Clone, Default)]
    pub struct RequestCalls(std::sync::Arc<std::sync::atomic::AtomicU64>);

    impl RequestCalls {
        pub async fn scope<F: std::future::Future>(&self, future: F) -> F::Output {
            REQUEST_CALLS.scope(self.0.clone(), future).await
        }

        pub fn count(&self) -> u64 {
            self.0.load(std::sync::atomic::Ordering::Relaxed)
        }
    }

    /// Carry only this request's counter into synchronous work on another thread. Like i18n's
    /// carrier, sync_scope restores the worker's previous context on return or unwind.
    pub fn carry<R>(f: impl FnOnce() -> R) -> impl FnOnce() -> R {
        let calls = if LOCK_METRICS.load(std::sync::atomic::Ordering::Acquire) {
            REQUEST_CALLS.try_with(std::sync::Arc::clone).ok()
        } else {
            None
        };
        move || match calls {
            Some(calls) => REQUEST_CALLS.sync_scope(calls, f),
            None => f(),
        }
    }

    pub(crate) fn store_acquired() {
        if !LOCK_METRICS.load(std::sync::atomic::Ordering::Acquire) {
            return;
        }
        let _ = REQUEST_CALLS.try_with(|calls| {
            calls.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        });
    }

    /// Same opt-in gate as lock timing; only the first transaction on a thread registers a handle.
    pub(crate) fn transaction_started() {
        if !LOCK_METRICS.load(std::sync::atomic::Ordering::Acquire) {
            return;
        }
        thread_local! {
            static TRANSACTIONS: std::cell::OnceCell<metrics::Counter> = const { std::cell::OnceCell::new() };
        }
        TRANSACTIONS.with(|counter| {
            counter
                .get_or_init(|| metrics::counter!("steve_store_transactions_total"))
                .increment(1);
        });
    }

    /// Shared by the store and core lock boundaries. Handles are registered once per source location
    /// and thread. Only that cold registration formats the bounded source label; repeated acquisitions
    /// clone handles without allocating strings or taking another application lock.
    pub struct LockTiming {
        since: std::time::Instant,
        wait: metrics::Histogram,
        hold: metrics::Histogram,
        count: metrics::Counter,
    }

    impl LockTiming {
        pub fn start(
            kind: &'static str,
            caller: &'static std::panic::Location<'static>,
        ) -> Option<Self> {
            if !LOCK_METRICS.load(std::sync::atomic::Ordering::Acquire) {
                return None;
            }
            type Handles = (metrics::Histogram, metrics::Histogram, metrics::Counter);
            thread_local! {
                static HANDLES: std::cell::RefCell<std::collections::HashMap<
                    (&'static str, &'static std::panic::Location<'static>), Handles
                >> = std::cell::RefCell::new(std::collections::HashMap::new());
            }
            let (wait, hold, count) = HANDLES.with(|cache| {
                cache
                    .borrow_mut()
                    .entry((kind, caller))
                    .or_insert_with(|| {
                        let site = format!("{}:{}", caller.file(), caller.line());
                        (
                            metrics::histogram!("steve_lock_wait_seconds", "lock" => kind, "caller" => site.clone()),
                            metrics::histogram!("steve_lock_hold_seconds", "lock" => kind, "caller" => site.clone()),
                            metrics::counter!("steve_lock_acquisitions_total", "lock" => kind, "caller" => site),
                        )
                    })
                    .clone()
            });
            Some(Self {
                since: std::time::Instant::now(),
                wait,
                hold,
                count,
            })
        }

        pub fn acquired(mut self) -> Self {
            self.wait.record(self.since.elapsed().as_secs_f64());
            self.count.increment(1);
            self.since = std::time::Instant::now();
            self
        }
    }

    impl Drop for LockTiming {
        fn drop(&mut self) {
            self.hold.record(self.since.elapsed().as_secs_f64());
        }
    }
    #[cfg(test)]
    mod tests {
        use super::*;
        use std::sync::{
            Arc,
            atomic::{AtomicUsize, Ordering},
        };

        struct Sink {
            registrations: AtomicUsize,
            samples: Arc<AtomicUsize>,
        }
        struct Count(Arc<AtomicUsize>);
        impl metrics::CounterFn for Count {
            fn increment(&self, value: u64) {
                self.0.fetch_add(value as usize, Ordering::Relaxed);
            }
            fn absolute(&self, value: u64) {
                self.0.store(value as usize, Ordering::Relaxed);
            }
        }
        impl metrics::Recorder for Sink {
            fn describe_counter(
                &self,
                _: metrics::KeyName,
                _: Option<metrics::Unit>,
                _: metrics::SharedString,
            ) {
            }
            fn describe_gauge(
                &self,
                _: metrics::KeyName,
                _: Option<metrics::Unit>,
                _: metrics::SharedString,
            ) {
            }
            fn describe_histogram(
                &self,
                _: metrics::KeyName,
                _: Option<metrics::Unit>,
                _: metrics::SharedString,
            ) {
            }
            fn register_counter(
                &self,
                key: &metrics::Key,
                _: &metrics::Metadata<'_>,
            ) -> metrics::Counter {
                assert_eq!(key.name(), "steve_store_transactions_total");
                self.registrations.fetch_add(1, Ordering::Relaxed);
                metrics::Counter::from_arc(Arc::new(Count(self.samples.clone())))
            }
            fn register_gauge(
                &self,
                _: &metrics::Key,
                _: &metrics::Metadata<'_>,
            ) -> metrics::Gauge {
                metrics::Gauge::noop()
            }
            fn register_histogram(
                &self,
                _: &metrics::Key,
                _: &metrics::Metadata<'_>,
            ) -> metrics::Histogram {
                metrics::Histogram::noop()
            }
        }

        #[test]
        fn transactions_are_disabled_until_enabled_and_register_once_per_thread() {
            std::thread::spawn(|| {
                let sink = Sink {
                    registrations: AtomicUsize::new(0),
                    samples: Arc::new(AtomicUsize::new(0)),
                };
                metrics::with_local_recorder(&sink, || {
                    transaction_started();
                    assert_eq!(sink.registrations.load(Ordering::Relaxed), 0);
                    enable_lock_metrics();
                    for _ in 0..3 {
                        transaction_started();
                    }
                    assert_eq!(sink.registrations.load(Ordering::Relaxed), 1);
                    assert_eq!(sink.samples.load(Ordering::Relaxed), 3);
                });
            })
            .join()
            .unwrap();
        }
    }
}

use instrumentation::LockTiming;

pub(crate) struct WriteGuard<'a> {
    conn: MutexGuard<'a, Connection>,
    timing: Option<LockTiming>,
}

impl std::ops::Deref for WriteGuard<'_> {
    type Target = Connection;
    fn deref(&self) -> &Self::Target {
        &self.conn
    }
}

impl Drop for WriteGuard<'_> {
    fn drop(&mut self) {
        drop(self.timing.take());
    }
}

/// Legacy storage grants; scheduling belongs to the Hub assembly root.
pub mod retention {
    use super::{Result, Store};
    pub use steve_store_sqlite::retention::{
        Budget, DEFAULT_BUDGET, SliceReport,
        policies::{ALL, Kind},
    };
    impl Store {
        /// Exactly one writer-grant lifetime. The scheduler invokes this separately for each slice.
        pub fn retention_slice(
            &self,
            kind: Kind,
            now: i64,
            budget: Budget,
            after: Option<i64>,
        ) -> Result<SliceReport<i64>> {
            let policy = kind.policy();
            self.with_conn(|c| {
                Ok(steve_store_sqlite::retention::run_slice(
                    c,
                    policy.as_ref(),
                    now,
                    budget,
                    after.as_ref(),
                )?)
            })
        }
        pub(crate) fn retention_payload_slice<G>(
            &self,
            session: &str,
            upper: i64,
            now: i64,
            budget: Budget,
            after: Option<i64>,
            guard: &mut impl FnMut() -> Option<G>,
        ) -> Result<Option<SliceReport<i64>>> {
            use std::{cell::RefCell, collections::BTreeMap};
            use steve_store_sqlite::retention as r;
            let cache = RefCell::new(BTreeMap::new());
            let action = |c: &rusqlite::Connection, row: &r::Row| {
                let id = r::policies::integer(row, "rowid")?;
                self.apply_retention_event(
                    c,
                    cache
                        .borrow()
                        .get(&id)
                        .ok_or_else(|| r::invalid("missing prepared event"))?,
                )
                .map_err(|e| rusqlite::Error::ToSqlConversionFailure(Box::new(e)))
            };
            let policy = r::policies::SessionEvents::new(session, upper, &action);
            let prepared = self.reader(|c| {
                super::read_snapshot(c, |snapshot| {
                    let candidates =
                        r::before_candidates(snapshot, &policy, now, budget, after.as_ref())?;
                    Ok(r::prepare(&policy, candidates, |row| {
                        let event = self
                            .prepare_retention_event(snapshot, row)
                            .map_err(|e| rusqlite::Error::ToSqlConversionFailure(Box::new(e)))?;
                        cache
                            .borrow_mut()
                            .insert(r::policies::integer(row, "rowid")?, event);
                        Ok(true)
                    }))
                })
            })?;
            let Some(_guard) = guard() else {
                return Ok(None);
            };
            let report =
                self.with_conn(|c| Ok(r::run_slice(c, &prepared, now, budget, after.as_ref())?))?;
            Ok(Some(report))
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use rusqlite::hooks::{AuthAction, AuthContext, Authorization, TransactionOperation};
        use std::sync::{
            Arc,
            atomic::{AtomicUsize, Ordering},
        };

        #[test]
        fn payload_page_uses_one_read_snapshot_for_all_candidates() {
            let dir = tempfile::tempdir().unwrap();
            let store =
                Store::open(dir.path().join("retention.sqlite3"), crate::NoPublisher).unwrap();
            store.ensure_workspace().unwrap();
            let session = store
                .sessions()
                .create(&crate::NewSession {
                    workspace_id: crate::WS.into(),
                    harness: "fixture".into(),
                    cwd: "/tmp".into(),
                    ..Default::default()
                })
                .unwrap();
            for _ in 0..3 {
                store.events().add(&session.id,"user",serde_json::json!({"text":"screenshot","images":[{"type":"image","mimeType":"image/png","data":"QUJD".repeat(3000)}]})).unwrap();
            }
            let upto = store.events().last_seq(&session.id).unwrap();
            let snapshots = Arc::new(AtomicUsize::new(0));
            // Instrument every pooled reader, including readers the page may check out later.
            let readers = (0..store.inner.readers.max_size())
                .map(|_| store.inner.readers.get().unwrap())
                .collect::<Vec<_>>();
            for reader in &readers {
                let count = snapshots.clone();
                reader
                    .authorizer(Some(move |context: AuthContext<'_>| {
                        if matches!(
                            context.action,
                            AuthAction::Transaction {
                                operation: TransactionOperation::Begin
                            }
                        ) {
                            count.fetch_add(1, Ordering::SeqCst);
                        }
                        Authorization::Allow
                    }))
                    .unwrap();
            }
            drop(readers);
            let report = store
                .retention_payload_slice(
                    &session.id,
                    upto,
                    crate::now(),
                    DEFAULT_BUDGET,
                    None,
                    &mut || Some(()),
                )
                .unwrap()
                .unwrap();
            assert_eq!((report.affected, report.failures), (3, 0));
            assert_eq!(
                snapshots.load(Ordering::SeqCst),
                1,
                "candidate selection and all hydration share a page snapshot"
            );
        }
    }
}
