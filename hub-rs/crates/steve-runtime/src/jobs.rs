//! Named, owned Tokio tasks. No scheduler or task registry lives in the runtime.
use crate::Clock;
use futures::FutureExt;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};
use std::time::{Duration, Instant};
use tokio::task::JoinHandle;

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
                    let message = payload
                        .downcast_ref::<String>()
                        .map(String::as_str)
                        .or_else(|| payload.downcast_ref::<&str>().copied())
                        .unwrap_or("non-string panic payload");
                    report_panic(name, message);
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
                    report_panic(self.name, &error.to_string());
                }
                Poll::Ready(())
            }
        }
    }
}

fn report_panic(job: &'static str, message: &str) {
    metrics::counter!("steve_job_panics_total", "job" => job).increment(1);
    tracing::error!(job, panic = message, "background task panicked");
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

struct Run {
    job: &'static str,
    started: Instant,
}
impl Run {
    fn new(job: &'static str) -> Self {
        metrics::counter!("steve_job_runs_total", "job" => job).increment(1);
        Self {
            job,
            started: Instant::now(),
        }
    }
}
impl Drop for Run {
    fn drop(&mut self) {
        metrics::histogram!("steve_job_duration_seconds", "job" => self.job)
            .record(self.started.elapsed().as_secs_f64());
    }
}

/// Starts one named future. Its lifetime is one measured run, including cancellation or panic.
pub fn spawn(name: &'static str, future: impl Future<Output = ()> + Send + 'static) -> Task {
    Task::start(name, async move {
        let _run = Run::new(name);
        future.await;
    })
}

#[derive(Clone, Copy)]
pub enum FirstRun {
    Immediate,
    AfterPeriod,
}

/// Periodic, non-overlapping work. The first run and missed-tick behavior are explicit.
/// A late tick resets the deadline; missed ticks are never replayed in a burst.
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
        if matches!(first, FirstRun::AfterPeriod) {
            clock.sleep(period).await;
        }
        loop {
            let run = async {
                let _run = Run::new(name);
                f().await;
            };
            match cadence {
                Cadence::FromStart => {
                    futures::future::join(clock.sleep(period), run).await;
                }
                Cadence::AfterWork => {
                    run.await;
                    clock.sleep(period).await;
                }
            }
        }
    })
}
