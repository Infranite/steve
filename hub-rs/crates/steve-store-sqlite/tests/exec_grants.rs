#[path = "exec_support.rs"]
mod support;
use std::sync::{Arc, Barrier, mpsc};
use steve_storage_api::uow::CommitError;
use steve_store_sqlite::exec::{GrantError, Lane};
use support::*;

#[test]
fn same_engine_facades_reenter_and_last_guard_owns_connection() {
    let (e, _, _) = executor();
    let facade = e.clone();
    let root = e.grant(Lane::Admin).unwrap();
    root.set(7);
    let nested = facade.grant(Lane::Background).unwrap();
    assert!(!root.is_nested());
    assert!(nested.is_nested());
    assert_eq!(nested.get(), 7);
    assert_eq!(e.queue_depth(Lane::Background), 0);
    drop(root); // non-LIFO drops must not release the writer
    assert!(matches!(
        e.submit(Lane::Interactive, job(1)),
        Err(CommitError::UowInsideGrant)
    ));
    let (waiting, ready) = queued_grant(e.clone(), Lane::Admin);
    assert!(ready.try_recv().is_err());
    drop(nested);
    ready.recv().unwrap();
    waiting.finish();
    assert_eq!(e.grant(Lane::Admin).unwrap().get(), 7);
}
#[test]
fn cross_engine_identity_and_submission_inside_grant_are_rejected_immediately() {
    let (a, _, _) = executor();
    let (b, _, _) = executor();
    let g = a.grant(Lane::Interactive).unwrap();
    match b.grant(Lane::Admin) {
        Err(GrantError::CrossFile { outer, inner, site }) => {
            assert_eq!(outer, a.id());
            assert_eq!(inner, b.id());
            assert!(site.contains("exec_grants.rs"));
        }
        _ => panic!("expected cross-engine error"),
    }
    let p = Probe::default();
    metrics::with_local_recorder(&p, || {
        assert!(matches!(
            a.submit(Lane::Ingest, job(1)),
            Err(CommitError::UowInsideGrant)
        ))
    });
    assert_eq!(p.total("steve_store_rejected_total"), 1.);
    assert_eq!(a.queue_depth(Lane::Ingest), 0);
    drop(g);
    assert!(!b.grant(Lane::Admin).unwrap().is_nested());
}
#[test]
fn panic_unwind_returns_connection_and_next_waiter_can_acquire() {
    let (e, _, _) = executor();
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let _root = e.grant(Lane::Admin).unwrap();
        let _nested = e.grant(Lane::Admin).unwrap();
        panic!("unwind both guards");
    }));
    assert!(result.is_err());
    let h = hold(&e, Lane::Interactive);
    h.finish();
}
#[test]
fn grant_and_units_share_fifo_and_only_adjacent_units_merge() {
    let (e, _, runner) = executor();
    let held = hold(&e, Lane::Background);
    let first = e.submit(Lane::Interactive, job(1)).unwrap();
    let (grant, ready) = queued_grant(e.clone(), Lane::Interactive);
    let second = e.submit(Lane::Interactive, job(2)).unwrap();
    held.finish();
    ready.recv().unwrap();
    assert_eq!(first.blocking_recv().unwrap().unwrap(), 1);
    assert_eq!(*runner.batches.lock().unwrap(), vec![vec![1]]);
    assert_eq!(e.queue_depth(Lane::Interactive), 1);
    grant.finish();
    assert_eq!(second.blocking_recv().unwrap().unwrap(), 2);
    assert_eq!(*runner.batches.lock().unwrap(), vec![vec![1], vec![2]]);
}
#[tokio::test]
async fn runtime_probe_counts_grants_on_a_real_tokio_worker() {
    let (e, _, _) = executor_with_probe(|| tokio::runtime::Handle::try_current().is_ok());
    let p = Probe::default();
    metrics::with_local_recorder(&p, || {
        let _g = e.grant(Lane::Interactive).unwrap();
    });
    assert_eq!(p.total("steve_store_tokio_grant_total"), 1.);
    assert_eq!(p.total("steve_store_hold_seconds"), 0.); // virtual clock has not advanced
}
#[test]
fn occupied_tokio_workers_do_not_block_spawn_blocking_grants() {
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .build()
        .unwrap();
    let gate = Arc::new(Barrier::new(3));
    let (entered, ack) = mpsc::channel();
    for _ in 0..2 {
        let gate = gate.clone();
        let entered = entered.clone();
        rt.spawn(async move {
            entered.send(()).unwrap();
            gate.wait();
        });
    }
    ack.recv().unwrap();
    ack.recv().unwrap(); // both Tokio workers are synchronously occupied
    let (e, _, _) = executor_with_probe(|| tokio::runtime::Handle::try_current().is_ok());
    let a = e.clone();
    let first = rt.spawn_blocking(move || {
        let g = a.grant(Lane::Admin).unwrap();
        g.set(1);
    });
    let second = rt.spawn_blocking(move || {
        let g = e.grant(Lane::Interactive).unwrap();
        g.set(2);
    });
    rt.block_on(async {
        first.await.unwrap();
        second.await.unwrap();
    });
    gate.wait();
}

