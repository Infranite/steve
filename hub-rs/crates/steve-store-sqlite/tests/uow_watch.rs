#[path = "uow_support.rs"]
mod support;
use std::{
    future::Future,
    sync::Arc,
    task::{Context, Waker},
};
use steve_storage_api::*;
use steve_store_sqlite::{domains::Registry, store::SqliteStore};
use support::*;
fn ready(watch: &CommitWatch) -> bool {
    let mut cx = Context::from_waker(Waker::noop());
    std::pin::pin!(watch.changed())
        .as_mut()
        .poll(&mut cx)
        .is_ready()
}
fn ready_notify(wake: &Arc<tokio::sync::Notify>) -> bool {
    let mut cx = Context::from_waker(Waker::noop());
    std::pin::pin!(wake.notified())
        .as_mut()
        .poll(&mut cx)
        .is_ready()
}
#[derive(Clone, Copy)]
enum WritePath {
    Callback,
    Guard,
}
fn each_layout(test: impl Fn(SqliteStore<String>)) {
    each_layout_on(WritePath::Callback, test);
}
fn each_layout_on(path: WritePath, test: impl Fn(SqliteStore<String>)) {
    for split in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        prepare_layout(dir.path(), split);
        let registry = Arc::new(Registry::verification().unwrap());
        let time = Arc::new(Time::default());
        let store = match path {
            WritePath::Callback => open_store(dir.path(), registry, time),
            WritePath::Guard => open_sync_store(dir.path(), registry, time),
        };
        store.install_registered().unwrap();
        test(store);
    }
}
fn raw(store: &SqliteStore<String>, sql: &str) {
    store
        .with_root(Domain::Core, Lane::Interactive, |c, _, _| {
            c.execute_batch(sql).unwrap();
            Ok(())
        })
        .unwrap();
}
fn with_writer<T>(
    path: WritePath,
    store: &SqliteStore<String>,
    domain: Domain,
    lane: Lane,
    run: impl FnOnce(&rusqlite::Connection) -> Result<T, steve_storage_api::CommitError>,
) -> Result<T, steve_storage_api::CommitError> {
    match path {
        WritePath::Callback => store.with_root(domain, lane, |c, _, _| run(c)),
        WritePath::Guard => {
            let guard = store.root_guard(domain, lane)?;
            let outcome = run(&guard);
            let leaked = !guard.is_autocommit();
            drop(guard); // Real public guard owns rollback, capture finish and lease release.
            match outcome {
                Ok(_) if leaked => Err(rejected()),
                outcome => outcome,
            }
        }
    }
}
fn raw_on(path: WritePath, store: &SqliteStore<String>, sql: &str) {
    with_writer(path, store, Domain::Core, Lane::Interactive, |c| {
        c.execute_batch(sql).unwrap();
        Ok(())
    })
    .unwrap();
}
fn rejected() -> steve_storage_api::CommitError {
    steve_storage_api::CommitError::Fatal(error(
        steve_types::Code::Invalid,
        "intentional callback failure",
    ))
}
fn case_1(path: WritePath) {
    each_layout_on(path, |store| {
        let wake = store.commit_wake(Domain::Core, &["verify_core"]);
        let clone = store.clone();
        with_writer(path, &clone, Domain::Core, Lane::Interactive, |c| {
            c.execute_batch("BEGIN; INSERT INTO verify_core(id,value,rev) VALUES('root',1,1)")
                .unwrap();
            assert!(!ready_notify(&wake));
            c.execute_batch("COMMIT").unwrap();
            assert!(
                !ready_notify(&wake),
                "commit hook cannot publish inside the callback"
            );
            Ok(())
        })
        .unwrap();
        assert!(ready_notify(&wake));
        assert!(!ready_notify(&wake));
        let entity = store.watch(&["verify_core".into()]);
        block_on(clone.commit(unit("clone-uow", vec![insert("uow", 2)]))).unwrap();
        assert!(ready(&entity));
        assert!(ready_notify(&wake));
        assert!(!ready_notify(&wake));
    });
}
fn case_2(path: WritePath) {
    each_layout_on(path, |store| {
        raw_on(
            path,
            &store,
            "CREATE TABLE discarded(id INTEGER PRIMARY KEY)",
        );
        let wake = store.commit_wake(Domain::Core, &["verify_core"]);
        let discarded = store.commit_wake(Domain::Core, &["discarded"]);
        for (id, tail) in [
            (
                "bare",
                "INSERT INTO verify_core(id,value,rev) VALUES('bare',2,2)",
            ),
            (
                "rollback",
                "BEGIN; INSERT INTO discarded VALUES(1); INSERT OR ROLLBACK INTO verify_core(id,value,rev) VALUES('rollback',2,2)",
            ),
        ] {
            let result: Result<(), _> =
                with_writer(path, &store, Domain::Core, Lane::Interactive, |c| {
                    c.execute("INSERT INTO verify_core(id,value,rev) VALUES(?1,1,1)", [id])
                        .unwrap();
                    assert!(!ready_notify(&wake));
                    assert!(c.execute_batch(tail).is_err());
                    Err(rejected())
                });
            assert!(result.is_err());
            assert!(ready_notify(&wake));
            assert!(!ready_notify(&wake));
            assert!(
                !ready_notify(&discarded),
                "the later failed transaction must stay quiet"
            );
        }
    });
}
fn case_3(path: WritePath) {
    each_layout_on(path, |store| {
        let wake = store.commit_wake(Domain::Core, &["verify_core"]);
        raw_on(
            path,
            &store,
            "BEGIN; INSERT INTO verify_core(id,value,rev) VALUES('explicit',1,1); ROLLBACK",
        );
        assert!(!ready_notify(&wake));
        let result: Result<(), _> =
            with_writer(path, &store, Domain::Core, Lane::Interactive, |c| {
                c.execute_batch(
                    "BEGIN; INSERT INTO verify_core(id,value,rev) VALUES('leaked',1,1)",
                )
                .unwrap();
                Err(rejected())
            });
        assert!(result.is_err());
        assert!(!ready_notify(&wake));
        let panic = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _: Result<(), _> =
                with_writer(path, &store, Domain::Core, Lane::Interactive, |c| {
                    c.execute_batch(
                        "BEGIN; INSERT INTO verify_core(id,value,rev) VALUES('panic',1,1)",
                    )
                    .unwrap();
                    panic!("root panic before commit")
                });
        }));
        assert!(panic.is_err());
        assert!(!ready_notify(&wake));
        block_on(store.commit(unit("after-panic", vec![insert("live", 1)]))).unwrap();
        assert!(ready_notify(&wake));
    });
}
fn case_4(path: WritePath) {
    each_layout_on(path, |store| {
        raw_on(
            path,
            &store,
            "CREATE TABLE hint_a(id INTEGER); CREATE TABLE hint_b(id INTEGER)",
        );
        let a = store.commit_wake(Domain::Core, &["hint_a"]);
        let b = store.commit_wake(Domain::Core, &["hint_b"]);
        raw_on(
            path,
            &store,
            "BEGIN; INSERT INTO hint_a VALUES(1); SAVEPOINT inner; INSERT INTO hint_b VALUES(2); ROLLBACK TO inner; RELEASE inner; INSERT INTO hint_a VALUES(3); COMMIT",
        );
        assert!(ready_notify(&a));
        assert!(!ready_notify(&a));
        // The rolled-back savepoint's table may receive an extra hint. Its
        // presence or absence is deliberately not part of this contract.
        let _ = ready_notify(&b);
        with_writer(path, &store, Domain::Core, Lane::Interactive, |c| {
            assert_eq!(
                c.query_row("SELECT count(*) FROM hint_b", [], |r| r.get::<_, i64>(0))
                    .unwrap(),
                0
            );
            Ok(())
        })
        .unwrap();
    });
}
fn case_5(path: WritePath) {
    each_layout_on(path, |store| {
        let watch = with_writer(path, &store, Domain::Core, Lane::Interactive, |c| {
            // There is no existing subscriber: hooks must be installed lazily here.
            c.execute_batch("BEGIN; INSERT INTO verify_core(id,value,rev) VALUES('before',1,1)")
                .unwrap();
            let watch = store.watch(&["verify_core".into()]);
            assert!(!ready(&watch));
            c.execute_batch("COMMIT").unwrap();
            assert!(!ready(&watch));
            Ok(watch)
        })
        .unwrap();
        assert!(ready(&watch));
        assert!(!ready(&watch));
        drop(watch);
        let wake = with_writer(path, &store, Domain::Core, Lane::Interactive, |c| {
            c.execute_batch("BEGIN; INSERT INTO verify_core(id,value,rev) VALUES('discard',1,1)")
                .unwrap();
            let wake = store.commit_wake(Domain::Core, &["verify_core"]);
            c.execute_batch("ROLLBACK").unwrap();
            Ok(wake)
        })
        .unwrap();
        assert!(!ready_notify(&wake));
    });
}
fn case_6(path: WritePath) {
    each_layout_on(path, |store| {
        raw_on(
            path,
            &store,
            "PRAGMA foreign_keys=ON; CREATE TABLE parent(id INTEGER PRIMARY KEY); CREATE TABLE child(id INTEGER PRIMARY KEY, parent INTEGER REFERENCES parent(id) DEFERRABLE INITIALLY DEFERRED)",
        );
        let wake = store.commit_wake(Domain::Core, &["child"]);
        let result = with_writer(path, &store, Domain::Core, Lane::Interactive, |c| {
            c.execute_batch("BEGIN; INSERT INTO child VALUES(1,99)")
                .unwrap();
            let error = c.execute_batch("COMMIT").unwrap_err();
            assert_eq!(
                error.sqlite_error_code(),
                Some(rusqlite::ErrorCode::ConstraintViolation)
            );
            assert!(!ready_notify(&wake));
            Ok(()) // with_root must roll back the still-open failed transaction.
        });
        assert!(result.is_err());
        assert!(!ready_notify(&wake));
        raw_on(
            path,
            &store,
            "INSERT INTO parent VALUES(99); INSERT INTO child VALUES(2,99)",
        );
        assert!(ready_notify(&wake));
        assert!(!ready_notify(&wake));
    });
}
fn case_7(path: WritePath) {
    // Known blind spot, see G2-R2. Production guard prohibits this on watched tables.
    each_layout_on(path, |store| {
        raw_on(
            path,
            &store,
            "CREATE TABLE truncate_probe(id INTEGER PRIMARY KEY); INSERT INTO truncate_probe VALUES(1),(2)",
        );
        let wake = store.commit_wake(Domain::Core, &["truncate_probe"]);
        raw_on(path, &store, "BEGIN; DELETE FROM truncate_probe; COMMIT");
        assert!(
            !ready_notify(&wake),
            "bundled update_hook misses truncate optimization"
        );
        with_writer(path, &store, Domain::Core, Lane::Interactive, |c| {
            assert_eq!(
                c.query_row("SELECT count(*) FROM truncate_probe", [], |r| r
                    .get::<_, i64>(0))
                    .unwrap(),
                0
            );
            Ok(())
        })
        .unwrap();
        // Reuse the same subscription: a predicate DELETE must still hint.
        raw_on(path, &store, "INSERT INTO truncate_probe VALUES(3)");
        assert!(ready_notify(&wake));
        raw_on(path, &store, "DELETE FROM truncate_probe WHERE id=3");
        assert!(ready_notify(&wake));
        assert!(!ready_notify(&wake));
    });
}
fn case_8(path: WritePath) {
    each_layout_on(path, |store| {
        raw_on(
            path,
            &store,
            "CREATE TABLE replace_probe(id INTEGER PRIMARY KEY, value TEXT); INSERT INTO replace_probe VALUES(1,'old')",
        );
        let wake = store.commit_wake(Domain::Core, &["replace_probe"]);
        raw_on(
            path,
            &store,
            "INSERT OR REPLACE INTO replace_probe VALUES(1,'new')",
        );
        assert!(ready_notify(&wake));
        assert!(!ready_notify(&wake));
        with_writer(path, &store, Domain::Core, Lane::Interactive, |c| {
            assert_eq!(
                c.query_row("SELECT value FROM replace_probe WHERE id=1", [], |r| r
                    .get::<_, String>(
                    0
                ))
                .unwrap(),
                "new"
            );
            Ok(())
        })
        .unwrap();
    });
}
#[test]
fn bundled_foreign_key_cascades_and_trigger_writes_hint_each_table() {
    each_layout(|store| {
        raw(
            &store,
            "PRAGMA foreign_keys=ON; CREATE TABLE parent(id INTEGER PRIMARY KEY); CREATE TABLE child(id INTEGER PRIMARY KEY,parent INTEGER REFERENCES parent(id) ON DELETE CASCADE); CREATE TABLE triggered(id INTEGER); CREATE TRIGGER log_delete AFTER DELETE ON child BEGIN INSERT INTO triggered VALUES(old.id); END; INSERT INTO parent VALUES(1); INSERT INTO child VALUES(2,1)",
        );
        let child = store.commit_wake(Domain::Core, &["child"]);
        let trigger = store.commit_wake(Domain::Core, &["triggered"]);
        raw(&store, "DELETE FROM parent WHERE id=1");
        assert!(ready_notify(&child));
        assert!(ready_notify(&trigger));
        assert!(!ready_notify(&child));
        assert!(!ready_notify(&trigger));
    });
}
#[test]
fn independent_connections_are_outside_this_store_hint_registry() {
    each_layout(|store| {
        let wake = store.commit_wake(Domain::Core, &["verify_core"]);
        let c = rusqlite::Connection::open(store.engine(Domain::Core).path()).unwrap();
        c.execute_batch("INSERT INTO verify_core(id,value,rev) VALUES('external',1,1)")
            .unwrap();
        assert!(!ready_notify(&wake));
        block_on(store.commit(unit("own", vec![insert("local", 1)]))).unwrap();
        assert!(ready_notify(&wake));
    });
}
#[test]
fn entity_names_resolve_to_physical_tables_across_files() {
    for split in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        prepare_layout(dir.path(), split);
        let mut registry = Registry::verification().unwrap();
        let mut desc = steve_store_sqlite::domains::fixtures::CORE.to_wire();
        desc.name = "core.test.logical".into();
        registry.register(desc, "physical_rows").unwrap();
        let store = open_store(dir.path(), Arc::new(registry), Arc::new(Time::default()));
        store.install_registered().unwrap();
        let watch = store.watch(&["core.test.logical".into(), "verify_comms".into()]);
        let u = unit(
            "logical",
            vec![steve_storage_api::uow::Mutation::Insert {
                entity: "core.test.logical".into(),
                row: row("logical", 1),
            }],
        );
        block_on(store.commit(u)).unwrap();
        assert!(ready(&watch));
        let mut u = unit(
            "comms",
            vec![steve_storage_api::uow::Mutation::Insert {
                entity: "verify_comms".into(),
                row: row("comms", 1),
            }],
        );
        u.domain = Domain::Comms;
        sign(&mut u);
        block_on(store.commit(u)).unwrap();
        assert!(ready(&watch));
        store
            .with_root(Domain::Comms, Lane::Interactive, |c, _, _| {
                c.execute_batch("BEGIN; DELETE FROM verify_comms WHERE id IS NOT NULL; COMMIT")
                    .unwrap();
                Ok(())
            })
            .unwrap();
        assert!(
            ready(&watch),
            "qualified deletion and peer-file legacy commits must wake"
        );
    }
}

