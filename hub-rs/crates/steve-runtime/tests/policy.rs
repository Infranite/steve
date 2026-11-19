#![cfg(feature = "policy")]
use rand::{RngExt, SeedableRng, rngs::StdRng};
use std::{
    num::NonZeroU32,
    sync::{Arc, Mutex},
    time::Duration,
};
use steve_runtime::policy::{Backoff, Jitter, Limiter, RateLimit, Retry, Throttle, Wait};
fn sec(n: u64) -> Duration {
    Duration::from_secs(n)
}
fn policy(base: Duration, max: Duration) -> Backoff {
    Backoff {
        base,
        max,
        factor: 2,
        jitter: Jitter::None,
        attempts: None,
        reset_after: None,
    }
}
#[test]
fn existing_backoff_sequences_and_jitter_bounds() {
    let mut rng = StdRng::seed_from_u64(17);
    // store/outbox.rs:428: zero-based 2^n seconds, ten-minute cap.
    for (n, s) in [
        (0, 1),
        (1, 2),
        (2, 4),
        (8, 256),
        (9, 512),
        (10, 600),
        (99, 600),
    ] {
        assert_eq!(policy(sec(1), sec(600)).delay(n, &mut rng), sec(s));
    }
    // hub/plugins/run.rs:165; settled for >=60s resets BEFORE this delay.
    let p = Backoff {
        reset_after: Some(sec(60)),
        ..policy(sec(1), sec(60))
    };
    let mut n = 0;
    for (ran, wait) in [
        (0, 1),
        (0, 2),
        (0, 4),
        (0, 8),
        (0, 16),
        (0, 32),
        (59, 60),
        (60, 1),
        (0, 2),
    ] {
        assert_eq!(p.supervised(&mut n, sec(ran), &mut rng), sec(wait));
    }
    // ext_ws/provider.rs:294 full jitter; node-cli/dial.rs:443 equal jitter.
    for (base, max, jitter) in [
        (sec(1), sec(60), Jitter::Full),
        (Duration::from_millis(200), sec(30), Jitter::Equal),
    ] {
        let p = Backoff {
            jitter,
            ..policy(base, max)
        };
        for n in [0, 1, 2, 10, 99] {
            let upper = policy(base, max).delay(n, &mut rng);
            let lower = if matches!(jitter, Jitter::Equal) {
                upper / 2
            } else {
                Duration::ZERO
            };
            for _ in 0..100 {
                let d = p.delay(n, &mut rng);
                assert!(
                    d >= lower
                        && if matches!(jitter, Jitter::Equal) {
                            d < upper
                        } else {
                            d <= upper
                        }
                );
            }
        }
    }
    assert_eq!(
        policy(Duration::MAX, Duration::MAX).delay(u32::MAX, &mut rng),
        Duration::MAX
    );
}
#[tokio::test]
async fn retry_classification_server_delay_limit_and_profile_table() {
    let waits = Arc::new(Mutex::new(Vec::new()));
    let w = waits.clone();
    let p = Backoff {
        attempts: Some(3),
        ..policy(sec(1), sec(60))
    };
    let mut calls = 0;
    let result: Result<(), u32> = p
        .retry_with(
            || {
                calls += 1;
                std::future::ready(Err(calls))
            },
            |n| match n {
                1 => Retry::Now,
                2 => Retry::After(sec(17)),
                _ => Retry::Backoff,
            },
            move |d| {
                w.lock().unwrap().push(d);
                std::future::ready(())
            },
        )
        .await;
    assert_eq!(result, Err(4)); // provider: three retries plus initial call; After respects cap too
    assert_eq!(*waits.lock().unwrap(), [sec(0), sec(17), sec(4)]);
    // modules/contacts/profile.rs:26 fixed table (last repeats), via server/explicit waits.
    let table = [1, 6, 24, 72, 72];
    let waits = Arc::new(Mutex::new(Vec::new()));
    let w = waits.clone();
    let mut calls = 0;
    let _: Result<(), usize> = policy(sec(1), sec(1))
        .retry_with(
            || {
                calls += 1;
                std::future::ready(Err(calls))
            },
            |n| {
                if *n <= 5 {
                    Retry::After(sec(table[*n - 1] * 3600))
                } else {
                    Retry::Stop
                }
            },
            move |d| {
                w.lock().unwrap().push(d);
                std::future::ready(())
            },
        )
        .await;
    assert_eq!(*waits.lock().unwrap(), table.map(|h| sec(h * 3600)));
}
#[tokio::test(start_paused = true)]
async fn fixed_sliding_windows_bucket_eviction_and_trailing_stats() {
    // hub/access.rs:134: ten attempts per 600 seconds, window anchored at first failure.
    let fixed = Limiter::new(RateLimit::Window {
        sliding: false,
        len: sec(600),
        max: 10,
    });
    for _ in 0..10 {
        assert_eq!(fixed.check(&"ip"), Ok(()));
    }
    assert_eq!(fixed.check(&"ip"), Err(Wait(sec(600))));
    tokio::time::advance(sec(600)).await;
    assert!(fixed.is_empty());
    assert!(fixed.check(&"ip").is_ok());
    // github/hooks.rs:53 sliding window: later requests do not reset with first request.
    let rate = RateLimit::Window {
        sliding: false,
        len: sec(60),
        max: 2,
    };
    let (fixed, sliding) = (
        Limiter::new(rate),
        Limiter::new(RateLimit::Window {
            len: sec(60),
            max: 2,
            sliding: true,
        }),
    );
    for l in [&fixed, &sliding] {
        assert!(l.check(&0).is_ok());
    }
    tokio::time::advance(sec(30)).await;
    for l in [&fixed, &sliding] {
        assert!(l.check(&0).is_ok());
    }
    tokio::time::advance(sec(30)).await;
    for l in [&fixed, &sliding] {
        assert!(l.check(&0).is_ok());
    }
    assert!(fixed.check(&0).is_ok());
    assert_eq!(sliding.check(&0), Err(Wait(sec(30))));
    // hub/nodes/pairing.rs:35: per-code burst 5, refill 0.1/s; global burst 200, 50/s.
    for (per, burst) in [(sec(10), 5), (Duration::from_millis(20), 200)] {
        let l = Limiter::new(RateLimit::Gcra {
            units: std::num::NonZeroU32::MIN,
            per,
            burst,
        });
        for _ in 0..burst {
            assert!(l.check(&0).is_ok());
        }
        assert_eq!(l.check(&0), Err(Wait(per)));
        assert!(l.check(&1).is_ok());
        tokio::time::advance(per).await;
        assert!(l.check(&0).is_ok());
        tokio::time::advance(per * burst).await;
        assert!(l.is_empty());
    }
    // hub/bus.rs:813: leading event, coalesced latest trailing event at five seconds.
    let mut t = Throttle::new(sec(5));
    let now = tokio::time::Instant::now();
    assert_eq!(t.offer(1, now), Some(1));
    assert_eq!(t.offer(2, now + sec(1)), None);
    assert_eq!(t.offer(3, now + sec(4)), None);
    assert_eq!(t.deadline(), Some(now + sec(5)));
    assert_eq!(t.due(now + sec(4)), None);
    assert_eq!(t.due(now + sec(5)), Some(3));
    assert_eq!(t.due(now + sec(6)), None);
    assert_eq!(t.offer(4, now + sec(6)), None);
    assert_eq!(t.offer(5, now + sec(10)), Some(5));
    assert_eq!(t.deadline(), None);
}

