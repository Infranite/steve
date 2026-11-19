use futures::poll;
use std::time::Duration;
use steve_runtime::{Clock, FakeClock, SystemClock};

#[tokio::test(start_paused = true)]
async fn system_sleep_starts_on_first_poll() {
    let mut wait = SystemClock.sleep(Duration::from_secs(10));
    tokio::time::advance(Duration::from_secs(20)).await;
    assert!(poll!(&mut wait).is_pending());
    tokio::time::advance(Duration::from_secs(9)).await;
    assert!(poll!(&mut wait).is_pending());
    tokio::time::advance(Duration::from_secs(1)).await;
    assert!(poll!(&mut wait).is_ready());
    assert!(SystemClock.now() > 0);
}
#[tokio::test]
async fn manual_clock_registration_handshake_deadlines_and_cancellation() {
    let c = FakeClock::new(100);
    assert!(!c.observed(100));
    let (mut a, mut b) = (
        c.sleep(Duration::from_millis(5)),
        c.sleep(Duration::from_millis(10)),
    );
    assert!(poll!(&mut a).is_pending());
    assert!(c.observed(100));
    let mut early = c.changed(); // old ManualClock sleeps wake on any advance
    assert_eq!(c.advance(4), 104);
    assert!(poll!(&mut early).is_ready());
    assert!(poll!(&mut a).is_pending());
    assert!(c.observed(104));
    c.advance_to(105);
    assert!(poll!(&mut a).is_ready());
    assert!(poll!(&mut b).is_pending());
    c.set(110);
    assert!(poll!(&mut b).is_ready());
    let (mut a, mut b) = (
        c.sleep(Duration::from_millis(1)),
        c.sleep(Duration::from_millis(1)),
    );
    assert!(poll!(&mut a).is_pending() && poll!(&mut b).is_pending());
    c.advance(1);
    assert!(poll!(&mut a).is_ready() && poll!(&mut b).is_ready());
    let cancelled = c.sleep(Duration::from_secs(99));
    drop(cancelled);
    c.set(-10);
    assert_eq!(c.now(), -10);
    let mut zero = c.sleep(Duration::ZERO);
    assert!(poll!(&mut zero).is_ready());
}

#[tokio::test]
async fn retry_waits_observe_any_wall_clock_change_without_waiting_for_the_period() {
    let c = FakeClock::new(100);
    let mut retry = c.sleep_or_change(Duration::from_secs(5));
    // Registration happens before the send, even if the wait has not polled yet.
    c.advance(1);
    assert!(poll!(&mut retry).is_ready());
    let mut retry = c.sleep_or_change(Duration::from_secs(5));
    assert!(poll!(&mut retry).is_pending());
    assert!(c.observed(101));
    c.advance(1);
    assert!(poll!(&mut retry).is_ready());
    let mut period = c.sleep(Duration::from_secs(5));
    assert!(poll!(&mut period).is_pending());
    c.advance(1);
    assert!(
        poll!(&mut period).is_pending(),
        "periodic waits retain their deadline"
    );
}

#[tokio::test]
async fn a_sleep_uses_elapsed_time_when_the_wall_clock_is_rewound() {
    let c = FakeClock::new(100);
    let mut wait = c.sleep(Duration::from_millis(10));
    assert!(poll!(&mut wait).is_pending());
    c.set(-1_000);
    c.advance(9);
    assert!(poll!(&mut wait).is_pending());
    c.advance(1);
    assert!(poll!(&mut wait).is_ready());
    assert_eq!(c.now(), -990);
}