#[test]
fn entity_and_legacy_table_subscriptions_share_commit_publication() {
    let dir = tempfile::tempdir().unwrap();
    prepare_layout(dir.path(), false);
    let store = open_store(
        dir.path(),
        Arc::new(Registry::verification().unwrap()),
        Arc::new(Time::default()),
    );
    store.install_registered().unwrap();
    let entity = store.watch(&["verify_core".into()]);
    let legacy = store.commit_wake(Domain::Core, &["verify_core"]);
    let unrelated = store.commit_wake(Domain::Core, &["verify_log"]);
    block_on(store.commit(unit("shared-publication", vec![insert("shared", 1)]))).unwrap();
    assert!(ready(&entity));
    assert!(ready_notify(&legacy));
    assert!(!ready_notify(&unrelated));
    let failed = unit(
        "shared-rollback",
        vec![insert("discarded", 1), insert("discarded", 2)],
    );
    assert!(block_on(store.commit(failed)).is_err());
    assert!(!ready(&entity));
    assert!(!ready_notify(&legacy));
}

#[test]
fn first_subscription_after_autocommit_is_conservative_and_survives_later_rollback() {
    each_layout(|store| {
        let wake = store.with_root(Domain::Core, Lane::Interactive, |c, _, _| {
            c.execute_batch("INSERT INTO verify_core(id,value,rev) VALUES('earlier',1,1)").unwrap();
            let wake = store.commit_wake(Domain::Core, &["verify_core"]);
            assert!(!ready_notify(&wake));
            c.execute_batch("BEGIN; INSERT INTO verify_core(id,value,rev) VALUES('discarded',1,1); ROLLBACK").unwrap();
            Ok(wake)
        }).unwrap();
        assert!(ready_notify(&wake));
        assert!(!ready_notify(&wake));
    });
}

