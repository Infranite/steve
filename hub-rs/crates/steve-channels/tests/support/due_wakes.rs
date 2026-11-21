//! Real sources run with a fixed domain wall time and independently advanced fake wait time.
use std::{
    future::Future,
    pin::Pin,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};
use steve_channels::Channels;
use steve_runtime::{
    Clock, FakeClock,
    due::{DueError, DueSource},
    policy::{Backoff, Jitter},
};
struct FixedWall {
    wall: i64,
    elapsed: Arc<FakeClock>,
}
impl Clock for FixedWall {
    fn now(&self) -> i64 {
        self.wall
    }
    fn sleep(&self, d: Duration) -> Pin<Box<dyn Future<Output = ()> + Send + '_>> {
        self.elapsed.sleep(d)
    }
}
struct Counted<S> {
    source: S,
    runs: AtomicUsize,
}
impl<S: DueSource> DueSource for Counted<S> {
    fn name(&self) -> &'static str {
        self.source.name()
    }
    async fn next_due(&self) -> Result<Option<i64>, DueError> {
        self.source.next_due().await
    }
    async fn run_due(&self, now: i64) -> Result<(), DueError> {
        self.runs.fetch_add(1, Ordering::SeqCst);
        // Keep a broken busy loop observable by this test instead of monopolizing its executor.
        tokio::task::yield_now().await;
        self.source.run_due(now).await
    }
}
async fn settle() {
    for _ in 0..100 {
        tokio::task::yield_now().await;
    }
}
pub async fn assert_no_self_wake_loop<S: DueSource>(
    source: S,
    ch: &Channels,
    tables: &[&'static str],
) {
    let wall = ch.now();
    let elapsed = Arc::new(FakeClock::new(0));
    let source = Arc::new(Counted {
        source,
        runs: AtomicUsize::new(0),
    });
    let wake = ch.store().commit_wake(tables);
    let touched = ch.store().transaction(|tx| -> steve_modules::ApiResult<_> {
        for table in tables {
            // Production constants contain trusted table names. Exercise a real watched write
            // without changing the fixture's business fields or advancing its wall clock.
            let n = tx.execute(&format!("UPDATE {table} SET rowid=rowid WHERE rowid IN (SELECT rowid FROM {table} LIMIT 1)"), [])?;
            if n > 0 { return Ok(n); }
        }
        Ok(0)
    }).unwrap();
    assert!(
        touched > 0,
        "fixture must exercise its production subscription"
    );
    let mut cx = std::task::Context::from_waker(std::task::Waker::noop());
    assert!(
        std::pin::pin!(wake.notified())
            .as_mut()
            .poll(&mut cx)
            .is_ready(),
        "a real commit must wake the source before boundedness is tested"
    );
    let task = steve_runtime::spawn(
        "test.commit_wake",
        steve_runtime::due::due_loop(
            source.clone(),
            Arc::new(FixedWall {
                wall,
                elapsed: elapsed.clone(),
            }),
            wake,
            Backoff {
                base: Duration::from_secs(1),
                max: Duration::from_secs(1),
                factor: 1,
                jitter: Jitter::None,
                attempts: None,
                reset_after: None,
            },
        ),
    );
    settle().await;
    assert!(!task.is_finished(), "the source loop must remain alive");
    assert_eq!(
        source.runs.load(Ordering::SeqCst),
        1,
        "a self-written row must leave its next deadline later"
    );
    for _ in 0..3 {
        elapsed.advance(60_000);
        settle().await;
        assert!(
            !task.is_finished(),
            "a stopped loop must not masquerade as bounded work"
        );
        assert_eq!(ch.now(), wall, "domain wall time is frozen");
        assert_eq!(
            source.runs.load(Ordering::SeqCst),
            1,
            "self notifications may recheck, never rerun due work without a wall deadline"
        );
    }
    task.stop();
    task.await;
}
