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

fn effect_event(id: &str) -> steve_store::BusEvent {
    steve_types::BusEvent::Domain(steve_types::DomainEvent::Work(
        steve_types::WorkEvent::Item {
            id: id.into(),
            ws: steve_store::WS.into(),
        },
    ))
}

fn pending(wake: &tokio::sync::Notify) -> bool {
    use std::{
        future::Future,
        task::{Context, Poll, Waker},
    };
    matches!(
        std::pin::pin!(wake.notified()).poll(&mut Context::from_waker(Waker::noop())),
        Poll::Ready(())
    )
}

fn stage_effects(store: &Store, id: &str) {
    store
        .transaction(|tx| {
            tx.execute("INSERT INTO root_effects VALUES(?)", [id])?;
            tx.publish(effect_event(id));
            steve_store::outbox::emit(
                tx,
                steve_store::WS,
                "root.effect",
                1,
                &serde_json::json!({"id":id}),
            )?;
            Ok::<_, StoreError>(())
        })
        .unwrap();
}

fn outer_effects(typed: bool, finish: &str) {
    use std::{
        panic::{AssertUnwindSafe, catch_unwind},
        sync::{Arc, Mutex},
    };
    let dir = tempfile::tempdir().unwrap();
    let events = Arc::new(Mutex::new(Vec::new()));
    let sink = events.clone();
    let store = Store::open(
        dir.path().join("steve.sqlite3"),
        steve_store::FnPublisher(move |e| sink.lock().unwrap().push(e)),
    )
    .unwrap();
    store.ensure_workspace().unwrap();
    store
        .with_conn(|c| Ok(c.execute_batch("CREATE TABLE root_effects(id TEXT)")?))
        .unwrap();
    events.lock().unwrap().clear();
    let wake = store.outbox_wake();
    let run = |c: &rusqlite::Connection| {
        c.execute_batch("BEGIN IMMEDIATE").unwrap();
        stage_effects(&store, "first");
        // RELEASE is not a physical COMMIT, for either raw or typed root owners.
        assert!(
            events.lock().unwrap().is_empty(),
            "facade RELEASE published before outer {finish}"
        );
        assert!(
            !pending(&wake),
            "facade RELEASE woke outbox before outer {finish}"
        );
        if finish == "panic" {
            panic!("outer owner panic");
        }
        c.execute_batch(finish).unwrap();
    };
    let result = catch_unwind(AssertUnwindSafe(|| {
        if typed {
            store
                .domain_store()
                .with_root(Domain::Core, Lane::Interactive, |c, _, _| {
                    run(c);
                    Ok(())
                })
                .unwrap();
        } else {
            let guard = store
                .domain_store()
                .root_guard(Domain::Core, Lane::Interactive)
                .unwrap();
            run(&guard);
        }
    }));
    assert_eq!(result.is_err(), finish == "panic");
    let committed = finish == "COMMIT";
    assert_eq!(
        *events.lock().unwrap(),
        if committed {
            vec![effect_event("first")]
        } else {
            vec![]
        }
    );
    assert_eq!(pending(&wake), committed);
    assert!(!pending(&wake), "each successful transaction wakes once");
    store
        .with_conn(|c| {
            assert!(c.is_autocommit());
            assert_eq!(
                c.query_row("SELECT count(*) FROM root_effects", [], |r| r
                    .get::<_, i64>(0))?,
                i64::from(committed)
            );
            assert_eq!(
                c.query_row(
                    "SELECT count(*) FROM outbox_events WHERE topic='root.effect'",
                    [],
                    |r| r.get::<_, i64>(0)
                )?,
                i64::from(committed)
            );
            Ok(())
        })
        .unwrap();
}

#[test]
fn raw_outer_rollback_discards_facade_effects() {
    outer_effects(false, "ROLLBACK");
}
#[test]
fn raw_outer_commit_publishes_facade_effects_once() {
    outer_effects(false, "COMMIT");
}
#[test]
fn raw_outer_panic_discards_facade_effects() {
    outer_effects(false, "panic");
}
#[test]
fn typed_outer_rollback_discards_facade_effects() {
    outer_effects(true, "ROLLBACK");
}
#[test]
fn typed_outer_commit_publishes_facade_effects_once() {
    outer_effects(true, "COMMIT");
}
#[test]
fn typed_outer_panic_discards_facade_effects() {
    outer_effects(true, "panic");
}

fn tracked_root() -> (
    tempfile::TempDir,
    Store,
    std::sync::Arc<std::sync::Mutex<Vec<steve_store::BusEvent>>>,
) {
    let dir = tempfile::tempdir().unwrap();
    let events = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let sink = events.clone();
    let store = Store::open(
        dir.path().join("steve.sqlite3"),
        steve_store::FnPublisher(move |e| sink.lock().unwrap().push(e)),
    )
    .unwrap();
    store.ensure_workspace().unwrap();
    store
        .with_conn(|c| Ok(c.execute_batch("CREATE TABLE root_effects(id TEXT)")?))
        .unwrap();
    events.lock().unwrap().clear();
    (dir, store, events)
}

