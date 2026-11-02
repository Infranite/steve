//! Time as a dependency (B1), so schedules, expiries and retention can be tested without waiting.

use std::sync::atomic::{AtomicI64, Ordering};

/// Unix milliseconds, like `Date.now()`.
pub trait Clock: Send + Sync {
    fn now(&self) -> i64;
}

/// The real time (`steve_store::now`).
#[derive(Debug, Clone, Copy, Default)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> i64 {
        steve_store::now()
    }
}

/// A clock that only moves when told to.
#[derive(Debug, Default)]
pub struct FakeClock(AtomicI64);

impl FakeClock {
    pub fn new(at: i64) -> Self {
        Self(AtomicI64::new(at))
    }

    pub fn set(&self, at: i64) {
        self.0.store(at, Ordering::SeqCst);
    }

    /// Moves forward by `ms` and returns the new time.
    pub fn advance(&self, ms: i64) -> i64 {
        self.0.fetch_add(ms, Ordering::SeqCst) + ms
    }
}

impl Clock for FakeClock {
    fn now(&self) -> i64 {
        self.0.load(Ordering::SeqCst)
    }
}
