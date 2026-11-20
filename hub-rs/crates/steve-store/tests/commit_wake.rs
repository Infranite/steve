//! Observable subscription behavior through Store's public connection/transaction API.
use std::{
    future::Future,
    sync::Arc,
    task::{Context, Waker},
};
use steve_store::{NoPublisher, Store, StoreError};
use tokio::sync::Notify;

fn setup() -> (tempfile::TempDir, Store) {
    let dir = tempfile::tempdir_in(env!("CARGO_MANIFEST_DIR")).unwrap();
    let store = Store::open(dir.path().join("commit.sqlite3"), NoPublisher).unwrap();
    store.with_conn(|c| {
        c.execute_batch("CREATE TABLE wake_source(id INTEGER PRIMARY KEY);
            CREATE TABLE wake_sink(id INTEGER PRIMARY KEY);
            CREATE TRIGGER wake_insert AFTER INSERT ON wake_source BEGIN INSERT INTO wake_sink VALUES(NEW.id); END;")?;
        Ok(())
    }).unwrap();
    (dir, store)
}
fn ready(wake: &Arc<Notify>) -> bool {
    let mut cx = Context::from_waker(Waker::noop());
    std::pin::pin!(wake.notified())
        .as_mut()
        .poll(&mut cx)
        .is_ready()
}
fn insert(store: &Store, id: i64) -> steve_store::Result<()> {
    store.transaction(|tx| {
        tx.execute("INSERT INTO wake_source VALUES(?)", [id])?;
        Ok(())
    })
}
#[test]
fn trigger_writes_wake_only_after_the_commit() {
    let (_dir, store) = setup();
    let wake = store.commit_wake(&["wake_sink"]);
    store
        .atomic(|| -> steve_store::Result<()> {
            insert(&store, 1)?;
            assert!(!ready(&wake));
            Ok(())
        })
        .unwrap();
    assert!(ready(&wake));
    assert!(!ready(&wake));
}
#[test]
fn rolled_back_trigger_writes_do_not_wake() {
    let (_dir, store) = setup();
    let wake = store.commit_wake(&["wake_sink"]);
    let result = store.atomic(|| -> steve_store::Result<()> {
        insert(&store, 1)?;
        Err(StoreError::Hook("rollback".into()))
    });
    assert!(result.is_err());
    assert!(!ready(&wake));
    store
        .reader(|c| {
            assert_eq!(
                c.query_row("SELECT count(*) FROM wake_sink", [], |r| r.get::<_, i64>(0))?,
                0
            );
            Ok::<_, StoreError>(())
        })
        .unwrap();
}
#[test]
fn inner_savepoint_rollback_keeps_the_outer_commit_hint() {
    let (_dir, store) = setup();
    let wake = store.commit_wake(&["wake_sink"]);
    store
        .atomic(|| -> steve_store::Result<()> {
            insert(&store, 1)?;
            let inner = store.atomic(|| -> steve_store::Result<()> {
                insert(&store, 2)?;
                Err(StoreError::Hook("inner rollback".into()))
            });
            assert!(inner.is_err());
            assert!(!ready(&wake));
            Ok(())
        })
        .unwrap();
    assert!(ready(&wake));
    store
        .reader(|c| {
            assert_eq!(
                c.query_row("SELECT sum(id) FROM wake_sink", [], |r| r.get::<_, i64>(0))?,
                1
            );
            Ok::<_, StoreError>(())
        })
        .unwrap();
}
fn detach_update_hook(store: &Store) -> bool {
    store
        .with_conn(|c| {
            // SAFETY: Store holds the unique writer lease. SQLite returns the previous callback's
            // opaque context without dereferencing it. No statement runs while detached; the public
            // Rust setter then drops its owned callback. This probe consumes this test's hook only.
            let previous = unsafe {
                rusqlite::ffi::sqlite3_update_hook(c.handle(), None, std::ptr::null_mut())
            };
            c.update_hook(None::<fn(rusqlite::hooks::Action, &str, &str, i64)>)?;
            Ok(!previous.is_null())
        })
        .unwrap()
}
#[test]
fn hooks_exist_only_for_live_subscribers() {
    for state in ["empty", "subscribed", "released"] {
        let (_dir, store) = setup();
        let mut wake = (state != "empty").then(|| store.commit_wake(&["wake_source"]));
        if state == "released" {
            drop(wake.take());
        }
        assert_eq!(
            detach_update_hook(&store),
            state == "subscribed",
            "hook state: {state}"
        );
        // Each fixture is consumed by the probe; no further statement uses its detached hook.
    }
}

#[test]
fn subscribing_inside_a_write_scope_covers_earlier_statements() {
    let (_dir, store) = setup();
    let wake = store
        .transaction(|tx| -> steve_store::Result<_> {
            tx.execute("INSERT INTO wake_source VALUES(1)", [])?;
            let wake = store.commit_wake(&["wake_sink"]);
            assert!(!ready(&wake));
            Ok(wake)
        })
        .unwrap();
    assert!(ready(&wake));
}

#[test]
fn r3d_autocommit_failure_keeps_an_earlier_successful_commit_hint() {
    for explicit_commit in [false, true] {
        let (_dir, store) = setup();
        insert(&store, 1).unwrap();
        let wake = store.commit_wake(&["wake_sink"]);
        let result = store.with_conn(|c| {
            if explicit_commit {
                c.execute_batch("BEGIN")?;
            }
            c.execute("INSERT INTO wake_source VALUES(2)", [])?;
            if explicit_commit {
                c.execute_batch("COMMIT")?;
            }
            assert!(
                !ready(&wake),
                "hints wait for the public writer scope to return"
            );
            c.execute("INSERT INTO wake_source VALUES(1)", [])?;
            Ok(())
        });
        assert!(result.is_err(), "the second statement must actually fail");
        assert!(
            ready(&wake),
            "the earlier SQLite commit must survive a later implicit rollback"
        );
        assert!(!ready(&wake));
        store
            .reader(|c| {
                assert_eq!(
                    c.query_row("SELECT sum(id) FROM wake_sink", [], |r| r.get::<_, i64>(0))?,
                    3
                );
                Ok::<_, StoreError>(())
            })
            .unwrap();
    }
}

#[test]
fn r3d_later_transaction_rollback_keeps_an_earlier_commit_hint() {
    let (_dir, store) = setup();
    let wake = store.commit_wake(&["wake_sink"]);
    store
        .with_conn(|c| {
            c.execute("INSERT INTO wake_source VALUES(1)", [])?;
            c.execute_batch("BEGIN; INSERT INTO wake_source VALUES(2); ROLLBACK;")?;
            Ok(())
        })
        .unwrap();
    assert!(ready(&wake));
    store
        .reader(|c| {
            assert_eq!(
                c.query_row("SELECT sum(id) FROM wake_sink", [], |r| r.get::<_, i64>(0))?,
                1
            );
            Ok::<_, StoreError>(())
        })
        .unwrap();
}
