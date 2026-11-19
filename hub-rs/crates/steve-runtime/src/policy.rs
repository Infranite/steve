//! Replaces retry, window/bucket and trailing-throttle shapes in W2b §1.2–1.3.
//! Limiter::check scans the entire key table to evict recovered entries (O(number of keys)).
//! Callers should periodically prune even when requests stop; no background task is started.
//! Default durations use monotonic Tokio time; with_clock injects the domain millisecond clock.
//! Backward wall-clock jumps do not replenish quotas or move limiter time backwards.
use backon::Retryable;
use rand::RngExt;
use std::{
    collections::{HashMap, VecDeque},
    future::Future,
    hash::Hash,
    num::NonZeroU32,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};
use tokio::time::Instant;

#[derive(Debug, Clone, Copy)]
pub enum Jitter {
    None,
    Full,
    Equal,
}
#[derive(Debug, Clone)]
pub struct Backoff {
    pub base: Duration,
    pub max: Duration,
    pub factor: u32,
    pub jitter: Jitter,
    /// Maximum retries AFTER the initial call, not total calls. None means unlimited.
    pub attempts: Option<u32>,
    pub reset_after: Option<Duration>,
}
#[derive(Debug, Clone, Copy)]
pub enum Retry {
    Stop,
    Now,
    After(Duration),
    Backoff,
}
impl Backoff {
    /// Samples a delay with the thread-local RNG, without keeping that RNG across an await.
    /// Call `delay` with an injected RNG when testing jitter distributions.
    pub fn sample_delay(&self, attempt: u32) -> Duration {
        self.delay(attempt, &mut rand::rng())
    }

