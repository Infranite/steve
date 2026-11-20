//! Integration tests for re-entrant `Store::atomic` and `SAVEPOINT` isolation.

mod common;

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::{Arc, Mutex};

use serde_json::{Map, Value, json};
use steve_store::{BusEvent, FnPublisher, NewBot, NewDecision, NewItem, Store, StoreError, WS};

fn open_tracked() -> (tempfile::TempDir, Store, Arc<Mutex<Vec<BusEvent>>>) {
    let dir = tempfile::tempdir().unwrap();
    let events = Arc::new(Mutex::new(Vec::new()));
    let sink = events.clone();
    let store = Store::open(
        dir.path().join("steve.sqlite3"),
        FnPublisher(move |e| sink.lock().unwrap().push(e)),
    )
    .unwrap();
    store.ensure_workspace().unwrap();
    store
        .bots()
        .seed(
            "steve",
            true,
            &NewBot {
                name: "Steve".into(),
                harness: "claude-code".into(),
                ..Default::default()
            },
            WS,
        )
        .unwrap();
    // Drain any initial setup events.
    events.lock().unwrap().clear();
    (dir, store, events)
}

#[test]
fn atomic_reads_uncommitted_writes_and_defers_events_until_outer_commit() {
    let (_dir, store, events) = open_tracked();
    let external = steve_store_sqlite::open::open_readonly(store.path()).unwrap();
    let wake = store.outbox_wake();

    let (item_id, dec_id) = store
        .atomic(|| -> steve_store::Result<(String, String)> {
            let item = store
                .items()
                .create(
                    WS,
                    &NewItem {
                        tags: common::task_tags(&store, WS),
                        bot_id: "steve".into(),
                        title: "atomic item".into(),
                        goal: "verify atomic commit".into(),
                        ..Default::default()
                    },
                )?
                .expect("created item");

            // Read-your-own-uncommitted-writes inside `atomic` (both `read` and `reader` paths).
            assert!(store.items().get(&item.id)?.is_some());
            let seen_in_reader: i64 = store.reader(|c| {
                Ok::<_, StoreError>(c.query_row(
                    "SELECT COUNT(*) FROM items WHERE id = ?",
                    [&item.id],
                    |r| r.get(0),
                )?)
            })?;
            assert_eq!(seen_in_reader, 1);

            // External connection does NOT see uncommitted WAL writes yet.
            assert_eq!(
                external.query_row("SELECT count(*) FROM items WHERE id=?", [&item.id], |r| r
                    .get::<_, i64>(
                    0
                ))?,
                0
            );

            let dec = store
                .decisions()
                .create(&NewDecision {
                    workspace_id: WS.into(),
                    kind: "ask".into(),
                    subject_type: "question".into(),
                    subject_id: item.id.clone(),
                    item_id: Some(item.id.clone()),
                    question: "proceed?".into(),
                    options: Some(vec!["yes".into(), "no".into()]),
                    requested_by: "steve".into(),

                    ..Default::default()
                })?
                .expect("created decision");

            store
                .timeline()
                .add(&item.id, "steve", "sys", "asked", None, &[])?;

            // Nested `transaction` emitting an outbox event must also wait for outer commit.
            store.transaction(|tx| {
                steve_store::outbox::emit(tx, WS, "test.topic", 1, &json!({"item": item.id}))?;
                Ok::<_, StoreError>(())
            })?;

            // Zero typed events have escaped while outer transaction is still open!
            assert!(events.lock().unwrap().is_empty());
            Ok((item.id, dec.id))
        })
        .unwrap();

    // Now committed: external connection sees everything, events published in order, outbox notified.
    assert_eq!(
        external
            .query_row("SELECT count(*) FROM items WHERE id=?", [&item_id], |r| r
                .get::<_, i64>(
                0
            ))
            .unwrap(),
        1
    );
    assert_eq!(
        external
            .query_row(
                "SELECT count(*) FROM decisions WHERE id=?",
                [&dec_id],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
        1
    );
    assert_eq!(
        external
            .query_row(
                "SELECT count(*) FROM timeline WHERE item_id=?",
                [&item_id],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
        1
    );

    let published = events.lock().unwrap().clone();
    assert_eq!(
        published,
        vec![
            steve_types::BusEvent::Domain(steve_types::DomainEvent::Work(
                steve_types::WorkEvent::Item {
                    id: {
                        let value: String = item_id.clone();
                        steve_types::ItemId::from(value)
                    },
                    ws: steve_types::WorkspaceId::from(steve_store::WS)
                }
            )),
            steve_types::BusEvent::Domain(steve_types::DomainEvent::Work(
                steve_types::WorkEvent::Decision {
                    id: {
                        let value: String = dec_id;
                        steve_types::DecisionId::from(value)
                    },
                    item_id: {
                        let value: Option<Option<String>> = Some(Some(item_id.clone()));
                        value.map(|value| value.map(Into::into))
                    },
                    ws: steve_types::WorkspaceId::from(steve_store::WS)
                }
            )),
            steve_types::BusEvent::Domain(steve_types::DomainEvent::Work(
                steve_types::WorkEvent::Item {
                    id: {
                        let value: String = item_id;
                        steve_types::ItemId::from(value)
                    },
                    ws: steve_types::WorkspaceId::from(steve_store::WS)
                }
            )),
        ]
    );

    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .build()
        .unwrap();
    rt.block_on(async {
        tokio::time::timeout(std::time::Duration::from_millis(50), wake.notified())
            .await
            .expect("outbox_wake should hold one permit after outer commit");
    });
}

#[test]
fn atomic_rolls_back_all_tables_and_discards_events_on_err_and_panic() {
    let (_dir, store, events) = open_tracked();
    let wake = store.outbox_wake();

    let err = store.atomic(|| -> steve_store::Result<()> {
        let item = store
            .items()
            .create(
                WS,
                &NewItem {
                    tags: common::task_tags(&store, WS),
                    bot_id: "steve".into(),
                    title: "will rollback".into(),
                    goal: "goal".into(),
                    ..Default::default()
                },
            )?
            .unwrap();
        let mut settings = Map::new();
        settings.insert("tz".into(), Value::String("UTC".into()));
        store.settings().set(WS, &settings, "me")?;
        store
            .timeline()
            .add(&item.id, "me", "sys", "step 1", None, &[])?;
        store.transaction(|tx| {
            steve_store::outbox::emit(tx, WS, "test.rollback", 1, &json!({"ok": false}))?;
            Ok::<_, StoreError>(())
        })?;
        Err(StoreError::Conflict("abort".into()))
    });
    assert!(err.is_err());
    assert!(store.items().list(WS, false).unwrap().is_empty());
    assert!(store.settings().all(WS).unwrap().is_empty());
    assert!(events.lock().unwrap().is_empty());

    // Panic unwind must also roll back cleanly, drop staged events, and leave the Store usable.
    let panicked = catch_unwind(AssertUnwindSafe(|| {
        let _ = store.atomic(|| -> steve_store::Result<()> {
            let _ = store.items().create(
                WS,
                &NewItem {
                    tags: common::task_tags(&store, WS),
                    bot_id: "steve".into(),
                    title: "panic item".into(),
                    goal: "panic goal".into(),
                    ..Default::default()
                },
            )?;
            panic!("simulated mid-atomic panic");
        });
    }));
    assert!(panicked.is_err());
    assert!(store.items().list(WS, false).unwrap().is_empty());
    assert!(events.lock().unwrap().is_empty());

    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .build()
        .unwrap();
    rt.block_on(async {
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(20), wake.notified())
                .await
                .is_err(),
            "rolled-back outbox writes must never wake the dispatcher"
        );
    });

    // Subsequent write on the same thread works normally and starts item numbering back at ST-1!
    let item = store
        .items()
        .create(
            WS,
            &NewItem {
                tags: common::task_tags(&store, WS),
                bot_id: "steve".into(),
                title: "healthy after panic".into(),
                goal: "ok".into(),
                ..Default::default()
            },
        )
        .unwrap()
        .unwrap();
    assert_eq!(item.id, "ST-1");
    assert_eq!(events.lock().unwrap().len(), 1);
}

