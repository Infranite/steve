//! Replaces modules::clock and node_runtime::clock (W2b §1.6).
use futures::future::BoxFuture;
use std::sync::atomic::{AtomicI64, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::sync::watch;

/// Unix milliseconds. Dropping a sleep cancels that wait.
pub trait Clock: Send + Sync {
    fn now(&self) -> i64;
    fn sleep(&self, d: Duration) -> BoxFuture<'_, ()>;
    /// Retry polling may wake early on a wall-clock change; callers recheck durable deadlines.
    /// System time has no change signal, so its normal timer remains the fallback.
    fn sleep_or_change(&self, d: Duration) -> BoxFuture<'_, ()> {
        self.sleep(d)
    }
}
#[derive(Debug, Clone, Copy, Default)]
pub struct SystemClock;
impl Clock for SystemClock {
    fn now(&self) -> i64 {
        match SystemTime::now().duration_since(UNIX_EPOCH) {
            Ok(d) => i64::try_from(d.as_millis()).unwrap_or(i64::MAX),
            Err(e) => -i64::try_from(e.duration().as_millis()).unwrap_or(i64::MAX),
        }
    }
    fn sleep(&self, d: Duration) -> BoxFuture<'_, ()> {
        Box::pin(async move { tokio::time::sleep(d).await })
    }
}
/// Manual wall clock. Sleeps subscribe when prepared, so advances before polling are not lost.
/// Unlike SystemClock, their deadline is relative to preparation, not first polling.
/// Forward updates advance test elapsed time; rewinding wall time never rewinds a pending sleep.
#[derive(Debug)]
pub struct FakeClock {
    time: watch::Sender<Time>,
    sleeping_at: AtomicI64,
}
#[derive(Debug, Clone, Copy)]
struct Time {
    wall: i64,
    elapsed: u128,
}
impl Default for FakeClock {
    fn default() -> Self {
        Self::new(0)
    }
}
impl FakeClock {
    pub fn new(at: i64) -> Self {
        Self {
            time: watch::channel(Time {
                wall: at,
                elapsed: 0,
            })
            .0,
            sleeping_at: AtomicI64::new(i64::MIN),
        }
    }
    pub fn set(&self, at: i64) {
        self.time.send_modify(|t| {
            let forward = (i128::from(at) - i128::from(t.wall)).max(0) as u128;
            t.elapsed = t.elapsed.saturating_add(forward);
            t.wall = at;
        });
    }
    pub fn advance(&self, ms: i64) -> i64 {
        let mut at = 0;
        self.time.send_modify(|t| {
            t.wall = t.wall.saturating_add(ms);
            t.elapsed = t.elapsed.saturating_add(ms.max(0) as u128);
            at = t.wall;
        });
        at
    }
    pub fn advance_to(&self, at: i64) {
        self.time.send_modify(|t| {
            assert!(at >= t.wall, "clock cannot go backwards");
            t.elapsed = t
                .elapsed
                .saturating_add((i128::from(at) - i128::from(t.wall)) as u128);
            t.wall = at;
        });
    }
    /// A sleep has polled and observed `at`, like the old ManualClock handshake.
    pub fn observed(&self, at: i64) -> bool {
        self.sleeping_at.load(Ordering::SeqCst) == at
    }
    /// Wake on any advance, including before the first poll (old ManualClock behavior).
    pub fn changed(&self) -> BoxFuture<'_, ()> {
        self.wait(None)
    }
    fn wait(&self, d: Option<Duration>) -> BoxFuture<'_, ()> {
        let mut rx = self.time.subscribe();
        let deadline = d.map(|d| self.time.borrow().elapsed.saturating_add(d.as_millis()));
        Box::pin(async move {
            loop {
                let now = *rx.borrow();
                if deadline.is_some_and(|at| now.elapsed >= at)
                    || (deadline.is_none() && rx.has_changed().unwrap_or(true))
                {
                    return;
                }
                self.sleeping_at.store(now.wall, Ordering::SeqCst);
                if rx.changed().await.is_err() {
                    return;
                }
                if deadline.is_none() {
                    return;
                }
            }
        })
    }
}
impl Clock for FakeClock {
    fn sleep_or_change(&self, _: Duration) -> BoxFuture<'_, ()> {
        self.changed()
    }

    fn now(&self) -> i64 {
        self.time.borrow().wall
    }
    fn sleep(&self, d: Duration) -> BoxFuture<'_, ()> {
        self.wait(Some(d))
    }
}
