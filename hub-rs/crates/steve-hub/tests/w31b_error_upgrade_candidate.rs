//! Candidate proof only: these SQL files are not the numbered Store migration or Node startup.
//! Replace the candidate includes with the actual upgrade entrypoints when they are assigned.
//! The receiver serialization, inbox identity check and apply watermarks below are real Store APIs.
use rusqlite::{Connection, OptionalExtension, params};
use serde_json::{Value, json};
use steve_node_api::{
    Completion, EventBody, NodeEvent, NodeReason, Observed, OperationResult, Outcome,
};
use steve_store::{NoPublisher, Store, StoreError, runtime as rt};

const OLD: &str = include_str!("fixtures/w31b-error-upgrade/protocol-10-events.json");
const NODE_SQL: &str = include_str!("fixtures/w31b-error-upgrade/node-discard.candidate.sql");
const HUB_SQL: &str = include_str!("fixtures/w31b-error-upgrade/hub-discard.candidate.sql");
const FIXED: &str = r#"{"code":"internal","retry":{"kind":"never"},"reason":"node.error_detail_discarded","params":{},"safe_detail":"error details from before the upgrade were discarded"}"#;

fn key(old: &Value) -> rt::StreamKey<'_> {
    rt::StreamKey {
        node: "n1",
        stream: old["cursor"]["stream"].as_str().unwrap(),
        epoch: "epoch_legacy",
    }
}