    /// Zero-based retry index. attempts does not restrict pure delay calculation.
    pub fn delay(&self, attempt: u32, rng: &mut impl rand::Rng) -> Duration {
        let ns = self
            .base
            .as_nanos()
            .saturating_mul(u128::from(self.factor).saturating_pow(attempt))
            .min(self.max.as_nanos());
        let ns = match self.jitter {
            Jitter::None => ns,
            Jitter::Full => rng.random_range(0..=ns),
            // Matches node-wire's half-open equal-jitter upper bound.
            Jitter::Equal => {
                let half = ns / 2;
                half + rng.random_range(0..(ns - half).max(1))
            }
        };
        Duration::new((ns / 1_000_000_000) as u64, (ns % 1_000_000_000) as u32)
    }
    /// Returns this restart's delay and advances the caller-owned retry index.
    pub fn supervised(
        &self,
        attempt: &mut u32,
        ran: Duration,
        rng: &mut impl rand::Rng,
    ) -> Duration {
        if self.reset_after.is_some_and(|d| ran >= d) {
            *attempt = 0;
        }
        let delay = self.delay(*attempt, rng);
        *attempt = attempt.saturating_add(1);
        delay
    }
    pub async fn retry<T, E, F, Fut>(&self, op: F, classify: impl Fn(&E) -> Retry) -> Result<T, E>
    where
        F: FnMut() -> Fut,
        Fut: Future<Output = Result<T, E>>,
    {
        self.retry_with(op, classify, |d| async move { tokio::time::sleep(d).await })
            .await
    }
    /// Inject sleeping for deterministic tests and callers with their own clock.
    pub async fn retry_with<T, E, F, Fut, S, SF>(
        &self,
        op: F,
        classify: impl Fn(&E) -> Retry,
        sleep: S,
    ) -> Result<T, E>
    where
        F: FnMut() -> Fut,
        Fut: Future<Output = Result<T, E>>,
        S: Fn(Duration) -> SF + 'static,
        SF: Future<Output = ()>,
    {
        let delays = std::iter::successors(Some(0u32), |n| Some(n.saturating_add(1)))
            .take(self.attempts.map_or(usize::MAX, |n| n as usize))
            .map(|n| self.delay(n, &mut rand::rng()));
        op.retry(delays)
            .adjust(|e, d| {
                d.and_then(|d| match classify(e) {
                    Retry::Stop => None,
                    Retry::Now => Some(Duration::ZERO),
                    Retry::After(d) => Some(d),
                    Retry::Backoff => Some(d),
                })
            })
            .sleep(sleep)
            .await
    }
}
#[derive(Debug, Clone, Copy)]
pub enum RateLimit {
    /// Replenish `units` per `per`, initially `burst` resource units. Integer debt
    /// preserves fractional unit periods without rounding admission or refill.
    Gcra {
        units: NonZeroU32,
        per: Duration,
        burst: u32,
    },
    Window {
        len: Duration,
        max: u32,
        sliding: bool,
    },
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Wait(pub Duration);
/// A request larger than the bucket can never succeed; waiting cannot fix it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CheckError {
    Wait(Wait),
    InsufficientCapacity { capacity: u32 },
}
struct PolicyClock {
    source: Option<Arc<dyn crate::clock::Clock>>,
    origin: Instant,
    epoch: i64,
    high_water: AtomicU64,
}
impl PolicyClock {
    fn new(source: Option<Arc<dyn crate::clock::Clock>>) -> Self {
        Self {
            epoch: source.as_ref().map_or(0, |c| c.now()),
            source,
            origin: Instant::now(),
            high_water: AtomicU64::new(0),
        }
    }
    fn now(&self) -> Duration {
        let elapsed = match &self.source {
            Some(c) => Duration::from_millis(c.now().saturating_sub(self.epoch).max(0) as u64),
            None => self.origin.elapsed(),
        };
        let nanos = u64::try_from(elapsed.as_nanos()).unwrap_or(u64::MAX);
        Duration::from_nanos(
            self.high_water
                .fetch_max(nanos, Ordering::Relaxed)
                .max(nanos),
        )
    }
}
enum State {
    // Theoretical arrival debt relative to `at`, scaled by units per period.
    // Keep it in u128 so byte-sized rates neither round nor accumulate drift.
    Fractional {
        at: Duration,
        debt: u128,
    },
    Window {
        start: Duration,
        hits: VecDeque<Duration>,
    },
}
struct Entry {
    state: State,
    expires: Duration,
}
pub struct Limiter<K> {
    rate: RateLimit,
    clock: PolicyClock,
    keys: Mutex<HashMap<K, Entry>>,
}
impl<K: Hash + Eq + Clone> Limiter<K> {
    /// Fixed windows begin with the first recorded event for each key.
    /// Panics for zero capacity/period or a GCRA recovery horizon that overflows Duration.
    pub fn new(rate: RateLimit) -> Self {
        Self::build(rate, None)
    }
    pub fn with_clock(rate: RateLimit, clock: Arc<dyn crate::clock::Clock>) -> Self {
        Self::build(rate, Some(clock))
    }
    fn build(rate: RateLimit, clock: Option<Arc<dyn crate::clock::Clock>>) -> Self {
        match rate {
            RateLimit::Gcra { per, burst, .. } => {
                assert!(!per.is_zero() && burst > 0);
                per.checked_mul(burst).expect("quota horizon overflow");
            }
            RateLimit::Window { len, max, .. } => assert!(!len.is_zero() && max > 0),
        }
        Self {
            rate,
            clock: PolicyClock::new(clock),
            keys: Mutex::new(HashMap::new()),
        }
    }
    /// Prunes fully recovered keys even if no further requests arrive for those keys.
    pub fn prune(&self) {
        let mut keys = self.keys.lock().unwrap_or_else(|e| e.into_inner());
        let now = self.clock.now();
        keys.retain(|_, e| e.expires > now);
    }
    pub fn len(&self) -> usize {
        self.prune();
        self.keys.lock().unwrap_or_else(|e| e.into_inner()).len()
    }
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
    pub fn check(&self, key: &K) -> Result<(), Wait> {
        self.check_n(key, NonZeroU32::new(1).unwrap())
            .map_err(|e| match e {
                CheckError::Wait(w) => w,
                CheckError::InsufficientCapacity { .. } => unreachable!("capacity is nonzero"),
            })
    }
    /// Atomically admits all units or consumes none. An oversized request is permanent failure.
    pub fn check_n(&self, key: &K, n: NonZeroU32) -> Result<(), CheckError> {
        let capacity = match self.rate {
            RateLimit::Gcra { burst, .. } => burst,
            RateLimit::Window { max, .. } => max,
        };
        if n.get() > capacity {
            return Err(CheckError::InsufficientCapacity { capacity });
        }
        let mut keys = self.keys.lock().unwrap_or_else(|e| e.into_inner());
        let now = self.clock.now();
        keys.retain(|_, e| e.expires > now);
        let e = keys.entry(key.clone()).or_insert_with(|| self.entry(now));
        match (&mut e.state, self.rate) {
            (State::Fractional { at, debt }, RateLimit::Gcra { units, per, burst }) => {
                let units = u128::from(units.get());
                *debt = debt.saturating_sub(now.saturating_sub(*at).as_nanos() * units);
                *at = now;
                let next = *debt + per.as_nanos() * u128::from(n.get());
                let capacity = per.as_nanos() * u128::from(burst);
                if next > capacity {
                    return Err(CheckError::Wait(Wait(scaled_duration(
                        next - capacity,
                        units,
                    ))));
                }
                *debt = next;
                e.expires = now + scaled_duration(next, units);
                Ok(())
            }
            (State::Window { start, hits }, RateLimit::Window { len, max, sliding }) => {
                Self::refresh(start, hits, now, len, sliding);
                if hits.len().saturating_add(n.get() as usize) > max as usize {
                    return Err(CheckError::Wait(Wait(
                        Self::until(
                            *start,
                            hits,
                            len,
                            sliding,
                            hits.len() + n.get() as usize - max as usize,
                        )
                        .saturating_sub(now),
                    )));
                }
                hits.extend(std::iter::repeat_n(now, n.get() as usize));
                e.expires = if sliding { now + len } else { *start + len };
                Ok(())
            }
            _ => unreachable!(),
        }
    }
    fn entry(&self, now: Duration) -> Entry {
        Entry {
            expires: now,
            state: match self.rate {
                RateLimit::Gcra { .. } => State::Fractional { at: now, debt: 0 },
                RateLimit::Window { .. } => State::Window {
                    start: now,
                    hits: VecDeque::new(),
                },
            },
        }
    }
    fn refresh(
        start: &mut Duration,
        hits: &mut VecDeque<Duration>,
        now: Duration,
        len: Duration,
        sliding: bool,
    ) {
        if sliding {
            while hits.front().is_some_and(|t| now.saturating_sub(*t) >= len) {
                hits.pop_front();
            }
        } else if now.saturating_sub(*start) >= len {
            hits.clear();
            *start = now;
        }
    }
    fn until(
        start: Duration,
        hits: &VecDeque<Duration>,
        len: Duration,
        sliding: bool,
        expiring: usize,
    ) -> Duration {
        if sliding {
            hits[expiring - 1] + len
        } else {
            start + len
        }
    }
    /// Window-only query. Does not record an event or allocate an idle key.
    ///
    /// # Panics
    /// Panics for Gcra; use check or check_n for GCRA admission.
    pub fn peek(&self, key: &K) -> Result<(), Wait> {
        let RateLimit::Window { len, max, sliding } = self.rate else {
            panic!("peek requires a Window");
        };
        let mut keys = self.keys.lock().unwrap_or_else(|e| e.into_inner());
        let now = self.clock.now();
        keys.retain(|_, e| e.expires > now);
        let Some(e) = keys.get_mut(key) else {
            return Ok(());
        };
        let State::Window { start, hits } = &mut e.state else {
            unreachable!()
        };
        Self::refresh(start, hits, now, len, sliding);
        if hits.len() >= max as usize {
            Err(Wait(
                Self::until(*start, hits, len, sliding, 1).saturating_sub(now),
            ))
        } else {
            Ok(())
        }
    }
    /// Window-only record, including failures racing with an earlier peek. Never denies a hit.
    /// Both window kinds retain at most max timestamps; sliding windows keep the latest ones.
    ///
    /// # Panics
    /// Panics for Gcra; use check_n for weighted GCRA admission.
    pub fn hit(&self, key: &K) {
        let RateLimit::Window { len, max, sliding } = self.rate else {
            panic!("hit requires a Window");
        };
        let mut keys = self.keys.lock().unwrap_or_else(|e| e.into_inner());
        let now = self.clock.now();
        keys.retain(|_, e| e.expires > now);
        let e = keys.entry(key.clone()).or_insert_with(|| self.entry(now));
        let State::Window { start, hits } = &mut e.state else {
            unreachable!()
        };
        Self::refresh(start, hits, now, len, sliding);
        if sliding || hits.len() < max as usize {
            hits.push_back(now);
        }
        while hits.len() > max as usize {
            hits.pop_front();
        }
        e.expires = if sliding { now + len } else { *start + len };
    }
}
// Round only the reported wait/expiry up to a whole nanosecond. Admission uses
// the unrounded debt above, including fractional credit across rejected batches.
fn scaled_duration(debt: u128, units: u128) -> Duration {
    let nanos = debt.div_ceil(units);
    Duration::new(
        (nanos / 1_000_000_000) as u64,
        (nanos % 1_000_000_000) as u32,
    )
}
/// One stream's leading event and latest trailing event; callers wait on deadline then call due.
pub struct Throttle<T> {
    pub every: Duration,
    last: Option<Instant>,
    held: Option<T>,
}
impl<T> Throttle<T> {
    pub fn new(every: Duration) -> Self {
        Self {
            every,
            last: None,
            held: None,
        }
    }
    pub fn offer(&mut self, value: T, now: Instant) -> Option<T> {
        if self
            .last
            .is_some_and(|t| now.duration_since(t) < self.every)
        {
            self.held = Some(value);
            None
        } else {
            self.last = Some(now);
            self.held = None;
            Some(value)
        }
    }
    /// No pending trailing event and the leading-event cooldown has expired.
    pub fn idle(&self, now: Instant) -> bool {
        self.held.is_none()
            && self
                .last
                .is_none_or(|last| now.saturating_duration_since(last) >= self.every)
    }
    pub fn deadline(&self) -> Option<Instant> {
        self.held.as_ref().and(self.last.map(|t| t + self.every))
    }
    pub fn due(&mut self, now: Instant) -> Option<T> {
        if self.deadline().is_some_and(|t| t <= now) {
            self.last = Some(now);
            self.held.take()
        } else {
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::FakeClock;

    #[test]
    fn both_window_kinds_keep_bounded_hit_history() {
        for sliding in [false, true] {
            let clock = Arc::new(FakeClock::new(0));
            let l = Limiter::with_clock(
                RateLimit::Window {
                    len: Duration::from_secs(10),
                    max: 3,
                    sliding,
                },
                clock.clone(),
            );
            for n in 0..8 {
                clock.set(n * 1000);
                l.hit(&"ip");
                let keys = l.keys.lock().unwrap();
                let State::Window { hits, .. } = &keys["ip"].state else {
                    unreachable!()
                };
                assert_eq!(hits.len(), (n as usize + 1).min(3));
            }
            assert_eq!(
                l.peek(&"ip"),
                Err(Wait(Duration::from_secs(if sliding { 8 } else { 3 })))
            );
        }
    }

    #[test]
    fn a_poisoned_failure_window_still_serves_queries_hits_checks_and_pruning() {
        let clock = Arc::new(FakeClock::new(0));
        let l = Limiter::with_clock(
            RateLimit::Window {
                len: Duration::from_secs(600),
                max: 2,
                sliding: false,
            },
            clock.clone(),
        );
        assert!(
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                let _guard = l.keys.lock().unwrap();
                panic!("poison the failure-table lock");
            }))
            .is_err()
        );
        assert!(l.keys.is_poisoned());
        l.hit(&"ip");
        assert_eq!(l.peek(&"ip"), Ok(()));
        assert_eq!(l.check(&"ip"), Ok(()));
        assert_eq!(l.peek(&"ip"), Err(Wait(Duration::from_secs(600))));
        assert_eq!(l.len(), 1);
        clock.advance(600_000);
        l.prune();
        assert!(l.is_empty());
    }
}
