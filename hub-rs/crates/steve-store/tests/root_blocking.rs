#[path = "../../steve-store-sqlite/tests/exec_support.rs"]
mod support;
use steve_storage_api::{Domain, Lane};
use steve_store::{NoPublisher, Store, StoreError, instrumentation};

#[test]
fn facade_and_domain_guard_share_engine_transaction_and_readers() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path().join("steve.sqlite3"), NoPublisher).unwrap();
    store
        .with_conn(|c| {
            assert!(c.is_autocommit());
            c.execute_batch(
                "CREATE TABLE facade_root(x); BEGIN IMMEDIATE; INSERT INTO facade_root VALUES(1)",
            )?;
            let nested = store
                .domain_store()
                .root_guard(Domain::Stream, Lane::Interactive)?;
            assert!(std::ptr::eq(c, nested.connection()));
            drop(nested);
            assert_eq!(
                store.reader(|c| Ok::<_, StoreError>(c.query_row(
                    "SELECT x FROM facade_root",
                    [],
                    |r| r.get::<_, i64>(0)
                )?))?,
                1
            );
            let err: Result<(), StoreError> = store.atomic(|| {
                store.with_conn(|c| {
                    c.execute("INSERT INTO facade_root VALUES(2)", [])?;
                    Ok(())
                })?;
                Err(StoreError::Conflict("inner failure".into()))
            });
            assert!(err.is_err());
            assert!(!c.is_autocommit());
            assert_eq!(
                c.query_row("SELECT sum(x) FROM facade_root", [], |r| r.get::<_, i64>(0))?,
                1
            );
            c.execute_batch("COMMIT")?;
            Ok(())
        })
        .unwrap();
    let guard = store
        .domain_store()
        .root_guard(Domain::Core, Lane::Interactive)
        .unwrap();
    guard
        .execute_batch("BEGIN IMMEDIATE; INSERT INTO facade_root VALUES(3)")
        .unwrap();
    // Plain DomainStore root -> facade read and nested transaction see the same writer.
    assert_eq!(
        store
            .reader(|c| Ok::<_, StoreError>(c.query_row(
                "SELECT sum(x) FROM facade_root",
                [],
                |r| r.get::<_, i64>(0)
            )?))
            .unwrap(),
        4
    );
    store
        .atomic(|| -> Result<(), StoreError> {
            store.with_conn(|c| {
                c.execute("INSERT INTO facade_root VALUES(4)", [])?;
                Ok(())
            })
        })
        .unwrap();
    assert!(!guard.is_autocommit());
    guard.execute_batch("ROLLBACK").unwrap();
    drop(guard);
    store
        .with_conn(|c| {
            assert!(c.is_autocommit());
            Ok(())
        })
        .unwrap();
}

#[test]
fn recognized_helpers_preserve_request_counter_and_record_zero_violations() {
    // The global recorder observes actual grants on blocking threads, unlike a
    // task-thread local recorder. Set up the fixture without any runtime.
    let recorder = support::Probe::default();
    assert!(metrics::set_global_recorder(recorder.clone()).is_ok());
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path().join("steve.sqlite3"), NoPublisher).unwrap();
    instrumentation::enable_lock_metrics();
    let request = instrumentation::RequestCalls::default();
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .build()
        .unwrap();
    rt.block_on(request.scope(async {
        let s = store.clone();
        steve_store::blocking(move || {
            s.with_conn(|c| {
                c.execute_batch("CREATE TABLE helper_root(x); INSERT INTO helper_root VALUES(1)")?;
                Ok(())
            })
        })
        .await
        .unwrap();
        let s = store.clone();
        tokio::spawn(async move {
            tokio::task::block_in_place(steve_store::for_block_in_place(|| {
                s.with_conn(|c| {
                    c.execute("INSERT INTO helper_root VALUES(2)", [])?;
                    Ok(())
                })
            }))
            .unwrap();
        })
        .await
        .unwrap();
    }));
    // Request task-local is not propagated by tokio::spawn; the first helper is.
    assert_eq!(request.count(), 1);
    assert_eq!(recorder.total("storage_grant_outside_blocking_scope"), 0.);
    let negative = store.clone();
    rt.block_on(async move {
        tokio::spawn(async move { negative.with_conn(|_| Ok(())) })
            .await
            .unwrap()
            .unwrap();
    });
    assert_eq!(
        recorder.total("storage_grant_outside_blocking_scope"),
        1.,
        "production probe must observe the negative control"
    );
    store
        .with_conn(|c| {
            assert_eq!(
                c.query_row("SELECT sum(x) FROM helper_root", [], |r| r.get::<_, i64>(0))?,
                3
            );
            Ok(())
        })
        .unwrap();
}
