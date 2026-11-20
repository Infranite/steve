//! Atomic SQLite work units. Revisions are reserved outside the batch transaction; each
//! unit owns a rollback-on-drop savepoint. Events leave the writer only after COMMIT.
use crate::{
    domains::{self, Registry, domain_name, invalid, next_sequence, sql_error, value, values},
    exec::BatchRunner,
    publish::{Attached, PendingEvents, Publisher},
    rev::RevAllocator,
};
use rusqlite::{Connection, OptionalExtension, Transaction, TransactionBehavior, params};
use serde::{Deserialize, Serialize};
use std::{
    panic::{AssertUnwindSafe, catch_unwind},
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
};
use steve_storage_api::{
    append::Seq,
    cursor::{CursorKey, CursorName, Position},
    entity::EntityKind,
    uow::*,
    *,
};

pub trait WallClock: Send + Sync + 'static {
    fn now(&self) -> Millis;
}
pub struct SystemWallClock;
impl WallClock for SystemWallClock {
    fn now(&self) -> Millis {
        Millis(
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis()
                .min(i64::MAX as u128) as i64,
        )
    }
}
static NEXT_SAVEPOINT: AtomicU64 = AtomicU64::new(1);
struct UnitSavepoint<'a> {
    c: &'a Connection,
    name: String,
    finished: bool,
}
impl<'a> UnitSavepoint<'a> {
    fn begin(c: &'a Connection) -> Result<Self, CommitError> {
        let name = format!(
            "storage_uow_{}",
            NEXT_SAVEPOINT.fetch_add(1, Ordering::Relaxed)
        );
        c.execute_batch(&format!("SAVEPOINT {name}"))
            .map_err(sql_error)?;
        Ok(Self {
            c,
            name,
            finished: false,
        })
    }
    fn commit(mut self) -> Result<(), CommitError> {
        self.c
            .execute_batch(&format!("RELEASE {}", self.name))
            .map_err(sql_error)?;
        self.finished = true;
        Ok(())
    }
}
impl Drop for UnitSavepoint<'_> {
    fn drop(&mut self) {
        if !self.finished {
            let _ = self
                .c
                .execute_batch(&format!("ROLLBACK TO {}; RELEASE {}", self.name, self.name));
        }
    }
}
#[derive(Serialize, Deserialize)]
struct Receipt {
    digest: RequestDigest,
    result: Committed,
}
/// Opaque authority allocated before the outer legacy transaction. A rollback never returns it.
pub struct RootRevision(Rev, usize);
impl RootRevision {
    pub fn rev(&self) -> Rev {
        self.0
    }
}
pub struct Applied<E> {
    pub committed: Committed,
    pub pending: Option<PendingEvents<E>>,
}

