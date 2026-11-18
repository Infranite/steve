//! Retry deadlines use Unix milliseconds, including after a coordinator restart.

use std::future::Future;
use std::pin::Pin;
use std::time::Duration;

/// Time used to schedule, lease and expire runtime retries. A prepared sleep may wake early
/// when time advances: the sender always rechecks durable deadlines before leasing. Preparing
/// before a send also observes advances during that send. Dropping a sleep cancels the wait.
pub trait Clock: Send + Sync + 'static {
    fn now_ms(&self) -> i64;
    fn sleep(&self, duration: Duration) -> Pin<Box<dyn Future<Output = ()> + Send + '_>>;
}

pub(super) struct SystemClock;

impl Clock for SystemClock {
    fn now_ms(&self) -> i64 {
        steve_store::now()
    }

    fn sleep(&self, duration: Duration) -> Pin<Box<dyn Future<Output = ()> + Send + '_>> {
        // Start the timer when polled, preserving the worker's interval after each send.
        Box::pin(async move { tokio::time::sleep(duration).await })
    }
}
