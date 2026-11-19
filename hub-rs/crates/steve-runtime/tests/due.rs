use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use steve_runtime::due::{FALLBACK, due_loop};
use steve_runtime::policy::{Backoff, Jitter};
use steve_runtime::{Clock, FakeClock};
use tokio::sync::Notify;

const T0: i64 = 1_000_000;

/// A domain's due rows (their due times) and how many batches ran; `failing` makes the next runs fail.
#[derive(Default)]
struct Source {
    rows: VecDeque<i64>,
    batches: usize,
    failing: usize,
}

fn spawn(clock: &Arc<FakeClock>, source: &Arc<Mutex<Source>>) -> tokio::task::JoinHandle<()> {
    let (next, run) = (source.clone(), source.clone());
    tokio::spawn(due_loop(
        "test",
        clock.clone() as Arc<dyn Clock>,
        Arc::new(Notify::new()),
        Backoff {
            base: Duration::from_secs(1),
            max: Duration::from_secs(60),
            factor: 2,
            jitter: Jitter::None,
            attempts: None,
            reset_after: None,
        },
        move || {
            let s = next.clone();
            async move { Ok::<_, String>(s.lock().unwrap().rows.iter().min().copied()) }
        },
        // one row per batch: a batch limit of 1
        move |now| {
            let s = run.clone();
            async move {
                let mut s = s.lock().unwrap();
                s.batches += 1;
                if s.failing > 0 {
                    s.failing -= 1;
                    return Err("source down".to_string());
                }
                if let Some(i) = s.rows.iter().position(|&at| at <= now) {
                    s.rows.remove(i);
                }
                Ok(())
            }
        },
    ))
}

/// Lets the spawned loop run until it waits again.
async fn settle() {
    for _ in 0..100 {
        tokio::task::yield_now().await;
    }
}

fn batches(s: &Arc<Mutex<Source>>) -> usize {
    s.lock().unwrap().batches
}

#[tokio::test]
async fn due_work_is_found_within_the_fallback_without_a_wake() {
    let clock = Arc::new(FakeClock::new(T0));
    let source = Arc::new(Mutex::new(Source::default()));
    let task = spawn(&clock, &source);
    settle().await;
    // a writer stores due work and forgets to wake the loop
    source.lock().unwrap().rows.push_back(T0 + 1_000);
    clock.advance(FALLBACK.as_millis() as i64 - 1);
    settle().await;
    assert_eq!(
        batches(&source),
        0,
        "the loop still sleeps out its fallback"
    );
    clock.advance(1);
    settle().await;
    assert_eq!(batches(&source), 1);
    assert!(source.lock().unwrap().rows.is_empty());
    task.abort();
}

#[tokio::test]
async fn a_failing_source_backs_off_and_recovers() {
    let clock = Arc::new(FakeClock::new(T0));
    let source = Arc::new(Mutex::new(Source {
        rows: VecDeque::from([T0]),
        failing: 2,
        ..Source::default()
    }));
    let task = spawn(&clock, &source);
    settle().await;
    assert_eq!(batches(&source), 1, "first try fails");
    clock.advance(999);
    settle().await;
    assert_eq!(batches(&source), 1, "backs off 1 s");
    clock.advance(1);
    settle().await;
    assert_eq!(batches(&source), 2, "second try fails");
    clock.advance(1_999);
    settle().await;
    assert_eq!(batches(&source), 2, "backs off 2 s");
    clock.advance(1);
    settle().await;
    assert_eq!(batches(&source), 3, "third try succeeds");
    assert!(source.lock().unwrap().rows.is_empty());
    // a success resets the backoff: the next failure waits 1 s again
    {
        let mut s = source.lock().unwrap();
        s.rows.push_back(T0);
        s.failing = 1;
    }
    clock.advance(FALLBACK.as_millis() as i64);
    settle().await;
    assert_eq!(batches(&source), 4);
    clock.advance(1_000);
    settle().await;
    assert_eq!(batches(&source), 5);
    task.abort();
}

#[tokio::test]
async fn more_due_work_runs_at_once() {
    let clock = Arc::new(FakeClock::new(T0));
    let source = Arc::new(Mutex::new(Source {
        rows: VecDeque::from([T0 - 3, T0 - 2, T0 - 1, T0 + 5_000]),
        ..Source::default()
    }));
    let task = spawn(&clock, &source);
    settle().await;
    // three due rows, one per batch, without the clock moving; the fourth is not due yet
    assert_eq!(batches(&source), 3);
    assert_eq!(source.lock().unwrap().rows, VecDeque::from([T0 + 5_000]));
    clock.advance(5_000);
    settle().await;
    assert_eq!(batches(&source), 4);
    task.abort();
}
