//! Deterministic executor/Engine integration tests: no wall-clock sleeps.
use super::*;
use crate::{
    checkpoint::{self, CheckpointPolicy, MaintenancePolicy, Mode},
    engine::Engine,
    open,
};
use rusqlite::hooks::{AuthAction, AuthContext, Authorization};
use std::sync::{atomic::AtomicUsize, mpsc};
#[derive(Default)]
struct Time(AtomicU64);
impl Clock for Time {
    fn now(&self) -> Duration {
        Duration::from_millis(self.0.load(Ordering::SeqCst))
    }
}
impl Time {
    fn advance(&self, ms: u64) {
        self.0.fetch_add(ms, Ordering::SeqCst);
    }
}
struct Runner {
    ticks: mpsc::Sender<(Duration, bool, Option<Duration>)>,
    inner: crate::store::EngineRunner<()>,
}
impl BatchRunner<Arc<Engine>> for Runner {
    type Job = ();
    type Output = ();
    fn run(&self, _: &Arc<Engine>, jobs: Vec<()>) -> Vec<Result<(), CommitError>> {
        jobs.into_iter().map(|_| Ok(())).collect()
    }
    fn maintenance(&self, c: &Arc<Engine>, now: Duration, idle: bool) -> Option<Duration> {
        let next = self.inner.maintenance(c, now, idle);
        let _ = self.ticks.send((now, idle, next));
        next
    }
}
type Exec = Executor<Arc<Engine>, Runner>;
struct Setup {
    _dir: tempfile::TempDir,
    engine: Arc<Engine>,
    exec: Exec,
    time: Arc<Time>,
    ticks: mpsc::Receiver<(Duration, bool, Option<Duration>)>,
    checkpoints: Arc<AtomicUsize>,
    optimizes: Arc<AtomicUsize>,
}
fn setup(admin: bool, policy: MaintenancePolicy) -> Setup {
    let dir = tempfile::tempdir().unwrap();
    let mut engine = open::open_runtime(dir.path()).unwrap();
    engine.writer_conn().unwrap().execute_batch("CREATE TABLE maintained(id INTEGER PRIMARY KEY,payload BLOB); INSERT INTO maintained(id,payload) VALUES(0,x'00');").unwrap();
    engine.checkpoint(Mode::Truncate).unwrap();
    if admin {
        drop(engine);
        engine = open::open_admin(dir.path()).unwrap();
    }
    let engine = Arc::new(engine.with_maintenance_policy(policy).unwrap());
    let checkpoints = Arc::new(AtomicUsize::new(0));
    let optimizes = Arc::new(AtomicUsize::new(0));
    let (cp, opt) = (checkpoints.clone(), optimizes.clone());
    engine
        .writer_conn()
        .unwrap()
        .authorizer(Some(move |ctx: AuthContext<'_>| {
            if let AuthAction::Pragma { pragma_name, .. } = ctx.action {
                if pragma_name == "wal_checkpoint" {
                    cp.fetch_add(1, Ordering::SeqCst);
                }
                if pragma_name == "optimize" {
                    opt.fetch_add(1, Ordering::SeqCst);
                }
            }
            Authorization::Allow
        }))
        .unwrap();
    let (tx, ticks) = mpsc::channel();
    let time = Arc::new(Time::default());
    let inner = crate::store::EngineRunner(Arc::new(
        crate::uow::SqliteBatchRunner::new(
            &engine.writer_conn().unwrap(),
            Arc::new(crate::domains::Registry::default()),
            Arc::new(crate::uow::SystemWallClock),
            Arc::new(|_, _: Vec<()>| {}),
        )
        .unwrap(),
    ));
    let exec = Executor::new(
        "maintained",
        engine.clone(),
        time.clone(),
        Runner { ticks: tx, inner },
        || false,
    )
    .unwrap();
    assert_eq!(
        ticks.recv_timeout(Duration::from_secs(10)).unwrap().0,
        Duration::ZERO
    );
    Setup {
        _dir: dir,
        engine,
        exec,
        time,
        ticks,
        checkpoints,
        optimizes,
    }
}
fn until(s: &Setup, ms: u64) -> (Duration, bool, Option<Duration>) {
    loop {
        let tick = s.ticks.recv_timeout(Duration::from_secs(10)).unwrap();
        if tick.0 == Duration::from_millis(ms) {
            return tick;
        }
    }
}
#[test]
fn idle_deadlines_run_passive_and_hourly_optimize_without_a_tokio_runtime() {
    let s = setup(false, MaintenancePolicy::default());
    assert_eq!(s.checkpoints.load(Ordering::SeqCst), 0);
    assert_eq!(s.optimizes.load(Ordering::SeqCst), 0);
    s.time.advance(30_000);
    s.exec.wake();
    until(&s, 30_000);
    assert_eq!(s.checkpoints.load(Ordering::SeqCst), 1);
    assert_eq!(s.optimizes.load(Ordering::SeqCst), 0);
    s.time.advance(3_570_000);
    s.exec.wake();
    until(&s, 3_600_000);
    assert_eq!(s.optimizes.load(Ordering::SeqCst), 1);
    let grant = s.exec.grant(Lane::Admin).unwrap();
    drop(grant);
    until(&s, 3_600_000);
    assert_eq!(s.optimizes.load(Ordering::SeqCst), 1);
    s.time.advance(3_600_000);
    s.exec.wake();
    until(&s, 7_200_000);
    assert_eq!(s.optimizes.load(Ordering::SeqCst), 2);
}
#[test]
fn held_grant_blocks_maintenance_pressure_runs_at_gap_and_busy_retries_then_truncates() {
    let s = setup(
        false,
        MaintenancePolicy {
            checkpoint: CheckpointPolicy {
                passive_bytes: 4096,
                force_bytes: 8192,
                interval: Duration::from_secs(30),
            },
            ..MaintenancePolicy::default()
        },
    );
    let reader = s.engine.read_conn().unwrap();
    reader
        .execute_batch("BEGIN; SELECT * FROM maintained;")
        .unwrap();
    let grant = s.exec.grant(Lane::Admin).unwrap();
    s.engine
        .writer_conn()
        .unwrap()
        .execute(
            "INSERT INTO maintained(payload) VALUES(zeroblob(32768))",
            [],
        )
        .unwrap();
    s.time.advance(1000);
    // Synchronize with the worker actually observing busy, rather than reading
    // a counter immediately after wake before the worker has had a turn.
    let shared = s.exec.shared();
    let mut state = lock(&shared.state);
    let observed = state.busy_waits;
    shared.changed.notify_all();
    while state.busy_waits == observed {
        let (next, timeout) = shared
            .changed
            .wait_timeout(state, Duration::from_secs(10))
            .unwrap();
        state = next;
        assert!(
            !timeout.timed_out(),
            "worker did not observe the held grant"
        );
    }
    assert_eq!(
        s.checkpoints.load(Ordering::SeqCst),
        0,
        "worker observed the held writer grant without performing maintenance"
    );
    drop(state);
    // Another caller queues behind the grant; submitting on this thread would
    // correctly fail UowInsideGrant before it ever reaches the queue.
    let queued = std::thread::scope(|scope| {
        scope
            .spawn(|| s.exec.submit(Lane::Interactive, ()).unwrap())
            .join()
            .unwrap()
    });
    drop(grant);
    let tick = until(&s, 1000);
    assert!(!tick.1, "queued work forces PASSIVE, not idle TRUNCATE");
    queued.blocking_recv().unwrap().unwrap();
    assert!(s.checkpoints.load(Ordering::SeqCst) >= 1);
    // Consume the maintenance turn requested when that batch released its writer.
    until(&s, 1000);
    let attempted = s.checkpoints.load(Ordering::SeqCst);
    s.time.advance(1000);
    s.exec.wake();
    until(&s, 2000);
    assert!(
        s.checkpoints.load(Ordering::SeqCst) > attempted,
        "pinned reader retries at the configured deadline"
    );
    assert!(
        std::fs::metadata(s.engine.path().with_extension("sqlite3-wal"))
            .unwrap()
            .len()
            > 0
    );
    reader.execute_batch("ROLLBACK").unwrap();
    drop(reader);
    s.time.advance(1000);
    s.exec.wake();
    until(&s, 3000);
    assert_eq!(
        std::fs::metadata(s.engine.path().with_extension("sqlite3-wal"))
            .unwrap()
            .len(),
        0
    );
    assert_eq!(
        s.engine
            .writer_conn()
            .unwrap()
            .pragma_query_value(None, "busy_timeout", |r| r.get::<_, u32>(0))
            .unwrap(),
        5000
    );
}
#[test]
fn administrative_engines_never_schedule_checkpoint_or_optimize() {
    let s = setup(true, MaintenancePolicy::default());
    s.time.advance(7_200_000);
    s.exec.wake();
    let grant = s.exec.grant(Lane::Admin).unwrap();
    drop(grant);
    until(&s, 7_200_000);
    assert_eq!(s.checkpoints.load(Ordering::SeqCst), 0);
    assert_eq!(s.optimizes.load(Ordering::SeqCst), 0);
}
#[test]
fn intervals_are_positive_and_defaults_are_the_documented_hour() {
    assert_eq!(checkpoint::OPTIMIZE_INTERVAL, Duration::from_secs(3600));
    let dir = tempfile::tempdir().unwrap();
    assert!(
        open::open_runtime(dir.path())
            .unwrap()
            .with_maintenance_policy(MaintenancePolicy {
                optimize_interval: Duration::ZERO,
                ..MaintenancePolicy::default()
            })
            .is_err()
    );
}