#[test]
fn synchronous_root_guard_owns_capture_and_publishes_once_on_release() {
    assert!(tokio::runtime::Handle::try_current().is_err());
    for split in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        prepare_layout(dir.path(), split);
        let store = open_sync_store(
            dir.path(),
            Arc::new(Registry::verification().unwrap()),
            Arc::new(Time::default()),
        );
        store.install_registered().unwrap();
        let clone = store.clone();
        let wake = store.commit_wake(Domain::Comms, &["verify_comms"]);
        let entity = store.watch(&["verify_comms".into()]);
        let guard = clone.root_guard(Domain::Comms, Lane::Interactive).unwrap();
        guard
            .execute_batch(
                "BEGIN; INSERT INTO verify_comms(id,value,rev) VALUES('sync',1,1); COMMIT",
            )
            .unwrap();
        assert!(!ready_notify(&wake));
        assert!(!ready(&entity));
        assert!(store.root_guard(Domain::Comms, Lane::Admin).is_err());
        drop(guard);
        assert!(ready_notify(&wake));
        assert!(!ready_notify(&wake));
        assert!(ready(&entity));
        assert!(!ready(&entity));
        let guard = store.root_guard(Domain::Comms, Lane::Interactive).unwrap();
        guard
            .execute_batch("BEGIN; INSERT INTO verify_comms(id,value,rev) VALUES('abandoned',1,1)")
            .unwrap();
        drop(guard);
        assert!(!ready_notify(&wake));
        assert!(!ready(&entity));
        assert!(tokio::runtime::Handle::try_current().is_err());
    }
}