// Deterministic endpoints exercise inclusive Full and half-open Equal distributions.
struct EdgeRng(u64);
impl rand::TryRng for EdgeRng {
    type Error = std::convert::Infallible;
    fn try_next_u32(&mut self) -> Result<u32, Self::Error> {
        Ok(self.0 as u32)
    }
    fn try_next_u64(&mut self) -> Result<u64, Self::Error> {
        Ok(self.0)
    }
    fn try_fill_bytes(&mut self, dst: &mut [u8]) -> Result<(), Self::Error> {
        dst.fill(self.0 as u8);
        Ok(())
    }
}
#[tokio::test]
async fn ext_ws_retry_after_extra_jitter_table_and_budget() {
    // provider.rs:308: retry_after_ms + uniform(0..=floor(ms/5)), then Retry::After.
    #[rustfmt::skip]
    let cases = [(0,0,0),(1,1,1),(4,4,4),(5,5,6),(999,999,1198),(1000,1000,1200),(120000,120000,144000)];
    for (server_ms, low, high) in cases {
        for (word, expected) in [(0, low), (u64::MAX, high)] {
            let mut rng = EdgeRng(word);
            let extra = rng.random_range(0..=server_ms / 5);
            let wait = Duration::from_millis(server_ms + extra);
            assert_eq!(wait, Duration::from_millis(expected));
            let waits = Arc::new(Mutex::new(Vec::new()));
            let w = waits.clone();
            let mut calls = 0;
            let p = Backoff {
                attempts: Some(3),
                ..policy(sec(1), sec(60))
            };
            let result: Result<(), u32> = p
                .retry_with(
                    || {
                        calls += 1;
                        std::future::ready(Err(calls))
                    },
                    |_| Retry::After(wait),
                    move |d| {
                        w.lock().unwrap().push(d);
                        std::future::ready(())
                    },
                )
                .await;
            assert_eq!(result, Err(4));
            assert_eq!(*waits.lock().unwrap(), [wait; 3]); // server wait can exceed max; budget still applies
        }
    }
    let full = Backoff {
        jitter: Jitter::Full,
        ..policy(Duration::from_nanos(10), sec(1))
    };
    assert_eq!(full.delay(0, &mut EdgeRng(0)), Duration::ZERO);
    assert_eq!(
        full.delay(0, &mut EdgeRng(u64::MAX)),
        Duration::from_nanos(10)
    );
    let p = Backoff {
        jitter: Jitter::Equal,
        ..policy(Duration::from_nanos(10), sec(1))
    };
    assert_eq!(p.delay(0, &mut EdgeRng(0)), Duration::from_nanos(5));
    assert_eq!(p.delay(0, &mut EdgeRng(u64::MAX)), Duration::from_nanos(9));
}

