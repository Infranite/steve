//! DomainStore assembly. One executor owns each validated Engine. Read services are an
//! explicit adapter seam, populated by scan/snapshot/transfer without changing writer semantics.
use crate::{
    domains::Registry,
    engine::Engine,
    exec::{BatchRunner, Executor, Lane, SystemClock},
    publish::{Attached, Publisher},
    uow::{RootRevision, SqliteBatchRunner, WallClock},
};
#[path = "domains/services.rs"]
mod services;
use std::{
    cell::RefCell,
    collections::{BTreeMap, BTreeSet},
    marker::PhantomData,
    rc::{Rc, Weak},
    future::Future,
    pin::Pin,
    sync::Arc,
};
use steve_storage_api::{
    retention::{Budget, RetentionReport},
    snapshot::LeaseId,
    *,
};

type Reply<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;
// Jobs share the store's owning Tokio blocking pool, even when a caller polls
// from a standard thread rather than a runtime worker.
async fn blocking<T: Send + 'static>(
    runtime: Option<tokio::runtime::Handle>,
    run: impl FnOnce() -> Result<T, Error> + Send + 'static,
) -> Result<T, Error> {
    let runtime = runtime.ok_or_else(|| {
        steve_storage_api::error(
            steve_types::Code::Unavailable,
            "SQLite async read runtime is not installed",
        )
    })?;
    let task = {
        let _context = runtime.enter();
        tokio::task::spawn_blocking(run)
    };
    task.await.map_err(|_| {
        steve_storage_api::error(steve_types::Code::Internal, "SQLite blocking task failed")
    })?
}