#[test]
fn synchronous_root_guard_first_subscription_and_unwind_share_cleanup_exit() {
    for split in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        prepare_layout(dir.path(), split);
        let store = open_sync_store(
            dir.path(),
            Arc::new(Registry::verification().unwrap()),
            Arc::new(Time::default()),
        );
        store.install_registered().unwrap();
        let guard = store.root_guard(Domain::Core, Lane::Interactive).unwrap();
        guard
            .execute_batch(
                "BEGIN; INSERT INTO verify_core(id,value,rev) VALUES('before-subscribe',1,1)",
            )
            .unwrap();
        let wake = store.commit_wake(Domain::Core, &["verify_core"]);
        guard.execute_batch("COMMIT").unwrap();
        assert!(!ready_notify(&wake));
        drop(guard);
        assert!(ready_notify(&wake));
        let panic = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let guard = store.root_guard(Domain::Core, Lane::Interactive).unwrap();
            guard
                .execute_batch("BEGIN; INSERT INTO verify_core(id,value,rev) VALUES('panic',1,1)")
                .unwrap();
            panic!("direct root guard unwind");
        }));
        assert!(panic.is_err());
        assert!(!ready_notify(&wake));
        let guard = store.root_guard(Domain::Core, Lane::Interactive).unwrap();
        guard
            .execute_batch("INSERT INTO verify_core(id,value,rev) VALUES('durable',1,1)")
            .unwrap();
        assert!(!ready_notify(&wake));
        drop(guard);
        assert!(ready_notify(&wake));
    }
}

