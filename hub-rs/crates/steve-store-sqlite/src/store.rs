//! DomainStore assembly. One executor owns each validated Engine. Read services are an
//! explicit adapter seam, populated by scan/snapshot/transfer without changing writer semantics.
use crate::{
    domains::Registry,
    engine::Engine,
    exec::{BatchRunner, Executor, Lane, SystemClock},
    publish::{Attached, Publisher},
    uow::{RootRevision, SqliteBatchRunner, WallClock},
};
use std::{
    collections::{BTreeMap, BTreeSet},
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
fn unsupported() -> Error {
    steve_storage_api::error(
        steve_types::Code::Unsupported,
        "SQLite read service not installed",
    )
}
/// Implemented by the read/lease/transfer assembly. Entity names are resolved through the
/// same Registry as writes; adapters must reject envelope/domain mismatches before SQL.
pub trait ReadServices: Send + Sync + 'static {
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
pub struct SqliteStore<E: Send + 'static = ()> {
    registry: Arc<Registry>,
    writers: BTreeMap<Domain, Arc<Writer<E>>>,
    reads: Arc<dyn ReadServices>,
    hello: Hello,
}
impl<E: Send + 'static> SqliteStore<E> {
    /// Engines must all come from the same validated layout. No new connection is opened here.
    pub fn new(
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
        Ok(Self {
            registry,
            writers,
            reads: Arc::new(NoReadServices),
            hello,
        })
    }
    pub fn with_reads(mut self, reads: Arc<dyn ReadServices>) -> Self {
        for offer in &mut self.hello.domains {
            offer.features.extend(reads.features(offer.domain));
        }
        self.reads = reads;
        self
    }
    pub fn registry(&self) -> &Arc<Registry> {
        &self.registry
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
    /// COMMIT/rollback and pending events. Nested operations pass their existing RootRevision.
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
        let _grant = w
            .executor
            .grant(lane)
            .map_err(|_| crate::domains::invalid("cannot acquire root grant"))?;
        if _grant.is_nested() {
            return Err(crate::domains::invalid(
                "nested bridge must reuse the existing connection and root revision",
            ));
        }
        let c = w.engine.writer_conn().map_err(CommitError::Fatal)?;
        let rev = w.runner.reserve_root(&c)?;
        let outcome =
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| f(&c, &w.runner, &rev)));
        let leaked = !c.is_autocommit();
        if leaked {
            let _ = c.execute_batch("ROLLBACK");
        }
        // Release the Engine mutex before resuming caller unwinding; a user panic must
        // not poison the physical writer and prevent the next executor turn.
        drop(c);
        drop(_grant);
        match outcome {
            Ok(Ok(_)) if leaked => Err(crate::domains::invalid(
                "root callback left an open transaction; rolled back",
            )),
            Ok(result) => result,
            Err(panic) => std::panic::resume_unwind(panic),
        }
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
// Written as boxed futures to implement the async_trait ABI without another macro dependency.
impl<E: Send + 'static> DomainStore for SqliteStore<E> {
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
        Box::pin(async move { self.reads.query(self.engine(q.domain), &self.registry, q) })
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
        Box::pin(async move { self.reads.snapshot(self.engine(d), &self.registry, d, qs) })
    }
    fn begin_snapshot<'s, 'a>(&'s self, d: Domain, lane: Lane) -> Reply<'a, Result<LeaseId, Error>>
    where
        's: 'a,
        Self: 'a,
    {
        Box::pin(async move {
            self.reads
                .begin_snapshot(self.engine(d).clone(), self.registry.clone(), d, lane)
        })
    }
    fn lease_query<'s, 'a>(&'s self, l: LeaseId, q: AnyQuery) -> Reply<'a, Result<AnyOutput, Error>>
    where
        's: 'a,
        Self: 'a,
    {
        Box::pin(async move { self.reads.lease_query(l, q) })
    }
    fn lease_end<'s, 'a>(&'s self, l: LeaseId) -> Reply<'a, Result<(), Error>>
    where
        's: 'a,
        Self: 'a,
    {
        Box::pin(async move { self.reads.lease_end(l) })
    }
    fn begin_export<'s, 'a>(&'s self, d: Domain) -> Reply<'a, Result<ExportSession, Error>>
    where
        's: 'a,
        Self: 'a,
    {
        Box::pin(async move { self.reads.begin_export(self.engine(d), &self.registry, d) })
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
        Box::pin(async move { self.reads.export(s, c) })
    }
    fn import<'s, 'a>(&'s self, b: ExportBatch) -> Reply<'a, Result<ImportReport, Error>>
    where
        's: 'a,
        Self: 'a,
    {
        Box::pin(async move { self.reads.import(b) })
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
        Box::pin(async move { self.reads.retention(e, b) })
    }
}
