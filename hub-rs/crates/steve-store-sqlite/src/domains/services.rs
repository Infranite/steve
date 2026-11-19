//! Real read/transfer assembly. Blocking work is scheduled by SqliteStore, while
//! lease owner threads alone use their reserved connections.
use super::{ReadServices, Writer};
use crate::uow::WallClock;
use crate::{
    domains::Registry,
    engine::Engine,
    exec::Lane,
    snapshot::{self, LeaseManager},
    transfer::{self, ExportManager},
};
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::{Arc, Mutex},
};
use steve_storage_api::{
    query::ResumeToken,
    retention::{Budget, RetentionReport},
    *,
};
fn invalid(s: &str) -> Error {
    error(steve_types::Code::Invalid, s)
}
fn stale() -> Error {
    error(steve_types::Code::Stale, "unknown or expired snapshot")
}
fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}
pub(super) struct NativeReads<E: Send + 'static> {
    registry: Arc<Registry>,
    writers: BTreeMap<Domain, Arc<Writer<E>>>,
    managers: BTreeMap<Domain, Arc<LeaseManager>>,
    leases: Mutex<BTreeMap<String, (LeaseId, Domain)>>,
    exports: ExportManager,
    clock: Arc<dyn WallClock>,
    lease_clock: Arc<dyn snapshot::Clock>,
    after: Mutex<BTreeMap<String, ResumeToken>>,
}
impl<E: Send + 'static> NativeReads<E> {
    pub(super) fn new(
        writers: BTreeMap<Domain, Arc<Writer<E>>>,
        registry: Arc<Registry>,
        clock: Arc<dyn WallClock>,
        lease_clock: Arc<dyn snapshot::Clock>,
    ) -> Result<Self, Error> {
        let mut by_file = BTreeMap::new();
        let mut managers = BTreeMap::new();
        for (domain, w) in &writers {
            let manager = match by_file.get(&w.engine.path()) {
                Some(m) => Arc::clone(m),
                None => {
                    let m = Arc::new(LeaseManager::with_clock(
                        w.engine.clone(),
                        lease_clock.clone(),
                    )?);
                    by_file.insert(w.engine.path(), m.clone());
                    m
                }
            };
            managers.insert(*domain, manager);
        }
        let entities = registry
            .entities()
            .map(|e| {
                let mut d = e.desc.clone();
                d.name = e.table.clone();
                d
            })
            .collect();
        let exports = ExportManager::new(writers[&Domain::Core].engine.clone(), entities)?;
        Ok(Self {
            registry,
            writers,
            managers,
            exports,
            clock,
            lease_clock,
            leases: Mutex::default(),
            after: Mutex::default(),
        })
    }
    // Catch inside the connection guard, as with_root does: a domain adapter panic
    // must roll back before the guard drops, not poison the writer for later commits.
    fn write<T>(
        &self,
        d: Domain,
        lane: Lane,
        run: impl FnOnce(&rusqlite::Connection, &crate::uow::SqliteBatchRunner<E>) -> Result<T, Error>,
    ) -> Result<T, Error> {
        let w = &self.writers[&d];
        let grant = w
            .executor
            .grant(lane)
            .map_err(|_| invalid("cannot acquire service grant"))?;
        let c = w.engine.writer_conn()?;
        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| run(&c, &w.runner)));
        let leaked = !c.is_autocommit();
        if leaked {
            let _ = c.execute_batch("ROLLBACK");
        }
        drop(c);
        drop(grant);
        match outcome {
            Ok(Ok(_)) if leaked => Err(invalid("service left an open transaction")),
            Ok(result) => result,
            Err(_) => Err(error(
                steve_types::Code::Internal,
                "domain service panicked",
            )),
        }
    }
    fn domain(&self, lease: &LeaseId) -> Result<Domain, Error> {
        lock(&self.leases)
            .get(&lease.id)
            .filter(|(id, _)| id == lease)
            .map(|(_, d)| *d)
            .ok_or_else(stale)
    }
}
impl<E: Send + 'static> ReadServices for NativeReads<E> {
    fn query(&self, engine: &Engine, reg: &Registry, q: AnyQuery) -> Result<AnyOutput, Error> {
        let c = engine.read_conn()?;
        let tx = c
            .unchecked_transaction()
            .map_err(crate::domains::sql_error)
            .map_err(Error::from)?;
        let output = reg.query_on(&tx, q)?;
        tx.commit()
            .map_err(crate::domains::sql_error)
            .map_err(Error::from)?;
        Ok(output)
    }
    fn snapshot(
        &self,
        engine: &Engine,
        reg: &Registry,
        d: Domain,
        qs: Vec<AnyQuery>,
    ) -> Result<Vec<AnyOutput>, Error> {
        let queries = qs
            .into_iter()
            .map(|q| {
                if q.domain != d {
                    return Err(invalid("mixed-domain snapshot"));
                }
                Ok((reg.query_context(&q)?, q))
            })
            .collect::<Result<_, _>>()?;
        snapshot::read_batch_with(engine, d, queries, |c, _, q| reg.query_on(c, q))
    }
    fn begin_snapshot(
        &self,
        _: Arc<Engine>,
        _: Arc<Registry>,
        domain: Domain,
        lane: Lane,
    ) -> Result<LeaseId, Error> {
        lock(&self.leases).retain(|_, (id, _)| id.expires_at.0 > self.lease_clock.now().0);
        let lease = self.managers[&domain].begin(domain, lane)?;
        lock(&self.leases).insert(lease.id.clone(), (lease.clone(), domain));
        Ok(lease)
    }
    fn lease_query(&self, lease: LeaseId, q: AnyQuery) -> Result<AnyOutput, Error> {
        let domain = self.domain(&lease)?;
        if domain != q.domain {
            return Err(invalid("mixed-domain lease"));
        }
        let reg = self.registry.clone();
        let result =
            self.managers[&domain].with_connection(&lease, domain, move |c| reg.query_on(c, q));
        if result
            .as_ref()
            .is_err_and(|e| e.code == steve_types::Code::Stale)
        {
            lock(&self.leases).remove(&lease.id);
        }
        result
    }
    fn lease_end(&self, lease: LeaseId) -> Result<(), Error> {
        let domain = self.domain(&lease)?;
        lock(&self.leases).remove(&lease.id);
        self.managers[&domain].end(&lease)
    }
    fn reap_expired(&self) -> usize {
        let mut files = BTreeSet::new();
        let mut n = 0;
        for (d, m) in &self.managers {
            if files.insert(self.writers[d].engine.path()) {
                n += m.reap_expired();
            }
        }
        lock(&self.leases).retain(|_, (id, _)| id.expires_at.0 > self.lease_clock.now().0);
        n
    }
    fn begin_export(&self, _: &Engine, reg: &Registry, d: Domain) -> Result<ExportSession, Error> {
        self.exports.begin_with(d, |c, d| reg.export_state(c, d))
    }
    fn export(&self, s: &ExportSession, mut cursor: ExportCursor) -> Result<ExportBatch, Error> {
        let after = cursor.after.clone();
        if cursor.entity.is_empty() {
            if cursor.after.is_some() {
                return Err(invalid("initial export cursor has a key"));
            }
            cursor.entity = self
                .registry
                .entities()
                .find(|e| e.desc.domain == s.domain)
                .ok_or_else(|| invalid("empty export domain"))?
                .table
                .clone();
        } else {
            cursor.entity = self
                .registry
                .entity(&cursor.entity, s.domain)
                .map_err(Error::from)?
                .table
                .clone();
        }
        let mut batch = self.exports.export(s, cursor)?;
        let logical = self
            .registry
            .entities()
            .find(|e| e.table == batch.entity.name && e.desc.domain == s.domain)
            .ok_or_else(|| invalid("unknown export entity"))?;
        batch.entity = logical.desc.clone();
        if let Some(next) = &mut batch.next
            && next.after.is_some()
        {
            next.entity = logical.desc.name.clone();
        } else {
            batch.next = self
                .registry
                .entities()
                .filter(|e| e.desc.domain == s.domain)
                .skip_while(|e| e.desc.name != logical.desc.name)
                .nth(1)
                .map(|e| ExportCursor {
                    entity: e.desc.name.clone(),
                    after: None,
                });
        }
        batch.id = format!(
            "{}:{}:{}",
            s.id,
            batch.entity.name,
            serde_json::to_string(&after).map_err(|_| invalid("invalid export cursor"))?
        );
        batch.digest = transfer::batch_digest(&batch)?;
        Ok(batch)
    }
    fn end_export(&self, session: &ExportSession) -> Result<(), Error> {
        self.exports.end(session)
    }
    fn import(&self, mut batch: ExportBatch) -> Result<ImportReport, Error> {
        let d = batch.session.domain;
        if !self.registry.can_export(d) {
            return Err(error(
                steve_types::Code::Unsupported,
                "domain import declaration is incomplete",
            ));
        }
        if transfer::batch_digest(&batch)? != batch.digest {
            return Err(invalid("import checksum mismatch"));
        }
        let wire_digest = batch.digest;
        let wire_id = batch.id.clone();
        let entity = self
            .registry
            .entity(&batch.entity.name, d)
            .map_err(Error::from)?;
        if entity.desc != batch.entity {
            return Err(invalid("import descriptor mismatch"));
        }
        let mut physical = entity.desc.clone();
        physical.name = entity.table.clone();
        batch.entity = physical.clone();
        batch.digest = transfer::batch_digest(&batch)?;
        self.write(d, Lane::Admin, |c, runner| {
            let receipt_key = format!("import:{wire_id}");
            use rusqlite::OptionalExtension;
            let prior: Option<String> = c
                .query_row(
                    "SELECT value FROM meta WHERE domain=?1 AND key=?2",
                    rusqlite::params![crate::domains::domain_name(d), receipt_key],
                    |r| r.get(0),
                )
                .optional()
                .map_err(crate::domains::sql_error)
                .map_err(Error::from)?;
            if let Some(prior) = prior {
                if serde_json::from_str::<uow::RequestDigest>(&prior)
                    .map_err(|_| invalid("invalid import receipt"))?
                    != wire_digest
                {
                    return Err(invalid("import batch identity mismatch"));
                }
                return Ok(ImportReport {
                    batch_id: wire_id,
                    imported: 0,
                    replayed: true,
                });
            }
            runner
                .revisions
                .advance_past(c, batch.session.rev)
                .map_err(Error::from)?;
            let tx =
                rusqlite::Transaction::new_unchecked(c, rusqlite::TransactionBehavior::Immediate)
                    .map_err(crate::domains::sql_error)
                    .map_err(Error::from)?;
            let report = transfer::import_with(&tx, &physical, batch, |c, d, s| {
                self.registry.restore_state(c, d, s, self.clock.now())
            })?;
            crate::layout::put_meta(
                &tx,
                crate::domains::domain_name(d),
                &receipt_key,
                &serde_json::to_string(&wire_digest)
                    .map_err(|_| invalid("invalid import digest"))?,
            )?;
            tx.commit()
                .map_err(crate::domains::sql_error)
                .map_err(Error::from)?;
            Ok(report)
        })
    }
    fn features(&self, domain: Domain) -> BTreeSet<String> {
        std::iter::once("snapshot_read".into())
            .chain(
                self.registry
                    .query_names(domain)
                    .map(|n| format!("query:{n}")),
            )
            .chain(self.registry.can_export(domain).then(|| "export".into()))
            .collect()
    }
    fn retention(&self, name: String, budget: Budget) -> Result<RetentionReport, Error> {
        if budget.rows == 0 || budget.target_ms == 0 {
            return Err(invalid("invalid retention budget"));
        }
        let e = self
            .registry
            .entities()
            .find(|e| e.desc.name == name)
            .ok_or_else(|| error(steve_types::Code::Unsupported, "unknown retention entity"))?;
        let definition = crate::domains::retention::authorize(e)?;
        let mut after = lock(&self.after);
        let report = self.write(e.desc.domain, Lane::Background, |c, runner| {
            let root = runner.reserve_root(c).map_err(Error::from)?;
            let tx =
                rusqlite::Transaction::new_unchecked(c, rusqlite::TransactionBehavior::Immediate)
                    .map_err(crate::domains::sql_error)
                    .map_err(Error::from)?;
            let policy = crate::domains::retention::EntityPolicy {
                entity: e,
                definition,
                rev: root.rev(),
            };
            let report = crate::retention::run_slice(
                &tx,
                &policy,
                self.clock.now().0,
                budget,
                after.get(&name),
            )
            .map_err(crate::domains::retention::read_error)?;
            tx.commit()
                .map_err(crate::domains::sql_error)
                .map_err(Error::from)?;
            Ok(report)
        })?;
        let more = report.more();
        // Publish continuation only after the outer transaction has committed.
        if let Some(next) = report.next {
            after.insert(name.clone(), next);
        } else {
            after.remove(&name);
        }
        Ok(RetentionReport {
            entity: name,
            affected: report.affected,
            slices: 1,
            max_slice_ms: report.elapsed_ms.min(u64::from(u32::MAX)) as u32,
            overruns: report.overruns,
            more,
        })
    }
}