#[test]
fn bridge_rollback_error_and_panic_discard_effects_inside_facade_outer_commit() {
    use std::panic::{AssertUnwindSafe, catch_unwind};
    for panic in [false, true] {
        let (_dir, store, events) = tracked_root();
        // Reserve the typed revision before the facade opens its BEGIN.
        store
            .domain_store()
            .with_root(Domain::Core, Lane::Interactive, |_, _, _| {
                store
                    .atomic(|| -> Result<(), StoreError> {
                        stage_effects(&store, "kept");
                        let result = catch_unwind(AssertUnwindSafe(|| {
                            store.domain_store().with_root(
                                Domain::Core,
                                Lane::Interactive,
                                |_, _, _| -> Result<(), steve_storage_api::CommitError> {
                                    stage_effects(&store, "discarded");
                                    if panic {
                                        panic!("nested bridge panic");
                                    }
                                    Err(steve_storage_api::CommitError::Fatal(
                                        steve_storage_api::error(
                                            steve_types::Code::Invalid,
                                            "bridge failed",
                                        ),
                                    ))
                                },
                            )
                        }));
                        assert!(matches!(result, Err(_) | Ok(Err(_))));
                        assert!(events.lock().unwrap().is_empty());
                        store.reader(|c| {
                            assert_eq!(
                                c.query_row(
                                    "SELECT group_concat(id) FROM root_effects",
                                    [],
                                    |r| r.get::<_, String>(0)
                                )?,
                                "kept"
                            );
                            Ok::<_, StoreError>(())
                        })?;
                        Ok(())
                    })
                    .unwrap();
                Ok(())
            })
            .unwrap();
        assert_eq!(*events.lock().unwrap(), vec![effect_event("kept")]);
        assert!(pending(&store.outbox_wake()));
        assert!(!pending(&store.outbox_wake()));
    }
}

#[test]
fn one_guard_keeps_prior_commits_and_discards_later_rollbacks_and_leaks() {
    let (_dir, store, events) = tracked_root();
    let guard = store
        .domain_store()
        .root_guard(Domain::Core, Lane::Interactive)
        .unwrap();
    guard.execute_batch("BEGIN IMMEDIATE").unwrap();
    stage_effects(&store, "committed");
    guard.execute_batch("COMMIT; BEGIN IMMEDIATE").unwrap();
    stage_effects(&store, "rolled-back");
    guard.execute_batch("ROLLBACK; BEGIN IMMEDIATE").unwrap();
    stage_effects(&store, "leaked");
    drop(guard);
    assert_eq!(*events.lock().unwrap(), vec![effect_event("committed")]);
    store
        .reader(|c| {
            assert_eq!(
                c.query_row("SELECT group_concat(id) FROM root_effects", [], |r| r
                    .get::<_, String>(0))?,
                "committed"
            );
            Ok::<_, StoreError>(())
        })
        .unwrap();
    assert!(pending(&store.outbox_wake()));
    assert!(!pending(&store.outbox_wake()));
}

#[test]
fn raw_begin_without_reserved_revision_rejects_typed_bridge_without_mutating_metadata() {
    let (_dir, store, events) = tracked_root();
    let guard = store
        .domain_store()
        .root_guard(Domain::Core, Lane::Interactive)
        .unwrap();
    let before: Option<String> = guard
        .query_row(
            "SELECT value FROM meta WHERE domain='core' AND key='rev_reserved'",
            [],
            |r| r.get(0),
        )
        .ok();
    guard.execute_batch("BEGIN IMMEDIATE").unwrap();
    let result = store
        .domain_store()
        .with_root(Domain::Core, Lane::Interactive, |_, _, _| Ok(()));
    assert!(
        result
            .unwrap_err()
            .to_string()
            .contains("reserve revisions before opening a transaction")
    );
    assert!(!guard.is_autocommit());
    guard.execute_batch("ROLLBACK").unwrap();
    let after: Option<String> = guard
        .query_row(
            "SELECT value FROM meta WHERE domain='core' AND key='rev_reserved'",
            [],
            |r| r.get(0),
        )
        .ok();
    assert_eq!(before, after);
    assert!(events.lock().unwrap().is_empty());
}

#[test]
fn failed_physical_commit_never_publishes_facade_attachments() {
    let (_dir, store, events) = tracked_root();
    let guard = store
        .domain_store()
        .root_guard(Domain::Core, Lane::Interactive)
        .unwrap();
    guard.execute_batch("CREATE TABLE effect_parent(id INTEGER PRIMARY KEY); CREATE TABLE effect_child(id REFERENCES effect_parent(id) DEFERRABLE INITIALLY DEFERRED); BEGIN IMMEDIATE").unwrap();
    stage_effects(&store, "rejected");
    guard
        .execute("INSERT INTO effect_child VALUES(99)", [])
        .unwrap();
    assert!(guard.execute_batch("COMMIT").is_err());
    assert!(!guard.is_autocommit());
    assert!(events.lock().unwrap().is_empty());
    assert!(!pending(&store.outbox_wake()));
    guard.execute_batch("ROLLBACK").unwrap();
    drop(guard);
    assert!(events.lock().unwrap().is_empty());
    assert!(!pending(&store.outbox_wake()));
    store
        .reader(|c| {
            assert_eq!(
                c.query_row("SELECT count(*) FROM root_effects", [], |r| r
                    .get::<_, i64>(0))?,
                0
            );
            Ok::<_, StoreError>(())
        })
        .unwrap();
}