#[test]
fn optimize_failure_retries_on_virtual_deadline_instead_of_waiting_another_hour() {
    let s = setup(false, MaintenancePolicy::default());
    let fail = Arc::new(std::sync::atomic::AtomicBool::new(true));
    let attempts = s.optimizes.clone();
    let failing = fail.clone();
    s.engine
        .writer_conn()
        .unwrap()
        .authorizer(Some(move |ctx: AuthContext<'_>| {
            if let AuthAction::Pragma {
                pragma_name: "optimize",
                ..
            } = ctx.action
            {
                attempts.fetch_add(1, Ordering::SeqCst);
                if failing.swap(false, Ordering::SeqCst) {
                    return Authorization::Deny;
                }
            }
            Authorization::Allow
        }))
        .unwrap();
    s.time.advance(3_600_000);
    s.exec.wake();
    until(&s, 3_600_000);
    assert_eq!(s.optimizes.load(Ordering::SeqCst), 1);
    s.time.advance(1000);
    s.exec.wake();
    until(&s, 3_601_000);
    assert_eq!(s.optimizes.load(Ordering::SeqCst), 2);
    let grant = s.exec.grant(Lane::Admin).unwrap();
    drop(grant);
    until(&s, 3_601_000);
    assert_eq!(s.optimizes.load(Ordering::SeqCst), 2);
}

