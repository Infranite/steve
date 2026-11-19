use std::sync::{Arc, Mutex};
use std::time::Duration;
use steve_runtime::due::contract::{Fixture, check};
use steve_runtime::due::{DueError, DueSource, FALLBACK, due_loop};
use steve_runtime::policy::{Backoff, Jitter};
use steve_runtime::{Clock, FakeClock};
use tokio::sync::Notify;

const T0: i64 = 1_000_000;
const LIMIT: usize = 5;
const TRIES: u32 = 3;

#[derive(Clone)]
struct Row {
    id: String,
    at: i64,
    poison: bool,
    tries: u32,
    done: bool,
    dead: bool,
}

/// A reference source over an in-memory table: filters by due time before its limit, retries a
/// failing row an hour later on its own, and dead-letters it after [`TRIES`].
#[derive(Default)]
struct Table {
    rows: Mutex<Vec<Row>>,
    /// The B3 defect, to show the contract check catches it: rows in insertion order, limit, then filter.
    limit_first: bool,
    /// The whole source is down for this many calls.
    down: Mutex<u32>,
    batches: Mutex<usize>,
}

impl DueSource for Table {
    fn name(&self) -> &'static str {
        "table"
    }
    async fn next_due(&self) -> Result<Option<i64>, DueError> {
        let rows = self.rows.lock().unwrap();
        Ok(rows
            .iter()
            .filter(|r| !r.done && !r.dead)
            .map(|r| r.at)
            .min())
    }
    async fn run_due(&self, now: i64) -> Result<(), DueError> {
        *self.batches.lock().unwrap() += 1;
        {
            let mut down = self.down.lock().unwrap();
            if *down > 0 {
                *down -= 1;
                return Err(DueError("store unavailable".into()));
            }
        }
        let mut rows = self.rows.lock().unwrap();
        let mut open: Vec<usize> = (0..rows.len())
            .filter(|&i| !rows[i].done && !rows[i].dead)
            .collect();
        let batch: Vec<usize> = if self.limit_first {
            // B3 as it was: oldest rows first, LIMIT, and only then the due filter
            open.into_iter()
                .take(LIMIT)
                .filter(|&i| rows[i].at <= now)
                .collect()
        } else {
            open.sort_by_key(|&i| rows[i].at);
            open.into_iter()
                .filter(|&i| rows[i].at <= now)
                .take(LIMIT)
                .collect()
        };
        for i in batch {
            let r = &mut rows[i];
            if r.poison {
                r.tries += 1;
                r.dead = r.tries >= TRIES;
                r.at = now + 3_600_000;
            } else {
                r.done = true;
            }
        }
        Ok(())
    }
}

impl Table {
    fn add(&self, at: i64, poison: bool) -> String {
        let mut rows = self.rows.lock().unwrap();
        let id = format!("r{}", rows.len());
        rows.push(Row {
            id: id.clone(),
            at,
            poison,
            tries: 0,
            done: false,
            dead: false,
        });
        id
    }
    fn ids(&self, f: impl Fn(&Row) -> bool) -> Vec<String> {
        self.rows
            .lock()
            .unwrap()
            .iter()
            .filter(|r| f(r))
            .map(|r| r.id.clone())
            .collect()
    }
    fn batches(&self) -> usize {
        *self.batches.lock().unwrap()
    }
}

impl Fixture for Table {
    type Source = Table;
    fn source(&self) -> &Table {
        self
    }
    fn batch(&self) -> usize {
        LIMIT
    }
    fn add(&self, at: i64, poison: bool) -> String {
        Table::add(self, at, poison)
    }
    fn done(&self) -> Vec<String> {
        self.ids(|r| r.done)
    }
    fn dead(&self) -> Vec<String> {
        self.ids(|r| r.dead)
    }
}

#[tokio::test]
async fn the_reference_source_meets_the_contract() {
    check(Table::default, T0, TRIES + 1).await;
}

#[tokio::test]
#[should_panic(expected = "filter by due time before the batch limit")]
async fn the_contract_catches_a_limit_before_the_due_filter() {
    check(
        || Table {
            limit_first: true,
            ..Table::default()
        },
        T0,
        TRIES + 1,
    )
    .await;
}

