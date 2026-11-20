//! Shared condition waiting for tests. The bound is deliberately global: slow machines may delay
//! observations, while a condition that already holds never pays for the bound or a polling tick.
use std::{future::Future, pin::Pin, time::Duration};
use tokio::{sync::Notify, time::Instant};

pub const EVENTUALLY: Duration = Duration::from_secs(60);
const POLL: Duration = Duration::from_millis(10);

/// A timeout description. Expensive state dumps are rendered only after waiting fails.
pub trait Description {
    fn describe(self) -> impl Future<Output = String> + Send;
}
impl Description for &str {
    async fn describe(self) -> String {
        self.to_owned()
    }
}
impl Description for String {
    async fn describe(self) -> String {
        self
    }
}
impl Description for &String {
    async fn describe(self) -> String {
        self.clone()
    }
}

/// Lazy diagnostics may read asynchronous state as well as format synchronous snapshots.
pub struct OnTimeout<F>(F);
pub fn on_timeout<F, Fut>(description: F) -> OnTimeout<F>
where
    F: FnOnce() -> Fut + Send,
    Fut: Future<Output = String> + Send,
{
    OnTimeout(description)
}
impl<F, Fut> Description for OnTimeout<F>
where
    F: FnOnce() -> Fut + Send,
    Fut: Future<Output = String> + Send,
{
    async fn describe(self) -> String {
        (self.0)().await
    }
}

/// Blocking adapter for synchronous tests. Conditions run outside the temporary runtime so
/// tested code cannot detach work onto a runtime that disappears when this wait returns.
/// The runtime only drives the shared poll wait and renders timeout diagnostics.
pub fn eventually_blocking<D: Description, F: FnMut() -> bool>(what: D, mut condition: F) -> bool {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .build()
        .expect("condition-wait runtime");
    let deadline = Instant::now() + EVENTUALLY;
    loop {
        if condition() {
            return true;
        }
        let waiting = Instant::now() < deadline
            && runtime.block_on(async {
                tokio::time::timeout_at(deadline, next_wait(None))
                    .await
                    .is_ok()
            });
        if !waiting {
            panic!(
                "timed out waiting for {}",
                runtime.block_on(what.describe())
            );
        }
    }
}

/// Observes a fixed negative window with Tokio's current-thread clock paused.
/// Unlike `advance`, awaiting the virtual sleep lets the runtime visit intermediate deadlines
/// and poll the tasks they wake before the window ends. Callers assert the absence of an effect
/// after this returns; effects exactly at the end boundary and external I/O need their own
/// synchronization. This is not a condition waiter.
///
/// # Panics
/// Panics unless the clock is paused, so this cannot hide a wall-clock wait in a test.
pub async fn quiet_for(window: Duration) {
    tokio::time::advance(Duration::ZERO).await;
    tokio::time::sleep(window).await;
}

/// Returns true once `condition` holds; panics with `what` at the shared deadline.
/// The boolean preserves assertions at call sites; a false result is never silently ignored.
pub async fn eventually<F, Fut>(what: impl Description, condition: F) -> bool
where
    F: FnMut() -> Fut,
    Fut: Future<Output = bool>,
{
    wait(what, None, condition).await
}

/// Synchronous condition adapter; the deadline and polling implementation are shared.
pub async fn eventually_sync<F>(what: impl Description, mut condition: F) -> bool
where
    F: FnMut() -> bool,
{
    wait(what, None, || std::future::ready(condition())).await
}

/// Event-driven version of [`eventually`]. Register before checking, so notifications during an
/// asynchronous state read are retained. A notification only requests another condition check.
pub async fn eventually_notified<F, Fut>(
    what: impl Description,
    wake: &Notify,
    condition: F,
) -> bool
where
    F: FnMut() -> Fut,
    Fut: Future<Output = bool>,
{
    wait(what, Some(wake), condition).await
}

/// One sleep point for async waits and the synchronous adapter.
fn next_wait(wake: Option<&Notify>) -> Pin<Box<dyn Future<Output = ()> + Send + '_>> {
    match wake {
        Some(wake) => {
            let mut notified = Box::pin(wake.notified());
            notified.as_mut().enable();
            notified
        }
        None => Box::pin(tokio::time::sleep(POLL)),
    }
}

async fn wait<F, Fut>(what: impl Description, wake: Option<&Notify>, mut condition: F) -> bool
where
    F: FnMut() -> Fut,
    Fut: Future<Output = bool>,
{
    let deadline = Instant::now() + EVENTUALLY;
    loop {
        let next = next_wait(wake);
        let ready = tokio::time::timeout_at(deadline, condition()).await;
        if matches!(ready, Ok(true)) {
            return true;
        }
        if ready.is_err() || Instant::now() >= deadline {
            panic!("timed out waiting for {}", what.describe().await);
        }
        if tokio::time::timeout_at(deadline, next).await.is_err() {
            panic!("timed out waiting for {}", what.describe().await);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    };

    #[tokio::test]
    #[should_panic(expected = "time is not frozen")]
    async fn quiet_window_rejects_an_unpaused_clock() {
        quiet_for(Duration::from_millis(1)).await;
    }

    #[tokio::test(start_paused = true)]
    async fn quiet_window_runs_timer_woken_tasks_before_returning() {
        let progress = Arc::new(AtomicUsize::new(0));
        let (entered, ready) = tokio::sync::oneshot::channel();
        let work = progress.clone();
        let worker = tokio::spawn(async move {
            let first = tokio::time::sleep(Duration::from_millis(10));
            tokio::pin!(first);
            entered.send(()).unwrap();
            first.await;
            work.fetch_add(1, Ordering::SeqCst);
            tokio::spawn(async move {
                tokio::time::sleep(Duration::from_millis(10)).await;
                work.fetch_add(1, Ordering::SeqCst);
            })
            .await
            .unwrap();
        });
        ready.await.unwrap();
        let started = Instant::now();
        quiet_for(Duration::from_millis(30)).await;
        // Check before joining: awaiting the worker here would mask an advance-only helper.
        assert_eq!(progress.load(Ordering::SeqCst), 2);
        assert!(worker.is_finished());
        assert_eq!(started.elapsed(), Duration::from_millis(30));
        worker.await.unwrap();
    }

    #[test]
    fn blocking_conditions_never_enter_the_temporary_runtime() {
        let mut calls = 0;
        assert!(eventually_blocking("a second poll outside Tokio", || {
            assert!(tokio::runtime::Handle::try_current().is_err());
            calls += 1;
            calls == 2
        }));
        assert_eq!(calls, 2);
        assert!(tokio::runtime::Handle::try_current().is_err());
    }

    #[tokio::test(start_paused = true)]
    async fn successful_waits_do_not_render_diagnostics() {
        assert!(
            eventually(
                on_timeout(|| async { panic!("unused state dump") }),
                || async { true }
            )
            .await
        );
    }

    #[tokio::test(start_paused = true)]
    #[should_panic(expected = "timed out waiting for state dump: last status=waiting")]
    async fn timeout_renders_an_asynchronous_state_dump() {
        eventually(
            on_timeout(|| async {
                tokio::task::yield_now().await;
                format!("state dump: last status={}", "waiting")
            }),
            || async { false },
        )
        .await;
    }

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

/// Unix test process ownership, opt-in with this testkit module.
#[cfg(unix)]
pub mod process;