#[test]
fn committed_event_observer_can_read_and_reenter_the_same_root() {
    use std::sync::{Arc, Mutex};
    let dir = tempfile::tempdir().unwrap();
    // Clear the slot before teardown so this observer does not retain the Store.
    let owner: Arc<Mutex<Option<Store>>> = Arc::default();
    let observed = Arc::new(Mutex::new(Vec::new()));
    let (slot, sink) = (owner.clone(), observed.clone());
    let store = Store::open(
        dir.path().join("steve.sqlite3"),
        steve_store::FnPublisher(move |event| {
            let store = slot.lock().unwrap().clone();
            if let Some(store) = store {
                store
                    .with_conn(|c| {
                        assert!(c.is_autocommit());
                        assert_eq!(
                            c.query_row("SELECT count(*) FROM root_effects", [], |r| r
                                .get::<_, i64>(0))?,
                            1
                        );
                        c.execute("INSERT INTO observer_writes VALUES(1)", [])?;
                        Ok(())
                    })
                    .unwrap();
                sink.lock().unwrap().push(event);
            }
        }),
    )
    .unwrap();
    store.ensure_workspace().unwrap();
    store
        .with_conn(|c| {
            Ok(c.execute_batch(
                "CREATE TABLE root_effects(id TEXT); CREATE TABLE observer_writes(x)",
            )?)
        })
        .unwrap();
    *owner.lock().unwrap() = Some(store.clone());
    stage_effects(&store, "observed");
    owner.lock().unwrap().take();
    assert_eq!(*observed.lock().unwrap(), vec![effect_event("observed")]);
    store
        .reader(|c| {
            assert_eq!(
                c.query_row("SELECT count(*) FROM observer_writes", [], |r| r
                    .get::<_, i64>(0))?,
                1
            );
            Ok::<_, StoreError>(())
        })
        .unwrap();
}

#[test]
fn one_raw_guard_preserves_separate_batches_for_each_physical_commit() {
    use std::sync::{Arc, Mutex};
    #[derive(Clone)]
    struct Batches(Arc<Mutex<Vec<Vec<steve_store::BusEvent>>>>);
    impl steve_store::Publisher for Batches {
        fn publish_batch(&self, events: Vec<steve_store::BusEvent>) {
            self.0.lock().unwrap().push(events);
        }
    }
    let dir = tempfile::tempdir().unwrap();
    let batches = Batches(Arc::default());
    let store = Store::open(dir.path().join("steve.sqlite3"), batches.clone()).unwrap();
    store.ensure_workspace().unwrap();
    store
        .with_conn(|c| Ok(c.execute_batch("CREATE TABLE root_effects(id TEXT)")?))
        .unwrap();
    batches.0.lock().unwrap().clear();
    let guard = store
        .domain_store()
        .root_guard(Domain::Core, Lane::Interactive)
        .unwrap();
    guard.execute_batch("BEGIN IMMEDIATE").unwrap();
    stage_effects(&store, "a1");
    stage_effects(&store, "a2");
    guard.execute_batch("COMMIT; BEGIN IMMEDIATE").unwrap();
    stage_effects(&store, "b");
    guard.execute_batch("COMMIT; BEGIN IMMEDIATE").unwrap();
    stage_effects(&store, "discarded");
    guard.execute_batch("ROLLBACK").unwrap();
    drop(guard);
    assert_eq!(
        *batches.0.lock().unwrap(),
        vec![
            vec![effect_event("a1"), effect_event("a2")],
            vec![effect_event("b")]
        ]
    );
    store
        .reader(|c| {
            assert_eq!(
                c.query_row("SELECT count(*) FROM root_effects", [], |r| r
                    .get::<_, i64>(0))?,
                3
            );
            Ok::<_, StoreError>(())
        })
        .unwrap();
}

#[derive(Clone, Default)]
struct R3Batches(std::sync::Arc<std::sync::Mutex<Vec<Vec<steve_store::BusEvent>>>>);
impl steve_store::Publisher for R3Batches {
    fn publish_batch(&self, events: Vec<steve_store::BusEvent>) {
        self.0.lock().unwrap().push(events);
    }
}