#[derive(Clone, Default)]
struct Warnings(Arc<std::sync::atomic::AtomicU64>);
impl tracing::Subscriber for Warnings {
    fn enabled(&self, _: &tracing::Metadata<'_>) -> bool {
        true
    }
    fn new_span(&self, _: &tracing::span::Attributes<'_>) -> tracing::span::Id {
        tracing::span::Id::from_u64(1)
    }
    fn record(&self, _: &tracing::span::Id, _: &tracing::span::Record<'_>) {}
    fn record_follows_from(&self, _: &tracing::span::Id, _: &tracing::span::Id) {}
    fn event(&self, event: &tracing::Event<'_>) {
        if *event.metadata().level() == tracing::Level::WARN {
            self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        }
    }
    fn enter(&self, _: &tracing::span::Id) {}
    fn exit(&self, _: &tracing::span::Id) {}
}
#[test]
fn long_hold_counters_record_every_hold_but_warn_once_per_site_per_minute() {
    let (e, time, _) = executor();
    let p = Probe::default();
    let warnings = Warnings::default();
    let counts = warnings.0.clone();
    fn long_hold(e: &Exec, time: &Time) {
        let _g = e.grant(Lane::Admin).unwrap();
        time.advance(51);
    }
    tracing::subscriber::with_default(warnings, || {
        metrics::with_local_recorder(&p, || {
            long_hold(&e, &time);
            long_hold(&e, &time);
            time.advance(60_000);
            long_hold(&e, &time);
        })
    });
    assert_eq!(p.total("steve_store_long_hold_total"), 3.);
    assert!((p.total("steve_store_hold_seconds") - 0.153).abs() < 0.000001);
    assert_eq!(counts.load(std::sync::atomic::Ordering::SeqCst), 2);
}

struct SqlRunner;
impl steve_store_sqlite::exec::BatchRunner<rusqlite::Connection> for SqlRunner {
    type Job = ();
    type Output = ();
    fn run(&self, _: &rusqlite::Connection, jobs: Vec<()>) -> Vec<Result<(), CommitError>> {
        jobs.into_iter().map(|_| Ok(())).collect()
    }
}
#[test]
fn sqlite_grant_preserves_autocommit_and_exposes_uncommitted_nested_reads() {
    let c = rusqlite::Connection::open_in_memory().unwrap();
    let e = steve_store_sqlite::exec::Executor::new(
        "sqlite-test",
        c,
        Arc::new(Time::default()),
        SqlRunner,
        || false,
    )
    .unwrap();
    let root = e.grant(Lane::Interactive).unwrap();
    assert_eq!(steve_store_sqlite::exec::current_engine(), Some(e.id()));
    assert!(root.is_autocommit());
    root.execute_batch("CREATE TABLE t(v INTEGER); INSERT INTO t VALUES(1);")
        .unwrap();
    assert!(root.is_autocommit());
    root.execute_batch("BEGIN IMMEDIATE; INSERT INTO t VALUES(2);")
        .unwrap();
    let nested = e.grant(Lane::Interactive).unwrap();
    assert!(nested.is_nested());
    assert!(!nested.is_autocommit());
    assert_eq!(
        nested
            .query_row("SELECT sum(v) FROM t", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        3
    );
    nested.execute_batch("SAVEPOINT caller_owned; INSERT INTO t VALUES(4); ROLLBACK TO caller_owned; RELEASE caller_owned;").unwrap();
    drop(nested);
    root.execute_batch("COMMIT").unwrap();
    drop(root);
    assert_eq!(steve_store_sqlite::exec::current_engine(), None);
    let next = e.grant(Lane::Admin).unwrap();
    assert!(next.is_autocommit());
    assert_eq!(
        next.query_row("SELECT sum(v) FROM t", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        3
    );
}

#[test]
fn thread_local_grant_releases_even_after_ownership_tls_is_destroyed() {
    use std::cell::{Cell, RefCell};
    use steve_store_sqlite::exec::RootGrant;
    thread_local! {
        static KEPT: RefCell<Option<RootGrant<Cell<usize>,Runner>>> = const { RefCell::new(None) };
    }
    for ownership_first in [false, true] {
        let (e, time, _) = executor();
        let facade = e.clone();
        std::thread::spawn(move || {
            if ownership_first {
                assert_eq!(steve_store_sqlite::exec::current_engine(), None);
            }
            KEPT.with(|_| {});
            let g = facade.grant(Lane::Admin).unwrap();
            g.set(42);
            time.advance(51); // exercise cached hold metrics and the warning path at teardown
            KEPT.with(|v| *v.borrow_mut() = Some(g));
        })
        .join()
        .unwrap();
        assert_eq!(e.grant(Lane::Interactive).unwrap().get(), 42);
    }
}
