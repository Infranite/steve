//! Named, owned Tokio tasks with per-round measurement and bounded panic recovery.
//! No scheduler or task registry lives in the runtime.
use crate::Clock;
use futures::FutureExt;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};
use std::time::{Duration, Instant};
use tokio::task::JoinHandle;

/// Initial backoff after a background job panics before restarting.
pub const PANIC_BACKOFF_BASE: Duration = Duration::from_secs(1);
/// Maximum backoff between consecutive panic restarts.
pub const PANIC_BACKOFF_MAX: Duration = Duration::from_secs(60);
/// Maximum consecutive panics before a supervised job stops retrying and marks itself down.
pub const PANIC_MAX_CONSECUTIVE: u32 = 5;
/// Healthy run duration after which the consecutive panic counter resets to zero.
pub const PANIC_RESET_AFTER: Duration = Duration::from_secs(60);

/// Dropping the owner or calling `stop` aborts the future at its next yield point.
/// Already running blocking work must finish on its own.
pub struct Task {
    name: &'static str,
    handle: JoinHandle<()>,
    watcher: Option<JoinHandle<()>>,
}
impl Task {
    fn start(name: &'static str, future: impl Future<Output = ()> + Send + 'static) -> Self {
        Self {
            name,
            watcher: None,
            handle: tokio::spawn(async move {
                if let Err(payload) = std::panic::AssertUnwindSafe(future).catch_unwind().await {
                    let message = panic_message(&payload);
                    let meters = JobMeters::new(name);
                    meters.panics.increment(1);
                    meters.up.set(0.0);
                    tracing::error!(job = name, panic = message, "background task panicked");
                }
            }),
        }
    }

    pub fn name(&self) -> &'static str {
        self.name
    }
    pub fn stop(&self) {
        self.handle.abort();
        if let Some(watcher) = &self.watcher {
            watcher.abort();
        }
    }
    /// Stop this owned job when its domain owner disappears. Dropping this wrapper still
    /// cancels both the job and its lifetime watcher; no detached task survives cancellation.
    pub fn until(mut self, ended: impl Future<Output = ()> + Send + 'static) -> Self {
        if let Some(watcher) = self.watcher.take() {
            watcher.abort();
        }
        let abort = self.handle.abort_handle();
        self.watcher = Some(tokio::spawn(async move {
            ended.await;
            abort.abort();
        }));
        self
    }

    pub fn is_finished(&self) -> bool {
        self.handle.is_finished()
    }
}
impl Drop for Task {
    fn drop(&mut self) {
        self.stop();
    }
}
impl Future for Task {
    type Output = ();
    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<()> {
        match Pin::new(&mut self.handle).poll(cx) {
            Poll::Pending => Poll::Pending,
            Poll::Ready(result) => {
                if let Err(error) = result
                    && error.is_panic()
                {
                    let meters = JobMeters::new(self.name);
                    meters.panics.increment(1);
                    meters.up.set(0.0);
                    tracing::error!(job = self.name, panic = %error, "background task panicked");
                }
                Poll::Ready(())
            }
        }
    }
}

fn panic_message(payload: &(dyn std::any::Any + Send)) -> &str {
    payload
        .downcast_ref::<String>()
        .map(String::as_str)
        .or_else(|| payload.downcast_ref::<&str>().copied())
        .unwrap_or("non-string panic payload")
}

/// Bounded reason why a periodic task skipped triggering a model call or session.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SkipReason {
    /// No new input (messages, items, notes, or packets) since the previous watermark.
    NoChange,
    /// Schedule is driven by a durable work queue rather than a blind wall-clock cron.
    QueueDriven,
    /// Task or workspace feature is disabled.
    Disabled,
    /// Wall-clock deadline was missed or not due.
    NotDue,
    /// Previous run or work item is still active.
    Overlap,
}

impl SkipReason {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::NoChange => "no_change",
            Self::QueueDriven => "queue_driven",
            Self::Disabled => "disabled",
            Self::NotDue => "not_due",
            Self::Overlap => "overlap",
        }
    }
}