#[test]
fn nested_savepoint_rollback_keeps_outer_writes_and_drops_inner_events() {
    let (_dir, store, events) = open_tracked();

    let kept_id = store
        .atomic(|| -> steve_store::Result<String> {
            let outer = store
                .items()
                .create(
                    WS,
                    &NewItem {
                        tags: common::task_tags(&store, WS),
                        bot_id: "steve".into(),
                        title: "outer kept".into(),
                        goal: "outer".into(),
                        ..Default::default()
                    },
                )?
                .unwrap();

            // Inner `atomic` fails: its SQLite writes AND its staged typed events roll back via SAVEPOINT.
            let inner_res = store.atomic(|| -> steve_store::Result<()> {
                let inner = store
                    .items()
                    .create(
                        WS,
                        &NewItem {
                            tags: common::task_tags(&store, WS),
                            bot_id: "steve".into(),
                            title: "inner discarded".into(),
                            goal: "inner".into(),
                            ..Default::default()
                        },
                    )?
                    .unwrap();
                store
                    .timeline()
                    .add(&inner.id, "me", "sys", "inner note", None, &[])?;
                Err(StoreError::Conflict("inner failed".into()))
            });
            assert!(inner_res.is_err());

            // Outer still sees `outer`, does NOT see `inner`, and counter rolled back so next item is ST-2!
            assert_eq!(store.items().list(WS, false)?.len(), 1);
            let second = store
                .items()
                .create(
                    WS,
                    &NewItem {
                        tags: common::task_tags(&store, WS),
                        bot_id: "steve".into(),
                        title: "second kept".into(),
                        goal: "second".into(),
                        ..Default::default()
                    },
                )?
                .unwrap();
            assert_eq!(second.id, "ST-2");
            Ok(outer.id)
        })
        .unwrap();

    assert_eq!(kept_id, "ST-1");
    let mut ids: Vec<String> = store
        .items()
        .list(WS, false)
        .unwrap()
        .into_iter()
        .map(|i| i.id)
        .collect();
    ids.sort();
    assert_eq!(ids, vec!["ST-1".to_string(), "ST-2".to_string()]);
    assert_eq!(
        *events.lock().unwrap(),
        vec![
            steve_types::BusEvent::Domain(steve_types::DomainEvent::Work(
                steve_types::WorkEvent::Item {
                    id: {
                        let value: String = "ST-1".into();
                        steve_types::ItemId::from(value)
                    },
                    ws: steve_types::WorkspaceId::from(steve_store::WS)
                }
            )),
            steve_types::BusEvent::Domain(steve_types::DomainEvent::Work(
                steve_types::WorkEvent::Item {
                    id: {
                        let value: String = "ST-2".into();
                        steve_types::ItemId::from(value)
                    },
                    ws: steve_types::WorkspaceId::from(steve_store::WS)
                }
            )),
        ]
    );
}