// Both public write paths share the same behavioral assertions and exact case IDs.
macro_rules! hint_case {
    ($callback:ident, $guard:ident, $case:ident) => {
        #[test]
        fn $callback() {
            $case(WritePath::Callback);
        }
        #[test]
        fn $guard() {
            $case(WritePath::Guard);
        }
    };
}
hint_case!(
    commit_hint_1_root_commit_wakes_after_return,
    commit_hint_1_root_commit_wakes_after_return_via_guard,
    case_1
);
hint_case!(
    commit_hint_2_autocommit_success_survives_later_failure,
    commit_hint_2_autocommit_success_survives_later_failure_via_guard,
    case_2
);
hint_case!(
    commit_hint_3_root_rollback_does_not_wake,
    commit_hint_3_root_rollback_does_not_wake_via_guard,
    case_3
);
hint_case!(
    commit_hint_4_savepoint_rollback_keeps_outer_and_each_table_once,
    commit_hint_4_savepoint_rollback_keeps_outer_and_each_table_once_via_guard,
    case_4
);
hint_case!(
    commit_hint_5_subscribe_after_write_in_transaction_still_covers,
    commit_hint_5_subscribe_after_write_in_transaction_still_covers_via_guard,
    case_5
);
hint_case!(
    commit_hint_6_commit_failure_does_not_wake,
    commit_hint_6_commit_failure_does_not_wake_via_guard,
    case_6
);
hint_case!(
    commit_hint_7_unconditional_delete_known_blind_spot,
    commit_hint_7_unconditional_delete_known_blind_spot_via_guard,
    case_7
);
hint_case!(
    commit_hint_8_replace_implicit_delete_still_wakes,
    commit_hint_8_replace_implicit_delete_still_wakes_via_guard,
    case_8
);

