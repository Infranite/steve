//! Synchronous repository facade over the storage engine's writer and read pool.
//! Runtime opens initialize or convert under the engine's lifetime lock. Repository
//! transactions keep their existing events and SAVEPOINT semantics on its RootGuard.

steve_i18n::init!();

pub mod base;
pub mod bot_dispatch;
mod bus;
pub mod caps;
pub mod compact;
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
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use r2d2_sqlite::SqliteConnectionManager;
use rusqlite::Connection;
use sha2::{Digest, Sha256};
use steve_storage_api::{Domain, Lane};
use steve_store_sqlite::{engine::Engine, store::SqliteStore};

pub use bus::{FnPublisher, NoPublisher, Publisher};
pub use repo::*;
pub use steve_store_sqlite::compat::{CurrentStorage, StorageCapabilities, StorageState};
pub use steve_types::BusEvent;
pub use tx::Tx;
pub use types::*;

/// The default workspace id (`WS` in repo.ts).
pub const WS: &str = "default";

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
    /// Engine errors retain their typed code and retry policy.
    #[error(transparent)]
    Storage(#[from] steve_types::Error),
    #[error(transparent)]
    Commit(#[from] steve_storage_api::CommitError),
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

/// Diagnostic assembly policy for an Engine whose open mode was already enforced.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FacadeMode {
    Runtime,
    Admin,
}

struct EnginePublisher;
impl steve_store_sqlite::publish::Publisher<()> for EnginePublisher {
    fn publish(&self, _: steve_types::Rev, _: Vec<()>) {}
}

fn storage_dir(path: &Path) -> Result<&Path> {
    if path.file_name() != Some(std::ffi::OsStr::new(steve_store_sqlite::layout::CORE_FILE)) {
        return Err(steve_storage_api::error(
            steve_types::Code::Invalid,
            "storage path must end in steve.sqlite3",
        )
        .into());
    }
    Ok(path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new(".")))
}

/// Shared handle to the database. Cheap to clone.
#[derive(Clone)]
pub struct Store {
    inner: Arc<Inner>,
}

struct Inner {
    maintenance: Arc<maintenance::Gate>,
    engine: SqliteStore<()>,
    readers: r2d2::Pool<SqliteConnectionManager>,
    bus: Box<dyn Publisher>,
    path: PathBuf,
    item_deleted: std::sync::OnceLock<Box<ItemDeletedHook>>,
    /// Notified after a commit that emitted an outbox event ([`outbox`]).
    outbox_wake: Arc<tokio::sync::Notify>,
    diagnostics: std::sync::OnceLock<diagnostics::Recorder>,
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
    /// Open the canonical database through the engine's runtime path.
    pub fn open(path: impl AsRef<Path>, bus: impl Publisher + 'static) -> Result<Self> {
        let dir = storage_dir(path.as_ref())?;
        Self::from_engine(
            steve_store_sqlite::open::open_runtime(dir)?,
            bus,
            FacadeMode::Runtime,
        )
    }

    /// Observe the same locked initialization/conversion/domain-upgrade flow.
    pub fn open_with_migration_progress(
        path: impl AsRef<Path>,
        bus: impl Publisher + 'static,
        observer: steve_store_sqlite::migrate::Observer,
    ) -> Result<Self> {
        let dir = storage_dir(path.as_ref())?;
        Self::from_engine(
            steve_store_sqlite::open::open_runtime_observed(dir, observer)?,
            bus,
            FacadeMode::Runtime,
        )
    }

