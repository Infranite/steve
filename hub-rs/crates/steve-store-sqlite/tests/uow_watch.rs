#[path = "uow_support.rs"]
mod support;
use std::{
    future::Future,
    sync::Arc,
    task::{Context, Waker},
};
use steve_storage_api::*;
use steve_store_sqlite::domains::Registry;
use support::*;
fn ready(watch: &CommitWatch) -> bool {
    let mut cx = Context::from_waker(Waker::noop());
    let mut future = std::pin::pin!(watch.changed());
    future.as_mut().poll(&mut cx).is_ready()
}
#[test]
fn root_hook_covers_without_rowid_delete_triggers_savepoints_and_rollbacks() {
    for split in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        prepare_layout(dir.path(), split);
        let store = open_store(
            dir.path(),
            Arc::new(Registry::verification().unwrap()),
            Arc::new(Time::default()),
        );
        store.install_registered().unwrap();
        // Replace the physical fixture while preserving its public entity contract.
        store.with_root(Domain::Core,Lane::Admin, |c,_,_| {
            c.execute_batch("DROP TABLE verify_core; CREATE TABLE verify_core(id TEXT PRIMARY KEY,value INTEGER,label TEXT,at INTEGER,data TEXT,bytes BLOB,real REAL,flag INTEGER,rev INTEGER NOT NULL) WITHOUT ROWID; CREATE TABLE source(value INTEGER); CREATE TRIGGER source_write AFTER INSERT ON source BEGIN INSERT INTO verify_core(id,value,rev) VALUES('trigger',NEW.value,1); END;").unwrap();
            Ok(())
        }).unwrap();
        let watch = store.watch(&["verify_core".into()]);
        // A real pending future is installed before the commit as well as after it.
        assert!(!ready(&watch));
        store
            .with_root(Domain::Core, Lane::Interactive, |c, _, _| {
                c.execute_batch("BEGIN; INSERT INTO source VALUES(7); COMMIT")
                    .unwrap();
                Ok(())
            })
            .unwrap();
        assert!(ready(&watch));
        store
            .with_root(Domain::Core, Lane::Interactive, |c, _, _| {
                c.execute_batch("BEGIN; DELETE FROM verify_core; COMMIT")
                    .unwrap();
                Ok(())
            })
            .unwrap();
        assert!(
            ready(&watch),
            "unqualified DELETE on WITHOUT ROWID must wake"
        );
        store
            .with_root(Domain::Core, Lane::Interactive, |c, _, _| {
                c.execute_batch(
                    "BEGIN; INSERT INTO verify_core(id,value,rev) VALUES('rollback',1,1); ROLLBACK",
                )
                .unwrap();
                Ok(())
            })
            .unwrap();
        assert!(!ready(&watch), "an explicit rollback must not wake");
        store.with_root(Domain::Core,Lane::Interactive, |c,_,_| {
            c.execute_batch("BEGIN; SAVEPOINT inner; INSERT INTO verify_core(id,value,rev) VALUES('savepoint',1,1); ROLLBACK TO inner; RELEASE inner; INSERT INTO source VALUES(8); COMMIT").unwrap(); Ok(())
        }).unwrap();
        assert!(ready(&watch));
        store.with_root(Domain::Core,Lane::Interactive, |c,_,_| {
            c.execute_batch("BEGIN; INSERT INTO verify_core(id,value,rev) VALUES('before-savepoint',1,1); SAVEPOINT inner; INSERT INTO verify_core(id,value,rev) VALUES('discarded',1,1); ROLLBACK TO inner; RELEASE inner; COMMIT").unwrap(); Ok(())
        }).unwrap();
        assert!(
            ready(&watch),
            "savepoint rollback must not erase earlier committed table changes"
        );
        let result: Result<(), _> = store.with_root(Domain::Core, Lane::Interactive, |c, _, _| {
            c.execute_batch("BEGIN; DELETE FROM verify_core;").unwrap();
            Err(steve_storage_api::CommitError::Fatal(error(
                steve_types::Code::Invalid,
                "abandon root",
            )))
        });
        assert!(result.is_err());
        assert!(!ready(&watch));
        // A durable commit still wakes if the legacy callback returns an error later.
        let result: Result<(), _> = store.with_root(Domain::Core, Lane::Interactive, |c, _, _| {
            c.execute_batch("BEGIN; DELETE FROM verify_core; COMMIT;")
                .unwrap();
            Err(steve_storage_api::CommitError::Fatal(error(
                steve_types::Code::Invalid,
                "after commit",
            )))
        });
        assert!(result.is_err());
        assert!(ready(&watch));
        drop(watch);
        // No subscriber: Capture never installs a hook (asserted directly by its unit test).
        block_on(store.commit(unit("no-subscriber", vec![insert("unit", 2)]))).unwrap();
    }
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
                c.execute_batch("BEGIN; DELETE FROM verify_comms; COMMIT")
                    .unwrap();
                Ok(())
            })
            .unwrap();
        assert!(
            ready(&watch),
            "rowid truncate deletion and peer-file legacy commits must wake"
        );
    }
}