/// Bounded reason why a model call or session was avoided before dispatch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AvoidReason {
    NoChange,
    QueueDriven,
    Batched,
    Cached,
    Deduplicated,
    Deterministic,
}

impl AvoidReason {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::NoChange => "no_change",
            Self::QueueDriven => "queue_driven",
            Self::Batched => "batched",
            Self::Cached => "cached",
            Self::Deduplicated => "deduplicated",
            Self::Deterministic => "deterministic",
        }
    }
}

/// Bounded token category for periodic-task usage metrics.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TokenKind {
    Input,
    Output,
    CacheRead,
    CacheWrite,
}

impl TokenKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Input => "input",
            Self::Output => "output",
            Self::CacheRead => "cache_read",
            Self::CacheWrite => "cache_write",
        }
    }
}

/// Records that a periodic task's deterministic gate passed and triggered real work.
pub fn record_periodic_triggered(task: &'static str) {
    metrics::counter!(
        "steve_periodic_task_evaluations_total",
        "task" => task,
        "outcome" => "triggered",
        "reason" => "changed"
    )
    .increment(1);
}

/// Records that a periodic task's deterministic gate skipped work without invoking a model or session.
pub fn record_periodic_skipped(task: &'static str, reason: SkipReason) {
    metrics::counter!(
        "steve_periodic_task_evaluations_total",
        "task" => task,
        "outcome" => "skipped",
        "reason" => reason.as_str()
    )
    .increment(1);
}

/// Records tokens spent by a periodic task's model call.
pub fn record_periodic_tokens(task: &'static str, kind: TokenKind, count: u64) {
    if count > 0 {
        metrics::counter!(
            "steve_periodic_task_tokens_total",
            "task" => task,
            "kind" => kind.as_str()
        )
        .increment(count);
    }
}

/// Records all four token buckets reported for a periodic task's model call.
pub fn record_periodic_token_counts(
    task: &'static str,
    input: u64,
    output: u64,
    cache_read: u64,
    cache_write: u64,
) {
    record_periodic_tokens(task, TokenKind::Input, input);
    record_periodic_tokens(task, TokenKind::Output, output);
    record_periodic_tokens(task, TokenKind::CacheRead, cache_read);
    record_periodic_tokens(task, TokenKind::CacheWrite, cache_write);
}

/// Records `count` model calls avoided by gating, batching, caching, or deterministic matching.
pub fn record_model_calls_avoided(purpose: &'static str, reason: AvoidReason, count: u64) {
    if count > 0 {
        metrics::counter!(
            "steve_model_calls_avoided_total",
            "purpose" => purpose,
            "reason" => reason.as_str()
        )
        .increment(count);
    }
}

/// Cached metric handles for one named background job; registered once when the job starts so
/// per-round recording allocates nothing and performs no registry lookup.
#[derive(Clone)]
pub struct JobMeters {
    name: &'static str,
    runs: metrics::Counter,
    duration: metrics::Histogram,
    panics: metrics::Counter,
    restarts: metrics::Counter,
    up: metrics::Gauge,
}

impl JobMeters {
    pub fn new(job: &'static str) -> Self {
        Self {
            name: job,
            runs: metrics::counter!("steve_job_runs_total", "job" => job),
            duration: metrics::histogram!("steve_job_duration_seconds", "job" => job),
            panics: metrics::counter!("steve_job_panics_total", "job" => job),
            restarts: metrics::counter!("steve_job_restarts_total", "job" => job),
            up: metrics::gauge!("steve_job_up", "job" => job),
        }
    }

    /// Marks the job healthy (`steve_job_up = 1`).
    pub fn mark_up(&self) {
        self.up.set(1.0);
    }

    /// Starts one measured round; dropping the guard records its duration.
    pub fn start_round(&self) -> Run {
        self.runs.increment(1);
        Run {
            duration: self.duration.clone(),
            started: Instant::now(),
        }
    }
}

/// Shared bounded panic-restart tracker used by periodic and supervised loops.
pub struct PanicPolicy {
    meters: JobMeters,
    consecutive: u32,
}

impl PanicPolicy {
    pub fn new(meters: JobMeters) -> Self {
        meters.mark_up();
        Self {
            meters,
            consecutive: 0,
        }
    }

