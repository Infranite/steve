//! One writer, four FIFO lanes, and caller-thread root grants sharing the same scheduler.
//! The connection moves between the scheduler and a thread-local Rc lease. No Send/Sync
//! implementation or connection pointer is needed, even for a Send + !Sync SQLite connection.
#[cfg(feature = "conformance")]
#[path = "access_log.rs"]
pub mod access_log;
#[cfg(test)]
mod maintenance_tests;
mod schedule;
pub(crate) mod watch;
pub use schedule::{Head, LANES, Lane, Scheduler, index, name, target};

use std::any::Any;
use std::cell::RefCell;
use std::collections::{HashMap, VecDeque};
use std::ops::Deref;
use std::panic::{AssertUnwindSafe, Location, catch_unwind};
use std::rc::{Rc, Weak};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::time::{Duration, Instant};
use steve_storage_api::uow::CommitError;
use tokio::sync::oneshot;

pub const QUEUE_LIMIT: usize = 1024;
pub const INGEST_BATCH: usize = 128;
pub const SLICE_TARGET: Duration = Duration::from_millis(20);
static NEXT_ENGINE: AtomicU64 = AtomicU64::new(1);

/// Monotonic time since an arbitrary origin; implementations must not block.
pub trait Clock: Send + Sync + 'static {
    fn now(&self) -> Duration;
}
pub struct SystemClock(Instant);
impl Default for SystemClock {
    fn default() -> Self {
        Self(Instant::now())
    }
}
impl Clock for SystemClock {
    fn now(&self) -> Duration {
        self.0.elapsed()
    }
}

/// Implementations commit (and hand off events in commit order) before returning outcomes.
/// Exactly one result per input is required, in input order. A unit failure must be isolated
/// inside the batch by the runner. This executor never starts a transaction itself.
pub trait BatchRunner<C>: Send + Sync + 'static {
    type Job: Send + 'static;
    type Output: Send + 'static;
    fn run(&self, conn: &C, jobs: Vec<Self::Job>) -> Vec<Result<Self::Output, CommitError>>;
    /// Runs exclusively at writer gaps, including idle clock deadlines. Return the
    /// next monotonic deadline, strictly after now. Never acquire another grant here.
    /// idle is true only on a due maintenance deadline with no queued work; a
    /// newly returned writer alone never permits truncation. The default does no
    /// maintenance and schedules no idle wakeups.
    fn maintenance(&self, _conn: &C, _now: Duration, _idle: bool) -> Option<Duration> {
        None
    }
}
pub type Completion<T> = oneshot::Receiver<Result<T, CommitError>>;
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Step {
    More,
    Done,
}
#[derive(Debug, thiserror::Error)]
pub enum GrantError {
    #[error("cross-engine write: {outer} -> {inner} at {site}")]
    CrossFile {
        outer: u64,
        inner: u64,
        site: String,
    },
    #[error("executor has stopped")]
    Closed,
}

type Slice<C> = Box<dyn FnMut(&C) -> Step + Send>;
enum Work<C, R: BatchRunner<C>> {
    Grant(u64),
    Unit(R::Job, oneshot::Sender<Result<R::Output, CommitError>>),
    Slice(Slice<C>, oneshot::Sender<Result<(), CommitError>>),
}
struct Queued<C, R: BatchRunner<C>> {
    enqueued: Duration,
    site: &'static Location<'static>,
    work: Work<C, R>,
}
struct State<C, R: BatchRunner<C>> {
    connection: Option<C>,
    busy: bool,
    reserved: Option<u64>,
    closing: bool,
    next_ticket: u64,
    maintenance_requested: bool,
    maintenance_at: Option<Duration>,
    // Test handshake: the worker has observed an active writer and will wait.
    #[cfg(test)]
    busy_waits: usize,
    queues: [VecDeque<Queued<C, R>>; 4],
    admitted: [usize; 4], // queued units and slices; a yielding slice keeps its admission slot
    scheduler: Scheduler,
    warnings: HashMap<(&'static str, &'static Location<'static>), Duration>,
}
struct Shared<C, R: BatchRunner<C>> {
    id: u64,
    file: String,
    clock: Arc<dyn Clock>,
    runner: R,
    runtime_probe: fn() -> bool,
    state: Mutex<State<C, R>>,
    changed: Condvar,
}
fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|e| e.into_inner())
}
struct Handle<C, R: BatchRunner<C>>(Arc<Shared<C, R>>);
impl<C, R: BatchRunner<C>> Drop for Handle<C, R> {
    fn drop(&mut self) {
        lock(&self.0.state).closing = true;
        self.0.changed.notify_all();
    }
}
/// Clone this handle for every facade of the same Engine. File labels do not establish identity.
/// Dropping the last handle cancels queued work; active grants remain valid until their last drop.
pub struct Executor<C: Send + 'static, R: BatchRunner<C>> {
    handle: Arc<Handle<C, R>>,
}
impl<C: Send + 'static, R: BatchRunner<C>> Clone for Executor<C, R> {
    fn clone(&self) -> Self {
        Self {
            handle: self.handle.clone(),
        }
    }
}
thread_local! { static ACTIVE: RefCell<Option<(u64, Weak<dyn Any>)>> = const { RefCell::new(None) }; }