#[test]
fn subscription_inside_root_covers_earlier_sql_and_waits_for_commit() {
    for split in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        prepare_layout(dir.path(), split);
        let store = open_store(
            dir.path(),
            Arc::new(Registry::verification().unwrap()),
            Arc::new(Time::default()),
        );
        store.install_registered().unwrap();
        let watch = store
            .with_root(Domain::Core, Lane::Interactive, |c, _, _| {
                c.execute_batch(
                    "BEGIN; INSERT INTO verify_core(id,value,at,rev) VALUES('before',1,0,1)",
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
        let watch = store
            .with_root(Domain::Comms, Lane::Interactive, |c, _, _| {
                c.execute_batch(
                    "BEGIN; INSERT INTO verify_comms(id,value,at,rev) VALUES('discard',1,0,1)",
                )
                .unwrap();
                let watch = store.commit_wake(Domain::Comms, &["verify_comms"]);
                assert!(!ready_notify(&watch));
                c.execute_batch("ROLLBACK").unwrap();
                Ok(watch)
            })
            .unwrap();
        assert!(!ready_notify(&watch));
        drop(watch);
        // No watched row changed: a subscription inside the scope is deliberately
        // conservative, and shares publication with the table-key legacy facade.
        let watch = store
            .with_root(Domain::Core, Lane::Interactive, |c, _, _| {
                c.execute_batch("BEGIN; UPDATE verify_core SET value=2 WHERE id='before'")
                    .unwrap();
                let watch = store.commit_wake(Domain::Core, &["verify_log"]);
                c.execute_batch("COMMIT").unwrap();
                Ok(watch)
            })
            .unwrap();
        assert!(ready_notify(&watch));
        drop(watch);
        let watch = store
            .with_root(Domain::Core, Lane::Interactive, |c, _, root| {
                c.execute_batch("BEGIN").unwrap();
                let watch = store.commit_wake(Domain::Core, &["verify_log"]);
                assert!(!ready_notify(&watch));
                c.execute_batch("COMMIT").unwrap();
                root.mark_committed();
                assert!(!ready_notify(&watch));
                Ok(watch)
            })
            .unwrap();
        assert!(
            ready_notify(&watch),
            "explicit publication also covers empty commits"
        );
    }
}

#[test]
fn preupdate_commit_hints_cover_truncate_replace_and_without_rowid() {
    let dir = tempfile::tempdir().unwrap();
    prepare_layout(dir.path(), false);
    let store = open_store(
        dir.path(),
        Arc::new(Registry::verification().unwrap()),
        Arc::new(Time::default()),
    );
    store.install_registered().unwrap();
    store.with_root(Domain::Core,Lane::Admin, |c,_,_| {
        c.execute_batch("CREATE TABLE rowid_probe(id INTEGER PRIMARY KEY); INSERT INTO rowid_probe VALUES(1); CREATE TABLE without_rowid_probe(id TEXT PRIMARY KEY) WITHOUT ROWID;").unwrap();Ok(())
    }).unwrap();
    for (table, sql, label) in [
        (
            "rowid_probe",
            "BEGIN; DELETE FROM rowid_probe; COMMIT",
            "truncate-optimized DELETE",
        ),
        (
            "rowid_probe",
            "BEGIN; INSERT INTO rowid_probe VALUES(1); COMMIT",
            "seed REPLACE",
        ),
        (
            "rowid_probe",
            "BEGIN; INSERT OR REPLACE INTO rowid_probe VALUES(1); COMMIT",
            "REPLACE",
        ),
        (
            "without_rowid_probe",
            "BEGIN; INSERT INTO without_rowid_probe VALUES('key'); COMMIT",
            "WITHOUT ROWID",
        ),
    ] {
        let watch = store.commit_wake(Domain::Core, &[table]);
        store
            .with_root(Domain::Core, Lane::Interactive, |c, _, root| {
                c.execute_batch(sql).unwrap();
                root.mark_committed();
                Ok(())
            })
            .unwrap();
        assert!(ready_notify(&watch), "preupdate must hint: {label}");
    }
}

fn ready_notify(wake: &Arc<tokio::sync::Notify>) -> bool {
    let mut cx = Context::from_waker(Waker::noop());
    std::pin::pin!(wake.notified())
        .as_mut()
        .poll(&mut cx)
        .is_ready()
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
