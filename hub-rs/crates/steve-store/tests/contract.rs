//! The storage contract: what a write emits goes out after its commit, in order, and nothing of a
//! write or a scope that rolled back stays or is published.

use std::sync::{Arc, Mutex};

use serde_json::json;
use steve_store::{
    BusEvent, FnPublisher, Ingest, NewSession, Store, StoreError, WS, stats_changed,
};

struct Fixture {
    _dir: tempfile::TempDir,
    store: Store,
    seen: Arc<Mutex<Vec<BusEvent>>>,
    session: String,
}

fn open() -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let seen = Arc::new(Mutex::new(Vec::new()));
    let sink = seen.clone();
    let store = Store::open(
        dir.path().join("steve.sqlite3"),
        FnPublisher(move |e| sink.lock().unwrap().push(e)),
    )
    .unwrap();
    store.ensure_workspace().unwrap();
    let session = store
        .sessions()
        .create(&NewSession {
            workspace_id: WS.into(),
            harness: "fixture".into(),
            cwd: "/tmp".into(),
            ..Default::default()
        })
        .unwrap()
        .id;
    seen.lock().unwrap().clear();
    Fixture {
        _dir: dir,
        store,
        seen,
        session,
    }
}

/// A marker event, told apart by its name.
fn mark(name: &str) -> BusEvent {
    stats_changed(name, None).into()
}

fn marks(seen: &Mutex<Vec<BusEvent>>) -> Vec<BusEvent> {
    seen.lock()
        .unwrap()
        .iter()
        .filter(|e| matches!(e, BusEvent::Domain(_)))
        .cloned()
        .collect()
}

fn texts(f: &Fixture) -> Vec<String> {
    f.store
        .events()
        .after(&f.session, 0, 100)
        .unwrap()
        .into_iter()
        .map(|e| e.data["text"].as_str().unwrap_or_default().to_owned())
        .collect()
}

#[test]
fn what_a_write_emits_goes_out_after_its_commit_and_never_after_its_rollback() {
    let f = open();
    let s = f.session.as_str();
    f.store
        .write(|w| {
            w.emit(stats_changed("a", None));
            w.events()
                .add(s, "text", json!({"text": "kept"}), None, 1)?;
            w.emit(stats_changed("b", None));
            assert!(
                f.seen.lock().unwrap().is_empty(),
                "nothing before the commit"
            );
            Ok::<_, StoreError>(())
        })
        .unwrap();
    assert_eq!(marks(&f.seen), vec![mark("a"), mark("b")]);
    assert_eq!(f.seen.lock().unwrap().len(), 3, "and the session event");

    f.seen.lock().unwrap().clear();
    let out: Result<(), StoreError> = f.store.write(|w| {
        w.emit(stats_changed("c", None));
        w.events()
            .add(s, "text", json!({"text": "gone"}), None, 2)?;
        Err(StoreError::Conflict("no".into()))
    });
    assert!(matches!(out, Err(StoreError::Conflict(_))));
    assert!(f.seen.lock().unwrap().is_empty());
    assert_eq!(texts(&f), vec!["kept"]);
}

#[test]
fn a_failed_scope_rolls_back_alone_with_what_it_emitted() {
    let f = open();
    let s = f.session.as_str();
    f.store
        .write(|w| {
            w.emit(stats_changed("a", None));
            let failed: Result<(), StoreError> = w.scope(|w| {
                w.events()
                    .add(s, "text", json!({"text": "gone"}), None, 1)?;
                w.emit(stats_changed("b", None));
                // what a domain not on the contract yet publishes in the scope goes with it too
                w.legacy().publish(stats_changed("b2", None));
                Err(StoreError::Conflict("no".into()))
            });
            assert!(failed.is_err());
            w.scope(|w| {
                w.events()
                    .add(s, "text", json!({"text": "kept"}), None, 2)?;
                w.emit(stats_changed("c", None));
                Ok::<_, StoreError>(())
            })?;
            w.emit(stats_changed("d", None));
            Ok::<_, StoreError>(())
        })
        .unwrap();
    assert_eq!(marks(&f.seen), vec![mark("a"), mark("c"), mark("d")]);
    assert_eq!(texts(&f), vec!["kept"]);
}

#[test]
fn a_statement_the_database_refuses_is_rejected_and_fails_only_its_scope() {
    let f = open();
    let s = f.session.as_str();
    let rejected = f
        .store
        .write(|w| {
            let e = w
                .scope(|w| {
                    w.events()
                        .add("no-such-session", "text", json!({}), None, 1)
                })
                .unwrap_err();
            w.events()
                .add(s, "text", json!({"text": "kept"}), None, 2)?;
            Ok::<_, StoreError>(e)
        })
        .unwrap();
    assert!(matches!(rejected, StoreError::Rejected(_)), "{rejected:?}");
    assert!(!rejected.is_storage_failure());
    assert_eq!(texts(&f), vec!["kept"]);
}

#[test]
fn a_write_inside_a_legacy_transaction_is_a_scope_of_it() {
    let f = open();
    let s = f.session.as_str();
    let out: Result<(), StoreError> = f.store.transaction(|tx| {
        tx.write::<_, StoreError>(|w| {
            w.events()
                .add(s, "text", json!({"text": "gone"}), None, 1)?;
            w.emit(stats_changed("a", None));
            Ok(())
        })?;
        assert!(
            f.seen.lock().unwrap().is_empty(),
            "held for the outer commit"
        );
        Err(StoreError::Conflict("the outer write fails".into()))
    });
    assert!(out.is_err());
    assert!(f.seen.lock().unwrap().is_empty());
    assert!(texts(&f).is_empty());

    f.store
        .transaction(|tx| {
            tx.write::<_, StoreError>(|w| {
                w.emit(stats_changed("b", None));
                Ok(())
            })
        })
        .unwrap();
    assert_eq!(marks(&f.seen), vec![mark("b")]);
}

#[test]
fn a_node_event_is_stored_once_by_its_stream_position() {
    let f = open();
    let s = f.session.as_str();
    let src = steve_store::EventSource {
        node: "n1",
        stream: "st",
        epoch: "e1",
        seq: 1,
        attempt: Some("a1"),
    };
    let added = |text: &str| {
        f.store
            .write(|w| {
                w.events()
                    .add(s, "text", json!({"text": text}), Some(src), 1)
            })
            .unwrap()
    };
    assert!(added("first").is_some());
    assert_eq!(added("again"), None);
    assert_eq!(texts(&f), vec!["first"]);
}