#[test]
fn weighted_checks_are_atomic_and_oversize_is_permanent() {
    use std::num::NonZeroU32;
    use steve_runtime::{FakeClock, policy::CheckError};
    let clock = Arc::new(FakeClock::new(100));
    let l = Limiter::with_clock(
        RateLimit::Gcra {
            units: std::num::NonZeroU32::MIN,
            per: sec(1),
            burst: 5,
        },
        clock.clone(),
    );
    for (ms, n, expected) in [
        (0, 6, Err(CheckError::InsufficientCapacity { capacity: 5 })),
        (0, 4, Ok(())),
        (0, 2, Err(CheckError::Wait(Wait(sec(1))))),
        (0, 1, Ok(())),
        (
            999,
            1,
            Err(CheckError::Wait(Wait(Duration::from_millis(1)))),
        ),
        (1000, 1, Ok(())),
        (1000, 1, Err(CheckError::Wait(Wait(sec(1))))),
    ] {
        clock.set(100 + ms);
        assert_eq!(l.check_n(&"node", NonZeroU32::new(n).unwrap()), expected);
    }
    clock.set(100 + 6000);
    assert!(l.is_empty());
}

#[test]
fn window_peek_hit_and_injected_clock_table() {
    use steve_runtime::FakeClock;
    let clock = Arc::new(FakeClock::new(-50));
    let l = Limiter::with_clock(
        RateLimit::Window {
            sliding: false,
            len: sec(600),
            max: 10,
        },
        clock.clone(),
    );
    for _ in 0..100 {
        assert_eq!(l.peek(&"ip"), Ok(()));
    }
    assert!(l.is_empty(), "query does not allocate or consume");
    for n in 1..=12 {
        l.hit(&"ip");
        assert_eq!(l.peek(&"ip").is_err(), n >= 10);
    }
    for (elapsed, wait_ms) in [(599_999, Some(1)), (600_000, None)] {
        clock.set(-50 + elapsed);
        assert_eq!(
            l.peek(&"ip"),
            wait_ms.map_or(Ok(()), |ms| Err(Wait(Duration::from_millis(ms))))
        );
    }
    assert!(l.is_empty());
    l.hit(&"ip");
    clock.advance(1);
    clock.set(-50); // rollback cannot reopen a window or refill a bucket
    assert_eq!(l.check(&"ip"), Ok(()));
    assert_eq!(l.len(), 1);
}

