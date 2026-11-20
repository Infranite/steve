//! Producer regression: nudge has no workspace argument; scope must come from the written row.
#[path = "../../steve-store/tests/support/digest_audit.rs"]
mod digest_audit;
use std::sync::{Arc, Mutex};
use steve_channels::decide;
use steve_modules::ApiError;
use steve_store::{BusEvent, NewDecision, Publisher, Store};
use steve_types::{DomainEvent, Scope, Scoped, WorkEvent};

#[derive(Clone, Default)]
struct Capture(Arc<Mutex<Vec<BusEvent>>>);
impl Publisher for Capture {
    fn publish(&self, event: BusEvent) {
        self.0.lock().unwrap().push(event);
    }
    fn publish_batch(&self, events: Vec<BusEvent>) {
        self.0.lock().unwrap().extend(events);
    }
}

#[test]
fn nudge_uses_each_decisions_workspace_and_discards_rolled_back_events() {
    let dir = tempfile::tempdir().unwrap();
    let capture = Capture::default();
    let store = Store::open(dir.path().join("steve.sqlite3"), capture.clone()).unwrap();
    let _audit = digest_audit::Audit(store.clone());
    store.ensure_workspace().unwrap();
    store.transaction(|tx| {
        tx.execute("INSERT INTO workspaces (id, name, created) VALUES ('scope-a', 'A', 1), ('scope-b', 'B', 1)", [])?;
        Ok::<_, steve_store::StoreError>(())
    }).unwrap();
    let mut decisions = Vec::new();
    for ws in ["scope-a", "scope-b"] {
        let decision = store
            .decisions()
            .create(&NewDecision {
                workspace_id: ws.into(),
                kind: "ask".into(),
                subject_type: "channel_event".into(),
                subject_id: ws.into(),
                question: "question".into(),
                requested_by: "steve".into(),
                ..Default::default()
            })
            .unwrap()
            .unwrap();
        decisions.push((decision.id, ws));
    }
    capture.0.lock().unwrap().clear();
    for (id, _) in &decisions {
        assert!(
            store
                .transaction(|tx| decide::nudge(tx, id, "another message"))
                .unwrap()
        );
    }
    let events = capture.0.lock().unwrap().clone();
    assert_eq!(events.len(), 2);
    for (event, (id, ws)) in events.iter().zip(&decisions) {
        let BusEvent::Domain(event) = event else {
            panic!("nudge must publish a typed domain event")
        };
        assert_eq!(event.scope(), Scope::Ws((*ws).into()));
        assert!(
            matches!(event, DomainEvent::Work(WorkEvent::Decision { id: actual, item_id: Some(None), ws: actual_ws }) if actual.as_str() == id.as_str() && actual_ws.as_str() == *ws)
        );
    }
    assert!(
        !store
            .transaction(|tx| decide::nudge(tx, "absent", "no row"))
            .unwrap()
    );
    let result: Result<(), ApiError> = store.transaction(|tx| {
        decide::nudge(tx, &decisions[0].0, "uncommitted")?;
        Err(ApiError::invalid("test rollback"))
    });
    assert!(result.is_err());
    assert_eq!(capture.0.lock().unwrap().len(), 2);
    assert!(
        !store
            .decisions()
            .get(&decisions[0].0)
            .unwrap()
            .unwrap()
            .question
            .contains("uncommitted")
    );
}