#[test]
fn public_table_subscription_does_not_require_a_registered_entity() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    struct Count(std::sync::Weak<AtomicUsize>);
    impl CommitHint for Count {
        fn is_alive(&self) -> bool {
            self.0.strong_count() > 0
        }
        fn notify(&self) {
            if let Some(count) = self.0.upgrade() {
                count.fetch_add(1, Ordering::SeqCst);
            }
        }
    }
    each_layout_on(WritePath::Guard, |store| {
        // No Registry descriptor exists for this deliberately physical-only table.
        raw(
            &store,
            "CREATE TABLE physical_hint_only(id INTEGER PRIMARY KEY)",
        );
        let count = Arc::new(AtomicUsize::new(0));
        store.subscribe_tables(
            Domain::Core,
            &["physical_hint_only"],
            Arc::new(Count(Arc::downgrade(&count))),
        );
        let legacy = store.commit_wake(Domain::Core, &["physical_hint_only"]);
        let guard = store.root_guard(Domain::Core, Lane::Interactive).unwrap();
        guard
            .execute_batch("INSERT INTO physical_hint_only VALUES(1)")
            .unwrap();
        assert_eq!(count.load(Ordering::SeqCst), 0);
        assert!(!ready_notify(&legacy));
        drop(guard);
        assert_eq!(count.load(Ordering::SeqCst), 1);
        assert!(ready_notify(&legacy));
        assert!(!ready_notify(&legacy));
        raw(&store, "INSERT INTO physical_hint_only VALUES(2)");
        assert_eq!(count.load(Ordering::SeqCst), 2);
        assert!(ready_notify(&legacy));
    });
}

// Each probe removes that callback. Probe after the SQL statement has finished,
// before dropping the root; no subsequent SQL may depend on the removed hooks.
fn assert_engine_hooks(c: &rusqlite::Connection, installed: bool) {
    // SAFETY: root guard owns this live SQLite connection on the current thread;
    // callback registration is changed only outside sqlite3_step execution.
    let callbacks = unsafe {
        let db = c.handle();
        [
            rusqlite::ffi::sqlite3_update_hook(db, None, std::ptr::null_mut()),
            rusqlite::ffi::sqlite3_commit_hook(db, None, std::ptr::null_mut()),
            rusqlite::ffi::sqlite3_rollback_hook(db, None, std::ptr::null_mut()),
        ]
    };
    for callback in callbacks {
        assert_eq!(!callback.is_null(), installed);
    }
}
#[test]
fn engine_root_hooks_exist_only_for_live_subscribers() {
    each_layout_on(WritePath::Guard, |store| {
        let guard = store.root_guard(Domain::Core, Lane::Interactive).unwrap();
        assert_engine_hooks(&guard, false);
        drop(guard);
        let wake = store.commit_wake(Domain::Core, &["verify_core"]);
        let guard = store.root_guard(Domain::Core, Lane::Interactive).unwrap();
        guard
            .execute_batch("INSERT INTO verify_core(id,value,rev) VALUES('hook-probe',1,1)")
            .unwrap();
        assert_engine_hooks(&guard, true);
        assert!(!ready_notify(&wake));
        drop(guard);
        assert!(ready_notify(&wake));
        drop(wake);
        let guard = store.root_guard(Domain::Core, Lane::Interactive).unwrap();
        assert_engine_hooks(&guard, false);
        drop(guard);
    });
}

