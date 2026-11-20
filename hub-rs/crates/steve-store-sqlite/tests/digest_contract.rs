//! The default test configuration must exercise the registered operation on real comms tables.
#[path = "support/digest.rs"]
mod digest_fixture;
#[path = "uow_support.rs"]
mod support;

use rusqlite::Connection;
use std::sync::Arc;
use steve_storage_api::{ops::comms::digest::*, uow::*, *};
use steve_store_sqlite::{
    digest::{self, Source as NativeSource},
    domains::Registry,
    publish::Attached,
    store::SqliteStore,
    uow::{RootRevision, SqliteBatchRunner},
};
use support::*;

fn sql_error(e: rusqlite::Error) -> CommitError {
    CommitError::Fatal(error(steve_types::Code::Internal, e.to_string()))
}
fn transaction(
    store: &SqliteStore<String>,
    run: impl FnOnce(&Connection, &SqliteBatchRunner<String>, &RootRevision) -> Result<(), CommitError>,
) -> Result<(), CommitError> {
    store.with_root(Domain::Comms, Lane::Admin, |c, runner, root| {
        let tx = c.unchecked_transaction().map_err(sql_error)?;
        run(&tx, runner, root)?;
        tx.commit().map_err(sql_error)
    })
}
fn operation(
    c: &Connection,
    runner: &SqliteBatchRunner<String>,
    root: &RootRevision,
    id: &str,
    request: SyncPendingMembership,
) -> Result<(), CommitError> {
    let mut work = unit(id, vec![Mutation::Op(request.to_wire().unwrap())]);
    work.domain = Domain::Comms;
    sign(&mut work);
    drop(runner.apply_in(c, Attached::new(work, vec![]), root)?);
    Ok(())
}
fn enqueue(id: &str, workspace: &str) -> SyncPendingMembership {
    SyncPendingMembership::enqueue_digestible(Source::ChanMessage, id.into(), workspace.into())
        .unwrap()
}
fn inspect(store: &SqliteStore<String>, check: impl FnOnce(&Connection)) {
    let c = steve_store_sqlite::open::open_readonly(store.engine(Domain::Comms).path()).unwrap();
    let tx = c.unchecked_transaction().unwrap();
    check(&tx);
    for workspace in ["ws", "other"] {
        assert!(digest::verify_pending(&tx, workspace).unwrap().is_empty());
    }
    assert!(
        !tx.prepare("PRAGMA foreign_key_check")
            .unwrap()
            .exists([])
            .unwrap()
    );
    tx.commit().unwrap();
}

#[test]
fn digest_contract_real_comms_enqueue_receipt_move_and_rollback() {
    for split in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        prepare_layout(dir.path(), split);
        let mut registry = Registry::default();
        registry.register_digest().unwrap();
        let store = open_store(dir.path(), Arc::new(registry), Arc::new(Time::default()));
        store
            .with_root(Domain::Comms, Lane::Admin, |c, _, _| {
                digest_fixture::install(c).map_err(sql_error)
            })
            .unwrap();
        transaction(&store, |c, runner, root| {
            for (id, at) in [("a", 10), ("b", 20)] {
                digest_fixture::put(
                    c,
                    NativeSource::ChanMessage,
                    id,
                    "ws",
                    "space:old",
                    "received",
                    at,
                    true,
                )
                .map_err(sql_error)?;
                operation(c, runner, root, id, enqueue(id, "ws"))?;
            }
            Ok(())
        })
        .unwrap();
        inspect(&store, |c| {
            assert_eq!(
                count(c, "digest_pending"),
                2,
                "registered enqueue maintains real membership"
            );
            let projection: (i64, i64, i64) = c
                .query_row(
                    "SELECT messages,oldest,newest FROM digest_pending_convs",
                    [],
                    |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
                )
                .unwrap();
            assert_eq!(projection, (2, 10, 20));
        });
        transaction(&store, |c, runner, root| {
            digest_fixture::receipt(c, "ws", "space:old", "a").map_err(sql_error)?;
            operation(
                c,
                runner,
                root,
                "receipt",
                SyncPendingMembership::dequeue_on_receipt("space:old", "a", "ws".into()).unwrap(),
            )
        })
        .unwrap();
        inspect(&store, |c| {
            assert_eq!(count(c, digest::RECEIPTS_TABLE), 1);
            assert_eq!(
                count(c, "digest_pending"),
                1,
                "receipt dequeues only its message"
            );
            let id: String = c
                .query_row("SELECT message_id FROM digest_pending", [], |r| r.get(0))
                .unwrap();
            assert_eq!(id, "b");
        });
        transaction(&store, |c, runner, root| {
            digest_fixture::put(
                c,
                NativeSource::ChanMessage,
                "b",
                "other",
                "space:new",
                "sent",
                30,
                false,
            )
            .map_err(sql_error)?;
            operation(c, runner, root, "move", enqueue("b", "other"))
        })
        .unwrap();
        inspect(&store, |c| {
            assert_eq!(count(c, "digest_pending_convs"), 1);
            let scope: (String, String) = c
                .query_row("SELECT workspace_id,conv FROM digest_pending", [], |r| {
                    Ok((r.get(0)?, r.get(1)?))
                })
                .unwrap();
            assert_eq!(
                scope,
                ("other".into(), "space:new".into()),
                "move removes the pre-change scope"
            );
        });
        let failed = transaction(&store, |c, runner, root| {
            digest_fixture::put(
                c,
                NativeSource::ChanMessage,
                "ghost",
                "other",
                "space:new",
                "received",
                40,
                true,
            )
            .map_err(sql_error)?;
            operation(c, runner, root, "rollback", enqueue("ghost", "other"))?;
            assert_eq!(
                count(c, "digest_pending"),
                2,
                "registered SQL ran before the failure"
            );
            let messages: i64 = c
                .query_row("SELECT messages FROM digest_pending_convs", [], |r| {
                    r.get(0)
                })
                .map_err(sql_error)?;
            assert_eq!(messages, 2, "projection changed before the failure");
            // A real late uniqueness failure must roll back both source and registered SQL effects.
            digest_fixture::put(
                c,
                NativeSource::ChanMessage,
                "b",
                "other",
                "space:new",
                "sent",
                30,
                true,
            )
            .map_err(sql_error)
        });
        assert!(failed.is_err());
        inspect(&store, |c| {
            assert_eq!(
                count(c, NativeSource::ChanMessage.shape().table),
                2,
                "failed source insertion rolls back"
            );
            assert_eq!(
                count(c, "digest_pending"),
                1,
                "failed registered membership rolls back"
            );
            let projection: (i64, i64, i64) = c
                .query_row(
                    "SELECT messages,oldest,newest FROM digest_pending_convs",
                    [],
                    |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
                )
                .unwrap();
            assert_eq!(
                projection,
                (1, 30, 30),
                "failed projection update rolls back"
            );
        });
    }
}