#[test]
fn injected_gcra_and_sliding_windows_keep_boundaries_and_keys_separate() {
    use steve_runtime::FakeClock;
    let clock = Arc::new(FakeClock::new(1_000_000));
    let l = Limiter::with_clock(
        RateLimit::Window {
            sliding: true,
            len: sec(60),
            max: 2,
        },
        clock.clone(),
    );
    for (ms, allowed) in [
        (0, true),
        (30_000, true),
        (59_999, false),
        (60_000, true),
        (60_000, false),
        (90_000, true),
    ] {
        clock.set(1_000_000 + ms);
        assert_eq!(l.check(&"a").is_ok(), allowed);
    }
    assert!(l.check(&"b").is_ok());
    clock.advance(60_000);
    assert!(l.is_empty());
}

#[test]
fn exact_byte_rate_preserves_fractional_credit_and_long_running_keys() {
    use std::num::NonZeroU32;
    use steve_runtime::{FakeClock, policy::CheckError};
    const MIB: u32 = 1024 * 1024;
    let clock = Arc::new(FakeClock::new(0));
    let l = Limiter::with_clock(
        RateLimit::Gcra {
            units: NonZeroU32::new(MIB).unwrap(),
            per: sec(1),
            burst: 64 * MIB,
        },
        clock.clone(),
    );
    let check = |n| l.check_n(&"node", NonZeroU32::new(n).unwrap());
    assert_eq!(
        check(64 * MIB + 1),
        Err(CheckError::InsufficientCapacity { capacity: 64 * MIB })
    );
    assert_eq!(check(64 * MIB), Ok(()));
    // Fractional byte credit survives both successful and rejected batches.
    for (ms, n, allowed) in [
        (0, 1, false),
        (1, 1048, true),
        (1, 1, false),
        (2, 1049, true),
        (2, 1, false),
    ] {
        clock.set(ms);
        assert_eq!(check(n).is_ok(), allowed, "{ms} {n}");
    }
    clock.set(90 * 86_400_000);
    assert!(
        l.is_empty(),
        "scaled time does not overflow after months of uptime"
    );
    assert_eq!(check(64 * MIB), Ok(()));
    clock.advance(1000);
    assert_eq!(check(MIB), Ok(()));
    assert_eq!(
        check(1),
        Err(CheckError::Wait(Wait(Duration::from_nanos(954))))
    );
    assert_eq!(
        l.check_n(&"other", NonZeroU32::new(64 * MIB).unwrap()),
        Ok(())
    );
}

#[tokio::test(start_paused = true)]
async fn exact_byte_rate_rounds_only_waits_not_admission() {
    use std::num::NonZeroU32;
    use steve_runtime::policy::CheckError;
    const MIB: u32 = 1024 * 1024;
    let l = Limiter::new(RateLimit::Gcra {
        units: NonZeroU32::new(MIB).unwrap(),
        per: sec(1),
        burst: 64 * MIB,
    });
    let check = |n| l.check_n(&"node", NonZeroU32::new(n).unwrap());
    assert_eq!(check(64 * MIB), Ok(()));
    assert_eq!(
        check(1),
        Err(CheckError::Wait(Wait(Duration::from_nanos(954))))
    );
    tokio::time::advance(Duration::from_nanos(953)).await;
    assert_eq!(
        check(1),
        Err(CheckError::Wait(Wait(Duration::from_nanos(1))))
    );
    tokio::time::advance(Duration::from_nanos(1)).await;
    assert_eq!(check(1), Ok(()));
    assert_eq!(
        check(1),
        Err(CheckError::Wait(Wait(Duration::from_nanos(954))))
    );
}

