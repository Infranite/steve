//! Authorizer evidence covers executed contract operations, not untested or legacy SQL.
//! The conformance_sqlite suite compares the same exact exception list in both layouts.
#![cfg(feature = "conformance")]
#[path = "uow_support.rs"]
mod support;
use std::collections::BTreeSet;
use steve_store_sqlite::exec::access_log;
// libtest can run cases together; their process-wide evidence windows must not overlap.
static ACCESS_WINDOW: std::sync::Mutex<()> = std::sync::Mutex::new(());
#[test]
fn cache_hits_and_trigger_access_keep_the_calling_operation() {
    use std::sync::Arc;
    use steve_storage_api::{uow::*, *};
    use support::*;
    let _window = ACCESS_WINDOW.lock().unwrap_or_else(|e| e.into_inner());
    let dir = tempfile::tempdir().unwrap();
    prepare_layout(dir.path(), false);
    let mut registry = steve_store_sqlite::domains::Registry::verification().unwrap();
    for name in ["core.verify.first", "core.verify.second"] {
        registry
            .register_op(name, Domain::Core, |c, op| {
                let Cell::Text(id) = &op.0[0].1 else {
                    panic!("text fixture id");
                };
                c.prepare_cached("INSERT INTO verify_core(id,value,at,rev) VALUES(?,1,0,1)")
                    .unwrap()
                    .execute([id])
                    .unwrap();
                Ok(())
            })
            .unwrap();
    }
    let store = open_store(dir.path(), Arc::new(registry), Arc::new(Time::default()));
    store.install_registered().unwrap();
    store.with_root(Domain::Core,Lane::Admin, |c,_,_| {
        c.execute_batch("CREATE TRIGGER cross_write AFTER INSERT ON verify_core BEGIN INSERT INTO verify_comms(id,value,at,rev) VALUES(NEW.id,1,0,1); END;").unwrap(); Ok(())
    }).unwrap();
    let _ = access_log::take();
    for (id, op) in [("one", "core.verify.first"), ("two", "core.verify.second")] {
        let u = unit(
            id,
            vec![Mutation::Op(AnyOp {
                name: op.into(),
                domain: Domain::Core,
                body: query::encode(&Row(vec![("id".into(), Cell::Text(id.into()))])).unwrap(),
            })],
        );
        block_on(store.commit(u)).unwrap();
    }
    let entries = access_log::take();
    for op in ["core.verify.first", "core.verify.second"] {
        assert!(
            entries
                .iter()
                .any(|a| a.operation == op && a.table == "verify_core" && a.mode == "write")
        );
        assert!(entries.iter().any(|a| a.operation == op
            && a.table == "verify_comms"
            && a.context == "verify"
            && a.mode == "write"));
    }
    assert_eq!(
        access_log::cross_context(&entries),
        BTreeSet::from(["verify\tverify_comms\twrite".into()])
    );
    assert!(access_log::expected(include_str!("cross_context.txt")).is_empty());
}
#[test]
fn read_pool_and_lease_queries_record_the_second_identical_operation() {
    use std::sync::Arc;
    use steve_storage_api::*;
    use support::*;
    let _window = ACCESS_WINDOW.lock().unwrap_or_else(|e| e.into_inner());
    let dir = tempfile::tempdir().unwrap();
    prepare_layout(dir.path(), true);
    let store = open_store(
        dir.path(),
        Arc::new(steve_store_sqlite::domains::Registry::verification().unwrap()),
        Arc::new(Time::default()),
    );
    store.install_registered().unwrap();
    block_on(store.commit(unit("seed", vec![insert("seed", 1)]))).unwrap();
    let query = || AnyQuery {
        name: "get".into(),
        domain: Domain::Core,
        body: Payload::Json(
            query::encode(&query::Get {
                key: query::Key::<Row> {
                    entity: "verify_core".into(),
                    values: vec![Cell::Text("seed".into())],
                    marker: std::marker::PhantomData,
                },
            })
            .unwrap(),
        ),
    };
    for _ in 0..2 {
        let _ = access_log::take();
        block_on(store.query(query())).unwrap();
        assert!(access_log::take().iter().any(|a| a.operation == "get"
            && a.context == "verify"
            && a.table == "verify_core"
            && a.mode == "read"));
    }
    let lease = block_on(store.begin_snapshot(Domain::Core, Lane::Interactive)).unwrap();
    for _ in 0..2 {
        let _ = access_log::take();
        block_on(store.lease_query(lease.clone(), query())).unwrap();
        assert!(
            access_log::take()
                .iter()
                .any(|a| a.operation == "get" && a.table == "verify_core" && a.mode == "read")
        );
    }
    block_on(store.lease_end(lease)).unwrap();
}
