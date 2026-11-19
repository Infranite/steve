//! Shared condition waiting for tests. The bound is deliberately global: slow machines may delay
//! observations, while a condition that already holds never pays for the bound or a polling tick.
use std::{future::Future, pin::Pin, time::Duration};
use tokio::{sync::Notify, time::Instant};

pub const EVENTUALLY: Duration = Duration::from_secs(60);
const POLL: Duration = Duration::from_millis(10);

/// Returns true once `condition` holds; panics with `what` at the shared deadline.
/// The boolean preserves assertions at call sites; a false result is never silently ignored.
pub async fn eventually<F, Fut>(what: &str, condition: F) -> bool
where
    F: FnMut() -> Fut,
    Fut: Future<Output = bool>,
{
    wait(what, None, condition).await
}

/// Synchronous condition adapter; the deadline and polling implementation are shared.
pub async fn eventually_sync<F>(what: &str, mut condition: F) -> bool
where
    F: FnMut() -> bool,
{
    wait(what, None, || std::future::ready(condition())).await
}

/// Event-driven version of [`eventually`]. Register before checking, so notifications during an
/// asynchronous state read are retained. A notification only requests another condition check.
pub async fn eventually_notified<F, Fut>(what: &str, wake: &Notify, condition: F) -> bool
where
    F: FnMut() -> Fut,
    Fut: Future<Output = bool>,
{
    wait(what, Some(wake), condition).await
}

async fn wait<F, Fut>(what: &str, wake: Option<&Notify>, mut condition: F) -> bool
where
    F: FnMut() -> Fut,
    Fut: Future<Output = bool>,
{
    let deadline = Instant::now() + EVENTUALLY;
    loop {
        let next: Pin<Box<dyn Future<Output = ()> + Send + '_>> = match wake {
            Some(wake) => {
                let mut notified = Box::pin(wake.notified());
                notified.as_mut().enable();
                notified
            }
            None => Box::pin(tokio::time::sleep(POLL)),
        };
        let ready = tokio::time::timeout_at(deadline, condition()).await;
        if matches!(ready, Ok(true)) {
            return true;
        }
        assert!(
            ready.is_ok() && Instant::now() < deadline,
            "timed out waiting for {what}"
        );
        assert!(
            tokio::time::timeout_at(deadline, next).await.is_ok(),
            "timed out waiting for {what}"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    };

    #[tokio::test(start_paused = true)]
    async fn ready_conditions_return_without_advancing_time() {
        let before = Instant::now();
        assert!(eventually("ready", || async { true }).await);
        assert!(eventually_sync("ready sync", || true).await);
        assert_eq!(Instant::now(), before);
    }

    #[tokio::test(start_paused = true)]
    #[should_panic(expected = "timed out waiting for a missing state")]
    async fn a_missing_condition_panics_with_its_description() {
        eventually("a missing state", || async { false }).await;
    }

    #[tokio::test(start_paused = true)]
    #[should_panic(expected = "timed out waiting for a stalled state read")]
    async fn the_deadline_also_bounds_the_condition_read() {
        eventually("a stalled state read", std::future::pending::<bool>).await;
    }

    #[tokio::test(start_paused = true)]
    async fn a_notification_during_a_state_read_is_not_lost() {
        let wake = Arc::new(Notify::new());
        let ready = Arc::new(AtomicBool::new(false));
        let (started, entered) = tokio::sync::oneshot::channel();
        let (release, released) = tokio::sync::oneshot::channel();
        let (w, r) = (wake.clone(), ready.clone());
        let worker = tokio::spawn(async move {
            entered.await.unwrap();
            r.store(true, Ordering::SeqCst);
            w.notify_waiters();
            release.send(()).unwrap();
        });
        let mut first = Some((started, released));
        assert!(
            eventually_notified("notified state", &wake, || {
                let sampled = ready.load(Ordering::SeqCst);
                let gate = first.take();
                async move {
                    if let Some((started, released)) = gate {
                        started.send(()).unwrap();
                        released.await.unwrap();
                    }
                    sampled
                }
            })
            .await
        );
        worker.await.unwrap();
    }
}