fn r3_fixture() -> (tempfile::TempDir, Store, R3Batches) {
    let dir = tempfile::tempdir().unwrap();
    let batches = R3Batches::default();
    let store = Store::open(dir.path().join("steve.sqlite3"), batches.clone()).unwrap();
    store.ensure_workspace().unwrap();
    store
        .with_conn(|c| {
            println!(
                "R3 bundled SQLite {} source={}",
                rusqlite::version(),
                c.query_row("SELECT sqlite_source_id()", [], |r| r.get::<_, String>(0))?
            );
            c.execute_batch("CREATE TABLE root_effects(id TEXT)")?;
            Ok(())
        })
        .unwrap();
    batches.0.lock().unwrap().clear();
    (dir, store, batches)
}

fn r3_publish_only(store: &Store, id: &str) {
    store
        .transaction(|tx| {
            tx.publish(effect_event(id));
            Ok::<_, StoreError>(())
        })
        .unwrap();
}

#[test]
fn r3_bundled_sqlite_read_only_commit_hook_observation() {
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };
    for (begin, end, commits, rollbacks) in [
        ("BEGIN DEFERRED", "COMMIT", 0, 0),
        ("BEGIN IMMEDIATE", "COMMIT", 1, 0),
        ("BEGIN DEFERRED", "ROLLBACK", 0, 1),
        ("BEGIN IMMEDIATE", "ROLLBACK", 0, 1),
    ] {
        let c = rusqlite::Connection::open_in_memory().unwrap();
        let seen = Arc::new(AtomicUsize::new(0));
        let undone = Arc::new(AtomicUsize::new(0));
        let hook = seen.clone();
        c.commit_hook(Some(move || {
            hook.fetch_add(1, Ordering::SeqCst);
            false
        }))
        .unwrap();
        let hook = undone.clone();
        c.rollback_hook(Some(move || {
            hook.fetch_add(1, Ordering::SeqCst);
        }))
        .unwrap();
        c.execute_batch(&format!("{begin}; SAVEPOINT s; SELECT 1; RELEASE s; {end}"))
            .unwrap();
        println!(
            "R3 SQLite {} {begin} {end}: commit_hook={} rollback_hook={}",
            rusqlite::version(),
            seen.load(Ordering::SeqCst),
            undone.load(Ordering::SeqCst)
        );
        assert_eq!(seen.load(Ordering::SeqCst), commits);
        assert_eq!(undone.load(Ordering::SeqCst), rollbacks);
    }
}

#[test]
fn r3_deferred_publish_only_commit_delivers_one_batch() {
    let (_dir, store, batches) = r3_fixture();
    let guard = store
        .domain_store()
        .root_guard(Domain::Core, Lane::Interactive)
        .unwrap();
    guard.execute_batch("BEGIN DEFERRED").unwrap();
    r3_publish_only(&store, "read-only-commit");
    assert!(batches.0.lock().unwrap().is_empty());
    guard.execute_batch("COMMIT").unwrap();
    drop(guard);
    println!(
        "R3 publish-only COMMIT batches={:?}",
        *batches.0.lock().unwrap()
    );
    assert_eq!(
        *batches.0.lock().unwrap(),
        vec![vec![effect_event("read-only-commit")]]
    );
}

#[test]
fn r3_immediate_publish_only_commit_control() {
    let (_dir, store, batches) = r3_fixture();
    let guard = store
        .domain_store()
        .root_guard(Domain::Core, Lane::Interactive)
        .unwrap();
    guard.execute_batch("BEGIN IMMEDIATE").unwrap();
    r3_publish_only(&store, "read-only-commit");
    assert!(batches.0.lock().unwrap().is_empty());
    guard.execute_batch("COMMIT").unwrap();
    drop(guard);
    assert_eq!(
        *batches.0.lock().unwrap(),
        vec![vec![effect_event("read-only-commit")]]
    );
}

#[test]
fn r3_deferred_publish_only_rollback_control() {
    let (_dir, store, batches) = r3_fixture();
    let guard = store
        .domain_store()
        .root_guard(Domain::Core, Lane::Interactive)
        .unwrap();
    guard.execute_batch("BEGIN DEFERRED").unwrap();
    r3_publish_only(&store, "rolled-back");
    guard.execute_batch("ROLLBACK").unwrap();
    drop(guard);
    assert!(batches.0.lock().unwrap().is_empty());
    assert!(!pending(&store.outbox_wake()));
}

#[test]
fn r3_deferred_commit_then_write_commit_preserves_two_batches() {
    let (_dir, store, batches) = r3_fixture();
    let guard = store
        .domain_store()
        .root_guard(Domain::Core, Lane::Interactive)
        .unwrap();
    guard.execute_batch("BEGIN DEFERRED").unwrap();
    r3_publish_only(&store, "read-only-commit");
    guard.execute_batch("COMMIT; BEGIN IMMEDIATE").unwrap();
    stage_effects(&store, "write-commit");
    guard.execute_batch("COMMIT").unwrap();
    drop(guard);
    println!("R3 two COMMIT batches={:?}", *batches.0.lock().unwrap());
    store
        .reader(|c| {
            assert_eq!(
                c.query_row("SELECT group_concat(id) FROM root_effects", [], |r| r
                    .get::<_, String>(0))?,
                "write-commit"
            );
            Ok::<_, StoreError>(())
        })
        .unwrap();
    assert_eq!(
        *batches.0.lock().unwrap(),
        vec![
            vec![effect_event("read-only-commit")],
            vec![effect_event("write-commit")]
        ]
    );
}