#[test]
fn returned_guard_keeps_multiple_commits_before_later_rollback() {
    each_layout_on(WritePath::Guard, |store| {
        raw(
            &store,
            "CREATE TABLE guard_b(id INTEGER); CREATE TABLE guard_discard(id INTEGER)",
        );
        let a = store.commit_wake(Domain::Core, &["verify_core"]);
        let b = store.commit_wake(Domain::Core, &["guard_b"]);
        let discarded = store.commit_wake(Domain::Core, &["guard_discard"]);
        let guard = store.root_guard(Domain::Core, Lane::Interactive).unwrap();
        guard.execute_batch("INSERT INTO verify_core(id,value,rev) VALUES('auto',1,1); BEGIN; INSERT INTO guard_b VALUES(2); COMMIT; BEGIN; INSERT INTO guard_discard VALUES(3); ROLLBACK; BEGIN; INSERT INTO guard_discard VALUES(4)").unwrap();
        assert!(!ready_notify(&a));
        assert!(!ready_notify(&b));
        assert!(!ready_notify(&discarded));
        drop(guard); // Roll back the last leaked transaction, preserving both prior commits.
        assert!(ready_notify(&a));
        assert!(ready_notify(&b));
        assert!(!ready_notify(&discarded));
        assert!(!ready_notify(&a));
        assert!(!ready_notify(&b));
        let read = store.engine(Domain::Core).read_conn().unwrap();
        assert_eq!(
            read.query_row("SELECT count(*) FROM guard_b", [], |r| r.get::<_, i64>(0))
                .unwrap(),
            1
        );
        assert_eq!(
            read.query_row("SELECT count(*) FROM guard_discard", [], |r| r
                .get::<_, i64>(0))
                .unwrap(),
            0
        );
    });
}

#[test]
fn cross_file_subscription_inside_core_root_is_retained() {
    let dir = tempfile::tempdir().unwrap();
    prepare_layout(dir.path(), true);
    let store = open_sync_store(
        dir.path(),
        Arc::new(Registry::verification().unwrap()),
        Arc::new(Time::default()),
    );
    store.install_registered().unwrap();
    let core = store.root_guard(Domain::Core, Lane::Interactive).unwrap();
    let tables = store.commit_wake(Domain::Comms, &["verify_comms"]);
    let entities = store.watch(&["verify_core".into(), "verify_comms".into()]);
    core.execute_batch("INSERT INTO verify_core(id,value,rev) VALUES('core',1,1)")
        .unwrap();
    assert!(!ready(&entities));
    drop(core);
    assert!(ready(&entities));
    assert!(!ready_notify(&tables));
    let comms = store.root_guard(Domain::Comms, Lane::Interactive).unwrap();
    comms
        .execute_batch("INSERT INTO verify_comms(id,value,rev) VALUES('comms',1,1)")
        .unwrap();
    assert!(!ready_notify(&tables));
    drop(comms);
    assert!(ready_notify(&tables));
    assert!(ready(&entities));
    assert!(!ready(&entities));
}

#[test]
fn subscription_returns_while_another_thread_holds_root() {
    each_layout_on(WritePath::Guard, |store| {
        let root = store.root_guard(Domain::Core, Lane::Interactive).unwrap();
        let clone = store.clone();
        let (send, received) = std::sync::mpsc::channel();
        let subscriber = std::thread::spawn(move || {
            // Synchronous watch must also return on an async executor thread.
            let runtime = tokio::runtime::Builder::new_current_thread()
                .build()
                .unwrap();
            runtime.block_on(async {
                let watch = clone.watch(&["verify_core".into(), "verify_comms".into()]);
                let tables = clone.commit_wake(Domain::Core, &["verify_core"]);
                send.send((watch, tables)).unwrap();
            });
        });
        // Timeout is only a deadlock watchdog, not a timing/scheduling assertion.
        // Release root before joining even on failure, so a regression cannot hang.
        let subscribed = received.recv_timeout(std::time::Duration::from_secs(5));
        drop(root);
        subscriber.join().unwrap();
        let (watch, tables) = subscribed.expect("subscription waited for a writer grant");
        assert!(!ready(&watch));
        assert!(!ready_notify(&tables));
        raw_on(
            WritePath::Guard,
            &store,
            "INSERT INTO verify_core(id,value,rev) VALUES('subscribed',1,1)",
        );
        assert!(ready(&watch));
        assert!(ready_notify(&tables));
    });
}
