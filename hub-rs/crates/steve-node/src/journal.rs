//! The Node's execution journal: what it accepted, what it reported and
//! where its streams stand, on disk. No business truth: the Hub keeps items, queues and decisions.
//!
//! One mutex owns the connection, counters and pending stream data. A timer flushes due data. State changes
//! ([`Journal::tx`]) commit immediately with pending data; streaming updates ([`Journal::events`])
//! share one 200 ms / 64 input events / 256 KiB window. Numbering belongs to the committing
//! transaction. Reads, including reconciliation snapshots, run under the same lock and see committed
//! state and stream heads together. Polling readers never flush the window.
//!
//! On every start the Node is a new incarnation. What was accepted and had not finished is then
//! `unknown`: the process that ran it is gone with the old incarnation (or still runs, which the Node
//! checks before letting anything go), what it did up to there is in its events, and only the Hub
//! decides what happens next. An empty memory never means "never ran".

use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, HashMap};
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::time::{Duration, Instant};

mod buffer;
mod schema;
use schema::{ReceiptWrite, VERSION};
#[cfg(test)]
mod tests;
pub(crate) use buffer::{needs_budget, prepare};

use rusqlite::{Connection, OptionalExtension, Transaction, TransactionBehavior, params};
use serde::{Serialize, de::DeserializeOwned};
use steve_node_api::*;

pub(crate) struct Journal {
    writer: Arc<Mutex<Writer>>,
    sender: Option<mpsc::SyncSender<()>>,
    thread: Option<std::thread::JoinHandle<()>>,
    metrics: Arc<Metrics>,
}

#[derive(Default)]
struct Metrics {
    used: AtomicU64,
    limit: AtomicU64,
    alarm: Alarm,
    #[cfg(test)]
    commits: AtomicU64,
}

/// One journal alarm: low bits are capacity/failure, high bits count lost windows.
/// Local admission failures never advance it; accepted windows do, even after recovery.
#[derive(Default)]
struct Alarm(AtomicU64);
impl Alarm {
    const FULL: u64 = 1;
    const FAILED: u64 = 2;
    fn full(&self) -> bool {
        self.0.load(Ordering::Acquire) & Self::FULL != 0
    }
    fn failed(&self) -> bool {
        self.0.load(Ordering::Acquire) & Self::FAILED != 0
    }
    fn epoch(&self) -> u64 {
        self.0.load(Ordering::Acquire) >> 2
    }
    fn set_full(&self, full: bool) {
        if full {
            self.0.fetch_or(Self::FULL, Ordering::AcqRel);
        } else {
            self.0.fetch_and(!Self::FULL, Ordering::AcqRel);
        }
    }
    fn fail(&self, error: &ApiError) {
        match error.code {
            ErrorCode::Exhausted => {
                self.0
                    .try_update(Ordering::AcqRel, Ordering::Acquire, |state| {
                        Some((state + 4) | Self::FULL)
                    })
                    .ok();
            }
            ErrorCode::Internal => self.fault(),
            _ => {}
        }
    }
    fn fault(&self) {
        self.0.fetch_or(Self::FAILED, Ordering::AcqRel);
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum WriteMode {
    Window,
    Control,
}

/// Space for SQLite's row/index growth while payloads are staged in memory.
const EVENT_HEADROOM: u64 = 16384;

fn data_fits(used: u64, bytes: u64, limit: u64) -> bool {
    used.saturating_add(bytes).saturating_add(EVENT_HEADROOM) <= limit
}

struct Writer {
    db: Connection,
    heads: BTreeMap<String, StreamHead>,
    counters: BTreeMap<String, u64>,
    accept_floor: i64,
    id: String,
    incarnation: u64,
    used: u64,
    file: u64,
    limit: u64,
    storage: crate::Storage,
    home: std::path::PathBuf,
    checked_space: Option<Instant>,
    free: Option<u64>,
    pending: buffer::Buffer,
    metrics: Arc<Metrics>,
    wake: Option<Arc<tokio::sync::Notify>>,
}

impl Drop for Journal {
    fn drop(&mut self) {
        self.sender.take();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

impl Journal {
    pub fn open(path: &Path) -> ApiResult<(Self, u64)> {
        let (writer, incarnation) = Writer::open(path)?;
        let metrics = writer.metrics.clone();
        let writer = Arc::new(Mutex::new(writer));
        let (sender, receiver) = mpsc::sync_channel(1);
        let timer_writer = writer.clone();
        let thread = std::thread::Builder::new()
            .name("node-journal-flush".into())
            .spawn(move || {
                let alarm = timer_writer
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .metrics
                    .clone();
                if std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    Self::run_timer(timer_writer, receiver)
                }))
                .is_err()
                {
                    alarm.alarm.fault();
                }
            })
            .map_err(internal)?;
        Ok((
            Self {
                writer,
                sender: Some(sender),
                thread: Some(thread),
                metrics,
            },
            incarnation,
        ))
    }

    fn run_timer(writer: Arc<Mutex<Writer>>, receiver: mpsc::Receiver<()>) {
        let mut next_sample = Instant::now() + Duration::from_millis(500);
        loop {
            let due = {
                let mut w = writer.lock().unwrap_or_else(|e| e.into_inner());
                let now = Instant::now();
                if w.pending.deadline().is_some_and(|due| due <= now) {
                    let _ = w.flush(); // Window errors belong to the alarm, never the next caller.
                }
                if now >= next_sample {
                    if let Err(error) = w.refresh() {
                        w.fail(&error);
                    }
                    next_sample = Instant::now() + Duration::from_millis(500);
                }
                w.pending.deadline().unwrap_or(next_sample).min(next_sample)
            };
            match receiver.recv_timeout(due.saturating_duration_since(Instant::now())) {
                Ok(()) | Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    let _ = writer.lock().unwrap_or_else(|e| e.into_inner()).flush();
                    return;
                }
            }
        }
    }

    pub fn configure(
        &self,
        storage: crate::Storage,
        wake: Arc<tokio::sync::Notify>,
    ) -> ApiResult<()> {
        self.call(move |w| {
            w.storage = storage;
            w.wake = Some(wake);
            w.refresh()
        })
    }

    fn call<R: Send>(&self, f: impl FnOnce(&mut Writer) -> ApiResult<R> + Send) -> ApiResult<R> {
        let mut writer = self.writer.lock().unwrap_or_else(|e| e.into_inner());
        // An unwind drops this guard. SQLite rolls back and commit_buffered restores the tail;
        // poison is deliberately recoverable, as with the original connection mutex.
        let result = f(&mut writer);
        drop(writer);
        let _ = self.sender.as_ref().expect("live journal").try_send(());
        result
    }

    pub fn failed(&self) -> bool {
        self.metrics.alarm.failed()
    }
    pub fn fault(&self) {
        self.metrics.alarm.fault();
    }
    pub fn full(&self) -> bool {
        self.metrics.alarm.full()
    }
    pub fn set_full(&self, full: bool) {
        self.metrics.alarm.set_full(full);
    }
    pub fn over_limit(&self) -> bool {
        // Admission hysteresis can reject a large request while existing work still fits.
        // Running attempts stop only for actual exhaustion or a lost-window epoch.
        self.metrics.used.load(Ordering::Acquire) >= self.metrics.limit.load(Ordering::Acquire)
    }
    pub fn budget_epoch(&self) -> u64 {
        self.metrics.alarm.epoch()
    }

    /// A lost accepted window remains a failure even after capacity recovers.
    pub fn recorded_completion(&self, since: u64, mut completion: Completion) -> Completion {
        if self.budget_epoch() != since && completion.outcome == Outcome::Succeeded {
            completion.outcome = Outcome::Cancelled;
            completion.stop_reason = "cancelled".into();
            completion.error = Some(NodeStop::JournalBudget.error());
            completion.transient = false;
            completion.result.clear();
            if let Some(job) = &mut completion.job {
                job.end = JobEnd::Stopped;
            }
        }
        completion
    }

    pub fn cap_pages(&self, pages: u64) -> ApiResult<u64> {
        self.call(move |w| w.cap_pages(pages))
    }

    /// Status/state changes are durable before replying. They take pending data with them.
    pub fn tx<R: Send>(&self, f: impl FnOnce(&Tx) -> ApiResult<R> + Send) -> ApiResult<R> {
        self.call(|w| w.write(f))
    }

    /// Delivery acknowledgements and retention only touch already committed data. They must
    /// not pull a new streaming tail out of its window (an ack/flush feedback loop).
    pub fn acknowledge(&self, ack: &EventAck) -> ApiResult<bool> {
        self.call(|w| w.tx(|t| t.ack(ack)))
    }

    pub fn prune(&self, now: i64) -> ApiResult<usize> {
        self.call(|w| w.tx(|t| t.prune(now)))
    }

    pub fn resume_pruning(&self) -> ApiResult<()> {
        self.call(|w| w.tx(|t| t.resume_pruning()))
    }

    pub fn snapshot_token(&self) -> ApiResult<u64> {
        self.call(|w| w.tx(|t| t.bump("snapshot")))
    }

    /// Reads see only committed state; polling delivery does not shorten the data window.
    pub fn read<R: Send>(&self, f: impl FnOnce(&Tx) -> ApiResult<R> + Send) -> ApiResult<R> {
        self.call(|w| w.transaction(TransactionBehavior::Deferred, f))
    }

