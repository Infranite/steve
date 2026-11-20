//! Outbound rate limits, shared by the bot's sends (F3, [`super::send`]) and sending
//! as you (F6, [`super::as_user`]):
//!
//! - [`Limiter`]: token buckets that make a send wait its turn before it goes out — per (bot
//!   connection, chat) ≤ [`CHAT_PER_SEC`]/s, per bot connection ≤ [`APP_PER_SEC`]/s (Feishu's
//!   published figures; the per-chat limit is shared by every bot in the group);
//! - [`with_retries`]: when Feishu still says “limited” (HTTP 429, 99991400, 230020), wait the
//!   `x-ogw-ratelimit-reset` time (one second when absent, at most [`MAX_BACKOFF`]) and try again
//!   with the same request, so the same `uuid`: Feishu drops the repeat if the first did get
//!   through.
//!
//! This is Feishu's debt-reservation protocol, not runtime's GCRA admission policy: every
//! reservation debits every key, even below zero, and returns the longest wait among their
//! individual deadlines. Rejecting without consuming would let concurrent senders reuse the
//! same quota instead of reserving successive turns. One mutex protects the whole key set.
//! The bucket remains owned by this adapter and moves with the Feishu plugin;
//! it is not a second general-purpose limiter or a missing runtime GCRA feature.

use std::collections::HashMap;
use std::future::Future;
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

use tokio::time::Instant;

use super::client::FeishuError;

/// Sends per second into one chat.
pub const CHAT_PER_SEC: u32 = 5;
/// Sends per second of one bot connection.
pub const APP_PER_SEC: u32 = 50;
/// Retries of a bot send after a rate limit.
pub const SEND_RETRIES: u32 = 3;
/// Retries of a send as you (each user ≤ 5/s; F6 keeps one).
pub const USER_RETRIES: u32 = 1;
/// The longest wait honoured before a retry.
pub const MAX_BACKOFF: Duration = Duration::from_secs(10);
/// The wait when Feishu gives no reset time (its windows are one second).
pub const DEFAULT_BACKOFF: Duration = Duration::from_secs(1);

/// Buckets idle this long are full again and dropped when the map grows.
const IDLE: Duration = Duration::from_secs(60);
const PRUNE_AT: usize = 4096;

/// One token bucket. Taking may run it into debt: the taker then waits until the debt is paid,
/// which keeps waiters in order without a queue.
#[derive(Debug, Clone)]
struct Bucket {
    tokens: f64,
    rate: f64,
    last: Instant,
}

impl Bucket {
    fn new(per_sec: u32, now: Instant) -> Self {
        Self {
            tokens: f64::from(per_sec.max(1)),
            rate: f64::from(per_sec.max(1)),
            last: now,
        }
    }

    /// Takes one token at `now`; how long to wait before using it.
    fn take(&mut self, now: Instant) -> Duration {
        let elapsed = now.saturating_duration_since(self.last).as_secs_f64();
        self.tokens = (self.tokens + elapsed * self.rate).min(self.rate);
        self.last = now;
        self.tokens -= 1.0;
        if self.tokens >= 0.0 {
            Duration::ZERO
        } else {
            Duration::from_secs_f64(-self.tokens / self.rate)
        }
    }
}

/// Token buckets by key. One per process ([`Limiter::global`]): Feishu counts per app, and the Hub
/// runs one consumer per app.
#[derive(Debug, Default)]
pub struct Limiter {
    buckets: Mutex<HashMap<String, Bucket>>,
}

impl Limiter {
    pub fn new() -> Self {
        Self::default()
    }

    /// The process-wide limiter.
    pub fn global() -> &'static Limiter {
        static L: OnceLock<Limiter> = OnceLock::new();
        L.get_or_init(Limiter::new)
    }

    /// Takes a token from each `(key, per_sec)` bucket and returns the longest wait.
    pub fn reserve(&self, keys: &[(&str, u32)]) -> Duration {
        let now = Instant::now();
        let mut b = self.buckets.lock().unwrap_or_else(|e| e.into_inner());
        if b.len() > PRUNE_AT {
            b.retain(|_, x| now.saturating_duration_since(x.last) < IDLE);
        }
        keys.iter()
            .map(|(k, rate)| {
                b.entry((*k).to_string())
                    .or_insert_with(|| Bucket::new(*rate, now))
                    .take(now)
            })
            .max()
            .unwrap_or(Duration::ZERO)
    }

    /// Waits for a token of each bucket.
    pub async fn acquire(&self, keys: &[(&str, u32)]) {
        let wait = self.reserve(keys);
        if !wait.is_zero() {
            tracing::debug!(
                wait_ms = wait.as_millis() as u64,
                "feishu: send waits for its rate"
            );
            tokio::time::sleep(wait).await;
        }
    }

    /// Waits for the bot's turn to send into `chat` (`None`: only the connection's rate).
    pub async fn bot_send(&self, conn: &str, chat: Option<&str>) {
        let app = format!("app:{conn}");
        match chat.filter(|c| !c.is_empty()) {
            Some(c) => {
                let chat = format!("chat:{conn}:{c}");
                self.acquire(&[(&app, APP_PER_SEC), (&chat, CHAT_PER_SEC)])
                    .await
            }
            None => self.acquire(&[(&app, APP_PER_SEC)]).await,
        }
    }
}

