//! Running what is due (W2b §2, R0b). A domain keeps its due times in its own table and implements
//! [`DueSource`]; [`due_loop`] runs one source as one task. No scheduler object, central queue or
//! registry: tokio already queues the timers, and write fairness belongs to the storage executor.
//! Every source must pass [`contract::check`] (feature `due-contract`), which pins the B3 class of
//! defects: filtering by due time before the batch limit, a failing row not blocking later rows, and
//! dead letters taking no further part (or continuing sources retrying individual rows).
use crate::{Clock, policy::Backoff};
use std::{fmt, future::Future, sync::Arc, time::Duration};
use tokio::sync::Notify;

mod row_deadlines;
pub use row_deadlines::RowDeadlines;

/// The longest sleep: a writer that forgot to wake the loop, a restore, a wall-clock step or a system
/// sleep is noticed within this.
pub const FALLBACK: Duration = Duration::from_secs(60);

/// A source failed as a whole (its store or a dependency); a single row's failure is the source's own
/// business (its retry columns and dead letters), not an error here.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DueError(pub String);
impl fmt::Display for DueError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}
impl std::error::Error for DueError {}

/// Work kept in a domain's own table, due at known times.
///
/// A row's next due time has exactly two homes: a column in storage, or [`RowDeadlines`].
/// Do not add another private per-row deadline map.
///
/// The contract (checked by [`contract::check`]):
/// - `next_due` is the earliest due time of rows that can still run (dead letters excluded).
/// - `run_due(now)` selects rows with `due <= now` **before** applying its batch limit, ordered by due
///   time, and handles each row on its own: a row that fails gets its own later due time (the domain's
///   [`Backoff`]) and, if the domain has a failure limit, becomes a dead letter that neither method
///   sees again. Continuing sources may retry indefinitely. Other rows of the batch go on.
pub trait DueSource: Send + Sync + 'static {
    /// The loop's name, also its metric label.
    fn name(&self) -> &'static str;
    fn next_due(&self) -> impl Future<Output = Result<Option<i64>, DueError>> + Send;
    fn run_due(&self, now: i64) -> impl Future<Output = Result<(), DueError>> + Send;
}

/// Runs `source` forever (cancel by aborting its task). Sleeps until the next due time, at most
/// [`FALLBACK`], or until `wake` is notified (use `notify_one`, so a wake before the wait is kept).
/// After each batch it asks `next_due` again, so a batch limit only means another round at once.
/// A source error backs off with `backoff`; a success resets it.
pub async fn due_loop<S: DueSource>(
    source: Arc<S>,
    clock: Arc<dyn Clock>,
    wake: Arc<Notify>,
    backoff: Backoff,
) {
    let name = source.name();
    let lag = metrics::histogram!("steve_due_lag_seconds", "loop" => name);
    let runs_ok = metrics::counter!("steve_due_runs_total", "loop" => name, "result" => "ok");
    let runs_err = metrics::counter!("steve_due_runs_total", "loop" => name, "result" => "err");
    let run_duration = metrics::histogram!("steve_due_run_duration_seconds", "loop" => name);
    let err_next =
        metrics::counter!("steve_due_errors_total", "loop" => name, "stage" => "next_due");
    let err_run = metrics::counter!("steve_due_errors_total", "loop" => name, "stage" => "run_due");
    let job_runs = metrics::counter!("steve_job_runs_total", "job" => name);
    let job_duration = metrics::histogram!("steve_job_duration_seconds", "job" => name);
    let fallback = FALLBACK.as_millis() as i64;
    let mut failures = 0u32;
    loop {
        let next = source.next_due().await;
        // A storage query may yield for longer than the remaining wait. Anchor both the
        // deadline comparison and sleep to the time after that query completes.
        let now = clock.now();
        let step = match next {
            Ok(Some(at)) if at <= now => {
                lag.record((now - at) as f64 / 1000.0);
                job_runs.increment(1);
                let started = std::time::Instant::now();
                let res = source.run_due(now).await;
                let elapsed = started.elapsed().as_secs_f64();
                run_duration.record(elapsed);
                job_duration.record(elapsed);
                match res {
                    Ok(()) => {
                        runs_ok.increment(1);
                        Ok(None)
                    }
                    Err(e) => {
                        runs_err.increment(1);
                        err_run.increment(1);
                        Err(e)
                    }
                }
            }
            Ok(next) => Ok(Some(next.map_or(fallback, |at| (at - now).min(fallback)))),
            Err(e) => {
                err_next.increment(1);
                Err(e)
            }
        };
        match step {
            Ok(None) => failures = 0,
            Ok(Some(ms)) => {
                failures = 0;
                let sleep = clock.sleep(Duration::from_millis(ms as u64));
                let woken = wake.clone().notified_owned();
                futures::pin_mut!(sleep, woken);
                futures::future::select(sleep, woken).await;
            }
            Err(e) => {
                tracing::warn!("{name}: {e}; trying again");
                let d = backoff.sample_delay(failures);
                failures = failures.saturating_add(1);
                clock.sleep(d).await;
            }
        }
    }
}

