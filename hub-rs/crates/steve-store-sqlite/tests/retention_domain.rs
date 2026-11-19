#[path = "uow_support.rs"]
mod support;
use std::sync::Arc;
use steve_storage_api::{retention::Budget, *};
use steve_store_sqlite::domains::Registry;
use support::*;

#[tokio::test]
async fn failed_outer_commit_rolls_back_retention_and_does_not_publish_the_cursor() {
    for split in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        prepare_layout(dir.path(), split);
        let registry = Arc::new(Registry::verification().unwrap());
        let store = open_store(dir.path(), registry.clone(), Arc::new(Time::default()));
        store.install_registered().unwrap();
        store.with_root(Domain::Core, Lane::Admin, |c, _, rev| {
            let entity = registry.entity("verify_log", Domain::Core)?;
            for id in ["a", "b"] {
                entity.insert(c, &row(id, 0), rev.rev(), false)?;
            }
            c.execute_batch("CREATE TABLE retention_parent(id TEXT PRIMARY KEY);
                CREATE TABLE retention_child(id TEXT REFERENCES retention_parent(id) DEFERRABLE INITIALLY DEFERRED);
                CREATE TRIGGER retention_commit_failure AFTER DELETE ON verify_log
                BEGIN INSERT INTO retention_child VALUES('missing'); END;").unwrap();
            Ok(())
        }).unwrap();
        let budget = Budget {
            rows: 1,
            target_ms: 20,
        };
        let e = store
            .retention("verify_log".into(), budget)
            .await
            .unwrap_err();
        assert_eq!(
            e.code,
            steve_types::Code::Invalid,
            "deferred FK fails only at the outer COMMIT"
        );
        store
            .with_root(Domain::Core, Lane::Admin, |c, _, _| {
                assert!(c.is_autocommit());
                assert_eq!(
                    count(c, "verify_log"),
                    2,
                    "no deletion escaped the failed transaction"
                );
                assert_eq!(count(c, "retention_child"), 0);
                c.execute_batch("DROP TRIGGER retention_commit_failure")
                    .unwrap();
                Ok(())
            })
            .unwrap();
        let first = store.retention("verify_log".into(), budget).await.unwrap();
        assert_eq!((first.affected, first.more), (1, true));
        store
            .with_root(Domain::Core, Lane::Admin, |c, _, _| {
                let entity = registry.entity("verify_log", Domain::Core)?;
                assert!(entity.get(c, &[Cell::Text("a".into())])?.is_none());
                assert!(
                    entity.get(c, &[Cell::Text("b".into())])?.is_some(),
                    "retry starts at a, not at the uncommitted cursor"
                );
                Ok(())
            })
            .unwrap();
        let tail = store.retention("verify_log".into(), budget).await.unwrap();
        assert_eq!((tail.affected, tail.more), (1, false));
    }
}