    /// Attach repositories to an already opened Engine without opening another writer or pool.
    /// The mode controls diagnostic business workers only; the engine has already enforced
    /// runtime/admin compatibility, locking and connection policy.
    pub fn from_engine(
        engine: Engine,
        bus: impl Publisher + 'static,
        mode: FacadeMode,
    ) -> Result<Self> {
        let path = engine.path();
        let readers = engine.readers().clone();
        let engine = SqliteStore::new_sync(
            vec![engine],
            Arc::default(),
            Arc::new(steve_store_sqlite::uow::SystemWallClock),
            Arc::new(EnginePublisher),
            || false,
        )?;
        let (maintenance, diagnostic_until) = {
            let c = engine.engine(Domain::Core).read_conn()?;
            let maintenance = Arc::new(maintenance::Gate::load(&c)?);
            let until = if mode == FacadeMode::Runtime {
                diagnostics::active_until(&c).unwrap_or(0)
            } else {
                0
            };
            (maintenance, until)
        };
        let store = Self {
            inner: Arc::new(Inner {
                maintenance,
                engine,
                readers,
                bus: Box::new(bus),
                path,
                item_deleted: std::sync::OnceLock::new(),
                outbox_wake: Arc::new(tokio::sync::Notify::new()),
                diagnostics: std::sync::OnceLock::new(),
            }),
        };
        if mode == FacadeMode::Runtime
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
    /// Known hint blind spots include truncate-optimized unconditional DELETEs and
    /// WITHOUT ROWID writes. These omissions are not permission for silent mutations of a
    /// subscribed table. REPLACE skips callbacks for its implicit deleted rows, but its
    /// replacement INSERT normally hints the same rowid table; this is a table-level hint,
    /// not a per-row deletion event.
    /// A rolled-back savepoint can leave a harmless extra hint when its outer write commits.
    /// Each SQLite commit seals its hint separately, so a later autocommit statement failure or
    /// rollback in the same writer closure cannot erase an earlier successful commit.
    /// Hooks install lazily for live subscribers and detach before the next writer lease
    /// without subscribers. Subscribing inside a write scope also hints at its outer commit.
    pub fn commit_wake(&self, tables: &[&'static str]) -> Arc<tokio::sync::Notify> {
        self.inner.engine.commit_wake(Domain::Core, tables)
    }

    /// Wakes the outbox dispatcher: notified (one permit) after each commit that emitted an event.
    pub fn outbox_wake(&self) -> Arc<tokio::sync::Notify> {
        self.inner.outbox_wake.clone()
    }

    /// Observe current storage metadata through the engine's shared reader pool.
    pub fn storage_state(&self) -> Result<StorageState> {
        self.read(|c| Ok(steve_store_sqlite::open::inspect(c)?))
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
    pub(crate) fn lock(&self) -> Result<WriteGuard<'_>> {
        let timing = LockTiming::start("store", std::panic::Location::caller());
        let conn = self
            .inner
            .engine
            .root_guard(Domain::Core, Lane::Interactive)?;
        instrumentation::store_acquired();
        Ok(WriteGuard {
            conn,
            timing: timing.map(LockTiming::acquired),
        })
    }

    #[track_caller]
    pub(crate) fn write<T>(&self, f: impl FnOnce(&W) -> Result<T>) -> Result<T> {
        if let Some(c) = self.active_conn() {
            return f(&W { c, store: self });
        }
        let c = self.lock()?;
        let result = f(&W { c: &c, store: self });
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

    /// Explicit bounded planner maintenance, scheduled through the same root authority.
    #[track_caller]
    pub fn optimize(&self) -> Result<()> {
        self.with_conn(|c| {
            c.pragma_update(
                None,
                "analysis_limit",
                steve_store_sqlite::pragmas::ANALYSIS_LIMIT,
            )?;
            Ok(c.execute_batch("PRAGMA optimize;")?)
        })
    }

    /// Runs `f` on the write connection under the write lock, for one-off maintenance and tests.
    /// Prefer a repository method; this exists so callers never need a second connection.
    #[track_caller]
    pub fn with_conn<T>(&self, f: impl FnOnce(&Connection) -> Result<T>) -> Result<T> {
        if let Some(c) = self.active_conn() {
            return f(c);
        }
        let c = self.lock()?;
        let result = f(&c);
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
    conn: steve_store_sqlite::store::RootGuard<'a, ()>,
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
            let store = Store::open(dir.path().join("steve.sqlite3"), crate::NoPublisher).unwrap();
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