#[test]
fn integral_unit_periods_keep_the_hand_calculated_admission_table() {
    use steve_runtime::{FakeClock, policy::CheckError};
    for (units, per) in [
        (NonZeroU32::MIN, Duration::from_millis(500)),
        (NonZeroU32::new(2).unwrap(), sec(1)),
    ] {
        let clock = Arc::new(FakeClock::new(0));
        let l = Limiter::with_clock(
            RateLimit::Gcra {
                units,
                per,
                burst: 5,
            },
            clock.clone(),
        );
        for (ms, weight, expected) in [
            (0, 6, Err(CheckError::InsufficientCapacity { capacity: 5 })),
            (0, 4, Ok(())),
            (
                0,
                2,
                Err(CheckError::Wait(Wait(Duration::from_millis(500)))),
            ),
            (0, 1, Ok(())),
            (
                499,
                1,
                Err(CheckError::Wait(Wait(Duration::from_millis(1)))),
            ),
            (500, 1, Ok(())),
            (
                500,
                1,
                Err(CheckError::Wait(Wait(Duration::from_millis(500)))),
            ),
            (
                1000,
                2,
                Err(CheckError::Wait(Wait(Duration::from_millis(500)))),
            ),
            (1000, 1, Ok(())),
            (2000, 2, Ok(())),
            (
                2000,
                1,
                Err(CheckError::Wait(Wait(Duration::from_millis(500)))),
            ),
            (4500, 5, Ok(())),
        ] {
            clock.set(ms);
            assert_eq!(
                l.check_n(&"node", NonZeroU32::new(weight).unwrap()),
                expected,
                "{units} at {ms}"
            );
        }
        assert_eq!(l.check_n(&"other", NonZeroU32::new(5).unwrap()), Ok(()));
    }
}

#[test]
fn sliding_hit_keeps_latest_capacity_without_changing_waits() {
    use steve_runtime::{FakeClock, policy::CheckError};
    let clock = Arc::new(FakeClock::new(0));
    let l = Limiter::with_clock(
        RateLimit::Window {
            len: sec(10),
            max: 3,
            sliding: true,
        },
        clock.clone(),
    );
    for ms in (0..8).map(|n| n * 1000) {
        clock.set(ms);
        l.hit(&"ip");
    }
    assert_eq!(l.peek(&"ip"), Err(Wait(sec(8))));
    assert_eq!(
        l.check_n(&"ip", NonZeroU32::new(2).unwrap()),
        Err(CheckError::Wait(Wait(sec(9))))
    );
    clock.set(14_999);
    assert_eq!(l.peek(&"ip"), Err(Wait(Duration::from_millis(1))));
    clock.set(15_000);
    assert_eq!(l.peek(&"ip"), Ok(()));
    assert_eq!(
        l.check_n(&"ip", NonZeroU32::new(3).unwrap()),
        Err(CheckError::Wait(Wait(sec(2))))
    );
    clock.set(17_000);
    assert!(l.is_empty());
    assert_eq!(l.check_n(&"ip", NonZeroU32::new(3).unwrap()), Ok(()));
}

#[test]
fn throttle_idle_and_boundary_emit_have_only_one_pending_state() {
    let at = tokio::time::Instant::now();
    let mut t = Throttle::new(sec(5));
    assert!(t.idle(at));
    assert_eq!(t.offer(1, at), Some(1));
    assert!(!t.idle(at + sec(4)));
    assert!(t.idle(at + sec(5)));
    assert_eq!(t.offer(2, at + sec(1)), None);
    assert!(
        !t.idle(at + sec(30)),
        "an overdue held event must not be evicted"
    );
    assert_eq!(t.offer(3, at + sec(5)), Some(3));
    assert_eq!(t.deadline(), None);
    assert_eq!(
        t.due(at + sec(5)),
        None,
        "the older held event must not follow the new leading event"
    );
    assert!(!t.idle(at + sec(9)));
    assert!(t.idle(at + sec(10)));
}
