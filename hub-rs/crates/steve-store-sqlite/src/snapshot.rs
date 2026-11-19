//! Single-domain reads and hard-deadline leases. Each lease's owner thread alone uses,
//! rolls back and returns its pooled connection; the reaper only interrupts and signals.
use crate::{
    engine::Engine,
    open::db_error,
    scan::{self, invalid},
};
use r2d2::PooledConnection;
use r2d2_sqlite::SqliteConnectionManager;
use rusqlite::Connection;
use std::{
    any::Any,
    collections::BTreeMap,
    panic::{AssertUnwindSafe, catch_unwind},
    sync::{
        Arc, Condvar, Mutex,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use steve_storage_api::{
    AnyOutput, AnyQuery, Domain, EntityDesc, Error, Lane, LeaseId, Millis, error,
};
use steve_types::Code;

pub const LEASE_MILLIS: i64 = 2_000;
type Reader = PooledConnection<SqliteConnectionManager>;
/// Own before BEGIN as well as while spawning: setup/spawn failures must roll back too.
struct OwnedReader(Option<Reader>);
impl OwnedReader {
    fn release(&mut self) {
        if let Some(c) = self.0.take() {
            if !c.is_autocommit() {
                let _ = c.execute_batch("ROLLBACK");
            }
            drop(c);
        }
    }
}
impl Drop for OwnedReader {
    fn drop(&mut self) {
        self.release();
    }
}
fn stale() -> Error {
    error(Code::Stale, "unknown or expired snapshot")
}
fn internal() -> Error {
    error(Code::Internal, "snapshot worker failed")
}
fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}
/// Injectable clock. Implementations must be monotonic and nonblocking. Test clocks
/// advance explicitly, then call reap_expired to deterministically synchronize cleanup.
pub trait Clock: Send + Sync + 'static {
    fn now(&self) -> Millis;
}
/// Wall-clock origin plus monotonic elapsed time prevents a backwards clock adjustment
/// from extending the hard lease lifetime.
pub struct SystemClock {
    epoch: Millis,
    start: Instant,
}
impl Default for SystemClock {
    fn default() -> Self {
        Self {
            epoch: Millis(
                SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_millis()
                    .min(i64::MAX as u128) as i64,
            ),
            start: Instant::now(),
        }
    }
}
impl Clock for SystemClock {
    fn now(&self) -> Millis {
        Millis(
            self.epoch
                .0
                .saturating_add(self.start.elapsed().as_millis().min(i64::MAX as u128) as i64),
        )
    }
}
/// Pin all databases on this connection immediately: BEGIN alone does not establish
/// SQLite's deferred read snapshot until the first read of each physical file.
pub(crate) fn pin(conn: &Connection) -> Result<(), Error> {
    let names = conn
        .prepare("PRAGMA database_list")
        .map_err(db_error)?
        .query_map([], |r| r.get::<_, String>(1))
        .map_err(db_error)?
        .collect::<rusqlite::Result<Vec<_>>>()
        .map_err(db_error)?;
    for name in names {
        conn.query_row(
            &format!("SELECT count(*) FROM {}.sqlite_schema", scan::ident(&name)),
            [],
            |r| r.get::<_, i64>(0),
        )
        .map_err(db_error)?;
    }
    Ok(())
}
/// One pooled connection and one deferred transaction. Domain and body checks are
/// delegated to the descriptor-driven reader; errors roll back before pool return.
pub fn read_batch(
    engine: &Engine,
    domain: Domain,
    queries: Vec<(EntityDesc, AnyQuery)>,
) -> Result<Vec<AnyOutput>, Error> {
    read_batch_with(engine, domain, queries, scan::query)
}
/// Dispatch seam for typed codecs and registered named queries; all dispatches share
/// this transaction and must validate their decoded body against the resolved entity.
pub fn read_batch_with(
    engine: &Engine,
    domain: Domain,
    queries: Vec<(EntityDesc, AnyQuery)>,
    mut run: impl FnMut(&Connection, &EntityDesc, AnyQuery) -> Result<AnyOutput, Error>,
) -> Result<Vec<AnyOutput>, Error> {
    if queries
        .iter()
        .any(|(e, q)| e.domain != domain || q.domain != domain)
    {
        return Err(invalid("mixed-domain snapshot"));
    }
    let conn = engine.read_conn()?;
    let tx = conn.unchecked_transaction().map_err(db_error)?;
    pin(&tx)?;
    let output = queries
        .into_iter()
        .map(|(e, q)| run(&tx, &e, q))
        .collect::<Result<_, _>>()?;
    tx.commit().map_err(db_error)?;
    Ok(output)
}
type Output = Box<dyn Any + Send>;
type Work = Box<dyn FnOnce(&Connection) -> Result<Output, Error> + Send>;
enum Command {
    Run(Work, mpsc::SyncSender<Result<Output, Error>>),
    Stop,
}
struct Entry {
    lease: LeaseId,
    domain: Domain,
    expired: Arc<AtomicBool>,
    interrupt: rusqlite::InterruptHandle,
    sender: mpsc::Sender<Command>,
    finished: Arc<(Mutex<bool>, Condvar)>,
    worker: Mutex<Option<JoinHandle<()>>>,
}
impl Entry {
    fn stop(&self) {
        if !self.expired.swap(true, Ordering::SeqCst) {
            self.interrupt.interrupt();
            let _ = self.sender.send(Command::Stop);
        }
    }
    fn wait(&self) {
        let (m, cv) = &*self.finished;
        let mut done = lock(m);
        while !*done {
            done = cv.wait(done).unwrap_or_else(|e| e.into_inner());
        }
    }
}
struct Shared {
    entries: Mutex<BTreeMap<String, Arc<Entry>>>,
    clock: Arc<dyn Clock>,
    stopping: Mutex<bool>,
    wake: Condvar,
}
impl Shared {
    fn expire_due(&self) -> Vec<Arc<Entry>> {
        let now = self.clock.now();
        let mut entries = lock(&self.entries);
        let mut expired = Vec::new();
        entries.retain(|_, e| {
            if e.lease.expires_at.0 <= now.0 || e.expired.load(Ordering::SeqCst) {
                e.stop();
                expired.push(e.clone());
                !*lock(&e.finished.0)
            } else {
                true
            }
        });
        expired
    }
}
/// Blocking facade for store.rs (call from its blocking task). Queries are serialized
/// on a lease-owner thread. A separate reaper can interrupt an actively running query.
pub struct LeaseManager {
    engine: Arc<Engine>,
    shared: Arc<Shared>,
    reaper: Option<JoinHandle<()>>,
}
impl LeaseManager {
    pub fn new(engine: Arc<Engine>) -> Result<Self, Error> {
        Self::with_clock(engine, Arc::new(SystemClock::default()))
    }
    pub fn with_clock(engine: Arc<Engine>, clock: Arc<dyn Clock>) -> Result<Self, Error> {
        let shared = Arc::new(Shared {
            entries: Mutex::new(BTreeMap::new()),
            clock,
            stopping: Mutex::new(false),
            wake: Condvar::new(),
        });
        let state = shared.clone();
        let reaper = thread::Builder::new()
            .name("sqlite-lease-reaper".into())
            .spawn(move || {
                loop {
                    state.expire_due();
                    let wait = lock(&state.entries)
                        .values()
                        .filter(|e| !e.expired.load(Ordering::SeqCst))
                        .map(|e| {
                            e.lease
                                .expires_at
                                .0
                                .saturating_sub(state.clock.now().0)
                                .max(0) as u64
                        })
                        .min()
                        .map(Duration::from_millis)
                        .unwrap_or(Duration::from_secs(2));
                    let stop = lock(&state.stopping);
                    if *stop {
                        break;
                    }
                    let (stop, _) = state
                        .wake
                        .wait_timeout(stop, wait)
                        .unwrap_or_else(|e| e.into_inner());
                    if *stop {
                        break;
                    }
                }
            })
            .map_err(|_| internal())?;
        Ok(Self {
            engine,
            shared,
            reaper: Some(reaper),
        })
    }
    pub fn begin(&self, domain: Domain, _lane: Lane) -> Result<LeaseId, Error> {
        let conn = OwnedReader(Some(self.engine.read_conn()?));
        let now = self.shared.clock.now();
        let lease = LeaseId {
            id: uuid::Uuid::new_v4().to_string(),
            expires_at: Millis(
                now.0
                    .checked_add(LEASE_MILLIS)
                    .ok_or_else(|| invalid("clock overflow"))?,
            ),
        };
        let connection = conn.0.as_ref().expect("new owner retains connection");
        connection
            .execute_batch("BEGIN DEFERRED")
            .map_err(db_error)?;
        pin(connection)?;
        let (sender, receiver) = mpsc::channel();
        let expired = Arc::new(AtomicBool::new(false));
        let finished = Arc::new((Mutex::new(false), Condvar::new()));
        let interrupt = connection.get_interrupt_handle();
        let worker_expired = expired.clone();
        let worker_finished = finished.clone();
        let clock = self.shared.clock.clone();
        let expires = lease.expires_at;
        let worker = thread::Builder::new()
            .name("sqlite-snapshot".into())
            .spawn(move || {
                owner(
                    conn,
                    receiver,
                    worker_expired,
                    worker_finished,
                    clock,
                    expires,
                )
            })
            .map_err(|_| internal())?;
        let entry = Arc::new(Entry {
            lease: lease.clone(),
            domain,
            expired,
            interrupt,
            sender,
            finished,
            worker: Mutex::new(Some(worker)),
        });
        lock(&self.shared.entries).insert(lease.id.clone(), entry);
        self.shared.wake.notify_one();
        Ok(lease)
    }
    pub fn query(&self, lease: &LeaseId, e: EntityDesc, q: AnyQuery) -> Result<AnyOutput, Error> {
        let domain = q.domain;
        self.with_connection(lease, domain, move |conn| scan::query(conn, &e, q))
    }
    /// Named-query seam: the closure stays on the owner thread, never receives a mutable
    /// connection, and cannot outlive the lease. It must execute SQLite reads only.
    pub fn with_connection<T: Send + 'static>(
        &self,
        lease: &LeaseId,
        domain: Domain,
        read: impl FnOnce(&Connection) -> Result<T, Error> + Send + 'static,
    ) -> Result<T, Error> {
        self.shared.expire_due();
        let entry = lock(&self.shared.entries)
            .get(&lease.id)
            .filter(|e| e.lease == *lease && !e.expired.load(Ordering::SeqCst))
            .cloned()
            .ok_or_else(stale)?;
        if domain != entry.domain {
            return Err(invalid("mixed-domain lease"));
        }
        let (send, receive) = mpsc::sync_channel(1);
        entry
            .sender
            .send(Command::Run(
                Box::new(move |c| read(c).map(|v| Box::new(v) as Output)),
                send,
            ))
            .map_err(|_| stale())?;
        let value = receive.recv().map_err(|_| stale())??;
        value.downcast::<T>().map(|v| *v).map_err(|_| internal())
    }
    pub fn end(&self, lease: &LeaseId) -> Result<(), Error> {
        let mut entries = lock(&self.shared.entries);
        let entry = entries
            .get(&lease.id)
            .filter(|e| e.lease == *lease)
            .cloned()
            .ok_or_else(stale)?;
        entries.remove(&lease.id);
        drop(entries);
        let expired = self.shared.clock.now().0 >= lease.expires_at.0;
        entry.stop();
        entry.wait();
        if expired { Err(stale()) } else { Ok(()) }
    }
    /// Deterministic virtual-clock seam: interrupt due leases and wait until every
    /// affected owner has exited its statement, rolled back and returned its connection.
    pub fn reap_expired(&self) -> usize {
        let expired = self.shared.expire_due();
        for e in &expired {
            e.wait();
        }
        expired.len()
    }
}
fn owner(
    mut owner: OwnedReader,
    receiver: mpsc::Receiver<Command>,
    expired: Arc<AtomicBool>,
    finished: Arc<(Mutex<bool>, Condvar)>,
    clock: Arc<dyn Clock>,
    expires: Millis,
) {
    while let Ok(command) = receiver.recv() {
        let Command::Run(work, reply) = command else {
            break;
        };
        if expired.load(Ordering::SeqCst) || clock.now().0 >= expires.0 {
            owner.release();
            let _ = reply.send(Err(stale()));
            break;
        }
        let result = catch_unwind(AssertUnwindSafe(|| {
            work(owner.0.as_ref().expect("owner retains connection"))
        }))
        .unwrap_or_else(|_| Err(internal()));
        if expired.load(Ordering::SeqCst) || clock.now().0 >= expires.0 {
            expired.store(true, Ordering::SeqCst);
            owner.release();
            let _ = reply.send(Err(stale()));
            break;
        }
        let _ = reply.send(result);
    }
    drop(owner);
    let (m, cv) = &*finished;
    *lock(m) = true;
    cv.notify_all();
}
impl Drop for LeaseManager {
    fn drop(&mut self) {
        *lock(&self.shared.stopping) = true;
        self.shared.wake.notify_one();
        if let Some(reaper) = self.reaper.take() {
            let _ = reaper.join();
        }
        let entries = std::mem::take(&mut *lock(&self.shared.entries));
        for e in entries.values() {
            e.stop();
        }
        for e in entries.values() {
            e.wait();
            if let Some(worker) = lock(&e.worker).take() {
                let _ = worker.join();
            }
        }
    }
}
