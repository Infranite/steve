use std::{
    sync::{Arc, Condvar, Mutex, mpsc},
    time::Duration,
};
use steve_storage_api::Domain;
use steve_store_sqlite::{
    domains::Registry, exec::Lane, open, store::SqliteStore, uow::SystemWallClock,
};

fn store(dir: &std::path::Path, admin: bool) -> SqliteStore {
    let engine = if admin {
        open::open_admin(dir)
    } else {
        open::open_runtime(dir)
    }
    .unwrap();
    SqliteStore::new_sync(
        vec![engine],
        Arc::new(Registry::verification().unwrap()),
        Arc::new(SystemWallClock),
        Arc::new(|_, _: Vec<()>| {}),
        || false,
    )
    .unwrap()
}
#[test]
fn synchronous_root_guard_reads_own_transaction_and_rejects_nested_facade_lock() {
    assert!(tokio::runtime::Handle::try_current().is_err());
    let dir = tempfile::tempdir().unwrap();
    let s = store(dir.path(), false);
    let g = s.root_guard(Domain::Core, Lane::Interactive).unwrap();
    g.execute_batch("CREATE TABLE engine_guard_test(x); BEGIN IMMEDIATE; INSERT INTO engine_guard_test VALUES(7)").unwrap();
    // Repository readers in a carried scope borrow this guard's connection.
    assert_eq!(
        g.connection()
            .query_row("SELECT x FROM engine_guard_test", [], |r| r
                .get::<_, i64>(0))
            .unwrap(),
        7
    );
    assert!(s.root_guard(Domain::Core, Lane::Admin).is_err());
    assert_eq!(
        s.engine(Domain::Core)
            .read_conn()
            .unwrap()
            .query_row("SELECT count(*) FROM engine_guard_test", [], |r| r
                .get::<_, i64>(0))
            .unwrap(),
        0
    );
    drop(g); // An uncommitted scope rolls back before returning authorization.
    let g = s.root_guard(Domain::Core, Lane::Admin).unwrap();
    assert_eq!(
        g.query_row("SELECT count(*) FROM engine_guard_test", [], |r| r
            .get::<_, i64>(0))
            .unwrap(),
        0
    );
    drop(g);
    drop(s);
    // Admin assembles the same synchronous core with no lease/export service.
    let admin_dir = tempfile::tempdir().unwrap();
    drop(open::open_runtime(admin_dir.path()).unwrap());
    let s = store(admin_dir.path(), true);
    drop(s.root_guard(Domain::Core, Lane::Admin).unwrap());
}
#[test]
fn panic_rolls_back_and_the_next_thread_gets_the_same_writer() {
    let dir = tempfile::tempdir().unwrap();
    let s = Arc::new(store(dir.path(), false));
    let child = s.clone();
    let panic = std::thread::spawn(move || {
        let g = child.root_guard(Domain::Core, Lane::Interactive).unwrap();
        g.execute_batch("BEGIN IMMEDIATE; CREATE TABLE engine_panicked(x)")
            .unwrap();
        panic!("fixture unwind");
    })
    .join();
    assert!(panic.is_err());
    let (tx, rx) = mpsc::channel();
    let child = s.clone();
    let next = std::thread::spawn(move || {
        let g = child.root_guard(Domain::Core, Lane::Admin).unwrap();
        assert!(g.is_autocommit());
        assert_eq!(
            g.query_row(
                "SELECT count(*) FROM sqlite_schema WHERE name='engine_panicked'",
                [],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
            0
        );
        g.execute_batch("CREATE TABLE engine_recovered(x)").unwrap();
        drop(g);
        tx.send(()).unwrap();
    });
    rx.recv_timeout(Duration::from_secs(10)).unwrap();
    next.join().unwrap();
}
#[test]
fn saturated_tokio_workers_do_not_block_synchronous_authorization() {
    let dir = tempfile::tempdir().unwrap();
    let s = Arc::new(store(dir.path(), false));
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .build()
        .unwrap();
    let release = Arc::new((Mutex::new(false), Condvar::new()));
    let (started, rx) = mpsc::channel();
    for _ in 0..2 {
        let (release, started) = (release.clone(), started.clone());
        runtime.spawn(async move {
            started.send(()).unwrap();
            let (m, c) = &*release;
            let mut done = m.lock().unwrap();
            while !*done {
                done = c.wait(done).unwrap();
            }
        });
    }
    for _ in 0..2 {
        rx.recv_timeout(Duration::from_secs(10)).unwrap();
    }
    let (done, rx) = mpsc::channel();
    let child = std::thread::spawn(move || {
        let g = s.root_guard(Domain::Core, Lane::Interactive).unwrap();
        g.execute_batch("CREATE TABLE engine_saturated(x)").unwrap();
        drop(g);
        done.send(()).unwrap();
    });
    let result = rx.recv_timeout(Duration::from_secs(10));
    // Release workers even on assertion failure; the test must never hang in shutdown.
    *release.0.lock().unwrap() = true;
    release.1.notify_all();
    result.unwrap();
    child.join().unwrap();
    drop(runtime);
}

#[test]
fn read_services_accept_an_explicit_handle_outside_its_runtime_context() {
    use steve_storage_api::DomainStore;
    let dir = tempfile::tempdir().unwrap();
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(1)
        .build()
        .unwrap();
    assert!(tokio::runtime::Handle::try_current().is_err());
    let s = store(dir.path(), false)
        .with_runtime(runtime.handle().clone())
        .unwrap();
    assert!(s.hello().domains.iter().all(|d| !d.features.is_empty()));
    // Handle injection, rather than a hidden runtime enter, enables read dispatch.
    let query = steve_storage_api::AnyQuery {
        name: "missing.query".into(),
        domain: Domain::Core,
        body: steve_storage_api::Payload::Json(steve_storage_api::query::encode(&()).unwrap()),
    };
    let result = runtime.block_on(s.query(query));
    assert_ne!(
        result.unwrap_err().safe_detail,
        "SQLite async read runtime is not installed"
    );
    drop(s);
    drop(runtime);
}

#[test]
fn failed_guard_rollback_permanently_rejects_the_writer() {
    use rusqlite::hooks::{AuthAction, AuthContext, Authorization, TransactionOperation};
    fn refuse_rollback(c: &rusqlite::Connection) {
        c.execute_batch("BEGIN IMMEDIATE; CREATE TABLE engine_failed_rollback(x)")
            .unwrap();
        c.authorizer(Some(|ctx: AuthContext<'_>| match ctx.action {
            AuthAction::Transaction {
                operation: TransactionOperation::Rollback,
            } => Authorization::Deny,
            _ => Authorization::Allow,
        }))
        .unwrap();
    }
    for callback in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        let s = store(dir.path(), false);
        if callback {
            let result = s.with_root(Domain::Core, Lane::Interactive, |c, _, _| {
                refuse_rollback(c);
                Ok(())
            });
            assert!(matches!(
                result,
                Err(steve_storage_api::CommitError::Fatal(_))
            ));
        } else {
            let g = s.root_guard(Domain::Core, Lane::Interactive).unwrap();
            refuse_rollback(&g);
            drop(g);
        }
        for _ in 0..2 {
            let error = s.root_guard(Domain::Core, Lane::Admin).err().unwrap();
            assert!(matches!(error, steve_storage_api::CommitError::Fatal(_)));
            assert!(error.to_string().contains("unusable"));
        }
    }
}
#[test]
fn root_panic_does_not_reuse_reserved_revisions_even_after_reopen() {
    let dir = tempfile::tempdir().unwrap();
    let s = store(dir.path(), false);
    let (tx, rx) = mpsc::channel();
    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        s.with_root(
            Domain::Core,
            Lane::Interactive,
            |c, _, rev| -> Result<(), steve_storage_api::CommitError> {
                tx.send(rev.rev().0).unwrap();
                c.execute_batch("BEGIN IMMEDIATE").unwrap();
                panic!("rollback root after revision issuance");
            },
        )
        .unwrap();
    }));
    let issued = rx.recv_timeout(Duration::from_secs(10)).unwrap();
    let next = s
        .with_root(Domain::Core, Lane::Interactive, |_, _, rev| Ok(rev.rev().0))
        .unwrap();
    assert!(next > issued);
    // This panic escapes the guard itself (not with_root's catch boundary),
    // poisoning the writer while the same runner still has buffered revisions.
    let panic = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let g = s.root_guard(Domain::Core, Lane::Interactive).unwrap();
        g.execute_batch("BEGIN IMMEDIATE").unwrap();
        panic!("poison writer with live revision reservation");
    }));
    assert!(panic.is_err());
    let after_poison = s
        .with_root(Domain::Core, Lane::Interactive, |_, _, rev| Ok(rev.rev().0))
        .unwrap();
    assert!(after_poison > next);
    drop(s);
    // Old executor teardown releases storage.lock asynchronously; wait for its
    // actual ownership condition rather than sleeping or introducing a runtime.
    let reopened = loop {
        match open::open_admin(dir.path()) {
            Ok(e) => break e,
            Err(e) if e.code == steve_types::Code::Busy => std::thread::yield_now(),
            Err(e) => panic!("{e}"),
        }
    };
    let s = SqliteStore::new_sync(
        vec![reopened],
        Arc::new(Registry::verification().unwrap()),
        Arc::new(SystemWallClock),
        Arc::new(|_, _: Vec<()>| {}),
        || false,
    )
    .unwrap();
    let after = s
        .with_root(Domain::Core, Lane::Admin, |_, _, rev| Ok(rev.rev().0))
        .unwrap();
    assert!(after > after_poison);
}