fn unsupported() -> Error {
    steve_storage_api::error(
        steve_types::Code::Unsupported,
        "SQLite read service not installed",
    )
}
/// Implemented by the read/lease/transfer assembly. Entity names are resolved through the
/// same Registry as writes; adapters must reject envelope/domain mismatches before SQL.
pub trait ReadServices: Send + Sync + 'static {
    fn reap_expired(&self) -> usize {
        0
    }
    fn query(&self, _: &Engine, _: &Registry, _: AnyQuery) -> Result<AnyOutput, Error> {
        Err(unsupported())
    }
    fn snapshot(
        &self,
        _: &Engine,
        _: &Registry,
        _: Domain,
        _: Vec<AnyQuery>,
    ) -> Result<Vec<AnyOutput>, Error> {
        Err(unsupported())
    }
    fn begin_snapshot(
        &self,
        _: Arc<Engine>,
        _: Arc<Registry>,
        _: Domain,
        _: Lane,
    ) -> Result<LeaseId, Error> {
        Err(unsupported())
    }
    fn lease_query(&self, _: LeaseId, _: AnyQuery) -> Result<AnyOutput, Error> {
        Err(unsupported())
    }
    fn lease_end(&self, _: LeaseId) -> Result<(), Error> {
        Err(unsupported())
    }
    fn begin_export(&self, _: &Engine, _: &Registry, _: Domain) -> Result<ExportSession, Error> {
        Err(unsupported())
    }
    fn export(&self, _: &ExportSession, _: ExportCursor) -> Result<ExportBatch, Error> {
        Err(unsupported())
    }
    fn end_export(&self, _: &ExportSession) -> Result<(), Error> {
        Err(unsupported())
    }
    fn import(&self, _: ExportBatch) -> Result<ImportReport, Error> {
        Err(unsupported())
    }
    fn retention(&self, _: String, _: Budget) -> Result<RetentionReport, Error> {
        Err(unsupported())
    }
    fn features(&self, _: Domain) -> BTreeSet<String> {
        BTreeSet::new()
    }
}
pub struct NoReadServices;
impl ReadServices for NoReadServices {}
pub struct EngineRunner<E: Send + 'static>(pub Arc<SqliteBatchRunner<E>>);
impl<E: Send + 'static> BatchRunner<Arc<Engine>> for EngineRunner<E> {
    type Job = Attached<E>;
    type Output = Committed;
    fn maintenance(
        &self,
        engine: &Arc<Engine>,
        now: std::time::Duration,
        idle: bool,
    ) -> Option<std::time::Duration> {
        engine.maintain(now, idle)
    }
    fn run(
        &self,
        engine: &Arc<Engine>,
        jobs: Vec<Attached<E>>,
    ) -> Vec<Result<Committed, CommitError>> {
        let count = jobs.len();
        match engine.writer_conn() {
            Ok(c) => self.0.run(&c, jobs),
            Err(e) => vec![Err(CommitError::Fatal(e)); count],
        }
    }
}
struct Writer<E: Send + 'static> {
    engine: Arc<Engine>,
    executor: Executor<Arc<Engine>, EngineRunner<E>>,
    runner: Arc<SqliteBatchRunner<E>>,
}
/// SQLite DomainStore. Hints cover ordinary rowid tables written through this Store
/// and its clones. Independent connections/processes and remote backends need the
/// consumer's 60-second polling fallback.
///
/// WITHOUT ROWID domain tables are prohibited by the schema guard. update_hook
/// misses truncate-optimized DELETE: the production source guard forbids clearing
/// watched tables without a predicate, including constant-true WHERE expressions.
/// REPLACE still hints through its INSERT. FTS shadow events do not imply a hint
/// under the virtual table's logical name; subscribe to ordinary domain tables.
pub struct SqliteStore<E: Send + 'static = ()> {
    registry: Arc<Registry>,
    writers: BTreeMap<Domain, Arc<Writer<E>>>,
    reads: Arc<dyn ReadServices>,
    hello: Hello,
    clock: Arc<dyn WallClock>,
    runtime: Option<tokio::runtime::Handle>,
}
impl<E: Send + 'static> Clone for SqliteStore<E> {
    fn clone(&self) -> Self {
        Self {
            registry: self.registry.clone(),
            writers: self.writers.clone(),
            reads: self.reads.clone(),
            hello: self.hello.clone(),
            clock: self.clock.clone(),
            runtime: self.runtime.clone(),
        }
    }
}
impl<E: Send + 'static> SqliteStore<E> {
    /// Engines must all come from the same validated layout. Writer connections are
    /// reused. Commit hints cover this Store and its clones, not independent connections.
    /// Construct within the owning Tokio runtime; it must outlive the store and its reads.
    pub fn new(
        engines: Vec<Engine>,
        registry: Arc<Registry>,
        clock: Arc<dyn WallClock>,
        publisher: Arc<dyn Publisher<E>>,
        runtime_probe: fn() -> bool,
    ) -> Result<Self, Error> {
        let runtime = tokio::runtime::Handle::try_current().map_err(|_| {
            steve_storage_api::error(
                steve_types::Code::Invalid,
                "SQLite store requires a Tokio runtime",
            )
        })?;
        Self::new_with_runtime(engines, registry, clock, publisher, runtime_probe, runtime)
    }
    /// Explicit read-service assembly. The caller owns the runtime and must keep
    /// it alive. The synchronous core/executor is identical in both constructors.
    pub fn new_with_runtime(
        engines: Vec<Engine>,
        registry: Arc<Registry>,
        clock: Arc<dyn WallClock>,
        publisher: Arc<dyn Publisher<E>>,
        runtime_probe: fn() -> bool,
        runtime: tokio::runtime::Handle,
    ) -> Result<Self, Error> {
        Self::new_sync(engines, registry, clock, publisher, runtime_probe)?.with_runtime(runtime)
    }
    /// Old facades, offline/admin tools and sync tests use this without entering
    /// Tokio. No lease/export services or temporary runtime are created.
    pub fn new_sync(
        engines: Vec<Engine>,
        registry: Arc<Registry>,
        clock: Arc<dyn WallClock>,
        publisher: Arc<dyn Publisher<E>>,
        runtime_probe: fn() -> bool,
    ) -> Result<Self, Error> {
        let mut writers = BTreeMap::new();
        let mut layout = None;
        for engine in engines {
            if layout.as_ref().is_some_and(|l| l != engine.layout()) {
                return Err(steve_storage_api::error(
                    steve_types::Code::Invalid,
                    "engines must share one layout",
                ));
            }
            layout = Some(engine.layout().clone());
            let path = engine.path();
            let engine = Arc::new(engine);
            let runner = Arc::new(
                SqliteBatchRunner::new(
                    &*engine.writer_conn()?,
                    registry.clone(),
                    clock.clone(),
                    publisher.clone(),
                )
                .map_err(Error::from)?,
            );
            let executor = Executor::new(
                path.file_name().unwrap_or_default().to_string_lossy(),
                engine.clone(),
                Arc::new(SystemClock::default()),
                EngineRunner(runner.clone()),
                runtime_probe,
            )
            .map_err(|_| {
                steve_storage_api::error(
                    steve_types::Code::Unavailable,
                    "cannot start storage executor",
                )
            })?;
            let writer = Arc::new(Writer {
                engine,
                executor,
                runner,
            });
            for (domain, file) in &writer.engine.layout().domains {
                if path == writer.engine.data_dir().join(file)
                    && writers.insert(*domain, writer.clone()).is_some()
                {
                    return Err(steve_storage_api::error(
                        steve_types::Code::Invalid,
                        "duplicate domain writer",
                    ));
                }
            }
        }
        if writers.len() != 3 {
            return Err(steve_storage_api::error(
                steve_types::Code::Invalid,
                "all three domain writers are required",
            ));
        }
        let hello = Hello {
            implementation: "steve-sqlite".into(),
            domains: [Domain::Core, Domain::Stream, Domain::Comms]
                .into_iter()
                .map(|domain| DomainOffer {
                    domain,
                    versions: (ContractVersion(1), ContractVersion(1)),
                    features: ["cas", "append_order"]
                        .into_iter()
                        .map(String::from)
                        .chain(
                            registry
                                .operation_names(domain)
                                .map(|name| format!("op:{name}")),
                        )
                        .collect(),
                    isolation: Isolation::Serializable,
                })
                .collect(),
        };
        let reads: Arc<dyn ReadServices> = Arc::new(NoReadServices);
        Ok(Self {
            registry,
            writers,
            reads,
            hello,
            clock,
            runtime: None,
        })
    }
    /// Add runtime-backed reads to an already constructed synchronous core.
    pub fn with_runtime(mut self, runtime: tokio::runtime::Handle) -> Result<Self, Error> {
        let reads = services::NativeReads::new(
            self.writers.clone(),
            self.registry.clone(),
            self.clock.clone(),
            Arc::new(crate::snapshot::SystemClock::default()),
        )?;
        self.runtime = Some(runtime);
        Ok(self.with_reads(Arc::new(reads)))
    }
    pub fn with_reads(mut self, reads: Arc<dyn ReadServices>) -> Self {
        for offer in &mut self.hello.domains {
            let previous = self.reads.features(offer.domain);
            offer.features.retain(|f| !previous.contains(f));
            offer.features.extend(reads.features(offer.domain));
        }
        self.reads = reads;
        self
    }
    /// Configure an injected monotonic lease clock before issuing requests. The wall
    /// clock for idempotency and retention does not control production hard deadlines.
    pub fn with_snapshot_clock(
        self,
        clock: Arc<dyn crate::snapshot::Clock>,
    ) -> Result<Self, Error> {
        if self.runtime.is_none() {
            return Err(steve_storage_api::error(
                steve_types::Code::Invalid,
                "install a read runtime before configuring snapshots",
            ));
        }
        let reads = services::NativeReads::new(
            self.writers.clone(),
            self.registry.clone(),
            self.clock.clone(),
            clock,
        )?;
        Ok(self.with_reads(Arc::new(reads)))
    }
    /// Release an export snapshot once its final batch no longer needs to be retried.
    /// DomainStore keeps sessions retryable, so callers explicitly close abandoned or
    /// completed exports here; dropping the store also releases all sessions.
    pub async fn end_export(&self, session: &ExportSession) -> Result<(), Error> {
        let reads = self.reads.clone();
        let session = session.clone();
        blocking(self.runtime.clone(), move || reads.end_export(&session)).await
    }
    /// Backend subscription entry keyed by physical table names in one writer
    /// file. No entity/descriptor lookup occurs here. Entity watches and the
    /// legacy Notify facade both attach weak consumer adapters to this registry.
    /// A registration inside its root grant is conservative until root release.
    pub fn subscribe_tables(&self, domain: Domain, tables: &[&str], hint: Arc<dyn CommitHint>) {
        if tables.is_empty() {
            return;
        }
        let writer = &self.writers[&domain];
        let tables: Vec<_> = tables.iter().map(|s| (*s).to_owned()).collect();
        // Registration never acquires a writer grant or connection: it is safe
        // while another thread holds a root, or this thread holds another file.
        writer.runner.watches.subscribe_hint(&tables, hint);
        // Only this thread's capture for this registry can conservatively mark
        // an in-scope subscription; other files/threads are ordinary registrations.
        crate::exec::watch::Capture::subscribed(&writer.runner.watches, &tables);
    }
    /// Backend-only table-key bridge for the legacy Store::commit_wake facade.
    /// It shares the entity watch registry, update hook and publication boundary.
    /// Subscribing inside a writer scope conservatively hints at its outer commit;
    /// pure rollbacks remain quiet. Domain code uses DomainStore::watch instead.
    pub fn commit_wake(&self, domain: Domain, tables: &[&str]) -> Arc<tokio::sync::Notify> {
        struct NotifyHint(std::sync::Weak<tokio::sync::Notify>);
        impl CommitHint for NotifyHint {
            fn is_alive(&self) -> bool {
                self.0.strong_count() > 0
            }
            fn notify(&self) {
                if let Some(notify) = self.0.upgrade() {
                    notify.notify_one();
                }
            }
        }
        let wake = Arc::new(tokio::sync::Notify::new());
        self.subscribe_tables(domain, tables, Arc::new(NotifyHint(Arc::downgrade(&wake))));
        wake
    }
    fn watch_across_files(&self, entities: &[EntityName]) -> CommitWatch {
        let watch = CommitWatch::new();
        let mut files = BTreeSet::new();
        for (domain, writer) in &self.writers {
            if !files.insert(writer.engine.path()) {
                continue;
            }
            let physical: Vec<_> = self
                .registry
                .entities()
                .filter(|e| {
                    entities.contains(&e.desc.name)
                        && Arc::ptr_eq(&self.writers[&e.desc.domain], writer)
                })
                .map(|e| e.table.as_str())
                .collect();
            self.subscribe_tables(*domain, &physical, watch.hint());
        }
        watch
    }
    pub fn registry(&self) -> &Arc<Registry> {
        &self.registry
    }
    /// Clock-injection seam: wait for expired lease owners to return their connections.
    pub fn reap_snapshots(&self) -> usize {
        self.reads.reap_expired()
    }
    pub fn engine(&self, domain: Domain) -> &Arc<Engine> {
        &self.writers[&domain].engine
    }
    pub fn submit_with_events(
        &self,
        u: UnitOfWork,
        events: Vec<E>,
    ) -> Result<crate::exec::Completion<Committed>, CommitError> {
        self.writers[&u.domain]
            .executor
            .submit(u.lane, Attached::new(u, events))
    }
    pub async fn commit_with_events(
        &self,
        u: UnitOfWork,
        events: Vec<E>,
    ) -> Result<Committed, CommitError> {
        self.submit_with_events(u, events)?.await.map_err(|_| {
            CommitError::Fatal(steve_storage_api::error(
                steve_types::Code::Unavailable,
                "storage executor stopped",
            ))
        })?
    }
    /// Legacy bridge seam: allocate before the callback opens a transaction. The callback owns
    /// COMMIT/rollback and pending events. SQL hooks record candidates; notifications
    /// leave only after the callback returns and successful statements are confirmed.
    /// Nested operations reuse the revision. Independent writers are outside this registry.
    /// Callbacks must preserve the writer's hooks/trace registration. Subscribe from
    /// the writer callback, outside SQL functions or SQLite hook execution.
    pub fn with_root<T>(
        &self,
        domain: Domain,
        lane: Lane,
        f: impl FnOnce(
            &rusqlite::Connection,
            &SqliteBatchRunner<E>,
            &RootRevision,
        ) -> Result<T, CommitError>,
    ) -> Result<T, CommitError> {
        let w = &self.writers[&domain];
        let c = self.root_guard(domain, lane)?;
        let rev = c.state.revision.clone();
        let owns_transaction = !c.nested || c.is_autocommit();
        // A nested bridge isolates its failure without rolling back its caller.
        let savepoint = if c.nested && !c.is_autocommit() {
            Some(NestedSavepoint::begin(&c)?)
        } else {
            None
        };
        let outcome =
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| f(&c, &w.runner, &rev)));
        let outcome = match outcome {
            Ok(Ok(value)) => match savepoint.map(NestedSavepoint::commit).transpose() {
                Ok(_) => Ok(Ok(value)),
                Err(error) => Ok(Err(error)),
            },
            other => {
                drop(savepoint);
                other
            }
        };
        let leaked = owns_transaction && !c.is_autocommit();
        let cleanup = if leaked {
            c.state.engine.rollback_root(&c)
        } else {
            Ok(())
        };
        // RootGuard publishes confirmed commits once, after rollback cleanup.
        // Release the Engine mutex before resuming caller unwinding; a user panic must
        // not poison the physical writer and prevent the next executor turn.
        drop(c);
        if outcome.is_ok() {
            cleanup.map_err(CommitError::Fatal)?;
        }
        match outcome {
            Ok(Ok(_)) if leaked => Err(crate::domains::invalid(
                "root callback left an open transaction; rolled back",
            )),
            Ok(result) => result,
            Err(panic) => std::panic::resume_unwind(panic),
        }
    }
    /// Thread-bound root authorization for the legacy facade. This is the guard
    /// extended by lane/carry integration, not a second synchronous executor.
    /// Same-Engine acquisitions share connection ownership on this thread, including
    /// non-LIFO guard drops. A different Engine still rejects before waiting.
    #[track_caller]
    pub fn root_guard(&self, domain: Domain, lane: Lane) -> Result<RootGuard<'_, E>, CommitError> {
        let w = &self.writers[&domain];
        let grant = w
            .executor
            .grant(lane)
            .map_err(|_| crate::domains::invalid("cannot acquire root grant"))?;
        let nested = grant.is_nested();
        let active = self.active_root(domain);
        let state = match active {
            Some(state) => state,
            None if nested => {
                return Err(crate::domains::invalid("root connection is not available in this executor callback"));
            }
            None => {
                let engine = w.engine.clone();
                let connection = engine.writer_conn().map_err(CommitError::Fatal)?;
                // Reserve before any facade can open BEGIN; nested DomainStore bridges
                // must reuse this authority even when the outer entry was a raw guard.
                let revision = Rc::new(w.runner.reserve_root(&connection)?);
                // SAFETY: Engine lives at a stable Arc allocation owned by RootConnection.
                // The private guard is dropped before that Arc and before the executor lease.
                // Rc/MutexGuard keep the whole state thread-bound; no mutable connection
                // reference escapes. Forgetting a guard leaks its Arc as well, so cannot
                // leave a dangling mutex. TLS stores only Weak ownership.
                let connection = unsafe {
                    std::mem::transmute::<
                        std::sync::MutexGuard<'_, rusqlite::Connection>,
                        std::sync::MutexGuard<'static, rusqlite::Connection>,
                    >(connection)
                };
                // SAFETY: both pointees live in stable Arc allocations. The shared
                // state retains Engine and runner, and drops Capture before either.
                let (captured_connection, captured_watches) = unsafe {
                    (&*std::ptr::from_ref(&*connection), &*std::ptr::from_ref(&w.runner.watches))
                };
                let capture = crate::exec::watch::Capture::begin(captured_connection, captured_watches)
                    .map_err(crate::domains::sql_error)?;
                let state = Rc::new(RootConnection {
                    capture: Some(capture),
                    connection,
                    revision,
                    _grant: Box::new(grant),
                    _runner: Box::new(w.runner.clone()),
                    engine,
                });
                ACTIVE_ROOT.with(|v| *v.borrow_mut() = Rc::downgrade(&state));
                return Ok(RootGuard { state, nested, _store: PhantomData });
            }
        };
        Ok(RootGuard { state, nested, _store: PhantomData })
    }
    fn active_root(&self, domain: Domain) -> Option<Rc<RootConnection>> {
        ACTIVE_ROOT.with(|v| v.borrow().upgrade())
            .filter(|state| Arc::ptr_eq(&state.engine, self.engine(domain)))
    }
    /// Borrow the authorized connection only inside a synchronous closure. Keeping
    /// the state alive also keeps its executor lease alive if outer guards drop.
    pub fn with_active_root<T>(&self, domain: Domain, run: impl FnOnce(&rusqlite::Connection) -> T) -> Option<T> {
        let state = self.active_root(domain)?;
        Some(run(&state.connection))
    }
    /// Explicit verification/bootstrap only. Existing production tables must already have their
    /// declared schema; registering descriptors never silently adds columns to legacy tables.
    pub fn install_registered(&self) -> Result<(), CommitError> {
        for domain in [Domain::Core, Domain::Stream, Domain::Comms] {
            self.with_root(domain, Lane::Admin, |c, _, _| {
                self.registry.install_domain(c, domain)
            })?;
        }
        Ok(())
    }
}
thread_local! {
    static ACTIVE_ROOT: RefCell<Weak<RootConnection>> = const { RefCell::new(Weak::new()) };
}
// Field order releases the physical mutex before the executor lease and Engine Arc.
struct RootConnection {
    capture: Option<crate::exec::watch::Capture<'static>>,
    connection: std::sync::MutexGuard<'static, rusqlite::Connection>,
    revision: Rc<RootRevision>,
    _grant: Box<dyn std::any::Any>,
    _runner: Box<dyn std::any::Any>,
    engine: Arc<Engine>,
}
impl Drop for RootConnection {
    fn drop(&mut self) {
        if !self.connection.is_autocommit() {
            let _ = self.engine.rollback_root(&self.connection);
        }
        if let Some(capture) = self.capture.take() {
            capture.finish();
        }
    }
}
/// Thread-bound root authorization. The last same-Engine guard rolls back any
/// leaked transaction, publishes confirmed hints once, releases the mutex, then
/// returns the executor lease.
/// Borrowed connections cannot outlive their guard. Never hold one across await.
pub struct RootGuard<'a, E: Send + 'static = ()> {
    state: Rc<RootConnection>,
    nested: bool,
    _store: PhantomData<&'a SqliteStore<E>>,
}
impl<E: Send + 'static> RootGuard<'_, E> {
    pub fn connection(&self) -> &rusqlite::Connection {
        &self.state.connection
    }

}
impl<E: Send + 'static> std::ops::Deref for RootGuard<'_, E> {
    type Target = rusqlite::Connection;
    fn deref(&self) -> &Self::Target {
        self.connection()
    }
}
struct NestedSavepoint<'a> {
    connection: &'a rusqlite::Connection,
    name: String,
    committed: bool,
}
impl<'a> NestedSavepoint<'a> {
    fn begin(connection: &'a rusqlite::Connection) -> Result<Self, CommitError> {
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let name = format!("steve_root_{}", NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed));
        connection.execute_batch(&format!("SAVEPOINT {name}")).map_err(crate::domains::sql_error)?;
        Ok(Self { connection, name, committed: false })
    }
    fn commit(mut self) -> Result<(), CommitError> {
        self.connection.execute_batch(&format!("RELEASE {}", self.name)).map_err(crate::domains::sql_error)?;
        self.committed = true;
        Ok(())
    }
}
impl Drop for NestedSavepoint<'_> {
    fn drop(&mut self) {
        if !self.committed {
            let _ = self.connection.execute_batch(&format!("ROLLBACK TO {0}; RELEASE {0}", self.name));
        }
    }
}
// Written as boxed futures to implement the async_trait ABI without another macro dependency.
impl<E: Send + 'static> DomainStore for SqliteStore<E> {
    fn watch(&self, entities: &[EntityName]) -> CommitWatch {
        self.watch_across_files(entities)
    }
    fn hello(&self) -> &Hello {
        &self.hello
    }
    fn commit<'s, 'a>(&'s self, u: UnitOfWork) -> Reply<'a, Result<Committed, CommitError>>
    where
        's: 'a,
        Self: 'a,
    {
        Box::pin(async move { self.commit_with_events(u, vec![]).await })
    }
    fn commit_many<'s, 'a>(
        &'s self,
        units: Vec<UnitOfWork>,
    ) -> Reply<'a, Vec<Result<Committed, CommitError>>>
    where
        's: 'a,
        Self: 'a,
    {
        Box::pin(async move {
            let pending = units
                .into_iter()
                .map(|u| self.submit_with_events(u, vec![]))
                .collect::<Vec<_>>();
            let mut out = Vec::with_capacity(pending.len());
            for p in pending {
                out.push(match p {
                    Err(e) => Err(e),
                    Ok(rx) => rx.await.unwrap_or_else(|_| {
                        Err(CommitError::Fatal(steve_storage_api::error(
                            steve_types::Code::Unavailable,
                            "executor stopped",
                        )))
                    }),
                });
            }
            out
        })
    }
    fn query<'s, 'a>(&'s self, q: AnyQuery) -> Reply<'a, Result<AnyOutput, Error>>
    where
        's: 'a,
        Self: 'a,
    {
        let (reads, engine, reg) = (
            self.reads.clone(),
            self.engine(q.domain).clone(),
            self.registry.clone(),
        );
        Box::pin(blocking(self.runtime.clone(), move || {
            reads.query(&engine, &reg, q)
        }))
    }
    fn snapshot<'s, 'a>(
        &'s self,
        d: Domain,
        qs: Vec<AnyQuery>,
    ) -> Reply<'a, Result<Vec<AnyOutput>, Error>>
    where
        's: 'a,
        Self: 'a,
    {
        let (reads, engine, reg) = (
            self.reads.clone(),
            self.engine(d).clone(),
            self.registry.clone(),
        );
        Box::pin(blocking(self.runtime.clone(), move || {
            reads.snapshot(&engine, &reg, d, qs)
        }))
    }
    fn begin_snapshot<'s, 'a>(&'s self, d: Domain, lane: Lane) -> Reply<'a, Result<LeaseId, Error>>
    where
        's: 'a,
        Self: 'a,
    {
        let (reads, engine, reg) = (
            self.reads.clone(),
            self.engine(d).clone(),
            self.registry.clone(),
        );
        Box::pin(blocking(self.runtime.clone(), move || {
            reads.begin_snapshot(engine, reg, d, lane)
        }))
    }
    fn lease_query<'s, 'a>(&'s self, l: LeaseId, q: AnyQuery) -> Reply<'a, Result<AnyOutput, Error>>
    where
        's: 'a,
        Self: 'a,
    {
        let reads = self.reads.clone();
        Box::pin(blocking(self.runtime.clone(), move || {
            reads.lease_query(l, q)
        }))
    }
    fn lease_end<'s, 'a>(&'s self, l: LeaseId) -> Reply<'a, Result<(), Error>>
    where
        's: 'a,
        Self: 'a,
    {
        let reads = self.reads.clone();
        Box::pin(blocking(self.runtime.clone(), move || reads.lease_end(l)))
    }
    fn begin_export<'s, 'a>(&'s self, d: Domain) -> Reply<'a, Result<ExportSession, Error>>
    where
        's: 'a,
        Self: 'a,
    {
        let (reads, engine, reg) = (
            self.reads.clone(),
            self.engine(d).clone(),
            self.registry.clone(),
        );
        Box::pin(blocking(self.runtime.clone(), move || {
            reads.begin_export(&engine, &reg, d)
        }))
    }
    fn export<'s, 'e, 'a>(
        &'s self,
        s: &'e ExportSession,
        c: ExportCursor,
    ) -> Reply<'a, Result<ExportBatch, Error>>
    where
        's: 'a,
        'e: 'a,
        Self: 'a,
    {
        let reads = self.reads.clone();
        let s = s.clone();
        Box::pin(blocking(self.runtime.clone(), move || reads.export(&s, c)))
    }
    fn import<'s, 'a>(&'s self, b: ExportBatch) -> Reply<'a, Result<ImportReport, Error>>
    where
        's: 'a,
        Self: 'a,
    {
        let reads = self.reads.clone();
        Box::pin(async move {
            if crate::exec::current_engine().is_some() {
                return Err(CommitError::UowInsideGrant.into());
            }
            blocking(self.runtime.clone(), move || reads.import(b)).await
        })
    }
    fn retention<'s, 'a>(
        &'s self,
        e: String,
        b: Budget,
    ) -> Reply<'a, Result<RetentionReport, Error>>
    where
        's: 'a,
        Self: 'a,
    {
        let reads = self.reads.clone();
        Box::pin(async move {
            if crate::exec::current_engine().is_some() {
                return Err(CommitError::UowInsideGrant.into());
            }
            blocking(self.runtime.clone(), move || reads.retention(e, b)).await
        })
    }
}