#[test]
fn r3_deferred_commit_then_write_rollback_keeps_prior_event() {
    let (_dir, store, batches) = r3_fixture();
    let guard = store
        .domain_store()
        .root_guard(Domain::Core, Lane::Interactive)
        .unwrap();
    guard.execute_batch("BEGIN DEFERRED").unwrap();
    r3_publish_only(&store, "read-only-commit");
    guard.execute_batch("COMMIT; BEGIN IMMEDIATE").unwrap();
    stage_effects(&store, "write-rollback");
    guard.execute_batch("ROLLBACK").unwrap();
    drop(guard);
    let notified = pending(&store.outbox_wake());
    println!(
        "R3 COMMIT then ROLLBACK batches={:?} outbox_wake={notified}",
        *batches.0.lock().unwrap()
    );
    store
        .reader(|c| {
            assert_eq!(
                c.query_row("SELECT count(*) FROM root_effects", [], |r| r
                    .get::<_, i64>(0))?,
                0
            );
            assert_eq!(
                c.query_row(
                    "SELECT count(*) FROM outbox_events WHERE topic='root.effect'",
                    [],
                    |r| r.get::<_, i64>(0)
                )?,
                0
            );
            Ok::<_, StoreError>(())
        })
        .unwrap();
    assert!(!notified);
    assert_eq!(
        *batches.0.lock().unwrap(),
        vec![vec![effect_event("read-only-commit")]]
    );
}

fn r3_raw_savepoint(keep_parent: bool, rollback_child: bool, commit_root: bool) {
    let (_dir, store, batches) = r3_fixture();
    let guard = store
        .domain_store()
        .root_guard(Domain::Core, Lane::Interactive)
        .unwrap();
    guard.execute_batch("BEGIN IMMEDIATE").unwrap();
    if keep_parent {
        stage_effects(&store, "parent");
    }
    guard.execute_batch("SAVEPOINT raw_child").unwrap();
    stage_effects(&store, "child");
    if rollback_child {
        guard.execute_batch("ROLLBACK TO raw_child").unwrap();
    }
    guard.execute_batch("RELEASE raw_child").unwrap();
    guard
        .execute_batch(if commit_root { "COMMIT" } else { "ROLLBACK" })
        .unwrap();
    drop(guard);
    let expected_ids: Vec<&str> = if !commit_root {
        vec![]
    } else if rollback_child {
        if keep_parent { vec!["parent"] } else { vec![] }
    } else if keep_parent {
        vec!["parent", "child"]
    } else {
        vec!["child"]
    };
    let notified = pending(&store.outbox_wake());
    store.reader(|c| {
        let ids: Vec<String> = c.prepare("SELECT id FROM root_effects ORDER BY rowid")?.query_map([], |r| r.get(0))?.collect::<rusqlite::Result<_>>()?;
        let outbox: Vec<String> = c.prepare("SELECT json_extract(payload,'$.id') FROM outbox_events WHERE topic='root.effect' ORDER BY seq")?.query_map([], |r| r.get(0))?.collect::<rusqlite::Result<_>>()?;
        println!("R3 raw savepoint parent={keep_parent} rollback_child={rollback_child} commit_root={commit_root}: rows={ids:?} outbox={outbox:?} batches={:?} wake={notified}", *batches.0.lock().unwrap());
        assert_eq!(ids, expected_ids);
        assert_eq!(outbox, expected_ids);
        Ok::<_, StoreError>(())
    }).unwrap();
    assert_eq!(
        notified,
        !expected_ids.is_empty(),
        "rolled-back outbox must not wake"
    );
    let expected: Vec<_> = expected_ids.iter().map(|id| effect_event(id)).collect();
    assert_eq!(
        *batches.0.lock().unwrap(),
        if expected.is_empty() {
            vec![]
        } else {
            vec![expected]
        }
    );
}

#[test]
fn r3_raw_savepoint_rollback_keeps_parent_and_discards_child_effects() {
    r3_raw_savepoint(true, true, true);
}
#[test]
fn r3_raw_savepoint_rollback_child_only_emits_nothing() {
    r3_raw_savepoint(false, true, true);
}
#[test]
fn r3_raw_savepoint_release_control() {
    r3_raw_savepoint(true, false, true);
}
#[test]
fn r3_raw_savepoint_root_rollback_control() {
    r3_raw_savepoint(true, true, false);
}