fn wal_bytes(s: &Setup) -> u64 {
    std::fs::metadata(s.engine.path().with_extension("sqlite3-wal"))
        .unwrap()
        .len()
}
fn commit_at(s: &Setup, advance_ms: u64) {
    let grant = s.exec.grant(Lane::Admin).unwrap();
    s.engine
        .writer_conn()
        .unwrap()
        .execute("INSERT INTO maintained(payload) VALUES(x'01')", [])
        .unwrap();
    s.time.advance(advance_ms);
    drop(grant);
    let tick = until(s, s.time.now().as_millis() as u64);
    assert!(!tick.1, "returning a writer alone is not an idle deadline");
}
#[test]
fn isolated_commits_do_not_immediately_truncate_wal() {
    let s = setup(false, MaintenancePolicy::default());
    commit_at(&s, 1000);
    let first = wal_bytes(&s);
    assert!(first > 0);
    commit_at(&s, 1000);
    assert!(wal_bytes(&s) > first);
    assert_eq!(s.checkpoints.load(Ordering::SeqCst), 0);
}
#[test]
fn idle_deadline_truncates_wal_after_commits_stop() {
    let s = setup(false, MaintenancePolicy::default());
    commit_at(&s, 1000);
    s.time.advance(28_999);
    s.exec.wake();
    // A grant round trip synchronizes the worker before the deadline.
    let grant = s.exec.grant(Lane::Admin).unwrap();
    drop(grant);
    until(&s, 29_999);
    assert_eq!(s.checkpoints.load(Ordering::SeqCst), 0);
    assert!(wal_bytes(&s) > 0);
    s.time.advance(1);
    s.exec.wake();
    let tick = until(&s, 30_000);
    assert!(tick.1, "empty queue at the due deadline permits TRUNCATE");
    assert_eq!(s.checkpoints.load(Ordering::SeqCst), 1);
    assert_eq!(wal_bytes(&s), 0);
}