    /// Acceptance into the bounded buffer is synchronous; durability is published at commit.
    pub fn events(
        &self,
        events: Vec<(Subject, EventBody)>,
        meter: Option<SessionId>,
    ) -> ApiResult<()> {
        let mut events = events;
        if let Some(session) = meter
            && has_cumulative_usage(&events)
        {
            self.call(|w| stamp_usage(&w.db, &session, &mut events))?;
        }
        // Fit and encode on the producer before handing data to the owner. The writer only
        // numbers the encoded payload and adds its small envelope at commit.
        let events = buffer::prepare(events)?;
        self.call(move |w| w.enqueue(events))
    }

    pub fn flush(&self) -> ApiResult<()> {
        self.call(|w| w.flush())
    }
}

/// The printed form of a journal id (the random value kept in meta).
fn journal_tag(v: i64) -> String {
    format!("{:016x}", v as u64)
}

/// One transaction on the journal.
pub(crate) struct Tx<'a> {
    sql: Transaction<'a>,
    heads: &'a BTreeMap<String, StreamHead>,
    head_changes: RefCell<BTreeMap<String, Option<StreamHead>>>,
    identity: &'a str,
    counters: &'a BTreeMap<String, u64>,
    accept_floor: i64,
    counter_changes: RefCell<BTreeMap<String, u64>>,
    used: u64,
    limit: u64,
    appended: Cell<u64>,
}

const RETENTION_MS: i64 = 24 * 60 * 60_000;
const PRUNE_BATCH: usize = 128;

#[derive(Clone)]
pub(crate) struct Op {
    pub method: String,
    pub digest: Digest,
    pub receipt: Receipt,
    pub execution: Option<ExecutionRef>,
    pub workspace: Option<WorkspaceRef>,
    pub cancel: bool,
}

pub(crate) struct WorkspaceRow {
    pub resource: String,
    pub spec: Digest,
    pub kind: PrepareKind,
    /// The repository of a worktree.
    pub repo: Option<String>,
    pub view: WorkspaceView,
    pub path: String,
    pub incarnation: u64,
}

#[derive(Clone)]
pub(crate) struct SessionRow {
    pub open: OpenSession,
    pub view: SessionView,
    pub incarnation: u64,
    pub acp_id: Option<String>,
}

/// A harness process group this Node started (to check it is gone before anything is freed).
pub(crate) struct Proc {
    pub session: SessionId,
    pub pgid: i64,
    pub incarnation: u64,
    /// The leader's start time when it was started (to tell a reused pid).
    pub started: Option<u64>,
}

fn json<T: Serialize>(v: &T) -> String {
    serde_json::to_string(v).expect("the contract's types serialize")
}

fn from<T: DeserializeOwned>(s: &str) -> rusqlite::Result<T> {
    serde_json::from_str(s).map_err(|e| {
        rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(e))
    })
}

fn conv<T>(r: Result<T, BadId>) -> rusqlite::Result<T> {
    r.map_err(|e| {
        rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(e))
    })
}

pub(crate) fn internal(e: impl std::fmt::Display) -> ApiError {
    ApiError::new(ErrorCode::Internal, format!("journal: {e}"))
}

fn has_cumulative_usage(events: &[(Subject, EventBody)]) -> bool {
    events.iter().any(|(_, body)| matches!(body, EventBody::Usage { report } if report.measurement == Measurement::Cumulative))
}

/// A collector report already supplies its identity. Rebasing it onto the process meter does not
/// need the unused reports ordinal, and never opens a write transaction for a usage snapshot.
fn stamp_usage(
    c: &Connection,
    session: &SessionId,
    events: &mut [(Subject, EventBody)],
) -> ApiResult<()> {
    if !has_cumulative_usage(events) {
        return Ok(());
    }
    let meter: Option<String> = c
        .query_row(
            "SELECT meter FROM procs WHERE session=?1",
            [session.as_str()],
            |r| r.get(0),
        )
        .optional()
        .map_err(internal)?;
    let Some(meter) = meter else {
        return Ok(());
    };
    for (_, body) in events {
        if let EventBody::Usage { report } = body
            && report.measurement == Measurement::Cumulative
        {
            report.report_id = derived_id("u", &format!("{meter}.{}", report.report_id));
            report.meter_epoch = Epoch::new(derived_id("m", &meter)).map_err(internal)?;
        }
    }
    Ok(())
}

impl Writer {
    /// Opens (or creates) the journal and begins a new incarnation: what was live is now unknown,
    /// sessions are released (their processes belong to the old incarnation).
    pub fn open(path: &Path) -> ApiResult<(Self, u64)> {
        Self::from_connection(path, Connection::open(path).map_err(internal)?)
    }

    fn from_connection(path: &Path, c: Connection) -> ApiResult<(Self, u64)> {
        c.execute_batch(
            "PRAGMA journal_mode=WAL; PRAGMA synchronous=FULL; PRAGMA busy_timeout=5000;",
        )
        .map_err(internal)?;
        let version: i64 = c
            .pragma_query_value(None, "user_version", |r| r.get(0))
            .map_err(internal)?;
        if version > VERSION {
            return Err(ApiError::new(
                ErrorCode::Unsupported,
                "journal needs a newer Node",
            ));
        }
        let mut j = Self {
            db: c,
            heads: BTreeMap::new(),
            counters: BTreeMap::new(),
            accept_floor: 0,
            id: String::new(),
            incarnation: 0,
            used: 0,
            file: 0,
            limit: u64::MAX,
            storage: crate::Storage::default(),
            home: path.parent().unwrap_or(Path::new(".")).into(),
            checked_space: None,
            free: None,
            pending: Default::default(),
            metrics: Arc::new(Metrics::default()),
            wake: None,
        };
        let (incarnation, id) = j.tx(|t| {
            // Schema, version, incarnation and recovery either all commit or all roll back.
            t.initialize(version)?;
            let id = t.journal_id()?;
            let inc = t.bump("incarnation")?;
            t.mark_unknown()?;
            Ok((inc, id))
        })?;
        j.id = id;
        j.incarnation = incarnation;
        j.counters = j
            .read(|t| {
                t.rows("SELECT key, value FROM meta WHERE key IN ('incarnation','event','revision','snapshot','workspace')", [], |r| {
                    Ok((r.get(0)?, r.get::<_, i64>(1)? as u64))
                })
            })?
            .into_iter()
            .collect();
        j.accept_floor = j.read(|t| {
            t.c()
                .query_row("SELECT value FROM meta WHERE key='accept_floor'", [], |r| {
                    r.get(0)
                })
                .optional()
                .map(|floor| floor.unwrap_or(0))
                .map_err(internal)
        })?;
        let heads = j.read(|t| {
            t.rows(
                "SELECT stream, epoch, produced, acknowledged FROM streams ORDER BY stream",
                [],
                |r| {
                    Ok(StreamHead {
                        stream: conv(StreamId::new(r.get::<_, String>(0)?))?,
                        epoch: conv(Epoch::new(r.get::<_, String>(1)?))?,
                        produced: r.get::<_, i64>(2)? as u64,
                        acknowledged: r.get::<_, i64>(3)? as u64,
                    })
                },
            )
        })?;
        j.heads = heads
            .into_iter()
            .map(|h| (h.stream.as_str().into(), h))
            .collect();
        j.counters.entry("event".into()).or_insert(0);
        j.counters.entry("revision".into()).or_insert(0);
        j.refresh()?;
        Ok((j, incarnation))
    }

    fn refresh(&mut self) -> ApiResult<()> {
        self.sample_pages()?;
        self.sample_space();
        Ok(())
    }

    fn sample_pages(&mut self) -> ApiResult<()> {
        let (page, count, free): (u64, u64, u64) = self.db.query_row(
            "SELECT (SELECT page_size FROM pragma_page_size), (SELECT page_count FROM pragma_page_count), (SELECT freelist_count FROM pragma_freelist_count)",
            [], |r| Ok((r.get::<_, i64>(0)? as u64, r.get::<_, i64>(1)? as u64, r.get::<_, i64>(2)? as u64))).map_err(internal)?;
        let previous_used = self.used;
        self.used = count.saturating_sub(free) * page;
        self.file = count * page;
        self.publish_space(previous_used, self.limit);
        Ok(())
    }

    fn sample_space(&mut self) {
        let previous_limit = self.limit;
        // The injected observer stays synchronous for admission/fault fixtures. Production
        // statvfs is sampled at most once per ten seconds; neither path queries SQLite pages.
        if let Some(observer) = &self.storage.free_space {
            self.free = Some(observer());
        } else if self
            .checked_space
            .is_none_or(|checked| checked.elapsed() >= Duration::from_secs(10))
        {
            self.free = crate::space::free_bytes(&self.home);
            self.checked_space = Some(Instant::now());
        }
        self.limit = match self.free {
            Some(free) => self.storage.quota.min(
                self.file
                    .saturating_add(free)
                    .saturating_sub(self.storage.reserve),
            ),
            None => self.storage.quota,
        };
        self.publish_space(self.used, previous_limit);
    }

    fn publish_space(&self, previous_used: u64, previous_limit: u64) {
        self.metrics.used.store(self.used, Ordering::Release);
        self.metrics.limit.store(self.limit, Ordering::Release);
        if self.used < self.limit.saturating_mul(self.storage.low_water as u64) / 100
            && (self.used < previous_used || self.limit > previous_limit)
        {
            self.metrics.alarm.set_full(false);
        }
    }

    fn fail(&self, e: &ApiError) {
        self.metrics.alarm.fail(e);
    }