/// Thread-local writer ownership, for callers such as backup's InsideWrite preflight.
pub fn current_engine() -> Option<u64> {
    ACTIVE.with(|v| v.borrow().as_ref().map(|(id, _)| *id))
}

struct Lease<C: Send + 'static, R: BatchRunner<C>> {
    shared: Arc<Shared<C, R>>,
    connection: Option<C>,
    lane: Lane,
    started: Duration,
    hold_metric: metrics::Histogram,
    long_hold_metric: metrics::Counter,
    site: &'static Location<'static>,
}
impl<C: Send + 'static, R: BatchRunner<C>> Lease<C, R> {
    fn enter(
        shared: Arc<Shared<C, R>>,
        connection: C,
        lane: Lane,
        site: &'static Location<'static>,
    ) -> Rc<Self> {
        let hold_metric = metrics::histogram!("steve_store_hold_seconds", "file"=>shared.file.clone(), "lane"=>name(lane));
        let long_hold_metric =
            metrics::counter!("steve_store_long_hold_total", "site"=>site.to_string());
        let lease = Rc::new(Self {
            started: shared.clock.now(),
            hold_metric,
            long_hold_metric,
            shared,
            connection: Some(connection),
            lane,
            site,
        });
        let erased: Rc<dyn Any> = lease.clone();
        ACTIVE.with(|v| *v.borrow_mut() = Some((lease.shared.id, Rc::downgrade(&erased))));
        lease
    }
}
impl<C: Send + 'static, R: BatchRunner<C>> Drop for Lease<C, R> {
    fn drop(&mut self) {
        // A caller may store its grant in TLS. ACTIVE can already have been destroyed
        // during thread teardown; the connection must still return to the executor.
        let _ = ACTIVE.try_with(|v| *v.borrow_mut() = None);
        let elapsed = self.shared.clock.now().saturating_sub(self.started);
        // Cached handles avoid consulting metrics' thread-local recorder during TLS teardown.
        self.hold_metric.record(elapsed.as_secs_f64());
        if elapsed > Duration::from_millis(50) {
            self.long_hold_metric.increment(1);
            self.shared.warn("long_hold", self.site);
        }
        let mut s = lock(&self.shared.state);
        s.scheduler.charge(self.lane, elapsed);
        s.connection = self.connection.take();
        s.busy = false;
        s.maintenance_requested = true;
        drop(s);
        self.shared.changed.notify_all();
    }
}
/// Thread-bound; may be dropped in any nesting order. Only the last lease releases the writer.
/// Nested marks the caller's need for a SAVEPOINT; the executor never creates one.
/// Keep the grant in synchronous code: thread ownership must not span an await.
pub struct RootGrant<C: Send + 'static, R: BatchRunner<C>> {
    lease: Rc<Lease<C, R>>,
    nested: bool,
}
impl<C: Send + 'static, R: BatchRunner<C>> RootGrant<C, R> {
    pub fn is_nested(&self) -> bool {
        self.nested
    }
    pub fn connection(&self) -> &C {
        self.lease.connection.as_ref().unwrap()
    }
}
impl<C: Send + 'static, R: BatchRunner<C>> Deref for RootGrant<C, R> {
    type Target = C;
    fn deref(&self) -> &C {
        self.connection()
    }
}
impl<C, R: BatchRunner<C>> Shared<C, R> {
    fn depth(&self, s: &State<C, R>, lane: Lane) {
        metrics::gauge!("steve_store_queue_depth", "file"=>self.file.clone(), "lane"=>name(lane))
            .set(s.queues[index(lane)].len() as f64);
    }
    fn wait(&self, lane: Lane, enqueued: Duration) {
        metrics::histogram!("steve_store_wait_seconds", "file"=>self.file.clone(), "lane"=>name(lane)).record(self.clock.now().saturating_sub(enqueued).as_secs_f64());
    }
    fn reject(&self, lane: Lane, reason: &'static str) {
        metrics::counter!("steve_store_rejected_total", "lane"=>name(lane), "reason"=>reason)
            .increment(1);
    }
    fn warn(&self, reason: &'static str, site: &'static Location<'static>) {
        let now = self.clock.now();
        let mut s = lock(&self.state);
        let key = (reason, site);
        if s.warnings
            .get(&key)
            .is_some_and(|&last| now.saturating_sub(last) < Duration::from_secs(60))
        {
            return;
        }
        s.warnings.insert(key, now);
        drop(s);
        tracing::warn!(file=%self.file, %site, reason, "storage writer scheduling warning");
    }
}
impl<C: Send + 'static, R: BatchRunner<C>> Executor<C, R> {
    /// `runtime_probe` identifies blocking calls made in a runtime's worker context. The Hub
    /// installs its adapter; synchronous hosts pass `|| false`. No Tokio runtime is required.
    pub fn new(
        file: impl Into<String>,
        connection: C,
        clock: Arc<dyn Clock>,
        runner: R,
        runtime_probe: fn() -> bool,
    ) -> std::io::Result<Self> {
        let shared = Arc::new(Shared {
            id: NEXT_ENGINE.fetch_add(1, Ordering::Relaxed),
            file: file.into(),
            clock,
            runner,
            runtime_probe,
            state: Mutex::new(State {
                connection: Some(connection),
                busy: false,
                reserved: None,
                closing: false,
                next_ticket: 0,
                maintenance_requested: true,
                maintenance_at: None,
                #[cfg(test)]
                busy_waits: 0,
                queues: std::array::from_fn(|_| VecDeque::new()),
                admitted: [0; 4],
                scheduler: Scheduler::default(),
                warnings: HashMap::new(),
            }),
            changed: Condvar::new(),
        });
        let worker = shared.clone();
        std::thread::Builder::new()
            .name("storage-executor".into())
            .spawn(move || worker_loop(worker))?;
        Ok(Self {
            handle: Arc::new(Handle(shared)),
        })
    }
    fn shared(&self) -> &Arc<Shared<C, R>> {
        &self.handle.0
    }
    pub fn id(&self) -> u64 {
        self.shared().id
    }
    /// Also wake after advancing an injected virtual clock. Production timeouts wake themselves.
    pub fn wake(&self) {
        // Serialize with the worker's predicate check / Condvar wait to avoid a lost
        // notification when an injected clock advances immediately before the wait.
        let _state = lock(&self.shared().state);
        self.shared().changed.notify_all();
    }
    pub fn queue_depth(&self, lane: Lane) -> usize {
        lock(&self.shared().state).queues[index(lane)].len()
    }
    #[track_caller]
    pub fn grant(&self, lane: Lane) -> Result<RootGrant<C, R>, GrantError> {
        let shared = self.shared();
        let site = Location::caller();
        // The host supplies the runtime-context probe: this crate only enables Tokio sync,
        // and must not require a runtime to perform blocking grants or run the writer.
        if (shared.runtime_probe)() {
            metrics::counter!("steve_store_tokio_grant_total", "site"=>site.to_string())
                .increment(1);
            shared.warn("tokio_grant", site);
        }
        let active = ACTIVE.with(|v| v.borrow().clone());
        if let Some((outer, weak)) = active {
            if outer != shared.id {
                shared.reject(lane, "cross_file");
                return Err(GrantError::CrossFile {
                    outer,
                    inner: shared.id,
                    site: site.to_string(),
                });
            }
            let lease = weak
                .upgrade()
                .unwrap()
                .downcast::<Lease<C, R>>()
                .ok()
                .unwrap();
            return Ok(RootGrant {
                lease,
                nested: true,
            });
        }
        let enqueued = shared.clock.now();
        let mut s = lock(&shared.state);
        if s.closing {
            return Err(GrantError::Closed);
        }
        let ticket = s.next_ticket;
        s.next_ticket += 1;
        s.queues[index(lane)].push_back(Queued {
            enqueued,
            site,
            work: Work::Grant(ticket),
        });
        shared.depth(&s, lane);
        shared.changed.notify_all();
        loop {
            if s.reserved == Some(ticket) {
                s.reserved = None;
                let c = s.connection.take().unwrap();
                drop(s);
                shared.wait(lane, enqueued);
                return Ok(RootGrant {
                    lease: Lease::enter(shared.clone(), c, lane, site),
                    nested: false,
                });
            }
            if s.closing {
                return Err(GrantError::Closed);
            }
            s = shared.changed.wait(s).unwrap_or_else(|e| e.into_inner());
        }
    }
    #[track_caller]
    pub fn submit(&self, lane: Lane, job: R::Job) -> Result<Completion<R::Output>, CommitError> {
        let (tx, rx) = oneshot::channel();
        self.enqueue(lane, Work::Unit(job, tx), Location::caller())?;
        Ok(rx)
    }
    #[track_caller]
    pub fn slice(
        &self,
        job: impl FnMut(&C) -> Step + Send + 'static,
    ) -> Result<Completion<()>, CommitError> {
        let (tx, rx) = oneshot::channel();
        self.enqueue(
            Lane::Background,
            Work::Slice(Box::new(job), tx),
            Location::caller(),
        )?;
        Ok(rx)
    }
    fn enqueue(
        &self,
        lane: Lane,
        work: Work<C, R>,
        site: &'static Location<'static>,
    ) -> Result<(), CommitError> {
        let shared = self.shared();
        if ACTIVE.with(|v| v.borrow().as_ref().is_some_and(|(id, _)| *id == shared.id)) {
            shared.reject(lane, "uow_inside_grant");
            return Err(CommitError::UowInsideGrant);
        }
        let mut s = lock(&shared.state);
        if s.closing || s.admitted[index(lane)] >= QUEUE_LIMIT {
            shared.reject(lane, if s.closing { "closed" } else { "queue_full" });
            return Err(CommitError::Retryable {
                after_ms: None,
                detail: "executor queue unavailable".into(),
            });
        }
        s.admitted[index(lane)] += 1;
        s.queues[index(lane)].push_back(Queued {
            enqueued: shared.clock.now(),
            site,
            work,
        });
        shared.depth(&s, lane);
        drop(s);
        shared.changed.notify_all();
        Ok(())
    }
}

