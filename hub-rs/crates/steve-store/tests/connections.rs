//! How the store's connections behave: write transactions take the write lock at BEGIN, the pragmas
//! each connection gets, and the maintenance gate's continuation check running outside its lock.

mod common;

use rusqlite::Connection;
use steve_store::StoreError;

/// A second process's connection to the same file, which does not wait for locks.
fn other(path: &std::path::Path) -> Connection {
    let c = Connection::open(path).unwrap();
    c.pragma_update(None, "busy_timeout", 0).unwrap();
    c
}

#[test]
fn a_write_transaction_holds_the_write_lock_from_its_start() {
    let t = common::fresh();
    let o = other(t.store.path());
    t.store
        .transaction(|tx| {
            // read first, as most repository writes do
            let n: i64 = tx.query_row("SELECT count(*) FROM audit", [], |r| r.get(0))?;
            assert_eq!(n, 0);
            // another process cannot slip a commit in between (with BEGIN DEFERRED it could, and
            // the write below would then fail at once with SQLITE_BUSY_SNAPSHOT)
            let err = o
                .execute(
                    "INSERT INTO audit (workspace_id, ts, actor, action, target) VALUES ('default', 1, 'x', 'other', 'y')",
                    [],
                )
                .unwrap_err();
            assert_eq!(err.sqlite_error_code(), Some(rusqlite::ErrorCode::DatabaseBusy));
            tx.execute(
                "INSERT INTO audit (workspace_id, ts, actor, action, target) VALUES ('default', 1, 'x', 'ours', 'y')",
                [],
            )?;
            Ok::<_, StoreError>(())
        })
        .unwrap();
    let actions: Vec<String> = o
        .prepare("SELECT action FROM audit")
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    assert_eq!(actions, ["ours"]);
}

#[test]
fn connections_get_the_pragmas() {
    let t = common::fresh();
    let writer = t
        .store
        .with_conn(|c| {
            let get = |p: &str| c.query_row(&format!("PRAGMA {p}"), [], |r| r.get::<_, i64>(0));
            Ok((
                get("temp_store")?,
                get("cache_size")?,
                get("journal_size_limit")?,
            ))
        })
        .unwrap();
    // temp_store 2 = MEMORY
    assert_eq!(writer, (2, -32_768, 64 * 1024 * 1024));
    let reader = t
        .store
        .reader(|c| {
            let get = |p: &str| c.query_row(&format!("PRAGMA {p}"), [], |r| r.get::<_, i64>(0));
            Ok::<_, StoreError>((get("temp_store")?, get("cache_size")?, get("busy_timeout")?))
        })
        .unwrap();
    assert_eq!(reader, (2, -16_384, 5000));
    t.store.optimize().unwrap();
}

#[test]
fn a_continuation_check_may_use_the_gate() {
    let t = common::fresh();
    t.store.maintenance_set(true).unwrap();
    // the check runs outside the gate lock: one that looks at the gate (or waits on a reader while
    // a permit is dropped elsewhere) no longer deadlocks
    let s = t.store.clone();
    let p = t
        .store
        .continuing_activity("mcp", || s.maintenance() && s.activities().is_empty());
    assert!(p.is_some());
    assert_eq!(t.store.activities().get("mcp"), Some(&1));
    drop(p);
    assert!(t.store.maintenance_seal());
    assert!(
        t.store.continuing_activity("mcp", || true).is_none(),
        "sealed admits nothing"
    );
}