    fn enqueue(&mut self, events: Vec<buffer::Prepared>) -> ApiResult<()> {
        // Refusal is local: nothing in this update has transferred to the shared window yet.
        let incoming = events
            .iter()
            .filter(|event| event.data())
            .fold(0u64, |bytes, event| {
                bytes.saturating_add(event.bytes() as u64)
            });
        if incoming > 0
            && !data_fits(
                self.used.saturating_add(self.pending.bytes()),
                incoming,
                self.limit,
            )
        {
            return Err(NodeStop::JournalBudget.error());
        }
        // Any causal/closing fact makes this whole update caller-owned until one control commit.
        // A failure cannot leave a committed prefix for the caller's terminal retry to duplicate.
        if events.iter().any(|event| event.boundary() || !event.data()) {
            let incarnation = self.incarnation;
            let mut update = buffer::Buffer::default();
            for event in events {
                update.push(event);
            }
            return self.write(|t| {
                for event in &update.events {
                    t.emit(event, incarnation)?;
                }
                Ok(())
            });
        }
        for event in events {
            self.pending.push(event);
            if self.pending.ready() {
                // These events have transferred to the window. Errors are published by flush's
                // alarm, never returned to a different producer or retried by this caller.
                let _ = self.flush();
            }
        }
        Ok(())
    }

    fn flush(&mut self) -> ApiResult<()> {
        if self.pending.events.is_empty() {
            return Ok(());
        }
        let result = self.commit_buffered(WriteMode::Window, |_| Ok(()));
        if let Err(error) = &result {
            self.fail(error);
            if !self.pending.events.is_empty() {
                self.pending.since = Some(Instant::now());
            }
        }
        result
    }

    fn write<R>(&mut self, f: impl FnOnce(&Tx) -> ApiResult<R>) -> ApiResult<R> {
        self.commit_buffered(WriteMode::Control, f)
    }

    fn commit_buffered<R>(
        &mut self,
        mode: WriteMode,
        f: impl FnOnce(&Tx) -> ApiResult<R>,
    ) -> ApiResult<R> {
        // Capacity is sampled once before admission; pages once after COMMIT. Ordinary event
        // enqueue and actor limit checks use the cached values without touching the connection.
        self.sample_space();
        let mut pending = self.pending.take();
        if pending.over_budget(self.used, self.limit) {
            let error = NodeStop::JournalBudget.error();
            if mode == WriteMode::Control {
                self.fail(&error);
            }
            // As with the old refused Held text, data cannot block cancellation, permission
            // answers or terminal receipts. The epoch makes the lost window visible to its run.
            if mode == WriteMode::Window {
                return Err(error);
            }
            pending = buffer::Buffer::default();
        }
        let incarnation = self.incarnation;
        let mut data_failed = false;
        let mut caller_panic = None;
        let result = self.transaction(TransactionBehavior::Immediate, |t| {
            for event in &pending.events {
                if let Err(e) = t.emit(event, incarnation) {
                    data_failed = true;
                    return Err(e);
                }
            }
            // Only caller code can return its window to the writer on unwind. An internal
            // emit panic must drop that window rather than repeatedly poisoning every flush.
            match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| f(t))) {
                Ok(result) => result,
                Err(panic) => {
                    caller_panic = Some(panic);
                    Err(internal("transaction callback panicked"))
                }
            }
        });
        if let Some(panic) = caller_panic {
            self.pending = pending;
            std::panic::resume_unwind(panic);
        }
        if let Err(e) = &result {
            // An admission can be exhausted for its own request-size/policy limit. Only an
            // actual event-budget refusal (or IO failure) changes running actors' journal alarm.
            if data_failed && mode == WriteMode::Control {
                self.fail(e);
            }
            if e.code != ErrorCode::Exhausted || !data_failed {
                self.pending = pending;
                // Keep the original deadline/count. Only the timer's failed flush resets its
                // retry cadence; rejected caller transactions cannot extend a data window.
            }
        }
        result
    }

    fn cap_pages(&mut self, pages: u64) -> ApiResult<u64> {
        self.db
            .pragma_update_and_check(None, "max_page_count", pages as i64, |r| r.get::<_, i64>(0))
            .map(|n| n as u64)
            .map_err(internal)
    }

    fn tx<R>(&mut self, f: impl FnOnce(&Tx) -> ApiResult<R>) -> ApiResult<R> {
        self.transaction(TransactionBehavior::Immediate, f)
    }

    fn read<R>(&mut self, f: impl FnOnce(&Tx) -> ApiResult<R>) -> ApiResult<R> {
        self.transaction(TransactionBehavior::Deferred, f)
    }

    fn transaction<R>(
        &mut self,
        behavior: TransactionBehavior,
        f: impl FnOnce(&Tx) -> ApiResult<R>,
    ) -> ApiResult<R> {
        let (r, changes, counters) = {
            let t = Tx {
                sql: self
                    .db
                    .transaction_with_behavior(behavior)
                    .map_err(internal)?,
                heads: &self.heads,
                head_changes: RefCell::new(BTreeMap::new()),
                identity: &self.id,
                counters: &self.counters,
                accept_floor: self.accept_floor,
                counter_changes: RefCell::new(BTreeMap::new()),
                used: self.used,
                limit: self.limit,
                appended: Cell::new(0),
            };
            let r = f(&t)?;
            let changes = t.head_changes.take();
            let counters = t.counter_changes.take();
            for (key, value) in &counters {
                t.c().execute("INSERT INTO meta(key,value) VALUES (?1,?2) ON CONFLICT(key) DO UPDATE SET value=excluded.value", params![key, *value as i64]).map_err(internal)?;
            }
            t.sql.commit().map_err(internal)?;
            (r, changes, counters)
        };
        // Private deltas never escape a failed closure or failed COMMIT.
        for (stream, head) in changes {
            match head {
                Some(h) => {
                    self.heads.insert(stream, h);
                }
                None => {
                    self.heads.remove(&stream);
                }
            }
        }
        self.counters.extend(counters);
        if matches!(behavior, TransactionBehavior::Immediate) {
            #[cfg(test)]
            self.metrics.commits.fetch_add(1, Ordering::Relaxed);
            if let Err(e) = self.sample_pages() {
                self.fail(&e);
            }
            if let Some(wake) = &self.wake {
                wake.notify_one();
            }
        }
        Ok(r)
    }
}