#[test]
fn r3_deferred_rollback_then_write_commit_control() {
    let (_dir, store, batches) = r3_fixture();
    let guard = store
        .domain_store()
        .root_guard(Domain::Core, Lane::Interactive)
        .unwrap();
    guard.execute_batch("BEGIN DEFERRED").unwrap();
    r3_publish_only(&store, "read-only-rollback");
    guard.execute_batch("ROLLBACK; BEGIN IMMEDIATE").unwrap();
    stage_effects(&store, "write-commit");
    guard.execute_batch("COMMIT").unwrap();
    drop(guard);
    assert_eq!(
        *batches.0.lock().unwrap(),
        vec![vec![effect_event("write-commit")]]
    );
}

// A0 is an independent callback feasibility probe, not Store wiring or a fix for
// either raw-savepoint regression above. Keep all FFI inside this test module.
#[cfg(test)]
mod a0_probe {
    use rusqlite::{Connection, ffi, hooks::Authorization};
    use std::{
        cell::RefCell,
        ffi::{c_char, c_int, c_void},
        panic::{AssertUnwindSafe, catch_unwind},
        ptr,
        sync::{Arc, Mutex},
    };

    type Log = Arc<Mutex<Vec<String>>>;
    thread_local! {
        static SQL: RefCell<Vec<String>> = const { RefCell::new(Vec::new()) };
    }

    // All fallible Rust work in callbacks is caught before returning to SQLite.
    // SQLite owns each Tab/empty cursor after the corresponding Box::into_raw;
    // xDisconnect/xDestroy and xClose respectively reclaim those exact boxes.
    #[repr(C)]
    struct Tab {
        base: ffi::sqlite3_vtab,
        callbacks: Log,
    }

    fn boundary(f: impl FnOnce() -> c_int) -> c_int {
        catch_unwind(AssertUnwindSafe(f)).unwrap_or(ffi::SQLITE_ERROR)
    }

    unsafe extern "C" fn connect(
        db: *mut ffi::sqlite3,
        aux: *mut c_void,
        _: c_int,
        _: *const *const c_char,
        out: *mut *mut ffi::sqlite3_vtab,
        _: *mut *mut c_char,
    ) -> c_int {
        boundary(|| unsafe {
            let rc = ffi::sqlite3_declare_vtab(db, c"CREATE TABLE x(token)".as_ptr());
            if rc != ffi::SQLITE_OK {
                return rc;
            }
            let callbacks = (&*aux.cast::<Log>()).clone();
            callbacks.lock().unwrap().push("connect".into());
            let tab = Box::new(Tab {
                base: std::mem::zeroed(),
                callbacks,
            });
            *out = Box::into_raw(tab).cast();
            ffi::SQLITE_OK
        })
    }

    unsafe extern "C" fn disconnect(tab: *mut ffi::sqlite3_vtab) -> c_int {
        boundary(|| unsafe {
            let tab = Box::from_raw(tab.cast::<Tab>());
            tab.callbacks.lock().unwrap().push("disconnect".into());
            ffi::SQLITE_OK
        })
    }

    unsafe extern "C" fn destroy_aux(aux: *mut c_void) {
        // No unwind can cross SQLite's void destructor boundary either.
        let _ = catch_unwind(AssertUnwindSafe(|| unsafe {
            let callbacks = Box::from_raw(aux.cast::<Log>());
            callbacks.lock().unwrap().push("destroy_aux".into());
        }));
    }

    unsafe extern "C" fn best_index(
        _: *mut ffi::sqlite3_vtab,
        _: *mut ffi::sqlite3_index_info,
    ) -> c_int {
        ffi::SQLITE_OK
    }

    unsafe extern "C" fn open(
        tab: *mut ffi::sqlite3_vtab,
        out: *mut *mut ffi::sqlite3_vtab_cursor,
    ) -> c_int {
        boundary(|| unsafe {
            *out = Box::into_raw(Box::new(ffi::sqlite3_vtab_cursor { pVtab: tab }));
            ffi::SQLITE_OK
        })
    }

    unsafe extern "C" fn close(cursor: *mut ffi::sqlite3_vtab_cursor) -> c_int {
        boundary(|| unsafe {
            drop(Box::from_raw(cursor));
            ffi::SQLITE_OK
        })
    }

    unsafe extern "C" fn filter(
        _: *mut ffi::sqlite3_vtab_cursor,
        _: c_int,
        _: *const c_char,
        _: c_int,
        _: *mut *mut ffi::sqlite3_value,
    ) -> c_int {
        ffi::SQLITE_OK
    }

    unsafe extern "C" fn next(_: *mut ffi::sqlite3_vtab_cursor) -> c_int {
        ffi::SQLITE_OK
    }

    unsafe extern "C" fn eof(_: *mut ffi::sqlite3_vtab_cursor) -> c_int {
        1 // The participant has no rows or backing storage.
    }

    unsafe extern "C" fn column(
        _: *mut ffi::sqlite3_vtab_cursor,
        ctx: *mut ffi::sqlite3_context,
        _: c_int,
    ) -> c_int {
        unsafe { ffi::sqlite3_result_null(ctx) };
        ffi::SQLITE_OK
    }

