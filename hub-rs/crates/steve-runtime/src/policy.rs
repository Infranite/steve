//! Retry backoff, keyed rate-limit buckets and trailing-throttle policies.
//! GCRA buckets are governor's keyed limiter; recovered keys are dropped every few admissions.
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
    /// Replenish `units` per `per`, initially `burst` resource units (governor's GCRA).
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
/// Monotonic time since the limiter was built: Tokio's clock, or the injected domain clock.
/// A backward domain clock does not move limiter time backwards.
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
#[derive(Clone)]
struct GovernorClock(Arc<PolicyClock>);
impl governor::clock::Clock for GovernorClock {
    type Instant = Duration;
    fn now(&self) -> Duration {
        self.0.now()
    }
}
type Bucket<K> = governor::RateLimiter<
    K,
    governor::state::keyed::DashMapStateStore<K>,
    GovernorClock,
    governor::middleware::NoOpMiddleware<Duration>,
>;
/// Recovered keys are dropped every this many admissions, and on `prune`/`len`.
const PRUNE_EVERY: u64 = 1024;
struct Window {
    start: Duration,
    hits: VecDeque<Duration>,
    expires: Duration,
}
enum Kind<K: Hash + Eq + Clone> {
    Gcra {
        bucket: Bucket<K>,
        burst: u32,
    },
    Window {
        len: Duration,
        max: u32,
        sliding: bool,
        keys: Mutex<HashMap<K, Window>>,
    },
}
pub struct Limiter<K: Hash + Eq + Clone> {
    kind: Kind<K>,
    clock: Arc<PolicyClock>,
    ops: AtomicU64,
}
impl<K: Hash + Eq + Clone> Limiter<K> {
    /// Fixed windows begin with the first recorded event for each key.
    /// Panics for zero capacity or period.
    pub fn new(rate: RateLimit) -> Self {
        Self::build(rate, None)
    }
    pub fn with_clock(rate: RateLimit, clock: Arc<dyn crate::clock::Clock>) -> Self {
        Self::build(rate, Some(clock))
    }
    fn build(rate: RateLimit, source: Option<Arc<dyn crate::clock::Clock>>) -> Self {
        let clock = Arc::new(PolicyClock::new(source));
        let kind = match rate {
            RateLimit::Gcra { units, per, burst } => {
                let quota = governor::Quota::with_period(per / units.get())
                    .expect("GCRA period must be nonzero")
                    .allow_burst(NonZeroU32::new(burst).expect("GCRA burst must be nonzero"));
                Kind::Gcra {
                    bucket: governor::RateLimiter::dashmap_with_clock(
                        quota,
                        GovernorClock(clock.clone()),
                    ),
                    burst,
                }
            }
            RateLimit::Window { len, max, sliding } => {
                assert!(!len.is_zero() && max > 0);
                Kind::Window {
                    len,
                    max,
                    sliding,
                    keys: Mutex::new(HashMap::new()),
                }
            }
        };
        Self {
            kind,
            clock,
            ops: AtomicU64::new(0),
        }
    }
    /// Drops fully recovered keys even if no further requests arrive for them.
    pub fn prune(&self) {
        match &self.kind {
            Kind::Gcra { bucket, .. } => {
                bucket.retain_recent();
                bucket.shrink_to_fit();
            }
            Kind::Window { keys, .. } => {
                let now = self.clock.now();
                lock(keys).retain(|_, w| w.expires > now);
            }
        }
    }
    pub fn len(&self) -> usize {
        self.prune();
        match &self.kind {
            Kind::Gcra { bucket, .. } => bucket.len(),
            Kind::Window { keys, .. } => lock(keys).len(),
        }
    }
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
    fn tick(&self) {
        if self.ops.fetch_add(1, Ordering::Relaxed) % PRUNE_EVERY == PRUNE_EVERY - 1 {
            self.prune();
        }
    }
    pub fn check(&self, key: &K) -> Result<(), Wait> {
        self.check_n(key, NonZeroU32::MIN).map_err(|e| match e {
            CheckError::Wait(w) => w,
            CheckError::InsufficientCapacity { .. } => unreachable!("capacity is nonzero"),
        })
    }
    /// Atomically admits all units or consumes none. An oversized request is permanent failure.
    pub fn check_n(&self, key: &K, n: NonZeroU32) -> Result<(), CheckError> {
        self.tick();
        match &self.kind {
            Kind::Gcra { bucket, burst } => match bucket.check_key_n(key, n) {
                Ok(Ok(_)) => Ok(()),
                Ok(Err(until)) => Err(CheckError::Wait(Wait(
                    until.wait_time_from(self.clock.now()),
                ))),
                Err(_) => Err(CheckError::InsufficientCapacity { capacity: *burst }),
            },
            Kind::Window {
                len,
                max,
                sliding,
                keys,
            } => {
                let (len, max, sliding) = (*len, *max, *sliding);
                if n.get() > max {
                    return Err(CheckError::InsufficientCapacity { capacity: max });
                }
                let now = self.clock.now();
                let mut keys = lock(keys);
                let w = keys.entry(key.clone()).or_insert_with(|| fresh(now));
                refresh(w, now, len, sliding);
                let n = n.get() as usize;
                if w.hits.len() + n > max as usize {
                    let until = until(w, len, sliding, w.hits.len() + n - max as usize);
                    return Err(CheckError::Wait(Wait(until.saturating_sub(now))));
                }
                w.hits.extend(std::iter::repeat_n(now, n));
                w.expires = if sliding { now + len } else { w.start + len };
                Ok(())
            }
        }
    }
    /// Window-only query. Does not record an event or allocate an idle key.
    ///
    /// # Panics
    /// Panics for Gcra; use check or check_n for GCRA admission.
    pub fn peek(&self, key: &K) -> Result<(), Wait> {
        let Kind::Window {
            len,
            max,
            sliding,
            keys,
        } = &self.kind
        else {
            panic!("peek requires a Window");
        };
        let now = self.clock.now();
        let mut keys = lock(keys);
        let Some(w) = keys.get_mut(key) else {
            return Ok(());
        };
        refresh(w, now, *len, *sliding);
        if w.hits.len() >= *max as usize {
            Err(Wait(until(w, *len, *sliding, 1).saturating_sub(now)))
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
        let Kind::Window {
            len,
            max,
            sliding,
            keys,
        } = &self.kind
        else {
            panic!("hit requires a Window");
        };
        self.tick();
        let (len, max, sliding) = (*len, *max as usize, *sliding);
        let now = self.clock.now();
        let mut keys = lock(keys);
        let w = keys.entry(key.clone()).or_insert_with(|| fresh(now));
        refresh(w, now, len, sliding);
        if sliding || w.hits.len() < max {
            w.hits.push_back(now);
        }
        while w.hits.len() > max {
            w.hits.pop_front();
        }
        w.expires = if sliding { now + len } else { w.start + len };
    }
}
fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}
fn fresh(now: Duration) -> Window {
    Window {
        start: now,
        hits: VecDeque::new(),
        expires: now,
    }
}
fn refresh(w: &mut Window, now: Duration, len: Duration, sliding: bool) {
    if sliding {
        while w
            .hits
            .front()
            .is_some_and(|t| now.saturating_sub(*t) >= len)
        {
            w.hits.pop_front();
        }
    } else if now.saturating_sub(w.start) >= len {
        w.hits.clear();
        w.start = now;
    }
}
/// When the `expiring`-th oldest hit stops counting.
fn until(w: &Window, len: Duration, sliding: bool, expiring: usize) -> Duration {
    if sliding {
        w.hits[expiring - 1] + len
    } else {
        w.start + len
    }
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

    fn window_keys<K: Hash + Eq + Clone>(l: &Limiter<K>) -> &Mutex<HashMap<K, Window>> {
        match &l.kind {
            Kind::Window { keys, .. } => keys,
            Kind::Gcra { .. } => unreachable!(),
        }
    }

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
                let keys = window_keys(&l).lock().unwrap();
                assert_eq!(keys["ip"].hits.len(), (n as usize + 1).min(3));
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
                let _guard = window_keys(&l).lock().unwrap();
                panic!("poison the failure-table lock");
            }))
            .is_err()
        );
        assert!(window_keys(&l).is_poisoned());
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
