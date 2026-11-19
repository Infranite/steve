//! Replaces retry, window/bucket and trailing-throttle shapes in W2b §1.2–1.3.
//! Durations use monotonic Tokio time; persisted deadlines use Backoff::delay plus Clock::now.
use backon::Retryable;
use rand::RngExt;
use std::{
    collections::{HashMap, VecDeque},
    future::Future,
    hash::Hash,
    num::NonZeroU32,
    sync::Mutex,
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
    /// One token per `per`, initially `burst` tokens (not burst tokens per period).
    Gcra {
        per: Duration,
        burst: u32,
    },
    Window {
        len: Duration,
        max: u32,
    },
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Wait(pub Duration);
#[derive(Debug, Clone, Copy, Default)]
struct MonoClock;
impl governor::clock::Clock for MonoClock {
    type Instant = std::time::Instant;
    fn now(&self) -> Self::Instant {
        Instant::now().into_std()
    }
}
type Bucket = governor::RateLimiter<
    governor::state::NotKeyed,
    governor::state::InMemoryState,
    MonoClock,
    governor::middleware::NoOpMiddleware<std::time::Instant>,
>;
enum State {
    Bucket(Bucket),
    Window {
        start: Instant,
        hits: VecDeque<Instant>,
    },
}
struct Entry {
    state: State,
    expires: Instant,
}
pub struct Limiter<K> {
    rate: RateLimit,
    sliding: bool,
    keys: Mutex<HashMap<K, Entry>>,
}
impl<K: Hash + Eq + Clone> Limiter<K> {
    /// Fixed windows begin with the first accepted event for each key.
    /// Panics for zero capacity/period or a GCRA recovery horizon that overflows Duration.
    pub fn new(rate: RateLimit) -> Self {
        match rate {
            RateLimit::Gcra { per, burst } => {
                assert!(!per.is_zero() && burst > 0);
                per.checked_mul(burst).expect("quota horizon overflow");
            }
            RateLimit::Window { len, max } => assert!(!len.is_zero() && max > 0),
        }
        Self {
            rate,
            sliding: false,
            keys: Mutex::new(HashMap::new()),
        }
    }
    pub fn sliding(rate: RateLimit) -> Self {
        assert!(matches!(rate, RateLimit::Window { .. }));
        Self {
            sliding: true,
            ..Self::new(rate)
        }
    }
    /// Prunes fully recovered keys even if no further requests arrive for those keys.
    pub fn prune(&self) {
        self.keys
            .lock()
            .unwrap()
            .retain(|_, e| e.expires > Instant::now());
    }
    pub fn len(&self) -> usize {
        self.prune();
        self.keys.lock().unwrap().len()
    }
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
    pub fn check(&self, key: &K) -> Result<(), Wait> {
        let now = Instant::now();
        let mut keys = self.keys.lock().unwrap();
        keys.retain(|_, e| e.expires > now);
        let e = keys.entry(key.clone()).or_insert_with(|| Entry {
            expires: now,
            state: match self.rate {
                RateLimit::Gcra { per, burst } => State::Bucket(Bucket::direct_with_clock(
                    governor::Quota::with_period(per)
                        .expect("GCRA period out of range")
                        .allow_burst(NonZeroU32::new(burst).unwrap()),
                    MonoClock,
                )),
                RateLimit::Window { .. } => State::Window {
                    start: now,
                    hits: VecDeque::new(),
                },
            },
        });
        match (&mut e.state, self.rate) {
            (State::Bucket(b), RateLimit::Gcra { per, burst }) => {
                let result = b
                    .check()
                    .map_err(|w| Wait(w.wait_time_from(now.into_std())));
                if result.is_ok() {
                    e.expires = now + per * burst;
                }
                result
            }
            (State::Window { start, hits }, RateLimit::Window { len, max }) => {
                if self.sliding {
                    while hits.front().is_some_and(|t| now.duration_since(*t) >= len) {
                        hits.pop_front();
                    }
                } else if now.duration_since(*start) >= len {
                    hits.clear();
                    *start = now;
                }
                let until = if self.sliding {
                    hits.front().copied().unwrap_or(now) + len
                } else {
                    *start + len
                };
                if hits.len() >= max as usize {
                    return Err(Wait(until - now));
                }
                hits.push_back(now);
                e.expires = if self.sliding { now + len } else { until };
                Ok(())
            }
            _ => unreachable!(),
        }
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