    unsafe extern "C" fn rowid(
        _: *mut ffi::sqlite3_vtab_cursor,
        out: *mut ffi::sqlite3_int64,
    ) -> c_int {
        unsafe { *out = 0 };
        ffi::SQLITE_OK
    }

    fn record(tab: *mut ffi::sqlite3_vtab, label: &str) -> c_int {
        boundary(|| unsafe {
            (*tab.cast::<Tab>())
                .callbacks
                .lock()
                .unwrap()
                .push(label.into());
            ffi::SQLITE_OK
        })
    }

    unsafe extern "C" fn update(
        tab: *mut ffi::sqlite3_vtab,
        _: c_int,
        _: *mut *mut ffi::sqlite3_value,
        out: *mut ffi::sqlite3_int64,
    ) -> c_int {
        // Required by the writable-module ABI, but zero-row enlist must not call it.
        if !out.is_null() {
            unsafe { *out = 1 };
        }
        record(tab, "update")
    }
    unsafe extern "C" fn begin(tab: *mut ffi::sqlite3_vtab) -> c_int {
        record(tab, "begin")
    }
    unsafe extern "C" fn sync(tab: *mut ffi::sqlite3_vtab) -> c_int {
        record(tab, "sync")
    }
    unsafe extern "C" fn commit(tab: *mut ffi::sqlite3_vtab) -> c_int {
        record(tab, "commit")
    }
    unsafe extern "C" fn rollback(tab: *mut ffi::sqlite3_vtab) -> c_int {
        record(tab, "rollback")
    }

    // Drop order is deliberate: Connection disconnects tables and destroys aux
    // before the boxed module is freed. sqlite3_create_module_v2 takes ownership
    // of aux (including on failure), but borrows the stable module pointer.
    struct Probe {
        conn: Connection,
        _module: Box<ffi::sqlite3_module>,
    }

    impl Probe {
        fn register(conn: Connection, callbacks: &Log) -> Self {
            let mut module: Box<ffi::sqlite3_module> = Box::new(unsafe { std::mem::zeroed() });
            module.iVersion = 1; // First-gate probe only; no savepoint protocol yet.
            // Equal xCreate/xConnect makes this eponymous: no schema DDL needed.
            module.xCreate = Some(connect);
            module.xConnect = Some(connect);
            module.xBestIndex = Some(best_index);
            module.xDisconnect = Some(disconnect);
            module.xDestroy = Some(disconnect);
            module.xOpen = Some(open);
            module.xClose = Some(close);
            module.xFilter = Some(filter);
            module.xNext = Some(next);
            module.xEof = Some(eof);
            module.xColumn = Some(column);
            module.xRowid = Some(rowid);
            module.xUpdate = Some(update);
            module.xBegin = Some(begin);
            module.xSync = Some(sync);
            module.xCommit = Some(commit);
            module.xRollback = Some(rollback);
            let aux = Box::into_raw(Box::new(callbacks.clone())).cast();
            let rc = unsafe {
                ffi::sqlite3_create_module_v2(
                    conn.handle(),
                    c"a0_participant".as_ptr(),
                    &*module,
                    aux,
                    Some(destroy_aux),
                )
            };
            assert_eq!(rc, ffi::SQLITE_OK);
            Self {
                conn,
                _module: module,
            }
        }
    }

    #[derive(Debug, PartialEq, Eq)]
    struct Observation {
        query_only: bool,
        counters: (i64, i64, i64),
        main_schema: Vec<(String, String, Option<String>)>,
        temp_schema: Vec<(String, String, Option<String>)>,
        autocommit: bool,
        txn_state: c_int,
    }

    fn observe(c: &Connection) -> Observation {
        let schema = |sql| {
            c.prepare(sql)
                .unwrap()
                .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
                .unwrap()
                .collect::<rusqlite::Result<Vec<_>>>()
                .unwrap()
        };
        Observation {
            query_only: c.query_row("PRAGMA query_only", [], |r| r.get(0)).unwrap(),
            counters: c
                .query_row(
                    "SELECT changes(), total_changes(), last_insert_rowid()",
                    [],
                    |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
                )
                .unwrap(),
            main_schema: schema("SELECT type,name,sql FROM main.sqlite_schema ORDER BY type,name"),
            temp_schema: schema("SELECT type,name,sql FROM temp.sqlite_schema ORDER BY type,name"),
            autocommit: c.is_autocommit(),
            txn_state: unsafe { ffi::sqlite3_txn_state(c.handle(), ptr::null()) },
        }
    }

    fn logs(auth: &Log) -> (Vec<String>, Vec<String>) {
        (
            SQL.with(|sql| std::mem::take(&mut *sql.borrow_mut())),
            std::mem::take(&mut *auth.lock().unwrap()),
        )
    }

