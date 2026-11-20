use std::sync::Arc;
use steve_storage_api::{CommitError, Domain};
use steve_store_sqlite::{
    domains::Registry,
    exec::{self, Lane},
    open,
    store::SqliteStore,
    uow::SystemWallClock,
};

fn store(dir: &std::path::Path) -> SqliteStore {
    SqliteStore::new_sync(
        vec![open::open_runtime(dir).unwrap()],
        Arc::new(Registry::default()),
        Arc::new(SystemWallClock),
        Arc::new(|_, _: Vec<()>| {}),
        exec::outside_blocking_scope,
    )
    .unwrap()
}

#[test]
fn same_engine_domains_share_connection_until_last_guard_drops() {
    let dir = tempfile::tempdir().unwrap();
    let s = store(dir.path());
    let outer = s.root_guard(Domain::Core, Lane::Interactive).unwrap();
    outer
        .execute_batch(
            "CREATE TABLE root_reentry(x); BEGIN IMMEDIATE; INSERT INTO root_reentry VALUES(7)",
        )
        .unwrap();
    let nested = s.root_guard(Domain::Stream, Lane::Background).unwrap();
    assert!(std::ptr::eq(outer.connection(), nested.connection()));
    drop(outer); // Non-LIFO is legal: nested still owns mutex and scheduler lease.
    assert_eq!(
        nested
            .query_row("SELECT x FROM root_reentry", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        7
    );
    assert_eq!(
        s.with_active_root(Domain::Comms, |c| c
            .query_row("SELECT x FROM root_reentry", [], |r| r.get::<_, i64>(0))
            .unwrap()),
        Some(7)
    );
    assert_eq!(
        s.engine(Domain::Core)
            .read_conn()
            .unwrap()
            .query_row("SELECT count(*) FROM root_reentry", [], |r| r
                .get::<_, i64>(0))
            .unwrap(),
        0
    );
    drop(nested);
    assert!(s.with_active_root(Domain::Core, |_| ()).is_none());
    let next = s.root_guard(Domain::Core, Lane::Admin).unwrap();
    assert!(next.is_autocommit());
    assert_eq!(
        next.query_row("SELECT count(*) FROM root_reentry", [], |r| r
            .get::<_, i64>(0))
            .unwrap(),
        0
    );
}

#[test]
fn nested_bridge_failure_and_panic_preserve_outer_transaction_and_revision() {
    let dir = tempfile::tempdir().unwrap();
    let s = store(dir.path());
    s.with_root(Domain::Core, Lane::Interactive, |c, _, outer_rev| {
        c.execute_batch(
            "CREATE TABLE root_bridge(x); BEGIN IMMEDIATE; INSERT INTO root_bridge VALUES(1)",
        )
        .unwrap();
        let result: Result<(), CommitError> =
            s.with_root(Domain::Stream, Lane::Interactive, |c, _, inner_rev| {
                assert_eq!(outer_rev.rev(), inner_rev.rev());
                c.execute_batch("INSERT INTO root_bridge VALUES(2)")
                    .unwrap();
                Err(CommitError::UowInsideGrant)
            });
        assert!(result.is_err());
        let panic = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _: Result<(), CommitError> = s.with_root(Domain::Comms, Lane::Admin, |c, _, _| {
                c.execute_batch("INSERT INTO root_bridge VALUES(3)")
                    .unwrap();
                panic!("inner business panic");
            });
        }));
        assert!(panic.is_err());
        assert!(!c.is_autocommit());
        assert_eq!(
            c.query_row("SELECT sum(x) FROM root_bridge", [], |r| r.get::<_, i64>(0))
                .unwrap(),
            1
        );
        c.execute_batch("INSERT INTO root_bridge VALUES(4); COMMIT")
            .unwrap();
        Ok(())
    })
    .unwrap();
    let next = s.root_guard(Domain::Core, Lane::Admin).unwrap();
    assert!(next.is_autocommit());
    assert_eq!(
        next.query_row("SELECT sum(x) FROM root_bridge", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        5
    );
}

#[test]
fn active_connection_closure_keeps_authorization_after_dropping_outer_guard() {
    let dir = tempfile::tempdir().unwrap();
    let s = store(dir.path());
    let outer = s.root_guard(Domain::Core, Lane::Interactive).unwrap();
    s.with_active_root(Domain::Core, |c| {
        drop(outer);
        assert!(exec::current_engine().is_some());
        let nested = s.root_guard(Domain::Stream, Lane::Admin).unwrap();
        assert!(std::ptr::eq(c, nested.connection()));
    })
    .unwrap();
    assert!(exec::current_engine().is_none());
}

#[test]
fn raw_guard_reserves_revision_before_nested_bridge_opens_in_transaction() {
    let dir = tempfile::tempdir().unwrap();
    let s = store(dir.path());
    let outer = s.root_guard(Domain::Core, Lane::Interactive).unwrap();
    outer
        .execute_batch("CREATE TABLE root_raw(x); BEGIN IMMEDIATE")
        .unwrap();
    s.with_root(Domain::Stream, Lane::Admin, |c, _, _| {
        c.execute_batch("INSERT INTO root_raw VALUES(1)").unwrap();
        Ok(())
    })
    .unwrap();
    assert!(!outer.is_autocommit());
    outer.execute_batch("COMMIT").unwrap();
}

#[derive(Default)]
struct Hint(std::sync::atomic::AtomicUsize);
impl steve_storage_api::CommitHint for Hint {
    fn is_alive(&self) -> bool {
        true
    }
    fn notify(&self) {
        self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    }
}
#[test]
fn reentrant_guards_keep_one_capture_and_publish_at_the_last_owner_exit() {
    use std::sync::atomic::Ordering::SeqCst;
    let dir = tempfile::tempdir().unwrap();
    let s = store(dir.path());
    s.root_guard(Domain::Core, Lane::Admin)
        .unwrap()
        .execute_batch("CREATE TABLE root_watch(x)")
        .unwrap();
    let hint = Arc::new(Hint::default());
    s.subscribe_tables(Domain::Core, &["root_watch"], hint.clone());
    let outer = s.root_guard(Domain::Core, Lane::Interactive).unwrap();
    let inner = s.root_guard(Domain::Stream, Lane::Background).unwrap();
    inner
        .execute_batch("INSERT INTO root_watch VALUES(1)")
        .unwrap();
    drop(outer);
    assert_eq!(hint.0.load(SeqCst), 0);
    drop(inner);
    assert_eq!(hint.0.load(SeqCst), 1);
    let outer = s.root_guard(Domain::Core, Lane::Interactive).unwrap();
    outer
        .execute_batch("BEGIN IMMEDIATE; INSERT INTO root_watch VALUES(2)")
        .unwrap();
    drop(s.root_guard(Domain::Comms, Lane::Admin).unwrap());
    assert!(!outer.is_autocommit());
    drop(outer); // Rollback still precedes the single capture exit.
    assert_eq!(hint.0.load(SeqCst), 1);
}
