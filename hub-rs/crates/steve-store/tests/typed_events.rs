//! Exercise real producers with independent workspaces, including rows whose scope is lost on delete.
use std::sync::{Arc, Mutex};

use serde_json::{Value, json};
use steve_store::*;
use steve_types::{Scope, Scoped};

#[derive(Clone, Default)]
struct Sink(Arc<Mutex<Vec<Vec<BusEvent>>>>);

impl Publisher for Sink {
    fn publish(&self, _: BusEvent) {
        panic!("store producer used legacy publication");
    }

    fn publish_batch(&self, events: Vec<BusEvent>) {
        if !events.is_empty() {
            self.0.lock().unwrap().push(events);
        }
    }
}

impl Sink {
    fn take(&self) -> Vec<Vec<BusEvent>> {
        std::mem::take(&mut *self.0.lock().unwrap())
    }

    fn scoped(&self, ws: &str) -> Vec<Value> {
        let events: Vec<_> = self.take().into_iter().flatten().collect();
        assert!(!events.is_empty(), "expected producer events");
        events
            .into_iter()
            .map(|event| {
                let (scope, wire) = match event {
                    BusEvent::Domain(event) => {
                        (event.scope(), serde_json::to_value(event).unwrap())
                    }
                    BusEvent::Live(event) => (event.scope(), serde_json::to_value(event).unwrap()),
                };
                assert_eq!(scope, Scope::Ws(ws.into()));
                assert_eq!(wire["ws"], ws);
                wire
            })
            .collect()
    }
}

fn fixture() -> (tempfile::TempDir, Store, Sink, Vec<String>) {
    let dir = tempfile::tempdir().unwrap();
    let sink = Sink::default();
    let store = Store::open(dir.path().join("store.sqlite3"), sink.clone()).unwrap();
    store.ensure_workspace().unwrap();
    sink.take();
    let user = store
        .users()
        .create("test-owner", "Owner", None, "owner")
        .unwrap();
    let mut workspaces = vec![];
    for prefix in ["A", "B"] {
        let ws = store
            .workspaces()
            .create(
                &NewWorkspace {
                    name: prefix.into(),
                    item_prefix: Some(prefix.into()),
                    ..Default::default()
                },
                &user,
            )
            .unwrap();
        let events = sink.scoped(&ws.id);
        assert!(events.iter().any(|event| event["type"] == "workspace"));
        assert!(events.iter().any(|event| event["type"] == "tag"));
        workspaces.push(ws.id);
    }
    (dir, store, sink, workspaces)
}

#[test]
fn real_work_producers_keep_scope_through_mutation_and_deletion() {
    let (_dir, s, sink, workspaces) = fixture();
    for ws in workspaces {
        let bot = s
            .bots()
            .create(
                &ws,
                &NewBot {
                    name: "Worker".into(),
                    harness: "claude-code".into(),
                    ..Default::default()
                },
                "owner",
            )
            .unwrap()
            .unwrap();
        let item = s
            .items()
            .create(
                &ws,
                &NewItem {
                    title: "Work".into(),
                    goal: "Result".into(),
                    bot_id: bot.id.clone(),
                    ..Default::default()
                },
            )
            .unwrap()
            .unwrap();
        let session = s
            .sessions()
            .create(&NewSession {
                workspace_id: ws.clone(),
                item_id: Some(item.id.clone()),
                bot_id: Some(bot.id),
                harness: "claude-code".into(),
                cwd: "/tmp".into(),
                title: "Session".into(),
                token: uid("token"),
                model: None,
                reasoning_effort: None,
            })
            .unwrap();
        let decision = s
            .decisions()
            .create(&NewDecision {
                workspace_id: ws.clone(),
                item_id: Some(item.id.clone()),
                kind: "ask".into(),
                question: "Proceed?".into(),
                ..Default::default()
            })
            .unwrap()
            .unwrap();
        let message = s
            .messages()
            .add(&ws, "me", "hello", None, &Value::Null, "steve")
            .unwrap();
        sink.scoped(&ws);

        s.transaction(|tx| -> Result<()> {
            item_patch_in(
                tx,
                &item.id,
                &ItemPatch {
                    title: Some("Revised".into()),
                    ..Default::default()
                },
            )?;
            decision_resolve_in(tx, &decision.id, &json!("yes"), "resolved")?;
            s.messages()
                .patch_text_meta(&message, "revised", &json!({"done": true}))?;
            s.queued().add(&session.id, "next", "me", &[])?;
            Ok(())
        })
        .unwrap();
        let batches = sink.0.lock().unwrap();
        assert_eq!(
            batches.len(),
            1,
            "nested producer writes form one committed batch"
        );
        drop(batches);
        let events = sink.scoped(&ws);
        for ty in ["item", "decision", "message", "session"] {
            assert!(
                events.iter().any(|event| event["type"] == ty),
                "missing {ty}"
            );
        }

        s.events()
            .add(&session.id, "user", json!({"text": "hello"}))
            .unwrap();
        let event = sink.scoped(&ws).pop().unwrap();
        assert_eq!(event["type"], "event");
        assert_eq!(event["session_id"], session.id);

        s.sessions().remove(&session.id).unwrap();
        let event = sink.scoped(&ws).pop().unwrap();
        assert_eq!(event["type"], "session");
        assert!(
            event.get("item_id").is_none(),
            "preserve absent item_id on removal"
        );
        s.items().remove(&item.id).unwrap();
        assert_eq!(sink.scoped(&ws).last().unwrap()["id"], item.id);
        s.items().remove(&item.id).unwrap();
        s.sessions().remove(&session.id).unwrap();
        s.decisions().reopen("missing").unwrap();
        assert!(
            sink.take().is_empty(),
            "missing rows have no workspace to publish"
        );
    }
}

