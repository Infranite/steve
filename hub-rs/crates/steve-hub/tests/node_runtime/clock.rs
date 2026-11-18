use super::Clock;
use std::sync::atomic::{AtomicI64, Ordering};

/// Each fixture owns its clock. Advancing wakes a sleeping sender without racing registration:
/// a watch receiver subscribes when preparing the wait, before the send can run.
pub(super) struct ManualClock {
    time: tokio::sync::watch::Sender<i64>,
    sleeping_at: AtomicI64,
}

impl ManualClock {
    pub(super) fn new() -> Self {
        Self {
            time: tokio::sync::watch::channel(steve_store::now()).0,
            sleeping_at: AtomicI64::new(0),
        }
    }

    pub(super) fn advance_to(&self, at: i64) {
        self.time.send_modify(|now| {
            assert!(at >= *now, "clock cannot go backwards");
            *now = at;
        });
    }

    pub(super) fn observed(&self, at: i64) -> bool {
        self.sleeping_at.load(Ordering::SeqCst) == at
    }
}

impl Clock for ManualClock {
    fn now_ms(&self) -> i64 {
        *self.time.borrow()
    }

    fn sleep(
        &self,
        _: std::time::Duration,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send + '_>> {
        let mut changed = self.time.subscribe();
        Box::pin(async move {
            let now = self.now_ms();
            if !changed.has_changed().unwrap() {
                self.sleeping_at.store(now, Ordering::SeqCst);
                // An advance may wake early; the worker must re-read the persisted deadline.
                let _ = changed.changed().await;
            }
        })
    }
}