    #[test]
    fn a0_real_store_query_only_publish_control() {
        // Establish that the query_only case is part of the real Store contract,
        // without installing the experimental module or replacing Store hooks.
        let (_dir, store, batches) = super::r3_fixture();
        let guard = store
            .domain_store()
            .root_guard(super::Domain::Core, super::Lane::Interactive)
            .unwrap();
        guard.execute_batch("PRAGMA query_only=ON").unwrap();
        let before = observe(guard.connection());
        guard.execute_batch("BEGIN DEFERRED").unwrap();
        super::r3_publish_only(&store, "a0-query-only");
        assert!(batches.0.lock().unwrap().is_empty());
        guard.execute_batch("COMMIT").unwrap();
        assert_eq!(observe(guard.connection()), before);
        drop(guard);
        println!(
            "A0 real Store query_only publish-only batches={:?}",
            *batches.0.lock().unwrap()
        );
        assert_eq!(
            *batches.0.lock().unwrap(),
            vec![vec![super::effect_event("a0-query-only")]]
        );
    }

    #[test]
    fn a0_query_only_first_enlist_is_readonly_counterexample() {
        let conn = Connection::open_in_memory().unwrap();
        let source: String = conn
            .query_row("SELECT sqlite_source_id()", [], |r| r.get(0))
            .unwrap();
        println!("A0 bundled SQLite {} source={source}", rusqlite::version());
        let auth: Log = Arc::default();
        let auth_log = auth.clone();
        conn.authorizer(Some(move |context: rusqlite::hooks::AuthContext<'_>| {
            auth_log.lock().unwrap().push(format!("{context:?}"));
            Authorization::Allow
        }))
        .unwrap();
        conn.trace_v2(
            rusqlite::trace::TraceEventCodes::SQLITE_TRACE_STMT,
            Some(|event| {
                if let rusqlite::trace::TraceEvent::Stmt(_, sql) = event {
                    SQL.with(|log| log.borrow_mut().push(sql.into()));
                }
            }),
        );
        conn.execute_batch("PRAGMA query_only=ON").unwrap();
        let initial = observe(&conn);
        assert!(initial.query_only);
        logs(&auth);

        // No-prototype control on this exact physical connection. A publish-only
        // transaction needs no SQL at stage time (real Store control is above).
        conn.execute_batch("BEGIN DEFERRED").unwrap();
        let control_open = observe(&conn);
        conn.execute_batch("COMMIT").unwrap();
        let control_committed = observe(&conn);
        let control_logs = logs(&auth);
        assert_eq!(initial, control_committed);
        println!("A0 control open={control_open:?} committed={control_committed:?}");
        println!("A0 control SQL/auth={control_logs:#?}");

        assert_eq!(observe(&conn), initial);
        let observation_logs = logs(&auth);
        let callbacks: Log = Arc::default();
        let probe = Probe::register(conn, &callbacks);
        let c = &probe.conn;
        let registered = observe(c);
        let registration_logs = logs(&auth);
        assert_eq!(registered, initial);
        assert_eq!(registration_logs, observation_logs);
        assert!(callbacks.lock().unwrap().is_empty());
        println!("A0 registered={registered:?} SQL/auth={registration_logs:#?}");

        c.execute_batch("BEGIN DEFERRED").unwrap();
        let prototype_open = observe(c);
        assert_eq!(prototype_open, control_open);
        // Try the least invasive writable enlist: zero rows, no metadata, no
        // xUpdate/rowid assignment. Do not toggle query_only or restore counters.
        let err = c
            .execute("INSERT INTO a0_participant SELECT 1 WHERE 0", [])
            .unwrap_err();
        let after_enlist = observe(c);
        let enlist_logs = logs(&auth);
        println!("A0 first enlist error={err:?} after={after_enlist:?}");
        println!("A0 first enlist SQL/auth={enlist_logs:#?}");
        println!("A0 callbacks={:?}", *callbacks.lock().unwrap());
        assert_eq!(
            err.sqlite_error().unwrap().extended_code,
            ffi::SQLITE_READONLY
        );
        assert_eq!(after_enlist, control_open);
        assert_eq!(*callbacks.lock().unwrap(), vec!["connect"]);
        assert!(
            enlist_logs
                .1
                .iter()
                .any(|line| line.contains("Insert") && line.contains("a0_participant"))
        );
        c.execute_batch("COMMIT").unwrap();
        let prototype_committed = observe(c);
        println!(
            "A0 prototype committed={prototype_committed:?} SQL/auth={:#?}",
            logs(&auth)
        );
        assert_eq!(prototype_committed, control_committed);
        assert_eq!(*callbacks.lock().unwrap(), vec!["connect"]);
        println!(
            "A0 ELIMINATED: first writable enlist is SQLITE_READONLY under query_only=ON; no transaction callbacks. Stop before savepoint/buffer experiments."
        );
        // probe drops its connection (and SQLite-owned boxes) before the module.
        drop(probe);
        assert_eq!(
            *callbacks.lock().unwrap(),
            vec!["connect", "disconnect", "destroy_aux"]
        );
        println!("A0 teardown callbacks={:?}", *callbacks.lock().unwrap());
    }
}