impl Tx<'_> {
    /// The body and subject were encoded once before numbering. Only the small envelope header
    /// is serialized here; the JSON payload is inserted verbatim into that envelope.
    pub fn emit(&self, event: &buffer::Prepared, incarnation: u64) -> ApiResult<()> {
        if event.data() && !data_fits(self.payload_bytes()?, event.bytes() as u64, self.limit()) {
            return Err(NodeStop::JournalBudget.error());
        }
        let stream = StreamId::new(crate::stream_of(&event.subject)).map_err(internal)?;
        let n = self.bump("event")?;
        let event_id = EventId::new(format!("ev_{}_{incarnation}_{n}", self.journal_id()?))
            .map_err(internal)?;
        self.append(&stream, |cursor| {
            #[derive(Serialize)]
            struct Header<'a> {
                schema: u16,
                id: &'a EventId,
                at: Timestamp,
                cursor: Watermark,
            }
            let header = Header {
                schema: EVENT_SCHEMA,
                id: &event_id,
                at: Timestamp(crate::now()),
                cursor,
            };
            let mut encoded = serde_json::to_string(&header).map_err(internal)?;
            encoded.pop();
            encoded.push_str(",\"subject\":");
            encoded.push_str(&event.subject_json);
            encoded.push_str(",\"body\":");
            encoded.push_str(&event.body_json);
            encoded.push('}');
            Ok(((), encoded))
        })
    }

    fn c(&self) -> &Connection {
        &self.sql
    }

    /// Additive auxiliary state: old Nodes ignore this table. There is no new startup refusal
    /// or journal version bump; grants are independently checked against workspace generation.
    pub fn put_artifact_grant(&self, grant: &crate::artifact::Grant) -> ApiResult<()> {
        self.c()
            .execute(
                "INSERT INTO artifact_grants(attempt,workspace,body) VALUES (?1,?2,?3)",
                params![
                    grant.execution.attempt().as_str(),
                    grant.workspace.id.as_str(),
                    json(grant)
                ],
            )
            .map_err(internal)?;
        Ok(())
    }

    pub fn artifact_grant(&self, attempt: &AttemptId) -> ApiResult<Option<crate::artifact::Grant>> {
        let body: Option<String> = self
            .c()
            .query_row(
                "SELECT body FROM artifact_grants WHERE attempt=?1",
                [attempt.as_str()],
                |r| r.get(0),
            )
            .optional()
            .map_err(internal)?;
        body.map(|s| from(&s).map_err(internal)).transpose()
    }

    /// A counter that never goes back (incarnations, revisions, event ids).
    pub fn bump(&self, key: &str) -> ApiResult<u64> {
        let current = self
            .counter_changes
            .borrow()
            .get(key)
            .copied()
            .or_else(|| self.counters.get(key).copied());
        let current = match current {
            Some(v) => v,
            // Only bootstrap/recovery needs this: normal counters were loaded at open.
            None => self
                .c()
                .query_row("SELECT value FROM meta WHERE key=?1", [key], |r| {
                    r.get::<_, i64>(0)
                })
                .optional()
                .map_err(internal)?
                .unwrap_or(0) as u64,
        };
        let next = current
            .checked_add(1)
            .ok_or_else(|| internal("counter exhausted"))?;
        self.counter_changes.borrow_mut().insert(key.into(), next);
        Ok(next)
    }

    /// The next session or workspace revision. It names this journal: a journal made again in the
    /// same home never hands out a revision the Hub holds from the old one.
    pub fn revision(&self) -> ApiResult<String> {
        let n = self.bump("revision")?;
        Ok(format!("r{}_{n}", self.journal_id()?))
    }

    fn journal_id(&self) -> ApiResult<String> {
        if !self.identity.is_empty() {
            return Ok(self.identity.to_owned());
        }
        self.c()
            .query_row("SELECT value FROM meta WHERE key='journal_id'", [], |r| {
                r.get(0)
            })
            .map(journal_tag)
            .map_err(internal)
    }

    pub fn limit(&self) -> u64 {
        self.limit
    }

    /// Loaded once after the migration commits; command admission never probes meta for this.
    pub(crate) fn accept_floor(&self) -> i64 {
        self.accept_floor
    }

    pub(crate) fn mark_unknown(&self) -> ApiResult<()> {
        self.write_receipt(ReceiptWrite::Restart)?;
        // nobody waits for an answer of the old processes
        self.c()
            .execute("DELETE FROM waits", [])
            .map_err(internal)?;
        self.c()
            .execute("DELETE FROM answers", [])
            .map_err(internal)?;
        let rows: Vec<(String, String)> = self.rows("SELECT id, view FROM sessions", [], |r| {
            Ok((r.get(0)?, r.get(1)?))
        })?;
        for (id, view) in rows {
            let mut v: SessionView = from(&view).map_err(internal)?;
            if v.phase != SessionPhase::Released {
                v.phase = SessionPhase::Released;
                self.c()
                    .execute(
                        "UPDATE sessions SET view = ?2 WHERE id = ?1",
                        params![id, json(&v)],
                    )
                    .map_err(internal)?;
            }
        }
        Ok(())
    }

    fn rows<T, P: rusqlite::Params>(
        &self,
        sql: &str,
        p: P,
        f: impl FnMut(&rusqlite::Row) -> rusqlite::Result<T>,
    ) -> ApiResult<Vec<T>> {
        let mut st = self.c().prepare(sql).map_err(internal)?;
        let v = st
            .query_map(p, f)
            .map_err(internal)?
            .collect::<Result<_, _>>()
            .map_err(internal)?;
        Ok(v)
    }

    // ── operations ──

    fn op_row(r: &rusqlite::Row) -> rusqlite::Result<Op> {
        let execution: Option<String> = r.get(3)?;
        let workspace: Option<String> = r.get(4)?;
        Ok(Op {
            method: r.get(0)?,
            digest: conv(Digest::new(r.get::<_, String>(1)?))?,
            receipt: from(&r.get::<_, String>(2)?)?,
            execution: execution.map(|e| from(&e)).transpose()?,
            workspace: workspace.map(|w| from(&w)).transpose()?,
            cancel: r.get::<_, i64>(5)? != 0,
        })
    }

    pub fn save_spec<T: Serialize>(&self, op: &OperationId, body: &T) -> ApiResult<()> {
        let body = json(body);
        self.c()
            .execute(
                "INSERT INTO command_specs (op, body) VALUES (?1, ?2)",
                params![op.as_str(), &body],
            )
            .map_err(internal)?;
        self.appended
            .set(self.appended.get().saturating_add(body.len() as u64));
        Ok(())
    }
    pub fn spec(&self, op: &OperationId) -> ApiResult<Option<serde_json::Value>> {
        let s: Option<String> = self
            .c()
            .query_row(
                "SELECT body FROM command_specs WHERE op=?1",
                [op.as_str()],
                |r| r.get(0),
            )
            .optional()
            .map_err(internal)?;
        s.map(|s| serde_json::from_str(&s).map_err(internal))
            .transpose()
    }
    pub fn session_of(&self, op: &OperationId) -> ApiResult<Option<SessionId>> {
        let s: Option<String> = self
            .c()
            .query_row("SELECT session FROM ops WHERE id=?1", [op.as_str()], |r| {
                r.get(0)
            })
            .map_err(internal)?;
        s.map(|s| SessionId::new(s).map_err(internal)).transpose()
    }
    pub fn unreserve_owned(&self, op: &OperationId) -> ApiResult<()> {
        self.c()
            .execute("DELETE FROM reservations WHERE op=?1", [op.as_str()])
            .map_err(internal)?;
        Ok(())
    }

    pub fn op(&self, id: &OperationId) -> ApiResult<Option<Op>> {
        self.c()
            .query_row(
                "SELECT method, digest, receipt, execution, workspace, cancel FROM ops WHERE id = ?1",
                [id.as_str()],
                Self::op_row,
            )
            .optional()
            .map_err(internal)
    }

    pub fn ops(&self) -> ApiResult<Vec<(OperationId, Op)>> {
        let rows: Vec<(String, Op)> = self.rows(
            "SELECT method, digest, receipt, execution, workspace, cancel, id FROM ops ORDER BY id",
            [],
            |r| Ok((r.get(6)?, Self::op_row(r)?)),
        )?;
        rows.into_iter()
            .map(|(id, o)| Ok((OperationId::new(id).map_err(internal)?, o)))
            .collect()
    }

    /// The latest explicit setup survives spec/event retention, including failure and an
    /// interrupted installation. An interrupted operation is observed, never replayed on boot.
    pub fn installation(&self, harness: &str) -> ApiResult<Option<Receipt>> {
        let op: Option<String> = self
            .c()
            .query_row(
                "SELECT op FROM harness_setups WHERE harness=?1",
                [harness],
                |r| r.get(0),
            )
            .optional()
            .map_err(internal)?;
        match op {
            Some(op) => Ok(self
                .op(&OperationId::new(op).map_err(internal)?)?
                .map(|o| o.receipt)),
            None => Ok(None),
        }
    }

    pub fn set_installation(&self, harness: &str, op: &OperationId) -> ApiResult<()> {
        self.c().execute("INSERT INTO harness_setups(harness,op) VALUES (?1,?2) ON CONFLICT(harness) DO UPDATE SET op=excluded.op", params![harness, op.as_str()]).map_err(internal)?;
        Ok(())
    }

    /// Every unfinished session operation, including open/configure/release and unknown.
    pub fn open_ops_of(&self, session: &SessionId) -> ApiResult<Vec<(OperationId, Op)>> {
        let rows: Vec<(String, Op)> = self.rows(
            "SELECT method, digest, receipt, execution, workspace, cancel, id FROM ops WHERE session = ?1",
            [session.as_str()],
            |r| Ok((r.get(6)?, Self::op_row(r)?)),
        )?;
        rows.into_iter()
            .filter(|(_, o)| !o.receipt.state.is_finished())
            .map(|(id, o)| Ok((OperationId::new(id).map_err(internal)?, o)))
            .collect()
    }

    /// How many commands ([`crate::exec`]) in `w` are not finished (running, or unknown since a
    /// restart).
    pub fn open_execs_in(&self, w: &WorkspaceRef) -> ApiResult<usize> {
        let rows: Vec<Op> = self.rows(
            "SELECT method, digest, receipt, execution, workspace, cancel FROM ops WHERE method = 'exec' AND workspace = ?1",
            [json(w)],
            Self::op_row,
        )?;
        Ok(rows
            .iter()
            .filter(|o| !o.receipt.state.is_finished())
            .count())
    }

    pub fn accept(
        &self,
        id: &OperationId,
        op: &Op,
        session: Option<&SessionId>,
        now: i64,
    ) -> ApiResult<()> {
        self.write_receipt(ReceiptWrite::Accept {
            id,
            op,
            session,
            at: now,
        })
    }

    /// Sets the state, unless it is final already (a final state never changes). False: unchanged.
    pub fn set_state(&self, id: &OperationId, state: &Observed) -> ApiResult<bool> {
        let Some(mut op) = self.op(id)? else {
            return Ok(false);
        };
        if op.receipt.state.is_finished() {
            return Ok(false);
        }
        op.receipt.state = state.clone();
        self.write_receipt(ReceiptWrite::Update {
            id,
            receipt: &op.receipt,
        })?;
        Ok(true)
    }

    pub fn set_cancel(&self, id: &OperationId) -> ApiResult<()> {
        self.c()
            .execute("UPDATE ops SET cancel = 1 WHERE id = ?1", [id.as_str()])
            .map_err(internal)?;
        Ok(())
    }

    pub fn attempt(&self, a: &AttemptId) -> ApiResult<Option<(ExecutionRef, OperationId)>> {
        let row: Option<(String, String)> = self
            .c()
            .query_row(
                "SELECT execution, op FROM attempts WHERE attempt = ?1",
                [a.as_str()],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()
            .map_err(internal)?;
        row.map(|(e, op)| {
            Ok((
                from(&e).map_err(internal)?,
                OperationId::new(op).map_err(internal)?,
            ))
        })
        .transpose()
    }

    pub fn take_attempt(&self, exec: &ExecutionRef, op: &OperationId) -> ApiResult<()> {
        self.c()
            .execute(
                "INSERT INTO attempts (attempt, execution, op) VALUES (?1, ?2, ?3)",
                params![exec.attempt().as_str(), json(exec), op.as_str()],
            )
            .map_err(internal)?;
        Ok(())
    }

    // ── events ──

    #[cfg(test)]
    fn record(
        &self,
        stream: &StreamId,
        build: impl FnOnce(Watermark) -> NodeEvent,
    ) -> ApiResult<NodeEvent> {
        self.append(stream, |cursor| {
            let event = build(cursor);
            let raw = json(&event);
            Ok((event, raw))
        })
    }

    /// Allocate a stream position and write the already encoded event. Both production events
    /// and the cache/pruning fixtures use this single numbering implementation.
    fn append<R>(
        &self,
        stream: &StreamId,
        build: impl FnOnce(Watermark) -> ApiResult<(R, String)>,
    ) -> ApiResult<R> {
        let mut head = match self.head(stream.as_str()) {
            Some(head) => head,
            None => {
                let epoch = Epoch::new(format!("j{}", self.journal_id()?)).map_err(internal)?;
                self.c().execute("INSERT INTO streams(stream,epoch,produced,acknowledged) VALUES (?1,?2,0,0)", params![stream.as_str(),epoch.as_str()]).map_err(internal)?;
                StreamHead {
                    stream: stream.clone(),
                    epoch,
                    produced: 0,
                    acknowledged: 0,
                }
            }
        };
        head.produced += 1;
        let (result, encoded) = build(Watermark {
            stream: stream.clone(),
            epoch: head.epoch.clone(),
            seq: head.produced,
        })?;
        self.c()
            .execute(
                "INSERT INTO events(stream,seq,event) VALUES (?1,?2,?3)",
                params![stream.as_str(), head.produced as i64, &encoded],
            )
            .map_err(internal)?;
        self.c()
            .execute(
                "UPDATE streams SET produced=?2 WHERE stream=?1",
                params![stream.as_str(), head.produced as i64],
            )
            .map_err(internal)?;
        self.appended
            .set(self.appended.get().saturating_add(encoded.len() as u64));
        self.head_changes
            .borrow_mut()
            .insert(stream.as_str().into(), Some(head));
        Ok(result)
    }

    /// Events not acknowledged yet, in order, at most `limit`, past what is in flight: an event at
    /// or below `sent[stream]` is skipped (its batch is out already). A stream's positions only
    /// grow, so this is what follows them.
    pub fn unacked_past(
        &self,
        sent: &HashMap<String, u64>,
        limit: usize,
    ) -> ApiResult<Vec<NodeEvent>> {
        let mut st = self
            .c()
            .prepare_cached("SELECT stream, seq, event FROM events ORDER BY rowid")
            .map_err(internal)?;
        let mut rows = st.query([]).map_err(internal)?;
        let mut out = vec![];
        while out.len() < limit {
            let Some(r) = rows.next().map_err(internal)? else {
                break;
            };
            let stream: String = r.get(0).map_err(internal)?;
            let seq: i64 = r.get(1).map_err(internal)?;
            if sent.get(&stream).is_some_and(|s| seq as u64 <= *s) {
                continue;
            }
            let event: String = r.get(2).map_err(internal)?;
            out.push(from(&event).map_err(internal)?);
        }
        Ok(out)
    }

    fn head(&self, stream: &str) -> Option<StreamHead> {
        self.head_changes
            .borrow()
            .get(stream)
            .cloned()
            .unwrap_or_else(|| self.heads.get(stream).cloned())
    }

    pub fn covered(&self, marks: &[Watermark]) -> bool {
        marks.iter().all(|m| {
            self.head(m.stream.as_str())
                .is_none_or(|h| h.epoch != m.epoch || h.acknowledged >= m.seq)
        })
    }

    pub fn heads(&self) -> ApiResult<Vec<StreamHead>> {
        let changes = self.head_changes.borrow();
        let mut out: Vec<_> = self
            .heads
            .iter()
            .filter(|(s, _)| !changes.contains_key(*s))
            .map(|(_, h)| h.clone())
            .chain(changes.values().filter_map(Clone::clone))
            .collect();
        out.sort_unstable_by(|a, b| a.stream.as_str().cmp(b.stream.as_str()));
        Ok(out)
    }

    /// Moves the watermarks and drops what they cover, all or nothing. Returns whether any moved.
    pub fn ack(&self, ack: &EventAck) -> ApiResult<bool> {
        let mut moved = false;
        for w in &ack.durable {
            let Some(mut head) = self.head(w.stream.as_str()) else {
                return Err(ApiError::new(
                    ErrorCode::Invalid,
                    "ack of an unknown stream",
                ));
            };
            let (epoch, produced, acknowledged) = (
                head.epoch.as_str(),
                head.produced as i64,
                head.acknowledged as i64,
            );
            if epoch != w.epoch.as_str() {
                return Err(ApiError::new(ErrorCode::Stale, "ack of another epoch"));
            }
            if w.seq as i64 > produced {
                return Err(ApiError::new(ErrorCode::Invalid, "ack ahead of the stream"));
            }
            if w.seq as i64 > acknowledged {
                moved = true;
                self.c()
                    .execute(
                        "UPDATE streams SET acknowledged = ?2 WHERE stream = ?1",
                        params![w.stream.as_str(), w.seq as i64],
                    )
                    .map_err(internal)?;
                self.c()
                    .execute(
                        "DELETE FROM events WHERE stream = ?1 AND seq <= ?2",
                        params![w.stream.as_str(), w.seq as i64],
                    )
                    .map_err(internal)?;
                head.acknowledged = w.seq;
                self.head_changes
                    .borrow_mut()
                    .insert(w.stream.as_str().into(), Some(head));
            }
        }
        self.compact()?;
        Ok(moved)
    }

    /// Drops what finished operations no longer need: the body of one whose final state the Hub
    /// has taken, the answer of one that finished. The final state is on the operation's stream
    /// (`OperationChanged`) or, for an execution, on its session's or job's (`AttemptEnded`); it is
    /// taken once every event of those streams is acknowledged (one that finished without any
    /// stream, like a cancel, has nothing to deliver and is taken at once). An operation that is not finished
    /// (running or unknown) keeps its body for recovery; the ops rows (id, digest, receipt) stay
    /// for idempotency until retention expires.
    fn compact(&self) -> ApiResult<()> {
        let specs: Vec<(String, String, Option<String>)> = self.rows(
            "SELECT o.id, o.receipt, o.execution FROM command_specs s JOIN ops o ON o.id = s.op",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )?;
        for (op, receipt, execution) in specs {
            if !self.final_taken(&op, &receipt, execution.as_deref())? {
                continue;
            }
            self.c()
                .execute("DELETE FROM command_specs WHERE op = ?1", [&op])
                .map_err(internal)?;
        }
        let answers: Vec<(String, String)> = self.rows(
            "SELECT o.id, o.receipt FROM answers a JOIN ops o ON o.id = a.op",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        for (op, receipt) in answers {
            let r: Receipt = from(&receipt).map_err(internal)?;
            if r.state.is_finished() {
                self.c()
                    .execute("DELETE FROM answers WHERE op = ?1", [&op])
                    .map_err(internal)?;
            }
        }
        Ok(())
    }

    // Shared by compaction and tombstone retention: missing streams have nothing to deliver.
    fn op_streams(op: &str, execution: Option<&str>) -> ApiResult<Vec<String>> {
        let mut streams = vec![crate::stream_of(&Subject::Operation {
            operation: OperationId::new(op).map_err(internal)?,
        })];
        if let Some(e) = execution {
            streams.push(crate::stream_of(&Subject::Execution(
                from(e).map_err(internal)?,
            )));
        }
        Ok(streams)
    }

    fn final_taken(&self, op: &str, receipt: &str, execution: Option<&str>) -> ApiResult<bool> {
        Ok(from::<Receipt>(receipt)
            .map_err(internal)?
            .state
            .is_finished()
            && Self::op_streams(op, execution)?
                .iter()
                .all(|s| self.head(s).is_none_or(|h| h.produced == h.acknowledged)))
    }

    pub(crate) fn resume_pruning(&self) -> ApiResult<()> {
        self.c()
            .execute("DELETE FROM meta WHERE key='prune_after'", [])
            .map_err(internal)?;
        Ok(())
    }

    /// One bounded slice of an hourly sweep; persist the cursor with the deletions, so failure or
    /// restart resumes safely. 24h exceeds the first-party Hub's largest accept window (10min).
    /// Session streams and externally referenced receipts/grants survive retention.
    pub(crate) fn prune(&self, now: i64) -> ApiResult<usize> {
        let next: i64 = self
            .c()
            .query_row(
                "SELECT COALESCE((SELECT value FROM meta WHERE key='prune_after'),0)",
                [],
                |r| r.get(0),
            )
            .map_err(internal)?;
        if now < next {
            return Ok(0);
        }
        let cursor: i64 = self
            .c()
            .query_row(
                "SELECT COALESCE((SELECT value FROM meta WHERE key='prune_cursor'),0)",
                [],
                |r| r.get(0),
            )
            .map_err(internal)?;
        let rows: Vec<(i64, String, String, Option<String>, i64)> = self.rows("SELECT rowid,id,receipt,execution,accepted FROM ops WHERE rowid>?1 ORDER BY rowid LIMIT ?2", params![cursor, PRUNE_BATCH as i64], |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?)))?;
        for (_, op, receipt, execution, accepted) in &rows {
            if *accepted >= now.saturating_sub(RETENTION_MS)
                || !self.final_taken(op, receipt, execution.as_deref())?
            {
                continue;
            }
            let held: bool = self.c().query_row("SELECT EXISTS(SELECT 1 FROM harness_setups WHERE op=?1 UNION ALL SELECT 1 FROM waits WHERE op=?1 UNION ALL SELECT 1 FROM attempts a JOIN artifact_grants g ON g.attempt=a.attempt WHERE a.op=?1)", [op], |r| r.get(0)).map_err(internal)?;
            if held {
                continue;
            }
            let streams = Self::op_streams(op, execution.as_deref())?;
            let pending: bool = self
                .c()
                .query_row(
                    "SELECT EXISTS(SELECT 1 FROM events WHERE stream=?1 OR stream=?2)",
                    params![streams[0], streams.get(1)],
                    |r| r.get(0),
                )
                .map_err(internal)?;
            if pending {
                continue;
            }
            let shared = match execution.as_deref().map(from::<ExecutionRef>).transpose().map_err(internal)? {
                Some(ExecutionRef::Turn { .. }) => true,
                Some(ExecutionRef::Job { job, .. }) => self.c().query_row("SELECT EXISTS(SELECT 1 FROM ops WHERE id!=?1 AND json_extract(execution,'$.job')=?2)", params![op,job.as_str()], |r| r.get(0)).map_err(internal)?,
                None => false,
            };
            for stream in streams.into_iter().take(if shared { 1 } else { 2 }) {
                self.c()
                    .execute(
                        "DELETE FROM streams WHERE stream=?1 AND produced=acknowledged",
                        [&stream],
                    )
                    .map_err(internal)?;
                self.head_changes.borrow_mut().insert(stream, None);
            }
            self.remove_op_dependents(op)?;
            self.c()
                .execute("DELETE FROM ops WHERE id=?1", [op])
                .map_err(internal)?;
        }
        let done = rows.len() < PRUNE_BATCH;
        self.c().execute("INSERT INTO meta(key,value) VALUES ('prune_cursor',?1) ON CONFLICT(key) DO UPDATE SET value=excluded.value", [if done { 0 } else { rows.last().unwrap().0 }]).map_err(internal)?;
        self.c().execute("INSERT INTO meta(key,value) VALUES ('prune_after',?1) ON CONFLICT(key) DO UPDATE SET value=excluded.value", [if done { now.saturating_add(60*60_000) } else { 0 }]).map_err(internal)?;
        Ok(rows.len())
    }

    /// Daily retention removes only this op's dependencies, after its eligibility checks.
    fn remove_op_dependents(&self, op: &str) -> ApiResult<()> {
        self.c().execute(
            "DELETE FROM artifact_grants WHERE attempt IN (SELECT attempt FROM attempts WHERE op=?1)",
            [op],
        ).map_err(internal)?;
        for table in [
            "attempts",
            "command_specs",
            "reservations",
            "answers",
            "waits",
            "harness_setups",
        ] {
            self.c()
                .execute(&format!("DELETE FROM {table} WHERE op=?1"), [op])
                .map_err(internal)?;
        }
        Ok(())
    }

    pub fn replay(&self, from_mark: &Watermark, max_bytes: u32) -> ApiResult<EventBatch> {
        let row: Option<(String, i64, i64)> = self
            .c()
            .query_row(
                "SELECT epoch, produced, acknowledged FROM streams WHERE stream = ?1",
                [from_mark.stream.as_str()],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .optional()
            .map_err(internal)?;
        let Some((epoch, produced, acknowledged)) = row else {
            return Err(ApiError::new(ErrorCode::Invalid, "no such stream"));
        };
        if epoch != from_mark.epoch.as_str() {
            return Err(ApiError::new(ErrorCode::Gap, "another stream epoch"));
        }
        if (from_mark.seq as i64) < acknowledged {
            return Err(ApiError::new(
                ErrorCode::Gap,
                "already acknowledged and dropped",
            ));
        }
        if from_mark.seq as i64 > produced {
            return Err(ApiError::new(ErrorCode::Invalid, "ahead of the stream"));
        }
        let rows: Vec<String> = self.rows(
            "SELECT event FROM events WHERE stream = ?1 AND seq > ?2 ORDER BY seq",
            params![from_mark.stream.as_str(), from_mark.seq as i64],
            |r| r.get(0),
        )?;
        let mut events = vec![];
        let mut used = 0usize;
        for s in rows {
            if used + s.len() > max_bytes as usize {
                if events.is_empty() {
                    return Err(ApiError::new(
                        ErrorCode::Exhausted,
                        "the next event does not fit in max_bytes",
                    ));
                }
                break;
            }
            used += s.len();
            events.push(from(&s).map_err(internal)?);
        }
        Ok(EventBatch { events })
    }

    // ── workspaces and sessions ──

    pub fn workspace(&self, id: &WorkspaceId) -> ApiResult<Option<WorkspaceRow>> {
        self.c()
            .query_row(
                "SELECT resource, spec, view, path, incarnation, kind, repo FROM workspaces WHERE id = ?1",
                [id.as_str()],
                |r| {
                    Ok(WorkspaceRow {
                        resource: r.get(0)?,
                        spec: conv(Digest::new(r.get::<_, String>(1)?))?,
                        view: from(&r.get::<_, String>(2)?)?,
                        path: r.get(3)?,
                        incarnation: r.get::<_, i64>(4)? as u64,
                        kind: from(&r.get::<_, String>(5)?)?,
                        repo: r.get(6)?,
                    })
                },
            )
            .optional()
            .map_err(internal)
    }

    /// Two workspaces may have been prepared at the same path or one inside another.
    /// Within an overlap the filesystem alone cannot prove item ownership.
    pub fn overlaps_other_workspace(
        &self,
        current: &WorkspaceId,
        base: &std::path::Path,
        file: &std::path::Path,
    ) -> ApiResult<bool> {
        let mut q = self
            .c()
            .prepare("SELECT path FROM workspaces WHERE id != ?")
            .map_err(internal)?;
        let mut rows = q.query([current.as_str()]).map_err(internal)?;
        while let Some(row) = rows.next().map_err(internal)? {
            let path: String = row.get(0).map_err(internal)?;
            let path = std::path::Path::new(&path);
            // A parent workspace cannot read inside this one either (the test is symmetric).
            if file.starts_with(path) && path.starts_with(base) || base.starts_with(path) {
                return Ok(true);
            }
        }
        Ok(false)
    }

    pub fn put_workspace(&self, w: &WorkspaceRow) -> ApiResult<()> {
        // Re-preparing the same resource creates a new directory generation. Its predecessors'
        // artifact grants must neither survive as authorization nor accumulate in the journal.
        self.c()
            .execute(
                "DELETE FROM artifact_grants WHERE workspace=?1",
                [w.view.workspace.id.as_str()],
            )
            .map_err(internal)?;
        self.c()
            .execute(
                "INSERT OR REPLACE INTO workspaces (id, resource, spec, kind, view, path, repo, incarnation)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                params![
                    w.view.workspace.id.as_str(),
                    w.resource,
                    w.spec.as_str(),
                    json(&w.kind),
                    json(&w.view),
                    w.path,
                    w.repo,
                    w.incarnation as i64
                ],
            )
            .map_err(internal)?;
        Ok(())
    }

    pub fn drop_workspace(&self, id: &WorkspaceId) -> ApiResult<()> {
        self.c()
            .execute(
                "DELETE FROM artifact_grants WHERE workspace = ?1",
                [id.as_str()],
            )
            .map_err(internal)?;
        self.c()
            .execute("DELETE FROM workspaces WHERE id = ?1", [id.as_str()])
            .map_err(internal)?;
        Ok(())
    }

    pub fn session(&self, id: &SessionId) -> ApiResult<Option<SessionRow>> {
        self.c()
            .query_row(
                "SELECT open, view, incarnation, acp_id FROM sessions WHERE id = ?1",
                [id.as_str()],
                |r| {
                    Ok(SessionRow {
                        open: from(&r.get::<_, String>(0)?)?,
                        view: from(&r.get::<_, String>(1)?)?,
                        incarnation: r.get::<_, i64>(2)? as u64,
                        acp_id: r.get(3)?,
                    })
                },
            )
            .optional()
            .map_err(internal)
    }

    pub fn sessions_in(&self, w: &WorkspaceRef) -> ApiResult<Vec<SessionId>> {
        let rows: Vec<(String, String)> = self.rows("SELECT id, open FROM sessions", [], |r| {
            Ok((r.get(0)?, r.get(1)?))
        })?;
        let mut out = vec![];
        for (id, open) in rows {
            let o: OpenSession = from(&open).map_err(internal)?;
            if o.workspace == *w {
                out.push(SessionId::new(id).map_err(internal)?);
            }
        }
        Ok(out)
    }

    pub fn put_session(&self, s: &SessionRow) -> ApiResult<()> {
        self.c()
            .execute(
                "INSERT INTO sessions (id, open, view, incarnation, acp_id) VALUES (?1, ?2, ?3, ?4, ?5)
                 ON CONFLICT(id) DO UPDATE SET open = excluded.open, view = excluded.view,
                   incarnation = excluded.incarnation, acp_id = excluded.acp_id",
                params![
                    s.view.id.as_str(),
                    json(&s.open),
                    json(&s.view),
                    s.incarnation as i64,
                    s.acp_id
                ],
            )
            .map_err(internal)?;
        Ok(())
    }

    // ── permission waits ──

    pub fn put_wait(
        &self,
        op: &OperationId,
        session: &SessionId,
        req: &PermissionRequest,
    ) -> ApiResult<()> {
        self.c()
            .execute(
                "INSERT INTO waits (id, op, session, request) VALUES (?1, ?2, ?3, ?4)",
                params![req.id.as_str(), op.as_str(), session.as_str(), json(req)],
            )
            .map_err(internal)?;
        Ok(())
    }

    pub fn wait(
        &self,
        id: &PermissionId,
    ) -> ApiResult<Option<(OperationId, SessionId, PermissionRequest)>> {
        let row: Option<(String, String, String)> = self
            .c()
            .query_row(
                "SELECT op, session, request FROM waits WHERE id = ?1",
                [id.as_str()],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .optional()
            .map_err(internal)?;
        row.map(|(op, s, req)| {
            Ok((
                OperationId::new(op).map_err(internal)?,
                SessionId::new(s).map_err(internal)?,
                from(&req).map_err(internal)?,
            ))
        })
        .transpose()
    }

    pub fn waits_of(&self, op: &OperationId) -> ApiResult<Vec<PermissionRequest>> {
        let rows: Vec<String> = self.rows(
            "SELECT request FROM waits WHERE op = ?1 ORDER BY id",
            [op.as_str()],
            |r| r.get(0),
        )?;
        rows.iter().map(|s| from(s).map_err(internal)).collect()
    }

    pub fn drop_wait(&self, id: &PermissionId) -> ApiResult<bool> {
        let n = self
            .c()
            .execute("DELETE FROM waits WHERE id = ?1", [id.as_str()])
            .map_err(internal)?;
        Ok(n > 0)
    }

    // ── harness process groups ──

    /// Records a new process group of the session. Refused while another one of it is recorded (it
    /// was not confirmed gone): never overwritten.
    pub fn put_proc(
        &self,
        session: &SessionId,
        pgid: i64,
        generation: u64,
        incarnation: u64,
        meter: &str,
        started: Option<u64>,
    ) -> ApiResult<()> {
        if self.proc_of(session)?.is_some() {
            return Err(ApiError::new(
                ErrorCode::Busy,
                "a harness of this session was not confirmed gone",
            ));
        }
        self.c()
            .execute(
                "INSERT INTO procs (session, pgid, generation, incarnation, meter, reports, started)
                 VALUES (?1, ?2, ?3, ?4, ?5, 0, ?6)",
                params![
                    session.as_str(),
                    pgid,
                    generation as i64,
                    incarnation as i64,
                    meter,
                    started.map(|s| s as i64)
                ],
            )
            .map_err(internal)?;
        Ok(())
    }

    // ── reservations (a workspace being prepared or tidied, an execution being abandoned) ──

    pub fn reservation(&self, key: &str) -> ApiResult<Option<OperationId>> {
        let row: Option<String> = self
            .c()
            .query_row("SELECT op FROM reservations WHERE key = ?1", [key], |r| {
                r.get(0)
            })
            .optional()
            .map_err(internal)?;
        row.map(|o| OperationId::new(o).map_err(internal))
            .transpose()
    }

    pub fn reserve(&self, key: &str, op: &OperationId) -> ApiResult<()> {
        self.c()
            .execute(
                "INSERT INTO reservations (key, op) VALUES (?1, ?2)",
                params![key, op.as_str()],
            )
            .map_err(internal)?;
        Ok(())
    }

    pub fn unreserve(&self, key: &str) -> ApiResult<()> {
        self.c()
            .execute("DELETE FROM reservations WHERE key = ?1", [key])
            .map_err(internal)?;
        Ok(())
    }

    /// Committed live SQLite pages plus payloads staged in this transaction. The owner samples
    /// exact live pages once per commit; per-event budget checks need no connection access.
    /// Free pages are reusable after acknowledgement; the WAL needs filesystem headroom.
    pub fn payload_bytes(&self) -> ApiResult<u64> {
        Ok(self.used.saturating_add(self.appended.get()))
    }

    /// Bytes of events the Hub has not acknowledged yet.
    pub fn unacked_bytes(&self) -> ApiResult<u64> {
        let n: i64 = self
            .c()
            .query_row(
                "SELECT COALESCE(SUM(length(CAST(event AS BLOB))), 0) FROM events",
                [],
                |r| r.get(0),
            )
            .map_err(internal)?;
        Ok(n as u64)
    }

    // ── answers being applied (a resolution accepted, not yet given to the harness) ──

    pub fn put_answer(&self, op: &OperationId, res: &PermissionResolution) -> ApiResult<()> {
        self.c()
            .execute(
                "INSERT INTO answers (op, permission, resolution) VALUES (?1, ?2, ?3)",
                params![op.as_str(), res.id.as_str(), json(res)],
            )
            .map_err(internal)?;
        Ok(())
    }

    pub fn answer_of(&self, op: &OperationId) -> ApiResult<Option<PermissionResolution>> {
        let row: Option<String> = self
            .c()
            .query_row(
                "SELECT resolution FROM answers WHERE op = ?1",
                [op.as_str()],
                |r| r.get(0),
            )
            .optional()
            .map_err(internal)?;
        row.map(|s| from(&s).map_err(internal)).transpose()
    }

    pub fn answer_pending_for(&self, permission: &PermissionId) -> ApiResult<bool> {
        Ok(self
            .c()
            .query_row(
                "SELECT 1 FROM answers WHERE permission = ?1",
                [permission.as_str()],
                |_| Ok(()),
            )
            .optional()
            .map_err(internal)?
            .is_some())
    }

    /// Last durable authorization check, in the transaction reserving the send. A queued Cancel
    /// message cannot be used as evidence: the cancel flag may already have committed.
    pub fn answer_for_send(&self, op: &OperationId, now: i64) -> ApiResult<()> {
        let stale = || ApiError::new(ErrorCode::Stale, "permission no longer applicable");
        let res = self.answer_of(op)?.ok_or_else(stale)?;
        let (attempt, _, req) = self.wait(&res.id)?.ok_or_else(stale)?;
        let run = self.op(&attempt)?.ok_or_else(stale)?;
        if run.cancel
            || run.receipt.state.is_finished()
            || matches!(run.receipt.state, Observed::Unknown { .. })
            || req.expires_at.0 <= now
            || req.execution != res.execution
            || req.digest != res.action_digest
        {
            return Err(stale());
        }
        Ok(())
    }

    pub fn drop_answer(&self, op: &OperationId) -> ApiResult<()> {
        self.c()
            .execute("DELETE FROM answers WHERE op = ?1", [op.as_str()])
            .map_err(internal)?;
        Ok(())
    }

    pub fn drop_proc(&self, session: &SessionId, pgid: i64) -> ApiResult<()> {
        self.c()
            .execute(
                "DELETE FROM procs WHERE session = ?1 AND pgid = ?2",
                params![session.as_str(), pgid],
            )
            .map_err(internal)?;
        Ok(())
    }

    pub fn procs(&self) -> ApiResult<Vec<Proc>> {
        let rows: Vec<(String, i64, i64, Option<i64>)> = self.rows(
            "SELECT session, pgid, incarnation, started FROM procs",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )?;
        rows.into_iter()
            .map(|(s, p, i, st)| {
                Ok(Proc {
                    session: SessionId::new(s).map_err(internal)?,
                    pgid: p,
                    incarnation: i as u64,
                    started: st.map(|v| v as u64),
                })
            })
            .collect()
    }

    pub fn proc_of(&self, session: &SessionId) -> ApiResult<Option<Proc>> {
        Ok(self.procs()?.into_iter().find(|p| p.session == *session))
    }

    /// Cumulative reports use the process meter, including reports accompanying configuration.
    pub fn stamp_usage(
        &self,
        session: &SessionId,
        events: &mut [(Subject, EventBody)],
    ) -> ApiResult<()> {
        stamp_usage(self.c(), session, events)
    }
}

