//! Observable subscription behavior through Store's public connection/transaction API.
use std::{
    future::Future,
    sync::Arc,
    task::{Context, Waker},
};
use steve_store::testkit::SqliteFixtures;
use steve_store::{NoPublisher, Store, StoreError};
use tokio::sync::Notify;

fn setup() -> (tempfile::TempDir, Store) {
    let dir = tempfile::tempdir_in(env!("CARGO_MANIFEST_DIR")).unwrap();
    let store = Store::open(dir.path().join("commit.sqlite3"), NoPublisher).unwrap();
    store.fixture_write(|c| {
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
    store.write(|tx| {
        steve_store::testkit::sqlite_writer(tx)
            .execute("INSERT INTO wake_source VALUES(?)", [id])?;
        Ok(())
    })
}

fn deferred_constraint(store: &Store) {
    store
        .fixture_write(|c| {
            c.execute_batch(
                "CREATE TABLE wake_parent(id INTEGER PRIMARY KEY);
            CREATE TABLE wake_child(id INTEGER PRIMARY KEY, parent INTEGER NOT NULL
                REFERENCES wake_parent(id) DEFERRABLE INITIALLY DEFERRED);",
            )?;
            Ok(())
        })
        .unwrap();
}

fn detach_update_hook(store: &Store) -> bool {
    store
        .fixture_write(|c| {
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

// 1. Root commit after the public write returns.
#[test]
fn commit_hint_1_root_commit_wakes_after_return() {
    let (_dir, store) = setup();
    let wake = store.commit_wake(&["wake_sink"]);
    store
        .write(|_| -> steve_store::Result<()> {
            insert(&store, 1)?;
            assert!(!ready(&wake));
            Ok(())
        })
        .unwrap();
    assert!(ready(&wake));
    assert!(!ready(&wake));
}

// 2. Earlier commits survive a later autocommit statement failure or rollback.
#[test]
fn commit_hint_2_autocommit_success_survives_later_failure() {
    for explicit_commit in [false, true] {
        let (_dir, store) = setup();
        insert(&store, 1).unwrap();
        let wake = store.commit_wake(&["wake_sink"]);
        let result = store.fixture_write(|c| {
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
            .fixture_read(|c| {
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
fn commit_hint_2_autocommit_success_survives_later_failure_explicit_rollback() {
    let (_dir, store) = setup();
    let wake = store.commit_wake(&["wake_sink"]);
    store
        .fixture_write(|c| {
            c.execute("INSERT INTO wake_source VALUES(1)", [])?;
            c.execute_batch("BEGIN; INSERT INTO wake_source VALUES(2); ROLLBACK;")?;
            Ok(())
        })
        .unwrap();
    assert!(ready(&wake));
    store
        .fixture_read(|c| {
            assert_eq!(
                c.query_row("SELECT sum(id) FROM wake_sink", [], |r| r.get::<_, i64>(0))?,
                1
            );
            Ok::<_, StoreError>(())
        })
        .unwrap();
}

// 3. Root rollback.
#[test]
fn commit_hint_3_root_rollback_does_not_wake() {
    let (_dir, store) = setup();
    let wake = store.commit_wake(&["wake_sink"]);
    let result = store.write(|_| -> steve_store::Result<()> {
        insert(&store, 1)?;
        Err(StoreError::Hook("rollback".into()))
    });
    assert!(result.is_err());
    assert!(!ready(&wake));
    store
        .fixture_read(|c| {
            assert_eq!(
                c.query_row("SELECT count(*) FROM wake_sink", [], |r| r.get::<_, i64>(0))?,
                0
            );
            Ok::<_, StoreError>(())
        })
        .unwrap();
}

// 4. Savepoints and one coalesced hint per written table.
#[test]
fn commit_hint_4_savepoint_rollback_keeps_outer_and_each_table_once_same_table() {
    let (_dir, store) = setup();
    let wake = store.commit_wake(&["wake_sink"]);
    store
        .write(|_| -> steve_store::Result<()> {
            insert(&store, 1)?;
            let inner = store.write(|_| -> steve_store::Result<()> {
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
        .fixture_read(|c| {
            assert_eq!(
                c.query_row("SELECT sum(id) FROM wake_sink", [], |r| r.get::<_, i64>(0))?,
                1
            );
            Ok::<_, StoreError>(())
        })
        .unwrap();
}

#[test]
fn commit_hint_4_savepoint_rollback_keeps_outer_and_each_table_once() {
    let (_dir, store) = setup();
    store
        .fixture_write(|c| {
            c.execute_batch(
                "CREATE TABLE savepoint_a(id INTEGER PRIMARY KEY);
            CREATE TABLE savepoint_b(id INTEGER PRIMARY KEY);
            CREATE TABLE savepoint_ignored(id INTEGER PRIMARY KEY);",
            )?;
            Ok(())
        })
        .unwrap();
    let a = store.commit_wake(&["savepoint_a"]);
    let b = store.commit_wake(&["savepoint_b"]);
    store
        .write(|_| -> steve_store::Result<()> {
            let rolled_back = store.write(|_| -> steve_store::Result<()> {
                store.fixture_write(|c| {
                    c.execute("INSERT INTO savepoint_ignored VALUES(1)", [])?;
                    Ok(())
                })?;
                Err(StoreError::Hook("inner rollback".into()))
            });
            assert!(rolled_back.is_err());
            store.write(|_| -> steve_store::Result<()> {
                store.fixture_write(|c| {
                    c.execute("INSERT INTO savepoint_a VALUES(2)", [])?;
                    Ok(())
                })
            })?;
            store.write(|_| -> steve_store::Result<()> {
                store.fixture_write(|c| {
                    c.execute("INSERT INTO savepoint_b VALUES(3)", [])?;
                    Ok(())
                })
            })?;
            assert!(!ready(&a) && !ready(&b));
            Ok(())
        })
        .unwrap();
    assert!(
        ready(&a),
        "successful savepoint A must be observed independently"
    );
    assert!(
        ready(&b),
        "successful savepoint B must be observed independently"
    );
    assert!(!ready(&a));
    assert!(!ready(&b));
    store
        .fixture_read(|c| {
            assert_eq!(
                c.query_row("SELECT sum(id) FROM savepoint_a", [], |r| r
                    .get::<_, i64>(0))?,
                2
            );
            assert_eq!(
                c.query_row("SELECT sum(id) FROM savepoint_b", [], |r| r
                    .get::<_, i64>(0))?,
                3
            );
            assert_eq!(
                c.query_row("SELECT count(*) FROM savepoint_ignored", [], |r| r
                    .get::<_, i64>(0))?,
                0
            );
            Ok::<_, StoreError>(())
        })
        .unwrap();
}

// 5. Subscribe inside the transaction, including before a failed commit.
#[test]
fn commit_hint_5_subscribe_after_write_in_transaction_still_covers() {
    let (_dir, store) = setup();
    let wake = store
        .write(|tx| -> steve_store::Result<_> {
            steve_store::testkit::sqlite_writer(tx)
                .execute("INSERT INTO wake_source VALUES(1)", [])?;
            let wake = store.commit_wake(&["wake_sink"]);
            assert!(!ready(&wake));
            Ok(wake)
        })
        .unwrap();
    assert!(ready(&wake));
}

#[test]
fn commit_hint_5_subscribe_after_write_in_transaction_still_covers_failed_commit() {
    let (_dir, store) = setup();
    deferred_constraint(&store);
    let mut subscription = None;
    let result = store.write(|tx| -> steve_store::Result<()> {
        steve_store::testkit::sqlite_writer(tx)
            .execute("INSERT INTO wake_child VALUES(1,999)", [])?;
        let wake = store.commit_wake(&["wake_child"]);
        assert!(!ready(&wake));
        subscription = Some(wake);
        Ok(())
    });
    assert!(result.is_err());
    let wake = subscription.unwrap();
    assert!(!ready(&wake));
    store.fixture_write(|_| Ok(())).unwrap();
    assert!(
        !ready(&wake),
        "conservative subscription hints must roll back too"
    );
}

// 6. A failed COMMIT does not publish, including on a later empty write.
#[test]
fn commit_hint_6_commit_failure_does_not_wake() {
    let (_dir, store) = setup();
    deferred_constraint(&store);
    let wake = store.commit_wake(&["wake_child"]);
    let mut reached_commit = false;
    let result = store.write(|tx| -> steve_store::Result<()> {
        steve_store::testkit::sqlite_writer(tx)
            .execute("INSERT INTO wake_child VALUES(1,999)", [])?;
        assert!(!ready(&wake));
        reached_commit = true;
        Ok(()) // The insert succeeded; COMMIT itself must fail its deferred constraint.
    });
    assert!(
        reached_commit,
        "the deferred insert must succeed before COMMIT fails"
    );
    assert!(result.is_err());
    assert!(!ready(&wake));
    store.fixture_write(|_| Ok(())).unwrap();
    assert!(
        !ready(&wake),
        "failed COMMIT must not leave a hint for the next write scope"
    );
    store
        .fixture_read(|c| {
            assert_eq!(
                c.query_row("SELECT count(*) FROM wake_child", [], |r| r
                    .get::<_, i64>(0))?,
                0
            );
            Ok::<_, StoreError>(())
        })
        .unwrap();
}

// 7. Known blind spot; see G2-R2. Not permission for a silent subscribed-table delete.
#[test]
fn commit_hint_7_unconditional_delete_known_blind_spot() {
    let (_dir, store) = setup();
    store
        .fixture_write(|c| {
            c.execute_batch(
                "CREATE TABLE wake_delete(id INTEGER PRIMARY KEY);
            INSERT INTO wake_delete VALUES(1),(2);",
            )?;
            Ok(())
        })
        .unwrap();
    let wake = store.commit_wake(&["wake_delete"]);
    store
        .fixture_write(|c| {
            assert_eq!(c.execute("DELETE FROM wake_delete", [])?, 2);
            Ok(())
        })
        .unwrap();
    // 已知盲点，见 G2-R2：钉住截断优化的漏报，不表示允许被订阅表静默删除。
    assert!(
        !ready(&wake),
        "known blind spot (G2-R2): truncate optimization does not report this table"
    );
    store
        .fixture_read(|c| {
            assert_eq!(
                c.query_row("SELECT count(*) FROM wake_delete", [], |r| r
                    .get::<_, i64>(0))?,
                0
            );
            Ok::<_, StoreError>(())
        })
        .unwrap();
    store
        .fixture_write(|c| {
            c.execute("INSERT INTO wake_delete VALUES(3)", [])?;
            Ok(())
        })
        .unwrap();
    assert!(ready(&wake), "the table subscription itself is connected");
    store
        .fixture_write(|c| {
            c.execute("DELETE FROM wake_delete WHERE id=3", [])?;
            Ok(())
        })
        .unwrap();
    assert!(
        ready(&wake),
        "a row-qualified delete uses the observable path"
    );
    assert!(!ready(&wake));
}

// 8. REPLACE still produces a table-level hint through its replacement INSERT.
#[test]
fn commit_hint_8_replace_implicit_delete_still_wakes() {
    let (_dir, store) = setup();
    store
        .fixture_write(|c| {
            c.execute_batch(
                "CREATE TABLE wake_replace(id INTEGER PRIMARY KEY, slot TEXT UNIQUE);
            INSERT INTO wake_replace VALUES(1,'same'),(2,'other');",
            )?;
            Ok(())
        })
        .unwrap();
    let wake = store.commit_wake(&["wake_replace"]);
    store
        .fixture_write(|c| {
            c.execute("REPLACE INTO wake_replace VALUES(3,'same')", [])?;
            assert!(!ready(&wake));
            Ok(())
        })
        .unwrap();
    // commit_wake is a table hint, not a per-row delete event. SQLite skips the implicit
    // deletion callback, but the replacement INSERT still wakes this rowid table.
    assert!(ready(&wake));
    assert!(!ready(&wake));
    store
        .fixture_read(|c| {
            let ids = c
                .prepare("SELECT id FROM wake_replace ORDER BY id")?
                .query_map([], |r| r.get::<_, i64>(0))?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            assert_eq!(ids, [2, 3]);
            Ok::<_, StoreError>(())
        })
        .unwrap();
}

// Subscription lifetime (independent of the eight mutation scenarios).
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