    pub fn meters(&self) -> &JobMeters {
        &self.meters
    }

    pub fn record_success(&mut self) {
        self.consecutive = 0;
        self.meters.mark_up();
    }

    /// Records a panic and returns the backoff delay before the next restart, or `None` once the
    /// consecutive panic cap ([`PANIC_MAX_CONSECUTIVE`]) is exceeded (marking `steve_job_up = 0`).
    pub fn on_panic(&mut self, message: &str, ran_for: Duration) -> Option<Duration> {
        self.meters.panics.increment(1);
        tracing::error!(
            job = self.meters.name,
            panic = message,
            "background task panicked"
        );
        if ran_for >= PANIC_RESET_AFTER {
            self.consecutive = 0;
        }
        self.consecutive = self.consecutive.saturating_add(1);
        if self.consecutive > PANIC_MAX_CONSECUTIVE {
            self.meters.up.set(0.0);
            tracing::error!(
                job = self.meters.name,
                panics = self.consecutive,
                "background job exceeded panic restart limit; stopped"
            );
            return None;
        }
        let shift = (self.consecutive - 1).min(16);
        let delay = PANIC_BACKOFF_BASE
            .saturating_mul(1u32 << shift)
            .min(PANIC_BACKOFF_MAX);
        self.meters.restarts.increment(1);
        tracing::warn!(
            job = self.meters.name,
            attempt = self.consecutive,
            backoff_ms = delay.as_millis() as u64,
            "restarting panicked background job"
        );
        Some(delay)
    }
}

/// One measured pass of a background job. Increments `steve_job_runs_total` on creation and
/// records `steve_job_duration_seconds` on drop (including cancellation or panic).
pub struct Run {
    duration: metrics::Histogram,
    started: Instant,
}

impl Run {
    pub fn new(job: &'static str) -> Self {
        thread_local! {
            static HANDLES: std::cell::RefCell<
                std::collections::HashMap<&'static str, (metrics::Counter, metrics::Histogram)>,
            > = std::cell::RefCell::new(std::collections::HashMap::new());
        }
        let duration = HANDLES.with(|cache| {
            let mut cache = cache.borrow_mut();
            let (runs, duration) = cache.entry(job).or_insert_with(|| {
                (
                    metrics::counter!("steve_job_runs_total", "job" => job),
                    metrics::histogram!("steve_job_duration_seconds", "job" => job),
                )
            });
            runs.increment(1);
            duration.clone()
        });
        Self {
            duration,
            started: Instant::now(),
        }
    }
}

impl Drop for Run {
    fn drop(&mut self) {
        self.duration.record(self.started.elapsed().as_secs_f64());
    }
}

/// Measures one iteration of an event- or wake-driven background job.
pub async fn round<T>(job: &'static str, future: impl Future<Output = T>) -> T {
    let _run = Run::new(job);
    future.await
}

/// Starts one named one-shot future. Its lifetime is one measured run. If it panics,
/// `steve_job_panics_total` increments and `steve_job_up` is set to `0` so health flags it.
pub fn spawn(name: &'static str, future: impl Future<Output = ()> + Send + 'static) -> Task {
    Task::start(name, async move {
        let meters = JobMeters::new(name);
        meters.mark_up();
        let _run = meters.start_round();
        future.await;
    })
}