#[cfg(test)]
mod answer_fence_tests {
    use super::*;
    use steve_node_api::testkit::build::{answer, permission};
    #[test]
    fn durable_cancel_wins_even_before_actor_sees_its_message() {
        let d = tempfile::tempdir().unwrap();
        let (j, _) = Journal::open(&d.path().join("j")).unwrap();
        let op = OperationId::new("run").unwrap();
        let resolve = OperationId::new("resolve").unwrap();
        let session = SessionId::new("s").unwrap();
        let exec = ExecutionRef::Turn {
            session: session.clone(),
            turn: TurnId::new("t").unwrap(),
            attempt: AttemptId::new("a").unwrap(),
        };
        let req = permission("p", exec.clone(), "echo hello");
        j.tx(|t| {
            t.accept(
                &op,
                &Op {
                    method: "start_turn".into(),
                    digest: Digest::sha256(b"run"),
                    receipt: Receipt {
                        operation: op.clone(),
                        state: Observed::Running,
                    },
                    execution: Some(exec),
                    workspace: None,
                    cancel: false,
                },
                Some(&session),
                1,
            )?;
            t.put_wait(&op, &session, &req)?;
            t.put_answer(&resolve, &answer(&req, PermissionDecision::AllowOnce))?;
            t.answer_for_send(&resolve, 1)
        })
        .unwrap();
        j.tx(|t| t.set_cancel(&op)).unwrap(); // no actor Cancel message has been consumed
        assert_eq!(
            j.tx(|t| t.answer_for_send(&resolve, 1)).unwrap_err().code,
            ErrorCode::Stale
        );
        j.read(|t| {
            assert!(t.wait(&req.id)?.is_some());
            assert!(t.answer_of(&resolve)?.is_some());
            Ok(())
        })
        .unwrap();
    }
}