/// The shared contract test every [`DueSource`] implementation runs against its real storage.
#[cfg(feature = "due-contract")]
pub mod contract {
    use super::DueSource;

    /// What the check needs from a source's test setup: add rows, and read back what happened.
    pub trait Fixture {
        type Source: DueSource;
        fn source(&self) -> &Self::Source;
        /// The source's batch limit.
        fn batch(&self) -> usize;
        /// Adds a row due at `at`; a `poison` row fails every time it runs. Returns its id.
        fn add(&self, at: i64, poison: bool) -> String;
        /// Ids of rows that ran successfully, in any order.
        fn done(&self) -> Vec<String>;
        /// Whether this domain retires failing rows. Continuing sources must back off per row.
        fn dead_letters(&self) -> bool {
            true
        }
        /// Ids of rows that became dead letters.
        fn dead(&self) -> Vec<String>;
        /// Expected next deadline of successful recurring rows; one-shot sources have none.
        /// The fixture computes this from healthy rows, never including the poison row.
        fn remaining_due(&self) -> Option<i64> {
            None
        }
    }

    /// Runs the three checks on fresh fixtures from `new`, starting at `now`. `rounds` bounds how many
    /// failing runs a poison row needs before it is a dead letter. Panics with the broken rule.
    pub async fn check<F: Fixture>(new: impl Fn() -> F, now: i64, rounds: u32) {
        const DAY: i64 = 86_400_000;
        // 1. due rows are found even when more than a batch of later rows sort before them
        let f = new();
        for _ in 0..f.batch() + 10 {
            f.add(now + DAY, false);
        }
        let due = f.add(now, false);
        f.source().run_due(now).await.expect("run_due");
        assert!(
            f.done().contains(&due),
            "{}: filter by due time before the batch limit",
            f.source().name()
        );
        // 2. a row that always fails does not hold back the rows due after it
        let f = new();
        let poison = f.add(now - 1, true);
        let ok = f.add(now, false);
        f.source()
            .run_due(now)
            .await
            .expect("run_due with a failing row");
        assert!(
            f.done().contains(&ok),
            "{}: a failing row must not block later rows",
            f.source().name()
        );
        // Continuing sources have no terminal row to exclude. Isolate their poison row so
        // healthy recurring deadlines cannot mask a hot retry. Bounded sources keep the second
        // fixture: factories may own exactly two setups, and healthy recurring rows must remain.
        if !f.dead_letters() {
            let f = new();
            let poison = f.add(now, true);
            let mut at = now;
            for _ in 0..rounds {
                f.source()
                    .run_due(at)
                    .await
                    .expect("run_due with a failing row");
                assert!(
                    !f.done().contains(&poison),
                    "{}: a failing row is not successful",
                    f.source().name()
                );
                let next = f.source().next_due().await.expect("next_due");
                assert!(
                    next.is_some_and(|next| next > at),
                    "{}: a failing row must get its own later due time",
                    f.source().name()
                );
                at = next.unwrap();
            }
            return;
        }
        // 3. A bounded source retires the failing row without losing healthy recurring work.
        let mut at = now;
        for _ in 0..rounds {
            if f.dead().contains(&poison) {
                break;
            }
            at += DAY;
            f.source().run_due(at).await.expect("run_due");
        }
        let name = f.source().name();
        assert!(
            f.dead().contains(&poison),
            "{name}: a row failing {rounds} times must become a dead letter"
        );
        assert_eq!(
            f.source().next_due().await.expect("next_due"),
            f.remaining_due(),
            "{name}: dead letters are not due"
        );
        f.source().run_due(at + DAY).await.expect("run_due");
        assert!(
            !f.done().contains(&poison),
            "{name}: a dead letter does not run"
        );
    }
}