#[test]
fn memory_events_use_the_memory_workspace_even_when_bot_belongs_elsewhere() {
    let (_dir, s, sink, ws) = fixture();
    let bot = s
        .bots()
        .create(
            &ws[0],
            &NewBot {
                name: "Shared".into(),
                harness: "claude-code".into(),
                ..Default::default()
            },
            "owner",
        )
        .unwrap()
        .unwrap();
    sink.scoped(&ws[0]);
    let memory = s.memories().add(&bot.id, &ws[1], "Remember", "me").unwrap();
    s.memories().edit(&memory, "Updated").unwrap();
    s.memories().remove(&memory).unwrap();
    let events = sink.scoped(&ws[1]);
    assert_eq!(events.len(), 3);
    assert!(
        events
            .iter()
            .all(|event| event["type"] == "bot" && event["id"] == bot.id)
    );
    s.memories().clear(&ws[1]).unwrap();
    assert_eq!(sink.scoped(&ws[1])[0]["id"], "*");
}

#[test]
fn annotations_tags_and_rollback_preserve_the_changed_rows_workspace() {
    let (_dir, s, sink, workspaces) = fixture();
    for ws in workspaces {
        let bot = s
            .bots()
            .create(
                &ws,
                &NewBot {
                    name: "Reviewer".into(),
                    harness: "claude-code".into(),
                    ..Default::default()
                },
                "owner",
            )
            .unwrap()
            .unwrap();
        let item = s
            .items()
            .create(
                &ws,
                &NewItem {
                    title: "Notes".into(),
                    goal: "Review".into(),
                    bot_id: bot.id,
                    ..Default::default()
                },
            )
            .unwrap()
            .unwrap();
        s.artifacts()
            .add_as(&item.id, "result", "text", None, Some("result"))
            .unwrap();
        let artifact = s.artifacts().list(&item.id).unwrap()[0].id.clone();
        sink.scoped(&ws);
        let annotation = s
            .annotations()
            .add(&item.id, &artifact, "", &json!({"type":"file"}), "note")
            .unwrap();
        s.annotations()
            .edit(&item.id, &annotation.id, "edit")
            .unwrap();
        s.annotations()
            .claim(&item.id, &[annotation.id.clone()], "send")
            .unwrap();
        s.annotations()
            .settle(&item.id, "send", false, None)
            .unwrap();
        s.annotations().remove(&item.id, &annotation.id).unwrap();
        assert_eq!(sink.scoped(&ws).len(), 5);
        let tag = s
            .tags()
            .create(
                &ws,
                &NewTag {
                    name: "custom".into(),
                    ..Default::default()
                },
                "me",
            )
            .unwrap();
        sink.scoped(&ws);
        s.tags().remove(&tag.id).unwrap();
        assert_eq!(sink.scoped(&ws)[0]["id"], tag.id);
        let result: Result<()> = s.atomic(|| {
            s.items().set_design_gate(&item.id, true)?;
            s.messages()
                .add(&ws, "me", "rolled back", None, &Value::Null, "steve")?;
            Err(StoreError::Conflict("rollback".into()))
        });
        assert!(result.is_err());
        assert!(sink.take().is_empty());
        assert!(!s.items().get(&item.id).unwrap().unwrap().design_gate);
    }
}