#[cfg(test)]
mod retention_tests {
    use super::*;

    fn add(
        t: &Tx,
        name: &str,
        accepted: i64,
        finished: bool,
        execution: Option<ExecutionRef>,
        acked: bool,
    ) -> ApiResult<Watermark> {
        let operation = OperationId::new(name).unwrap();
        let receipt: Receipt = from(&json(&serde_json::json!({"operation": name, "state": {"state": if finished { "finished" } else { "running" }, "outcome": "succeeded"}}))).unwrap();
        t.accept(
            &operation,
            &Op {
                method: "test".into(),
                digest: Digest::sha256(name.as_bytes()),
                receipt: receipt.clone(),
                execution: execution.clone(),
                workspace: None,
                cancel: false,
            },
            None,
            accepted,
        )?;
        let mut mark = None;
        for subject in std::iter::once(Subject::Operation {
            operation: operation.clone(),
        })
        .chain(execution.map(Subject::Execution))
        {
            let stream = StreamId::new(crate::stream_of(&subject)).unwrap();
            let ev = t.record(&stream, |cursor| serde_json::from_value(serde_json::json!({
                "schema": EVENT_SCHEMA, "id": format!("ev_{name}_{}",cursor.seq), "at": accepted,
                "cursor": cursor, "subject": subject, "body": {"type": "operation_changed", "receipt": receipt}
            })).unwrap())?;
            if acked {
                t.ack(&EventAck {
                    durable: vec![ev.cursor.clone()],
                })?;
            }
            mark = Some(ev.cursor);
        }
        t.save_spec(&operation, &serde_json::json!({}))?;
        Ok(mark.unwrap())
    }