/// Supervises a long-running background task factory (such as a `due_loop` or bus consumer).
/// Normal completion stops cleanly; panics are counted and restarted with capped exponential
/// backoff via `clock`. After [`PANIC_MAX_CONSECUTIVE`] consecutive panics it stops and sets
/// `steve_job_up` to `0`.
pub fn supervise<F, Fut>(name: &'static str, clock: Arc<dyn Clock>, mut make: F) -> Task
where
    F: FnMut() -> Fut + Send + 'static,
    Fut: Future<Output = ()> + Send + 'static,
{
    Task::start(name, async move {
        let mut policy = PanicPolicy::new(JobMeters::new(name));
        loop {
            let started = Instant::now();
            match std::panic::AssertUnwindSafe(make()).catch_unwind().await {
                Ok(()) => break,
                Err(payload) => {
                    let msg = panic_message(&payload);
                    match policy.on_panic(msg, started.elapsed()) {
                        Some(delay) => clock.sleep(delay).await,
                        None => break,
                    }
                }
            }
        }
    })
}

#[derive(Clone, Copy)]
pub enum FirstRun {
    Immediate,
    AfterPeriod,
    After(Duration),
}

/// Periodic, non-overlapping work. The first run and missed-tick behavior are explicit.
/// A late tick resets the deadline; missed ticks are never replayed in a burst.
/// Panicking rounds are restarted with bounded exponential backoff up to [`PANIC_MAX_CONSECUTIVE`].
pub fn every<F, Fut>(
    name: &'static str,
    clock: Arc<dyn Clock>,
    period: Duration,
    first: FirstRun,
    f: F,
) -> Task
where
    F: FnMut() -> Fut + Send + 'static,
    Fut: Future<Output = ()> + Send,
{
    repeating(name, clock, period, first, Cadence::FromStart, f)
}

/// Fixed-delay work: wait a complete period after each callback finishes.
/// Idle waiting is outside the measured run, just as it is for [`every`].
/// Panicking rounds are restarted with bounded exponential backoff up to [`PANIC_MAX_CONSECUTIVE`].
pub fn every_after<F, Fut>(
    name: &'static str,
    clock: Arc<dyn Clock>,
    period: Duration,
    first: FirstRun,
    f: F,
) -> Task
where
    F: FnMut() -> Fut + Send + 'static,
    Fut: Future<Output = ()> + Send,
{
    repeating(name, clock, period, first, Cadence::AfterWork, f)
}

#[derive(Clone, Copy)]
enum Cadence {
    FromStart,
    AfterWork,
}

fn repeating<F, Fut>(
    name: &'static str,
    clock: Arc<dyn Clock>,
    period: Duration,
    first: FirstRun,
    cadence: Cadence,
    mut f: F,
) -> Task
where
    F: FnMut() -> Fut + Send + 'static,
    Fut: Future<Output = ()> + Send,
{
    assert!(!period.is_zero(), "job period must be positive");
    let period =
        Duration::from_millis(u64::try_from(period.as_millis()).unwrap_or(u64::MAX).max(1));
    // The sleep starts with the callback. Waiting for both preserves Delay semantics when a run
    // overruns its period, without anchoring monotonic waits to a mutable wall-clock timestamp.
    Task::start(name, async move {
        let mut policy = PanicPolicy::new(JobMeters::new(name));
        match first {
            FirstRun::Immediate => {}
            FirstRun::AfterPeriod => clock.sleep(period).await,
            FirstRun::After(delay) => {
                if !delay.is_zero() {
                    clock.sleep(delay).await;
                }
            }
        }
        loop {
            let started = Instant::now();
            let meters = policy.meters().clone();
            let outcome = match cadence {
                Cadence::FromStart => {
                    let sleep = clock.sleep(period);
                    let run = std::panic::AssertUnwindSafe(async {
                        let _run = meters.start_round();
                        f().await;
                    })
                    .catch_unwind();
                    futures::pin_mut!(sleep, run);
                    match futures::future::select(run, sleep).await {
                        futures::future::Either::Left((outcome, rest_sleep)) => {
                            if outcome.is_ok() {
                                rest_sleep.await;
                            }
                            outcome
                        }
                        futures::future::Either::Right(((), rest_run)) => rest_run.await,
                    }
                }
                Cadence::AfterWork => {
                    let outcome = std::panic::AssertUnwindSafe(async {
                        let _run = meters.start_round();
                        f().await;
                    })
                    .catch_unwind()
                    .await;
                    if outcome.is_ok() {
                        clock.sleep(period).await;
                    }
                    outcome
                }
            };
            match outcome {
                Ok(()) => policy.record_success(),
                Err(payload) => {
                    let msg = panic_message(&payload);
                    match policy.on_panic(msg, started.elapsed()) {
                        Some(delay) => clock.sleep(delay).await,
                        None => break,
                    }
                }
            }
        }
    })
}