fn heads<C, R: BatchRunner<C>>(s: &State<C, R>) -> [Option<Head>; 4] {
    std::array::from_fn(|i| {
        s.queues[i].front().map(|q| {
            let delayed = i == index(Lane::Ingest)
                && matches!(q.work, Work::Unit(..))
                && s.queues[i]
                    .iter()
                    .take_while(|q| matches!(q.work, Work::Unit(..)))
                    .take(INGEST_BATCH)
                    .count()
                    < INGEST_BATCH;
            Head {
                enqueued: q.enqueued,
                ready_at: if delayed {
                    q.enqueued.saturating_add(SLICE_TARGET)
                } else {
                    q.enqueued
                },
            }
        })
    })
}
fn fatal(detail: &str) -> CommitError {
    CommitError::Fatal(steve_storage_api::error(
        steve_types::Code::Internal,
        detail,
    ))
}
fn worker_loop<C: Send + 'static, R: BatchRunner<C>>(shared: Arc<Shared<C, R>>) {
    loop {
        let mut s = lock(&shared.state);
        if s.closing {
            let queued = std::mem::replace(&mut s.queues, std::array::from_fn(|_| VecDeque::new()));
            for lane in LANES {
                shared.depth(&s, lane);
            }
            drop(s);
            drop(queued);
            return;
        }
        if s.busy {
            #[cfg(test)]
            {
                s.busy_waits += 1;
                shared.changed.notify_all();
            }
            drop(shared.changed.wait(s).unwrap_or_else(|e| e.into_inner()));
            continue;
        }
        let now = shared.clock.now();
        let maintenance_due = s.maintenance_at.is_some_and(|at| at <= now);
        if s.maintenance_requested || maintenance_due {
            s.maintenance_requested = false;
            // A momentarily empty queue after a commit is not an idle deadline.
            let idle = maintenance_due && s.queues.iter().all(VecDeque::is_empty);
            let connection = s.connection.take().unwrap();
            s.busy = true;
            drop(s);
            let next = catch_unwind(AssertUnwindSafe(|| {
                shared.runner.maintenance(&connection, now, idle)
            }));
            let mut s = lock(&shared.state);
            s.connection = Some(connection);
            s.busy = false;
            s.scheduler
                .charge(Lane::Background, shared.clock.now().saturating_sub(now));
            s.maintenance_at = match next {
                Ok(next) => next.map(|at| at.max(now.saturating_add(Duration::from_millis(1)))),
                Err(_) => {
                    tracing::warn!(file=%shared.file, "storage maintenance panicked; retrying");
                    Some(now.saturating_add(crate::checkpoint::RETRY_INTERVAL))
                }
            };
            drop(s);
            shared.changed.notify_all();
            continue;
        }
        let heads = heads(&s);
        let Some(lane) = s.scheduler.choose(now, heads) else {
            let until = heads
                .iter()
                .flatten()
                .map(|h| h.ready_at)
                .filter(|&d| d > now)
                .chain(s.maintenance_at)
                .min();
            if let Some(deadline) = until {
                drop(
                    shared
                        .changed
                        .wait_timeout(s, deadline.saturating_sub(now))
                        .unwrap_or_else(|e| e.into_inner()),
                );
            } else {
                drop(shared.changed.wait(s).unwrap_or_else(|e| e.into_inner()));
            }
            continue;
        };
        let i = index(lane);
        let first = s.queues[i].pop_front().unwrap();
        s.busy = true;
        if let Work::Grant(ticket) = first.work {
            s.reserved = Some(ticket);
            shared.depth(&s, lane);
            drop(s);
            shared.changed.notify_all();
            continue;
        }
        let connection = s.connection.take().unwrap();
        let mut batch = vec![first];
        if matches!(batch[0].work, Work::Unit(..)) {
            let limit = match lane {
                Lane::Ingest => INGEST_BATCH,
                Lane::Interactive => QUEUE_LIMIT,
                _ => 1,
            };
            while batch.len() < limit
                && s.queues[i]
                    .front()
                    .is_some_and(|q| matches!(q.work, Work::Unit(..)))
            {
                batch.push(s.queues[i].pop_front().unwrap());
            }
            s.admitted[i] -= batch.len();
        }
        shared.depth(&s, lane);
        drop(s);
        for q in &batch {
            shared.wait(lane, q.enqueued);
        }
        let site = batch[0].site;
        let lease = Lease::enter(shared.clone(), connection, lane, site);
        if matches!(batch[0].work, Work::Unit(..)) {
            let (jobs, senders): (Vec<_>, Vec<_>) = batch
                .into_iter()
                .map(|q| match q.work {
                    Work::Unit(j, tx) => (j, tx),
                    _ => unreachable!(),
                })
                .unzip();
            let result = catch_unwind(AssertUnwindSafe(|| {
                shared.runner.run(lease.connection.as_ref().unwrap(), jobs)
            }));
            match result {
                Ok(outcomes) if outcomes.len() == senders.len() => {
                    for (tx, result) in senders.into_iter().zip(outcomes) {
                        let _ = tx.send(result);
                    }
                }
                _ => {
                    for tx in senders {
                        let _ = tx.send(Err(fatal(
                            "batch runner panicked or returned an invalid result count",
                        )));
                    }
                }
            }
        } else {
            let q = batch.pop().unwrap();
            let Work::Slice(mut job, tx) = q.work else {
                unreachable!()
            };
            let start = shared.clock.now();
            let step = catch_unwind(AssertUnwindSafe(|| job(lease.connection.as_ref().unwrap())));
            if shared.clock.now().saturating_sub(start) > SLICE_TARGET {
                metrics::counter!("steve_store_slice_overruns_total", "file"=>shared.file.clone())
                    .increment(1);
            }
            let mut s = lock(&shared.state);
            if matches!(step, Ok(Step::More)) && !s.closing {
                s.queues[i].push_back(Queued {
                    enqueued: shared.clock.now(),
                    site: q.site,
                    work: Work::Slice(job, tx),
                });
            } else {
                s.admitted[i] -= 1;
                let _ = tx.send(if matches!(step, Ok(Step::Done)) {
                    Ok(())
                } else {
                    Err(fatal("slice stopped or panicked"))
                });
            }
            shared.depth(&s, lane);
        }
        drop(lease); // charge actual time and release before choosing another lane
    }
}