    fn consistent(j: &Journal) {
        j.read(|t| {
            let sql: Vec<String> = t.rows("SELECT json_object('stream',stream,'epoch',epoch,'produced',produced,'acknowledged',acknowledged) FROM streams ORDER BY stream", [], |r| r.get(0))?;
            let sql: Vec<StreamHead> = sql.iter().map(|s| from(s).unwrap()).collect();
            assert_eq!(t.heads()?,sql);
            Ok(())
        }).unwrap();
    }

    fn sweep(j: &Journal, now: i64) {
        j.tx(|t| {
            t.c()
                .execute(
                    "DELETE FROM meta WHERE key IN ('prune_after','prune_cursor')",
                    [],
                )
                .map_err(internal)?;
            Ok(())
        })
        .unwrap();
        while j.tx(|t| t.prune(now)).unwrap() == PRUNE_BATCH {}
    }

    #[test]
    fn pruning_retains_live_unacked_recent_session_and_referenced_state_across_restart() {
        let d = tempfile::tempdir().unwrap();
        let path = d.path().join("j");
        let (j, _) = Journal::open(&path).unwrap();
        let now = crate::now();
        let old = now - RETENTION_MS - 1;
        let job = |job: &str, attempt: &str| {
            Some(ExecutionRef::Job {
                job: JobId::new(job).unwrap(),
                attempt: AttemptId::new(attempt).unwrap(),
            })
        };
        j.tx(|t| {
            for (name, age, finished, acked) in [
                ("old", old, true, true),
                ("young", now, true, true),
                ("unacked", old, true, false),
                ("live", old, false, true),
                ("installation", old, true, true),
            ] {
                add(t, name, age, finished, None, acked)?;
            }
            add(
                t,
                "turn",
                old,
                true,
                Some(ExecutionRef::Turn {
                    session: SessionId::new("session").unwrap(),
                    turn: TurnId::new("turn").unwrap(),
                    attempt: AttemptId::new("turn_attempt").unwrap(),
                }),
                true,
            )?;
            add(t, "job_old", old, true, job("shared", "a1"), true)?;
            add(t, "job_live", old, false, job("shared", "a2"), true)?;
            add(t, "job_last", old, true, job("last", "a3"), true)?;
            t.take_attempt(
                &job("last", "a3").unwrap(),
                &OperationId::new("job_last").unwrap(),
            )?;
            t.c()
                .execute(
                    "INSERT INTO reservations (key, op) VALUES ('owned','old')",
                    [],
                )
                .map_err(internal)?;
            t.set_installation("test", &OperationId::new("installation").unwrap())?;
            Ok(())
        })
        .unwrap();
        sweep(&j, now);
        consistent(&j);
        let before = j
            .read(|t| {
                let ids: Vec<_> = t
                    .ops()?
                    .into_iter()
                    .map(|(id, _)| id.as_str().to_owned())
                    .collect();
                assert_eq!(
                    ids,
                    ["installation", "job_live", "live", "unacked", "young"]
                );
                assert_eq!(t.heads()?.len(), 7); // Five retained op streams, the shared job and session.
                assert_eq!(
                    t.c()
                        .query_row("SELECT count(*) FROM attempts", [], |r| r.get::<_, i64>(0))
                        .unwrap(),
                    0
                );
                assert_eq!(
                    t.c()
                        .query_row("SELECT count(*) FROM reservations", [], |r| r
                            .get::<_, i64>(0))
                        .unwrap(),
                    0
                );
                assert!(t.spec(&OperationId::new("old").unwrap())?.is_none());
                Ok(t.heads()?)
            })
            .unwrap();
        drop(j);
        let (j, _) = Journal::open(&path).unwrap();
        consistent(&j);
        j.read(|t| {
            assert_eq!(t.heads()?, before);
            assert!(matches!(
                t.op(&OperationId::new("live").unwrap())?
                    .unwrap()
                    .receipt
                    .state,
                Observed::Unknown { .. }
            ));
            assert!(t.op(&OperationId::new("old").unwrap())?.is_none());
            Ok(())
        })
        .unwrap();
    }