/// How long to wait after a rate-limit answer.
pub fn backoff(reset: Option<Duration>) -> Duration {
    reset.unwrap_or(DEFAULT_BACKOFF).min(MAX_BACKOFF)
}

/// Runs `call`, and again up to `retries` times while Feishu answers “rate limited”, waiting
/// [`backoff`] in between. `call` must make the same request each time (same `uuid`). Returns the
/// answer and how many retries it took; the last rate-limit error when they ran out.
pub async fn with_retries<T, F, Fut>(retries: u32, call: F) -> Result<(T, u32), FeishuError>
where
    F: FnMut() -> Fut,
    Fut: Future<Output = Result<T, FeishuError>>,
{
    with_retries_capped(retries, MAX_BACKOFF, "send", call).await
}

/// [`with_retries`] waiting at most `cap` each time; `what` names the request in the log.
pub async fn with_retries_capped<T, F, Fut>(
    retries: u32,
    cap: Duration,
    what: &str,
    mut call: F,
) -> Result<(T, u32), FeishuError>
where
    F: FnMut() -> Fut,
    Fut: Future<Output = Result<T, FeishuError>>,
{
    use std::sync::atomic::{AtomicU32, Ordering};
    use steve_runtime::policy::{Backoff, Jitter, Retry};
    let calls = AtomicU32::new(0);
    let policy = Backoff {
        base: DEFAULT_BACKOFF,
        max: cap,
        factor: 1,
        jitter: Jitter::None,
        attempts: Some(retries),
        reset_after: None,
    };
    let result = policy
        .retry(
            || {
                calls.fetch_add(1, Ordering::Relaxed);
                call()
            },
            |error| match error {
                FeishuError::RateLimited { code, reset } => {
                    let wait = reset.unwrap_or(DEFAULT_BACKOFF).min(cap);
                    tracing::info!(
                        code,
                        what,
                        attempt = calls.load(Ordering::Relaxed),
                        wait_ms = wait.as_millis() as u64,
                        "feishu: rate limited, retrying the same request"
                    );
                    Retry::After(wait)
                }
                _ => Retry::Stop,
            },
        )
        .await;
    result.map(|value| (value, calls.load(Ordering::Relaxed) - 1))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};

    #[test]
    fn buckets_allow_a_burst_then_space_out() {
        let t0 = Instant::now();
        let mut b = Bucket::new(5, t0);
        for _ in 0..5 {
            assert_eq!(b.take(t0), Duration::ZERO);
        }
        let w = b.take(t0);
        assert!((w.as_secs_f64() - 0.2).abs() < 1e-6, "{w:?}");
        let w = b.take(t0);
        assert!((w.as_secs_f64() - 0.4).abs() < 1e-6, "in order: {w:?}");
        // a second later the debt is paid and three tokens are back
        let t1 = t0 + Duration::from_secs(1);
        assert_eq!(b.take(t1), Duration::ZERO);
        assert!((b.tokens - 2.0).abs() < 1e-6);
        // never more than the burst
        let mut b = Bucket::new(5, t0);
        b.take(t0 + Duration::from_secs(100));
        assert!((b.tokens - 4.0).abs() < 1e-6);
    }

    #[test]
    fn the_strictest_bucket_wins() {
        let l = Limiter::new();
        for _ in 0..5 {
            assert_eq!(l.reserve(&[("a", 50), ("c", 5)]), Duration::ZERO);
        }
        assert!(l.reserve(&[("a", 50), ("c", 5)]) > Duration::from_millis(150));
        // another chat of the same connection is not held up
        assert_eq!(l.reserve(&[("a", 50), ("d", 5)]), Duration::ZERO);
    }

    #[test]
    fn backoff_is_capped() {
        assert_eq!(backoff(None), DEFAULT_BACKOFF);
        assert_eq!(backoff(Some(Duration::from_secs(600))), MAX_BACKOFF);
        assert_eq!(backoff(Some(Duration::ZERO)), Duration::ZERO);
    }

    #[tokio::test]
    async fn retries_only_rate_limits_and_only_so_often() {
        let calls = AtomicU32::new(0);
        let limited = || FeishuError::RateLimited {
            code: 230020,
            reset: Some(Duration::ZERO),
        };
        let r = with_retries(3, || async {
            let n = calls.fetch_add(1, Ordering::SeqCst);
            if n < 2 { Err(limited()) } else { Ok(n) }
        })
        .await;
        assert_eq!(r, Ok((2, 2)));

        calls.store(0, Ordering::SeqCst);
        let r: Result<(u32, u32), _> = with_retries(3, || async {
            calls.fetch_add(1, Ordering::SeqCst);
            Err(limited())
        })
        .await;
        assert_eq!(r, Err(limited()));
        assert_eq!(calls.load(Ordering::SeqCst), 4, "one try and three retries");

        calls.store(0, Ordering::SeqCst);
        let r: Result<(u32, u32), _> = with_retries(3, || async {
            calls.fetch_add(1, Ordering::SeqCst);
            Err(FeishuError::Server(502))
        })
        .await;
        assert_eq!(r, Err(FeishuError::Server(502)));
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }
}