fn replica(old: &Value, ack_received: bool) -> Connection {
    let c = Connection::open_in_memory().unwrap();
    c.execute_batch("CREATE TABLE ops(id TEXT PRIMARY KEY,receipt TEXT NOT NULL); CREATE TABLE events(stream TEXT,seq INTEGER,event TEXT); CREATE TABLE streams(stream TEXT PRIMARY KEY,produced INTEGER,acknowledged INTEGER); PRAGMA user_version=4;").unwrap();
    let receipt = &serde_json::from_str::<Vec<Value>>(OLD).unwrap()[0]["body"]["receipt"];
    c.execute(
        "INSERT INTO ops VALUES('op_legacy',?1)",
        [receipt.to_string()],
    )
    .unwrap();
    c.execute("INSERT INTO ops VALUES('op_success',?1)", [r#"{"operation":"op_success","state":{"state":"finished","outcome":"succeeded","error":null,"result":{"kind":"unit"}}}"#]).unwrap();
    c.execute(
        "INSERT INTO streams VALUES(?1,1,?2)",
        params![key(old).stream, i64::from(ack_received)],
    )
    .unwrap();
    // Frozen pre-upgrade states, not a replacement implementation of Node acknowledgement.
    if !ack_received {
        c.execute(
            "INSERT INTO events VALUES(?1,1,?2)",
            params![key(old).stream, old.to_string()],
        )
        .unwrap();
    }
    c
}

fn candidate_node(c: &Connection) {
    let tx = c.unchecked_transaction().unwrap();
    for statement in NODE_SQL.split(';').filter(|s| !s.trim().is_empty()) {
        tx.execute(statement, [FIXED]).unwrap();
    }
    tx.commit().unwrap();
}

fn replay(c: &Connection) -> Option<NodeEvent> {
    let event: Option<String> = c
        .query_row("SELECT event FROM events", [], |r| r.get(0))
        .optional()
        .unwrap();
    event.map(|v| serde_json::from_str(&v).unwrap())
}

fn receiver_text(event: &NodeEvent) -> String {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path().join("hub.sqlite3"), NoPublisher).unwrap();
    let value = serde_json::to_value(event).unwrap();
    // Same to_value -> inbox_put_in -> private to_json path as receive(), not a sorted JSON model.
    store
        .transaction::<_, StoreError>(|tx| {
            let k = rt::StreamKey {
                node: "n1",
                stream: event.cursor.stream.as_str(),
                epoch: event.cursor.epoch.as_str(),
            };
            assert_eq!(
                rt::inbox_insert_in(tx, k, 1, event.id.as_str(), &value, 100)?,
                rt::Inboxed::New
            );
            Ok(())
        })
        .unwrap();
    store
        .reader::<_, StoreError>(|c| {
            Ok(c.query_row("SELECT payload FROM node_event_inbox", [], |r| r.get(0))?)
        })
        .unwrap()
}

fn assert_fixed(completion: &Completion) {
    assert_eq!(completion.outcome, Outcome::Failed);
    assert_eq!(
        serde_json::to_string(completion.error.as_ref().unwrap()).unwrap(),
        FIXED
    );
    assert_eq!(completion.stop_reason, "model_service_unavailable");
    assert!(completion.transient);
    assert!(!completion.acted);
}

fn assert_fixed_event(event: &NodeEvent) {
    match &event.body {
        EventBody::AttemptEnded { completion } => assert_fixed(completion),
        EventBody::OperationChanged { receipt } => {
            let Observed::Finished {
                outcome,
                result,
                error,
            } = &receipt.state
            else {
                panic!("lost terminal receipt")
            };
            assert_eq!(*outcome, Outcome::Failed);
            assert_eq!(
                serde_json::to_string(error.as_ref().unwrap()).unwrap(),
                FIXED
            );
            let Some(OperationResult::Execution(completion)) = result else {
                panic!("lost completion")
            };
            assert_fixed(completion);
        }
        _ => panic!("unexpected fixture event"),
    }
}

fn apply_watermarks(store: &Store, old: &Value) -> usize {
    store
        .transaction::<_, StoreError>(|tx| {
            let pending = rt::next_events_in(tx, key(old), 10)?;
            for p in &pending {
                let event: NodeEvent = serde_json::from_value(p.payload.clone())?;
                assert_fixed_event(&event);
                assert!(rt::mark_applied_in(tx, key(old), p.seq)?);
            }
            Ok(pending.len())
        })
        .unwrap()
}

#[derive(Clone, Copy, Debug)]
enum HubState {
    Pending,
    Applied,
    Pruned,
}

fn window(old: &Value, ack_received: bool, state: HubState) {
    let node = replica(old, ack_received);
    let success: String = node
        .query_row("SELECT receipt FROM ops WHERE id='op_success'", [], |r| {
            r.get(0)
        })
        .unwrap();
    candidate_node(&node);
    assert_eq!(
        node.query_row("SELECT receipt FROM ops WHERE id='op_success'", [], |r| {
            r.get::<_, String>(0)
        })
        .unwrap(),
        success
    );
    let retained = replay(&node);
    assert_eq!(retained.is_none(), ack_received);
    let expected_node = replica(old, false);
    candidate_node(&expected_node);
    let event = replay(&expected_node).unwrap();
    assert_fixed_event(&event);
    let expected_text = receiver_text(&event);
    let dir = tempfile::tempdir().unwrap();
    let hub = Store::open(dir.path().join("hub.sqlite3"), NoPublisher).unwrap();
    hub.transaction::<_, StoreError>(|tx| {
        assert_eq!(
            rt::inbox_insert_in(tx, key(old), 1, old["id"].as_str().unwrap(), old, 100)?,
            rt::Inboxed::New
        );
        if !matches!(state, HubState::Pending) {
            assert!(rt::mark_applied_in(tx, key(old), 1)?);
        }
        if matches!(state, HubState::Pruned) {
            tx.execute("DELETE FROM node_event_inbox", [])?;
        }
        tx.execute(HUB_SQL, [])?;
        Ok(())
    })
    .unwrap();
    let payload: Option<String> = hub
        .reader::<_, StoreError>(|c| {
            Ok(
                c.query_row("SELECT payload FROM node_event_inbox", [], |r| r.get(0))
                    .optional()?,
            )
        })
        .unwrap();
    if matches!(state, HubState::Pruned) {
        assert!(payload.is_none());
    } else {
        assert_eq!(
            payload.as_deref(),
            Some(expected_text.as_str()),
            "{ack_received}/{state:?}"
        );
    }
    if let Some(event) = retained {
        hub.transaction::<_, StoreError>(|tx| {
            assert_eq!(
                rt::inbox_insert_in(
                    tx,
                    key(old),
                    1,
                    event.id.as_str(),
                    &serde_json::to_value(&event)?,
                    101
                )?,
                rt::Inboxed::Seen
            );
            Ok(())
        })
        .unwrap();
    }
    assert_eq!(
        apply_watermarks(&hub, old),
        usize::from(matches!(state, HubState::Pending))
    );
    assert_eq!(apply_watermarks(&hub, old), 0);
}

#[test]
fn candidate_error_payloads_match_real_receiver_text_for_every_ack_window() {
    let expected =
        NodeReason::error_detail_discarded("error details from before the upgrade were discarded");
    assert_eq!(serde_json::to_string(&expected).unwrap(), FIXED);
    for old in serde_json::from_str::<Vec<Value>>(OLD).unwrap() {
        for ack in [false, true] {
            for state in [HubState::Pending, HubState::Applied, HubState::Pruned] {
                window(&old, ack, state);
            }
        }
    }
}

#[test]
fn candidate_error_replacement_does_not_relax_the_position_conflict() {
    let old = serde_json::from_str::<Vec<Value>>(OLD).unwrap().remove(1);
    let node = replica(&old, false);
    candidate_node(&node);
    let event = replay(&node).unwrap();
    let value = serde_json::to_value(&event).unwrap();
    let dir = tempfile::tempdir().unwrap();
    let hub = Store::open(dir.path().join("hub.sqlite3"), NoPublisher).unwrap();
    hub.transaction::<_, StoreError>(|tx| {
        assert_eq!(
            rt::inbox_insert_in(tx, key(&old), 1, event.id.as_str(), &value, 100)?,
            rt::Inboxed::New
        );
        tx.execute(HUB_SQL, [])?;
        let mut different = value.clone();
        different["body"]["completion"]["error"]["safe_detail"] = json!("a different payload");
        assert_eq!(
            rt::inbox_insert_in(tx, key(&old), 1, event.id.as_str(), &different, 101)?,
            rt::Inboxed::Conflict(rt::Conflict::Position)
        );
        Ok(())
    })
    .unwrap();
}