    #[test]
    fn record_ack_prune_and_failed_commit_publish_only_committed_heads() {
        let d = tempfile::tempdir().unwrap();
        let (j, _) = Journal::open(&d.path().join("j")).unwrap();
        let now = crate::now();
        let mark = j.tx(|t| add(t, "op", now, true, None, false)).unwrap();
        let ack = EventAck {
            durable: vec![mark.clone()],
        };
        assert!(j.tx(|t| t.ack(&ack)).unwrap());
        assert!(!j.tx(|t| t.ack(&ack)).unwrap());
        consistent(&j);
        let baseline = j.read(|t| t.heads()).unwrap();
        assert!(
            j.tx(|t| {
                add(t, "rolled_back", now, true, None, false)?;
                let mut bad = mark.clone();
                bad.stream = StreamId::new("missing").unwrap();
                t.ack(&EventAck { durable: vec![bad] })
            })
            .is_err()
        );
        assert!(
            j.tx::<()>(|t| {
                t.c()
                    .execute(
                        "DELETE FROM meta WHERE key IN ('prune_after','prune_cursor')",
                        [],
                    )
                    .map_err(internal)?;
                t.prune(now + RETENTION_MS + 1)?;
                assert!(t.heads()?.is_empty());
                Err(internal("rollback after prune"))
            })
            .is_err()
        );
        assert_eq!(j.read(|t| t.heads()).unwrap(), baseline);
        consistent(&j);
        j.call(|w| w.db.execute_batch("PRAGMA foreign_keys=ON; CREATE TABLE parent(id PRIMARY KEY); CREATE TABLE child(id REFERENCES parent(id) DEFERRABLE INITIALLY DEFERRED);").map_err(internal)).unwrap();
        assert!(
            j.tx(|t| {
                add(t, "commit_failure", now, true, None, true)?;
                t.c()
                    .execute("INSERT INTO child (id) VALUES (1)", [])
                    .map_err(internal)?;
                Ok(())
            })
            .is_err()
        );
        consistent(&j);
        assert_eq!(j.read(|t| t.heads()).unwrap(), baseline);
        for (epoch, seq, code) in [
            ("wrong", 1, ErrorCode::Stale),
            (mark.epoch.as_str(), 2, ErrorCode::Invalid),
        ] {
            let mut bad = mark.clone();
            bad.epoch = Epoch::new(epoch).unwrap();
            bad.seq = seq;
            assert_eq!(
                j.tx(|t| t.ack(&EventAck { durable: vec![bad] }))
                    .unwrap_err()
                    .code,
                code
            );
        }
    }

    #[test]
    fn ack_and_heads_work_without_a_streams_table_to_select() {
        let d = tempfile::tempdir().unwrap();
        let (j, _) = Journal::open(&d.path().join("j")).unwrap();
        let mark = j
            .tx(|t| add(t, "cached", crate::now(), true, None, true))
            .unwrap();
        // Every SELECT from streams would fail, including a per-watermark SELECT. The redundant
        // ack needs no writes, and the entire heads vector must still come from committed memory.
        j.tx(|t| {
            t.c()
                .execute("ALTER TABLE streams RENAME TO hidden_streams", [])
                .map_err(internal)?;
            Ok(())
        })
        .unwrap();
        j.read(|t| {
            assert!(!t.ack(&EventAck {
                durable: vec![mark]
            })?);
            assert_eq!(t.heads()?.len(), 1);
            Ok(())
        })
        .unwrap();
    }

    #[test]
    fn failed_pruning_cannot_roll_back_a_committed_ack() {
        let (j, _) = Journal::open(Path::new(":memory:")).unwrap();
        let now = crate::now();
        let mark = j
            .tx(|t| add(t, "old", now - RETENTION_MS - 1, true, None, false))
            .unwrap();
        j.tx(|t| {
            t.c().execute_batch("CREATE TRIGGER fail_prune BEFORE DELETE ON ops BEGIN SELECT RAISE(ABORT,'prune failed'); END").map_err(internal)?;
            t.ack(&EventAck { durable: vec![mark.clone()] })
        }).unwrap();
        assert!(j.tx(|t| t.prune(now)).is_err());
        consistent(&j);
        j.read(|t| {
            assert!(t.covered(&[mark]));
            assert_eq!(t.heads()?[0].acknowledged, 1);
            assert!(t.unacked_past(&HashMap::new(), 10)?.is_empty());
            assert!(t.op(&OperationId::new("old").unwrap())?.is_some());
            Ok(())
        })
        .unwrap();
    }

    #[test]
    fn pruning_is_bounded_and_hourly() {
        let d = tempfile::tempdir().unwrap();
        let (j, _) = Journal::open(&d.path().join("j")).unwrap();
        let now = crate::now();
        j.tx(|t| {
            for i in 0..(PRUNE_BATCH + 1) {
                add(
                    t,
                    &format!("op{i}"),
                    now - RETENTION_MS - 1,
                    true,
                    None,
                    false,
                )?;
            }
            Ok(())
        })
        .unwrap();
        // Unacknowledged operations consume scan slots but survive, so each sweep is bounded.
        assert_eq!(j.tx(|t| t.prune(now)).unwrap(), PRUNE_BATCH);
        assert_eq!(j.tx(|t| t.prune(now)).unwrap(), 1);
        assert_eq!(j.tx(|t| t.prune(now)).unwrap(), 0);
        assert_eq!(j.tx(|t| t.prune(now + 60 * 60_000)).unwrap(), PRUNE_BATCH);
        consistent(&j);
    }
}