#[cfg(test)]
mod retry_sequence_tests {
    use super::*;
    #[tokio::test(start_paused = true)]
    async fn reservation_table_preserves_debt_and_independent_keys() {
        for rate in [5, 50] {
            let limiter = Limiter::new();
            for n in 1..=rate + 3 {
                let expected = if n <= rate {
                    0.0
                } else {
                    f64::from(n - rate) / f64::from(rate)
                };
                assert!((limiter.reserve(&[("app", rate)]).as_secs_f64() - expected).abs() < 1e-6);
            }
            assert_eq!(limiter.reserve(&[("another", rate)]), Duration::ZERO);
            tokio::time::advance(Duration::from_secs(2)).await;
            assert_eq!(limiter.reserve(&[("app", rate)]), Duration::ZERO);
        }
    }
    #[test]
    fn bucket_balance_table_pins_debt_refill_and_the_burst_ceiling() {
        let start = Instant::now();
        let mut bucket = Bucket::new(2, start);
        for (at_ms, tokens, wait_ms) in [
            (0, 1.0, 0),
            (0, 0.0, 0),
            (0, -1.0, 500),
            (0, -2.0, 1000),
            (250, -2.5, 1250),
            (1000, -2.0, 1000),
            (2500, 0.0, 0),
            (1_000_000, 1.0, 0),
        ] {
            let wait = bucket.take(start + Duration::from_millis(at_ms));
            assert_eq!(bucket.tokens, tokens, "balance at {at_ms}ms");
            assert_eq!(
                wait,
                Duration::from_millis(wait_ms),
                "deadline at {at_ms}ms"
            );
        }
    }

    #[tokio::test(start_paused = true)]
    async fn multi_key_table_reserves_every_key_and_preserves_individual_deadlines() {
        let limiter = Limiter::new();
        for (advance_ms, app_tokens, chat_tokens, app_wait_ms, chat_wait_ms) in [
            (0, 1.0, 0.0, 0, 0),
            (0, 0.0, -1.0, 0, 1000),
            (0, -1.0, -2.0, 500, 2000),
            (250, -1.5, -2.75, 750, 2750),
            (4750, 1.0, 0.0, 0, 0),
        ] {
            tokio::time::advance(Duration::from_millis(advance_ms)).await;
            let now = Instant::now();
            let wait = limiter.reserve(&[("app", 2), ("chat", 1)]);
            let buckets = limiter.buckets.lock().unwrap();
            let app = &buckets["app"];
            let chat = &buckets["chat"];
            assert_eq!(
                (app.last, chat.last),
                (now, now),
                "one reservation time for both keys"
            );
            assert_eq!(
                (app.tokens, chat.tokens),
                (app_tokens, chat_tokens),
                "the available key is debited even when its partner is in debt"
            );
            // Per-key deadlines are inferred from balances, not returned by reserve.
            // The product-facing deadline contract is only the returned maximum below.
            for (bucket, expected_ms) in [(app, app_wait_ms), (chat, chat_wait_ms)] {
                let deadline =
                    bucket.last + Duration::from_secs_f64((-bucket.tokens / bucket.rate).max(0.0));
                assert_eq!(deadline, now + Duration::from_millis(expected_ms));
            }
            assert_eq!(
                wait,
                Duration::from_millis(app_wait_ms.max(chat_wait_ms)),
                "the whole reservation waits until every key can be used"
            );
        }
        assert_eq!(limiter.reserve(&[]), Duration::ZERO);
    }

    #[tokio::test(start_paused = true)]
    async fn retry_table_honours_hints_caps_and_counts() {
        for (hint, cap, wait_ms) in [
            (None, 10000, 1000),
            (Some(20000), 10000, 10000),
            (Some(2000), 500, 500),
            (Some(0), 500, 0),
        ] {
            let started = Instant::now();
            let calls = std::sync::atomic::AtomicU32::new(0);
            let result = with_retries_capped(3, Duration::from_millis(cap), "test", || async {
                let n = calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                if n < 2 {
                    Err(FeishuError::RateLimited {
                        code: 429,
                        reset: hint.map(Duration::from_millis),
                    })
                } else {
                    Ok(n)
                }
            })
            .await;
            assert_eq!(result, Ok((2, 2)));
            assert_eq!(started.elapsed(), Duration::from_millis(2 * wait_ms));
        }
    }
}