pub struct SqliteBatchRunner<E: Send + 'static> {
    pub registry: Arc<Registry>,
    pub revisions: Arc<RevAllocator>,
    pub(crate) watches: CommitWatches,
    clock: Arc<dyn WallClock>,
    publisher: Arc<dyn Publisher<E>>,
}
impl<E: Send + 'static> SqliteBatchRunner<E> {
    pub fn new(
        c: &Connection,
        registry: Arc<Registry>,
        clock: Arc<dyn WallClock>,
        publisher: Arc<dyn Publisher<E>>,
    ) -> Result<Self, CommitError> {
        Ok(Self {
            registry,
            revisions: Arc::new(RevAllocator::open(c)?),
            watches: CommitWatches::default(),
            clock,
            publisher,
        })
    }
    pub fn reserve_root(&self, c: &Connection) -> Result<RootRevision, CommitError> {
        Ok(RootRevision(
            self.revisions.issue(c, 1)?[0],
            Arc::as_ptr(&self.revisions) as usize,
        ))
    }
    pub fn apply_in(
        &self,
        c: &Connection,
        job: Attached<E>,
        root: &RootRevision,
    ) -> Result<Applied<E>, CommitError> {
        if root.1 != Arc::as_ptr(&self.revisions) as usize {
            return Err(invalid("root revision belongs to another writer"));
        }
        if c.is_autocommit() {
            return Err(invalid("apply_in requires the caller's active transaction"));
        }
        let committed = self.isolated(c, &job.unit, root.0)?;
        let pending = (!committed.replayed).then_some(PendingEvents {
            rev: committed.rev,
            events: job.events,
        });
        Ok(Applied { committed, pending })
    }
    fn isolated(&self, c: &Connection, u: &UnitOfWork, rev: Rev) -> Result<Committed, CommitError> {
        match catch_unwind(AssertUnwindSafe(|| {
            let savepoint = UnitSavepoint::begin(c)?;
            let result = self.apply(c, u, rev)?;
            savepoint.commit()?;
            Ok(result)
        })) {
            Ok(r) => r,
            Err(_) => Err(domains::failure(
                steve_types::Code::Internal,
                "work unit panicked; savepoint rolled back",
            )),
        }
    }
    fn validate(&self, u: &UnitOfWork) -> Result<(), CommitError> {
        u.validate_age(self.clock.now())?;
        u.verify_digest().map_err(CommitError::Fatal)?;
        u.validate_cursor_idempotency()
            .map_err(CommitError::Fatal)?;
        for p in &u.preconditions {
            match p {
                Precondition::Rev { entity, .. }
                | Precondition::Absent { entity, .. }
                | Precondition::Present { entity, .. } => {
                    self.registry.entity(entity, u.domain)?;
                }
                _ => {}
            }
        }
        for m in &u.mutations {
            match m {
                Mutation::Insert { entity, .. }
                | Mutation::Upsert { entity, .. }
                | Mutation::Update { entity, .. }
                | Mutation::Delete { entity, .. }
                | Mutation::DeleteWhere { entity, .. } => {
                    self.registry.entity(entity, u.domain)?;
                }
                Mutation::Append { stream, .. } => {
                    self.registry.entity(&stream.0, u.domain)?;
                }
                Mutation::Op(o) => self.registry.check_op(o, u.domain)?,
                _ => {}
            }
        }
        for f in &u.facts {
            self.registry.check_topic(&f.topic, u.domain)?;
        }
        Ok(())
    }
    #[cfg(feature = "conformance")]
    fn access_entity(
        &self,
        c: &Connection,
        op: &str,
        entity: &str,
        domain: Domain,
    ) -> crate::exec::access_log::Operation {
        let context = self
            .registry
            .entity(entity, domain)
            .map(|e| crate::exec::access_log::table_context(&e.table))
            .unwrap_or_else(|_| "unmapped".into());
        crate::exec::access_log::Operation::on(c, format!("{op}:{entity}"), context)
    }
    #[cfg(feature = "conformance")]
    fn access_precondition(
        &self,
        c: &Connection,
        p: &Precondition,
        domain: Domain,
    ) -> crate::exec::access_log::Operation {
        match p {
            Precondition::Rev { entity, .. }
            | Precondition::Absent { entity, .. }
            | Precondition::Present { entity, .. } => {
                self.access_entity(c, "precondition", entity, domain)
            }
            _ => crate::exec::access_log::Operation::on(c, "cursor_precondition", "contract"),
        }
    }
    #[cfg(feature = "conformance")]
    fn access_mutation(
        &self,
        c: &Connection,
        m: &Mutation,
        domain: Domain,
    ) -> crate::exec::access_log::Operation {
        match m {
            Mutation::Insert { entity, .. } => self.access_entity(c, "insert", entity, domain),
            Mutation::Upsert { entity, .. } => self.access_entity(c, "upsert", entity, domain),
            Mutation::Update { entity, .. } => self.access_entity(c, "update", entity, domain),
            Mutation::Delete { entity, .. } => self.access_entity(c, "delete", entity, domain),
            Mutation::DeleteWhere { entity, .. } => {
                self.access_entity(c, "delete_where", entity, domain)
            }
            Mutation::Append { stream, .. } => self.access_entity(c, "append", &stream.0, domain),
            Mutation::Op(o) => crate::exec::access_log::Operation::on(
                c,
                &o.name,
                crate::exec::access_log::named_context(&o.name, domain),
            ),
            _ => crate::exec::access_log::Operation::on(c, "advance_cursor", "contract"),
        }
    }
    fn natural(&self, u: &UnitOfWork) -> Result<Option<String>, CommitError> {
        let Idempotency::Natural { entity, index } = &u.idempotency else {
            return Ok(None);
        };
        let e = self.registry.entity(entity, u.domain)?;
        let [
            Mutation::Insert {
                entity: target,
                row,
            },
        ] = u.mutations.as_slice()
        else {
            return Err(invalid("unproven natural work unit"));
        };
        if target != entity || !u.facts.is_empty() || !u.preconditions.is_empty() {
            return Err(invalid("natural work unit has side effects"));
        }
        if matches!(e.desc.kind, EntityKind::Append { .. }) {
            return Err(invalid("append requires sequence allocation"));
        }
        e.check_row(row)?;
        let fields = if index.is_empty() {
            &e.desc.key
        } else {
            &e.desc
                .indexes
                .iter()
                .find(|i| i.name == *index && i.unique)
                .ok_or_else(|| invalid("natural index must be unique"))?
                .fields
        };
        let key = values(row, fields);
        if key.contains(&Cell::Null) {
            return Err(invalid("natural index cannot contain null"));
        }
        // Collision-free tuple encoding; domain is stored separately. No recorded intent row.
        Ok(Some(format!(
            "natural:{}",
            serde_json::to_string(&(entity, index, key))
                .map_err(|_| invalid("invalid natural key"))?
        )))
    }
    fn apply(&self, c: &Connection, u: &UnitOfWork, rev: Rev) -> Result<Committed, CommitError> {
        #[cfg(feature = "conformance")]
        let _access = crate::exec::access_log::Operation::on(c, "commit", "contract");
        self.validate(u)?;
        let d = domain_name(u.domain);
        let natural = self.natural(u)?;
        // The public identity is (domain, issued_at, key). SQL's (domain,key) stores an
        // injective encoding of (issued_at,key), never a delimiter concatenation.
        let identity = serde_json::to_string(&(u.id.issued_at.0, &u.id.key))
            .map_err(|_| invalid("invalid intent identity"))?;
        let receipt: Option<Receipt> = if let Some(key) = &natural {
            let text: Option<String> = c
                .query_row(
                    "SELECT value FROM meta WHERE domain=? AND key=?",
                    params![d, key],
                    |r| r.get(0),
                )
                .optional()
                .map_err(sql_error)?;
            text.map(|s| serde_json::from_str(&s).map_err(|_| invalid("invalid natural receipt")))
                .transpose()?
        } else if u.idempotency == Idempotency::Cursor {
            None
        } else {
            let stored: Option<(Vec<u8>, Vec<u8>)> = c
                .query_row(
                    "SELECT digest,result FROM uow_applied WHERE domain=? AND key=?",
                    params![d, identity],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .optional()
                .map_err(sql_error)?;
            stored
                .map(|(digest, result)| {
                    Ok(Receipt {
                        digest: RequestDigest(
                            digest
                                .try_into()
                                .map_err(|_| invalid("invalid stored digest"))?,
                        ),
                        result: serde_json::from_slice(&result)
                            .map_err(|_| invalid("invalid stored result"))?,
                    })
                })
                .transpose()?
        };
        if let Some(receipt) = receipt {
            if receipt.digest != u.digest {
                return Err(CommitError::IdempotencyMismatch);
            }
            let mut result = receipt.result;
            result.replayed = true;
            return Ok(result);
        }
        let mut failures = Vec::new();
        for p in &u.preconditions {
            #[cfg(feature = "conformance")]
            let _access = self.access_precondition(c, p, u.domain);
            let (ok, actual_rev, actual_position) = match p {
                Precondition::Cursor { name, at } => {
                    let old = cursor(c, u.domain, name)?;
                    (old == *at, None, old)
                }
                Precondition::Rev { entity, key, .. }
                | Precondition::Absent { entity, key }
                | Precondition::Present { entity, key } => {
                    let old = self
                        .registry
                        .entity(entity, u.domain)?
                        .get(c, key)?
                        .map(|(_, r)| r);
                    let ok = match p {
                        Precondition::Rev { rev, .. } => old == Some(*rev),
                        Precondition::Absent { .. } => old.is_none(),
                        _ => old.is_some(),
                    };
                    (ok, old, None)
                }
            };
            if !ok {
                failures.push(FailedPrecondition {
                    condition: p.clone(),
                    actual_rev,
                    actual_position,
                });
            }
        }
        if !failures.is_empty() {
            return Err(CommitError::Conflict(failures));
        }
        let mut seqs = Vec::new();
        let mut touched = std::collections::BTreeSet::new();
        for mutation in &u.mutations {
            #[cfg(feature = "conformance")]
            let _access = self.access_mutation(c, mutation, u.domain);
            match mutation {
                Mutation::Insert { entity, row } | Mutation::Upsert { entity, row } => {
                    let e = self.registry.entity(entity, u.domain)?;
                    if matches!(e.desc.kind, EntityKind::Append { .. }) {
                        return Err(invalid("append entities require Append"));
                    }
                    e.insert(c, row, rev, matches!(mutation, Mutation::Upsert { .. }))?;
                }
                Mutation::Update { entity, key, set } => {
                    let e = self.registry.entity(entity, u.domain)?;
                    if matches!(e.desc.kind, EntityKind::Append { .. }) {
                        return Err(invalid("append entities are immutable"));
                    }
                    let expected = u.preconditions.iter().find_map(|p| match p {
                        Precondition::Rev {
                            entity: n,
                            key: k,
                            rev,
                        } if n == entity && k == key => Some(*rev),
                        _ => None,
                    });
                    // Preconditions describe the initial state. Later mutations in this
                    // same unit may already have replaced its revision (or deleted it).
                    let expected = expected.filter(|_| !touched.contains(entity));
                    let changed = e.update(c, key, set, rev, expected)?;
                    if let Some(expected) = expected
                        && changed != 1
                    {
                        return Err(CommitError::Conflict(vec![FailedPrecondition {
                            condition: Precondition::Rev {
                                entity: entity.clone(),
                                key: key.clone(),
                                rev: expected,
                            },
                            actual_rev: e.get(c, key)?.map(|(_, r)| r),
                            actual_position: None,
                        }]));
                    }
                }
                Mutation::Delete { entity, key } => {
                    self.registry.entity(entity, u.domain)?.delete(c, key)?
                }
                Mutation::DeleteWhere {
                    entity,
                    index,
                    range,
                    filter,
                    limit,
                } => self
                    .registry
                    .entity(entity, u.domain)?
                    .delete_where(c, index, range, filter, *limit)?,
                Mutation::Append { stream, records } => {
                    let e = self.registry.entity(&stream.0, u.domain)?;
                    let EntityKind::Append { seq, .. } = &e.desc.kind else {
                        return Err(invalid("not an append stream"));
                    };
                    let mut allocated = Vec::new();
                    for record in records {
                        if value(record, seq).is_some() {
                            return Err(invalid("sequence is backend allocated"));
                        }
                        let n = next_sequence(c, u.domain, &format!("seq:{}", stream.0))?;
                        let mut row = record.clone();
                        row.0.push((seq.clone(), Cell::Int(n)));
                        e.insert(c, &row, rev, false)?;
                        allocated.push(Seq(n));
                    }
                    seqs.push((stream.clone(), allocated));
                }
                Mutation::AdvanceCursor { name, to } => {
                    advance(c, u.domain, name, to, self.clock.now())?
                }
                Mutation::Op(o) => self.registry.operate(c, o, rev)?,
            }
            match mutation {
                Mutation::Insert { entity, .. }
                | Mutation::Upsert { entity, .. }
                | Mutation::Update { entity, .. }
                | Mutation::Delete { entity, .. }
                | Mutation::DeleteWhere { entity, .. } => {
                    touched.insert(entity.clone());
                }
                Mutation::Op(_) => {
                    touched.extend(self.registry.entities().map(|e| e.desc.name.clone()))
                }
                _ => {}
            }
        }
        for f in &u.facts {
            self.registry.fact(c, u.domain, f, self.clock.now())?;
        }
        let result = Committed {
            rev,
            seqs,
            replayed: false,
        };
        if let Some(key) = natural {
            // One durable receipt per logical unique key, independent of delivery IDs.
            // It survives deletion of the row, as the reference backend requires, and
            // is not pruned along with the time-windowed recorded intent ledger.
            let receipt = serde_json::to_string(&Receipt {
                digest: u.digest,
                result: result.clone(),
            })
            .map_err(|_| invalid("invalid natural receipt"))?;
            c.execute(
                "INSERT INTO meta(domain,key,value) VALUES(?,?,?)",
                params![d, key, receipt],
            )
            .map_err(sql_error)?;
        } else if u.idempotency != Idempotency::Cursor {
            c.execute("INSERT INTO uow_applied(domain,key,issued_at,digest,rev,result) VALUES(?,?,?,?,?,?)",params![d,identity,u.id.issued_at.0,u.digest.0.as_slice(),rev.0 as i64,serde_json::to_vec(&result).map_err(|_|invalid("invalid committed result"))?]).map_err(sql_error)?;
        }
        Ok(result)
    }
}
impl<E: Send + 'static> BatchRunner<Connection> for SqliteBatchRunner<E> {
    type Job = Attached<E>;
    type Output = Committed;
    fn run(&self, c: &Connection, jobs: Vec<Attached<E>>) -> Vec<Result<Committed, CommitError>> {
        if jobs.is_empty() {
            return vec![];
        }
        let n = jobs.len();
        let run = || -> Result<Vec<Result<Committed, CommitError>>, CommitError> {
            #[cfg(feature = "conformance")]
            let _access = crate::exec::access_log::Operation::on(c, "commit_batch", "contract");
            let capture =
                crate::exec::watch::Capture::begin(c, &self.watches).map_err(sql_error)?;
            let revisions = self.revisions.issue(c, n)?;
            let tx =
                Transaction::new_unchecked(c, TransactionBehavior::Immediate).map_err(sql_error)?;
            let results = jobs
                .iter()
                .zip(revisions)
                .map(|(j, rev)| self.isolated(&tx, &j.unit, rev))
                .collect::<Vec<_>>();
            tx.commit().map_err(sql_error)?;
            // If every UOW rolled back its savepoint (or replayed), no watched
            // mutation committed. Keep those conservative savepoint hints private.
            if results
                .iter()
                .any(|r| r.as_ref().is_ok_and(|done| !done.replayed))
            {
                capture.finish();
            }
            Ok(results)
        };
        let results = match catch_unwind(AssertUnwindSafe(run)) {
            Ok(Ok(r)) => r,
            Ok(Err(e)) => return vec![Err(e); n],
            Err(_) => {
                return vec![
                    Err(domains::failure(
                        steve_types::Code::Internal,
                        "batch panicked; transaction rolled back"
                    ));
                    n
                ];
            }
        };
        for (job, result) in jobs.into_iter().zip(&results) {
            if let Ok(done) = result
                && !done.replayed
            {
                // A notification callback cannot change a durable commit's outcome.
                if catch_unwind(AssertUnwindSafe(|| {
                    self.publisher.publish(done.rev, job.events)
                }))
                .is_err()
                {
                    tracing::error!("storage publisher panicked after commit");
                }
            }
        }
        results
    }
}
pub fn apply_in<E: Send + 'static>(
    runner: &SqliteBatchRunner<E>,
    tx: &Connection,
    uow: Attached<E>,
    root: &RootRevision,
) -> Result<Applied<E>, CommitError> {
    runner.apply_in(tx, uow, root)
}
pub fn cursor(
    c: &Connection,
    domain: Domain,
    name: &CursorName,
) -> Result<Option<Position>, CommitError> {
    let s: Option<String> = c
        .query_row(
            "SELECT key FROM cursor WHERE domain=? AND name=?",
            params![domain_name(domain), name.0],
            |r| r.get(0),
        )
        .optional()
        .map_err(sql_error)?;
    s.map(|s| serde_json::from_str(&s).map_err(|_| invalid("invalid stored cursor")))
        .transpose()
}
fn same_shape(a: &CursorKey, b: &CursorKey) -> bool {
    match (a, b) {
        (CursorKey::Int(_), CursorKey::Int(_)) | (CursorKey::Text(_), CursorKey::Text(_)) => true,
        (CursorKey::Composite(a), CursorKey::Composite(b)) => {
            a.len() == b.len() && a.iter().zip(b).all(|(a, b)| same_shape(a, b))
        }
        _ => false,
    }
}
fn advance(
    c: &Connection,
    d: Domain,
    name: &CursorName,
    to: &Position,
    now: Millis,
) -> Result<(), CommitError> {
    if cursor(c, d, name)?.is_some_and(|old| !same_shape(&old.0, &to.0) || *to < old) {
        return Err(invalid("cursor cannot move backwards or change shape"));
    }
    c.execute("INSERT INTO cursor(domain,name,key,updated) VALUES(?,?,?,?) ON CONFLICT(domain,name) DO UPDATE SET key=excluded.key,updated=excluded.updated",params![domain_name(d),name.0,serde_json::to_string(to).map_err(|_|invalid("invalid cursor"))?,now.0]).map_err(sql_error)?;
    Ok(())
}