#[test]
fn separate_store_instances_on_same_thread_do_not_cross_talk() {
    let (_dir_a, store_a, events_a) = open_tracked();
    let (_dir_b, store_b, events_b) = open_tracked();

    let res = store_a.atomic(|| -> steve_store::Result<()> {
        store_a.items().create(
            WS,
            &NewItem {
                tags: common::task_tags(&store_a, WS),
                bot_id: "steve".into(),
                title: "in A".into(),
                goal: "A".into(),
                ..Default::default()
            },
        )?;
        // `store_b` is NOT in an atomic scope: its write commits & publishes immediately, and stays in B.
        store_b.items().create(
            WS,
            &NewItem {
                tags: common::task_tags(&store_b, WS),
                bot_id: "steve".into(),
                title: "in B".into(),
                goal: "B".into(),
                ..Default::default()
            },
        )?;
        assert!(events_a.lock().unwrap().is_empty());
        assert_eq!(events_b.lock().unwrap().len(), 1);
        Err(StoreError::Conflict("rollback A only".into()))
    });
    assert!(res.is_err());
    assert!(store_a.items().list(WS, false).unwrap().is_empty());
    assert!(events_a.lock().unwrap().is_empty());
    assert_eq!(store_b.items().list(WS, false).unwrap().len(), 1);
    assert_eq!(events_b.lock().unwrap().len(), 1);
}

#[test]
fn commit_listeners_wake_independently_only_after_outer_commit() {
    use std::future::Future;
    fn ready(wake: &tokio::sync::Notify) -> bool {
        let mut cx = std::task::Context::from_waker(std::task::Waker::noop());
        std::pin::pin!(wake.notified())
            .as_mut()
            .poll(&mut cx)
            .is_ready()
    }
    let (_dir, store, _) = open_tracked();
    let a = store.commit_wake(&["settings"]);
    let b = store.commit_wake(&["settings"]);
    let unrelated = store.commit_wake(&["items"]);
    store
        .atomic(|| -> steve_store::Result<()> {
            store.transaction(|tx| -> steve_store::Result<()> {
            tx.execute("INSERT INTO settings(workspace_id,key,value,updated) VALUES(?,'commit-wake','true',0) ON CONFLICT(workspace_id,key) DO UPDATE SET value='false'", [WS])?;
            Ok(())
        })?;
            assert!(!ready(&a));
            assert!(!ready(&b));
            Ok(())
        })
        .unwrap();
    assert!(ready(&a));
    assert!(ready(&b));
    assert!(!ready(&unrelated));
    let rolled_back: steve_store::Result<()> = store.atomic(|| {
        store.transaction(|tx| -> steve_store::Result<()> {
            tx.execute("INSERT INTO settings(workspace_id,key,value,updated) VALUES(?,'commit-wake','true',0) ON CONFLICT(workspace_id,key) DO UPDATE SET value='false'", [WS])?;
            Ok(())
        })?;
        Err(StoreError::Hook("rollback".into()))
    });
    assert!(rolled_back.is_err());
    assert!(!ready(&a));
    assert!(!ready(&b));
}