fn spawn(clock: &Arc<FakeClock>, table: &Arc<Table>) -> tokio::task::JoinHandle<()> {
    let backoff = Backoff {
        base: Duration::from_secs(1),
        max: Duration::from_secs(60),
        factor: 2,
        jitter: Jitter::None,
        attempts: None,
        reset_after: None,
    };
    tokio::spawn(due_loop(
        table.clone(),
        clock.clone() as Arc<dyn Clock>,
        Arc::new(Notify::new()),
        backoff,
    ))
}

/// Lets the spawned loop run until it waits again.
async fn settle() {
    for _ in 0..100 {
        tokio::task::yield_now().await;
    }
}

#[tokio::test]
async fn due_work_is_found_within_the_fallback_without_a_wake() {
    let clock = Arc::new(FakeClock::new(T0));
    let table = Arc::new(Table::default());
    let task = spawn(&clock, &table);
    settle().await;
    // a writer stores due work and forgets to wake the loop
    table.add(T0 + 1_000, false);
    clock.advance(FALLBACK.as_millis() as i64 - 1);
    settle().await;
    assert_eq!(table.batches(), 0, "the loop still sleeps out its fallback");
    clock.advance(1);
    settle().await;
    assert_eq!(table.batches(), 1);
    assert_eq!(table.done().len(), 1);
    task.abort();
}

#[tokio::test]
async fn a_failing_source_backs_off_and_recovers() {
    let clock = Arc::new(FakeClock::new(T0));
    let table = Arc::new(Table {
        down: Mutex::new(2),
        ..Table::default()
    });
    table.add(T0, false);
    let task = spawn(&clock, &table);
    settle().await;
    assert_eq!(table.batches(), 1, "first try fails");
    clock.advance(999);
    settle().await;
    assert_eq!(table.batches(), 1, "backs off 1 s");
    clock.advance(1);
    settle().await;
    assert_eq!(table.batches(), 2, "second try fails");
    clock.advance(1_999);
    settle().await;
    assert_eq!(table.batches(), 2, "backs off 2 s");
    clock.advance(1);
    settle().await;
    assert_eq!(table.batches(), 3, "third try succeeds");
    assert_eq!(table.done().len(), 1);
    // a success resets the backoff: the next failure waits 1 s again
    table.add(clock.now(), false);
    *table.down.lock().unwrap() = 1;
    clock.advance(FALLBACK.as_millis() as i64);
    settle().await;
    assert_eq!(table.batches(), 4);
    clock.advance(1_000);
    settle().await;
    assert_eq!(table.batches(), 5);
    assert_eq!(table.done().len(), 2);
    task.abort();
}

#[tokio::test]
async fn more_due_work_than_a_batch_runs_at_once() {
    let clock = Arc::new(FakeClock::new(T0));
    let table = Arc::new(Table::default());
    for i in 0..(2 * LIMIT + 1) as i64 {
        table.add(T0 - 100 + i, false);
    }
    table.add(T0 + 5_000, false);
    let task = spawn(&clock, &table);
    settle().await;
    // eleven due rows, five per batch, without the clock moving; the twelfth is not due yet
    assert_eq!(table.batches(), 3);
    assert_eq!(table.done().len(), 2 * LIMIT + 1);
    clock.advance(5_000);
    settle().await;
    assert_eq!(table.batches(), 4);
    assert_eq!(table.done().len(), 2 * LIMIT + 2);
    task.abort();
}

#[tokio::test]
async fn a_source_can_keep_a_shorter_recovery_bound() {
    struct Short(Arc<Table>);
    impl DueSource for Short {
        fn name(&self) -> &'static str {
            "short"
        }
        fn max_sleep(&self) -> Duration {
            Duration::from_secs(10)
        }
        async fn next_due(&self) -> Result<Option<i64>, DueError> {
            self.0.next_due().await
        }
        async fn run_due(&self, now: i64) -> Result<(), DueError> {
            self.0.run_due(now).await
        }
    }
    let clock = Arc::new(FakeClock::new(T0));
    let table = Arc::new(Table::default());
    let task = tokio::spawn(due_loop(
        Arc::new(Short(table.clone())),
        clock.clone(),
        Arc::new(Notify::new()),
        Backoff {
            base: Duration::from_secs(1),
            max: Duration::from_secs(60),
            factor: 2,
            jitter: Jitter::None,
            attempts: None,
            reset_after: None,
        },
    ));
    settle().await;
    table.add(T0, false);
    clock.advance(9999);
    settle().await;
    assert_eq!(table.batches(), 0);
    clock.advance(1);
    settle().await;
    assert_eq!(table.batches(), 1);
    task.abort();
}
