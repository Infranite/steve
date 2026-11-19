//! One loop for "run what is due" (W2b §2, R0b): the domain keeps its due times in its own table, says
//! when the next one is (`next_due`) and runs a batch (`run_due`). No scheduler object, queue or registry:
//! each domain calls this directly. Row-level failures belong in the domain's `run_due` (its own retry
//! columns); this loop only backs off when the whole source fails.
use crate::{Clock, policy::Backoff};
use std::{fmt::Display, future::Future, sync::Arc, time::Duration};
use tokio::sync::Notify;

/// The longest sleep: a writer that forgot to wake the loop, a restore, a wall-clock step or a system
/// sleep is noticed within this.
pub const FALLBACK: Duration = Duration::from_secs(60);

/// Runs forever (cancel by dropping or aborting its task). Sleeps until `next_due` or at most
/// [`FALLBACK`], or until `wake` is notified (use `notify_one`, so a wake before the wait is kept).
/// After each batch `run_due(now)` the loop asks `next_due` again, so a batch limit only means another
/// round without waiting. Errors from either back off with `backoff`; a success resets it.
pub async fn due_loop<E, N, NF, R, RF>(
    name: &'static str,
    clock: Arc<dyn Clock>,
    wake: Arc<Notify>,
    backoff: Backoff,
    mut next_due: N,
    mut run_due: R,
) where
    E: Display,
    N: FnMut() -> NF,
    NF: Future<Output = Result<Option<i64>, E>>,
    R: FnMut(i64) -> RF,
    RF: Future<Output = Result<(), E>>,
{
    let lag = metrics::histogram!("steve_due_lag_seconds", "loop" => name);
    let fallback = FALLBACK.as_millis() as i64;
    let mut failures = 0u32;
    loop {
        let now = clock.now();
        let step = match next_due().await {
            Ok(Some(at)) if at <= now => {
                lag.record((now - at) as f64 / 1000.0);
                // after a batch, ask again: more may be due, or the next due time moved
                run_due(now).await.map(|()| None)
            }
            Ok(next) => Ok(Some(next.map_or(fallback, |at| (at - now).min(fallback)))),
            Err(e) => Err(e),
        };
        match step {
            Ok(None) => failures = 0,
            Ok(Some(ms)) => {
                failures = 0;
                let sleep = clock.sleep(Duration::from_millis(ms as u64));
                let woken = wake.notified();
                futures::pin_mut!(sleep, woken);
                futures::future::select(sleep, woken).await;
            }
            Err(e) => {
                tracing::warn!("{name}: {e}; trying again");
                let d = backoff.delay(failures, &mut rand::rng());
                failures = failures.saturating_add(1);
                clock.sleep(d).await;
            }
        }
    }
}
