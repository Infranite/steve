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
fn each_layout(test: impl Fn(SqliteStore<String>)) {
    for split in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        prepare_layout(dir.path(), split);
        let store = open_store(
            dir.path(),
            Arc::new(Registry::verification().unwrap()),
            Arc::new(Time::default()),
        );
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
fn rejected() -> steve_storage_api::CommitError {
    steve_storage_api::CommitError::Fatal(error(
        steve_types::Code::Invalid,
        "intentional callback failure",
    ))
}
#[test]
fn commit_hint_1_root_commit_wakes_after_return() {
    each_layout(|store| {
        let wake = store.commit_wake(Domain::Core, &["verify_core"]);
        let clone = store.clone();
        clone
            .with_root(Domain::Core, Lane::Interactive, |c, _, _| {
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
#[test]
fn commit_hint_2_autocommit_success_survives_later_failure() {
    each_layout(|store| {
        raw(&store, "CREATE TABLE discarded(id INTEGER PRIMARY KEY)");
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
                store.with_root(Domain::Core, Lane::Interactive, |c, _, _| {
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
#[test]
fn commit_hint_3_root_rollback_does_not_wake() {
    each_layout(|store| {
        let wake = store.commit_wake(Domain::Core, &["verify_core"]);
        raw(
            &store,
            "BEGIN; INSERT INTO verify_core(id,value,rev) VALUES('explicit',1,1); ROLLBACK",
        );
        assert!(!ready_notify(&wake));
        let result: Result<(), _> = store.with_root(Domain::Core, Lane::Interactive, |c, _, _| {
            c.execute_batch("BEGIN; INSERT INTO verify_core(id,value,rev) VALUES('leaked',1,1)")
                .unwrap();
            Err(rejected())
        });
        assert!(result.is_err());
        assert!(!ready_notify(&wake));
        let panic = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _: Result<(), _> = store.with_root(Domain::Core, Lane::Interactive, |c, _, _| {
                c.execute_batch("BEGIN; INSERT INTO verify_core(id,value,rev) VALUES('panic',1,1)")
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
#[test]
fn commit_hint_4_savepoint_rollback_keeps_outer_and_each_table_once() {
    each_layout(|store| {
        raw(
            &store,
            "CREATE TABLE hint_a(id INTEGER); CREATE TABLE hint_b(id INTEGER)",
        );
        let a = store.commit_wake(Domain::Core, &["hint_a"]);
        let b = store.commit_wake(Domain::Core, &["hint_b"]);
        raw(
            &store,
            "BEGIN; INSERT INTO hint_a VALUES(1); SAVEPOINT inner; INSERT INTO hint_b VALUES(2); ROLLBACK TO inner; RELEASE inner; INSERT INTO hint_a VALUES(3); COMMIT",
        );
        assert!(ready_notify(&a));
        assert!(!ready_notify(&a));
        assert!(
            ready_notify(&b),
            "savepoint rollback may conservatively over-report its table"
        );
        assert!(!ready_notify(&b));
        store
            .with_root(Domain::Core, Lane::Interactive, |c, _, _| {
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
#[test]
fn commit_hint_5_subscribe_after_write_in_transaction_still_covers() {
    each_layout(|store| {
        let watch = store
            .with_root(Domain::Core, Lane::Interactive, |c, _, _| {
                // There is no existing subscriber: hooks must be installed lazily here.
                c.execute_batch(
                    "BEGIN; INSERT INTO verify_core(id,value,rev) VALUES('before',1,1)",
                )
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
        let wake = store
            .with_root(Domain::Core, Lane::Interactive, |c, _, _| {
                c.execute_batch(
                    "BEGIN; INSERT INTO verify_core(id,value,rev) VALUES('discard',1,1)",
                )
                .unwrap();
                let wake = store.commit_wake(Domain::Core, &["verify_core"]);
                c.execute_batch("ROLLBACK").unwrap();
                Ok(wake)
            })
            .unwrap();
        assert!(!ready_notify(&wake));
    });
}
#[test]
fn commit_hint_6_commit_failure_does_not_wake() {
    each_layout(|store| {
        raw(
            &store,
            "PRAGMA foreign_keys=ON; CREATE TABLE parent(id INTEGER PRIMARY KEY); CREATE TABLE child(id INTEGER PRIMARY KEY, parent INTEGER REFERENCES parent(id) DEFERRABLE INITIALLY DEFERRED)",
        );
        let wake = store.commit_wake(Domain::Core, &["child"]);
        let result = store.with_root(Domain::Core, Lane::Interactive, |c, _, _| {
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
        raw(
            &store,
            "INSERT INTO parent VALUES(99); INSERT INTO child VALUES(2,99)",
        );
        assert!(ready_notify(&wake));
        assert!(!ready_notify(&wake));
    });
}
#[test]
fn commit_hint_7_unconditional_delete_known_blind_spot() {
    // Known blind spot, see G2-R2. Production guard prohibits this on watched tables.
    each_layout(|store| {
        raw(
            &store,
            "CREATE TABLE truncate_probe(id INTEGER PRIMARY KEY); INSERT INTO truncate_probe VALUES(1),(2)",
        );
        let wake = store.commit_wake(Domain::Core, &["truncate_probe"]);
        raw(&store, "BEGIN; DELETE FROM truncate_probe; COMMIT");
        assert!(
            !ready_notify(&wake),
            "bundled update_hook misses truncate optimization"
        );
        store
            .with_root(Domain::Core, Lane::Interactive, |c, _, _| {
                assert_eq!(
                    c.query_row("SELECT count(*) FROM truncate_probe", [], |r| r
                        .get::<_, i64>(0))
                        .unwrap(),
                    0
                );
                Ok(())
            })
            .unwrap();
    });
}
#[test]
fn commit_hint_8_replace_implicit_delete_still_wakes() {
    each_layout(|store| {
        raw(
            &store,
            "CREATE TABLE replace_probe(id INTEGER PRIMARY KEY, value TEXT); INSERT INTO replace_probe VALUES(1,'old')",
        );
        let wake = store.commit_wake(Domain::Core, &["replace_probe"]);
        raw(
            &store,
            "INSERT OR REPLACE INTO replace_probe VALUES(1,'new')",
        );
        assert!(ready_notify(&wake));
        assert!(!ready_notify(&wake));
        store
            .with_root(Domain::Core, Lane::Interactive, |c, _, _| {
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
