mod common;

use std::time::{Duration, Instant};

use rusqlite::params;
use serde_json::{Value, json};
use steve_store::diagnostics::{Details, Filter, MAX_ITEM_BYTES, Record, RecordStatus};
use steve_store::{NewBot, NewDecision, NewItem, NewSession, NoPublisher, Store, WS};

fn setup() -> (common::T, String, String) {
    let t = common::fresh();
    t.store
        .bots()
        .seed(
            "worker",
            false,
            &NewBot {
                name: "Worker".into(),
                harness: "fixture".into(),
                ..Default::default()
            },
            WS,
        )
        .unwrap();
    let item = t
        .store
        .items()
        .create(
            WS,
            &NewItem {
                title: "Diagnostic fixture".into(),
                goal: "Observe a failure".into(),
                bot_id: "worker".into(),
                ..Default::default()
            },
        )
        .unwrap()
        .unwrap();
    let session = t
        .store
        .sessions()
        .create(&NewSession {
            workspace_id: WS.into(),
            item_id: Some(item.id.clone()),
            bot_id: Some("worker".into()),
            harness: "fixture".into(),
            cwd: t.dir.path().display().to_string(),
            ..Default::default()
        })
        .unwrap();
    (t, item.id, session.id)
}

fn record(item: &str, session: &str, kind: &str, detail: bool) -> Record {
    Record {
        workspace_id: WS.into(),
        item_id: Some(item.into()),
        session_id: Some(session.into()),
        kind: kind.into(),
        reason_code: "fixture_condition".into(),
        detail,
        ..Default::default()
    }
}

fn settled(store: &Store, item: &str) -> Value {
    let end = Instant::now() + Duration::from_secs(5);
    loop {
        let view = store.diagnostics().explain(WS, item).unwrap();
        if view["coverage"]["instance_pending"] == 0 {
            return view;
        }
        assert!(
            Instant::now() < end,
            "diagnostic worker did not drain: {view}"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn events(store: &Store, item: &str) -> Value {
    settled(store, item);
    store
        .diagnostics()
        .events(WS, item, Filter::default())
        .unwrap()
}

#[test]
fn critical_facts_survive_reopen_and_details_obey_the_capture_window() {
    let (t, item, session) = setup();
    let s = &t.store;
    s.diagnostics()
        .capture(WS, &item, "owner", Some(0))
        .unwrap();
    assert_eq!(
        s.diagnostics()
            .record(record(&item, &session, "turn.ended", false)),
        RecordStatus::Queued
    );
    assert_eq!(
        s.diagnostics()
            .record(record(&item, &session, "dependency.checked", true)),
        RecordStatus::Skipped
    );
    assert_eq!(events(s, &item)["events"].as_array().unwrap().len(), 1);
    let capture = s.diagnostics().capture(WS, &item, "owner", None).unwrap();
    assert_eq!(capture["capture"]["active"], true);
    assert_eq!(
        capture["capture"]["expires_at"].as_i64().unwrap()
            - capture["capture"]["started_at"].as_i64().unwrap(),
        30 * 60_000
    );
    assert!(
        s.diagnostics()
            .capture(WS, &item, "owner", Some(61))
            .is_err()
    );
    s.diagnostics()
        .record(record(&item, &session, "dependency.checked", true));
    assert_eq!(events(s, &item)["events"].as_array().unwrap().len(), 2);
    s.with_conn(|c| {
        Ok(c.execute(
            "UPDATE diagnostic_captures SET expires_at=? WHERE item_id=?",
            params![steve_store::now() - 1, item],
        )?)
    })
    .unwrap();
    s.diagnostics()
        .record(record(&item, &session, "dependency.checked", true));
    let expired = settled(s, &item);
    assert_eq!(expired["capture"]["active"], false);
    assert_eq!(expired["capture"]["stop_reason"], "expired");
    let reopened = Store::open(s.path(), NoPublisher).unwrap();
    assert_eq!(
        events(&reopened, &item)["events"].as_array().unwrap().len(),
        2
    );
    assert_eq!(
        reopened.diagnostics().explain(WS, &item).unwrap()["capture"]["active"],
        false
    );
    s.diagnostics()
        .capture(WS, &item, "owner", Some(1))
        .unwrap();
    assert_eq!(
        s.diagnostics()
            .capture(WS, &item, "owner", Some(0))
            .unwrap()["capture"]["stop_reason"],
        "manual"
    );
}

#[test]
fn new_items_capture_immediately_without_reads_or_owner_activation() {
    let (t, item, session) = setup();
    let s = &t.store;
    // Recording before the first debug read proves creation also activates the recorder cache.
    assert_eq!(
        s.diagnostics()
            .record(record(&item, &session, "dependency.checked", true)),
        RecordStatus::Queued
    );
    assert_eq!(events(s, &item)["events"].as_array().unwrap().len(), 1);
    let created = s.items().get(&item).unwrap().unwrap().created;
    let capture = s.diagnostics().explain(WS, &item).unwrap()["capture"].clone();
    assert_eq!(capture["active"], true);
    assert_eq!(capture["started_at"], created);
    assert_eq!(capture["expires_at"], created + 30 * 60_000);
    let actor: String = s
        .reader(|c| {
            Ok::<_, steve_store::StoreError>(c.query_row(
                "SELECT actor FROM diagnostic_captures WHERE item_id=?",
                [&item],
                |r| r.get(0),
            )?)
        })
        .unwrap();
    assert_eq!(actor, "system:item-created");
    let reopened = Store::open(s.path(), NoPublisher).unwrap();
    assert_eq!(
        reopened.diagnostics().explain(WS, &item).unwrap()["capture"],
        capture
    );
    let chat = reopened
        .items()
        .create(
            WS,
            &NewItem {
                kind: Some("chat".into()),
                title: "New conversation".into(),
                bot_id: "worker".into(),
                ..Default::default()
            },
        )
        .unwrap()
        .unwrap();
    let chat_capture = reopened.diagnostics().explain(WS, &chat.id).unwrap();
    assert_eq!(chat_capture["capture"]["active"], true);
    assert_eq!(chat_capture["capture"]["started_at"], chat.created);
}

#[test]
fn default_capture_does_not_restart_after_expiry_manual_stop_or_legacy_reads() {
    let (t, item, _) = setup();
    let s = &t.store;
    let original = s.diagnostics().explain(WS, &item).unwrap()["capture"].clone();
    s.diagnostics()
        .capture(WS, &item, "owner", Some(0))
        .unwrap();
    let reopened = Store::open(s.path(), NoPublisher).unwrap();
    reopened
        .items()
        .patch(
            &item,
            &steve_store::ItemPatch {
                stage: Some("执行".into()),
                ..Default::default()
            },
        )
        .unwrap();
    for _ in 0..2 {
        let stopped = reopened.diagnostics().explain(WS, &item).unwrap();
        assert_eq!(stopped["capture"]["active"], false);
        assert_eq!(stopped["capture"]["stop_reason"], "manual");
        assert_eq!(stopped["capture"]["expires_at"], original["expires_at"]);
    }
    s.with_conn(|c| {
        c.execute("UPDATE diagnostic_captures SET stop_reason=NULL,stopped_at=NULL,expires_at=? WHERE item_id=?",
            params![steve_store::now() - 1, item])?;
        Ok(())
    }).unwrap();
    let expired_store = Store::open(s.path(), NoPublisher).unwrap();
    for _ in 0..2 {
        let expired = expired_store.diagnostics().explain(WS, &item).unwrap();
        assert_eq!(expired["capture"]["active"], false);
        assert_eq!(expired["capture"]["stop_reason"], "expired");
    }
    // A pre-upgrade item has no capture row; neither opening the Store nor reading creates one.
    s.with_conn(|c| {
        c.execute("DELETE FROM diagnostic_captures WHERE item_id=?", [&item])?;
        Ok(())
    })
    .unwrap();
    let legacy = Store::open(s.path(), NoPublisher).unwrap();
    let view = legacy.diagnostics().explain(WS, &item).unwrap();
    assert_eq!(view["capture"]["active"], false);
    assert!(view["capture"]["started_at"].is_null());
    assert!(view["capture"]["expires_at"].is_null());
}

#[test]
fn stable_cursor_pages_do_not_skip_same_time_events_or_absorb_new_events() {
    let (t, item, session) = setup();
    let s = &t.store;
    for kind in ["turn.ended", "decision.created", "turn.ended"] {
        s.diagnostics().record(record(&item, &session, kind, false));
    }
    settled(s, &item);
    let at = steve_store::now() - 100;
    s.with_conn(|c| {
        Ok(c.execute(
            "UPDATE diagnostic_events SET at=? WHERE item_id=?",
            params![at, item],
        )?)
    })
    .unwrap();
    let filter = Filter {
        limit: 1,
        kinds: vec!["turn.ended".into()],
        since: Some(at),
        until: Some(at),
        session_id: Some(session.clone()),
        ..Default::default()
    };
    let first = s.diagnostics().events(WS, &item, filter.clone()).unwrap();
    assert_eq!(first["events"].as_array().unwrap().len(), 1);
    assert_eq!(first["has_more"], true);
    s.diagnostics()
        .record(record(&item, &session, "turn.ended", false));
    settled(s, &item);
    s.with_conn(|c| {
        Ok(c.execute(
            "UPDATE diagnostic_events SET at=? WHERE item_id=?",
            params![at, item],
        )?)
    })
    .unwrap();
    let second = s
        .diagnostics()
        .events(
            WS,
            &item,
            Filter {
                after: first["next_cursor"].as_i64(),
                through: first["snapshot_cursor"].as_i64(),
                ..filter
            },
        )
        .unwrap();
    assert_eq!(second["events"].as_array().unwrap().len(), 1);
    assert_eq!(second["has_more"], false);
    assert_ne!(
        first["events"][0]["event_id"],
        second["events"][0]["event_id"]
    );
    let id = second["events"][0]["event_id"].as_str().unwrap();
    let exact = s
        .diagnostics()
        .events(
            WS,
            &item,
            Filter {
                event_id: Some(id.into()),
                ..Default::default()
            },
        )
        .unwrap();
    assert_eq!(exact["events"].as_array().unwrap().len(), 1);
    assert_eq!(exact["events"][0]["event_id"], id);
    assert_eq!(events(s, &item)["events"].as_array().unwrap().len(), 4);
}

#[test]
fn a_full_capture_stops_instead_of_continuously_evicting_and_recording_details() {
    let (t, item, session) = setup();
    let s = &t.store;
    s.diagnostics().capture(WS, &item, "owner", None).unwrap();
    s.diagnostics()
        .record(record(&item, &session, "dependency.checked", true));
    settled(s, &item);
    // A full retained budget is a resource state; no large fixture or thousands of inserts needed.
    s.with_conn(|c| {
        c.execute(
            "UPDATE diagnostic_events SET bytes=? WHERE item_id=?",
            params![MAX_ITEM_BYTES, item],
        )?;
        c.execute(
            "UPDATE diagnostic_item_stats SET bytes=? WHERE item_id=?",
            params![MAX_ITEM_BYTES, item],
        )?;
        Ok(())
    })
    .unwrap();
    s.diagnostics()
        .record(record(&item, &session, "dependency.checked", true));
    let full = settled(s, &item);
    assert_eq!(full["capture"]["active"], false);
    assert_eq!(full["capture"]["stop_reason"], "item_byte_limit");
    assert_eq!(full["capture"]["dropped"], 1);
    assert_eq!(
        s.diagnostics()
            .record(record(&item, &session, "dependency.checked", true)),
        RecordStatus::Skipped
    );
    assert_eq!(events(s, &item)["events"].as_array().unwrap().len(), 1);
}

#[test]
fn a_manual_stop_preserves_observations_already_queued_before_its_cutoff() {
    let (t, item, session) = setup();
    let s = &t.store;
    s.diagnostics().capture(WS, &item, "owner", None).unwrap();
    // Hold the writer to deterministically place manual stop between enqueue and consumption.
    s.with_conn(|c| {
        assert_eq!(
            s.diagnostics()
                .record(record(&item, &session, "dependency.checked", true)),
            RecordStatus::Queued
        );
        c.execute(
            "UPDATE diagnostic_captures SET stop_reason='manual',stopped_at=? WHERE item_id=?",
            params![steve_store::now(), item],
        )?;
        Ok(())
    })
    .unwrap();
    let page = events(s, &item);
    assert_eq!(page["events"].as_array().unwrap().len(), 1);
    assert_eq!(
        s.diagnostics().explain(WS, &item).unwrap()["capture"]["active"],
        false
    );
    assert_eq!(
        s.diagnostics()
            .record(record(&item, &session, "dependency.checked", true)),
        RecordStatus::Skipped
    );
}

#[test]
fn retention_reports_a_real_gap_and_does_not_delete_business_history() {
    let (t, item, session) = setup();
    let s = &t.store;
    s.events()
        .add(&session, "system", json!({"text":"business history"}))
        .unwrap();
    s.diagnostics()
        .record(record(&item, &session, "turn.ended", false));
    let first = events(s, &item);
    let cursor = first["events"][0]["cursor"].as_i64().unwrap();
    s.with_conn(|c| {
        Ok(c.execute(
            "UPDATE diagnostic_events SET at=? WHERE item_id=?",
            params![steve_store::now() - 8 * 86_400_000, item],
        )?)
    })
    .unwrap();
    s.diagnostics()
        .record(record(&item, &session, "turn.ended", false));
    let page = events(s, &item);
    assert_eq!(page["events"].as_array().unwrap().len(), 1);
    assert_eq!(page["truncated"], true);
    assert_eq!(page["coverage"]["removed_through"], cursor);
    assert_eq!(s.events().after(&session, 0, 100).unwrap().len(), 1);
}

#[test]
fn recording_under_the_writer_lock_never_reenters_it_and_scope_is_rechecked() {
    let (t, item, session) = setup();
    let s = &t.store;
    s.with_conn(|_| {
        let mut event = record(&item, &session, "turn.ended", false);
        event.workspace_id.clear();
        event.item_id = None;
        assert_eq!(s.diagnostics().record(event), RecordStatus::Queued);
        Ok(())
    })
    .unwrap();
    let initial = events(s, &item);
    assert_eq!(initial["events"].as_array().unwrap().len(), 1);
    let chat = s
        .sessions()
        .create(&NewSession {
            workspace_id: WS.into(),
            bot_id: Some("worker".into()),
            harness: "fixture".into(),
            cwd: t.dir.path().display().to_string(),
            ..Default::default()
        })
        .unwrap();
    let mut chat_observation = record(&item, &chat.id, "turn.ended", false);
    chat_observation.item_id = None;
    assert_eq!(
        s.diagnostics().record(chat_observation.clone()),
        RecordStatus::Queued
    );
    let after_chat = events(s, &item);
    assert_eq!(after_chat["events"], initial["events"]);
    assert_eq!(
        after_chat["coverage"]["instance_dropped"],
        initial["coverage"]["instance_dropped"]
    );
    // A real chat session does not excuse false item or workspace ownership claims.
    chat_observation.item_id = Some(item.clone());
    s.diagnostics().record(chat_observation.clone());
    chat_observation.item_id = None;
    chat_observation.workspace_id = "other".into();
    s.diagnostics().record(chat_observation);
    assert_eq!(
        events(s, &item)["coverage"]["instance_dropped"],
        initial["coverage"]["instance_dropped"].as_u64().unwrap() + 2
    );
    assert!(s.diagnostics().explain("other", &item).is_err());
    assert!(
        s.diagnostics()
            .events("other", &item, Filter::default())
            .is_err()
    );
    assert!(
        s.diagnostics()
            .capture("other", &item, "owner", None)
            .is_err()
    );
    let mut wrong = record(&item, &session, "turn.ended", false);
    wrong.workspace_id = "other".into();
    s.diagnostics().record(wrong);
    let mut unsafe_code = record(&item, &session, "turn.ended", false);
    unsafe_code.details = Details {
        code: Some("private secret body".into()),
        ..Default::default()
    };
    assert_eq!(s.diagnostics().record(unsafe_code), RecordStatus::Invalid);
    for (source_node, source_epoch) in [
        (Some("private node body".into()), None),
        (None, Some("private epoch body".into())),
    ] {
        let mut unsafe_source = record(&item, &session, "turn.ended", false);
        unsafe_source.details.source_node = source_node;
        unsafe_source.details.source_epoch = source_epoch;
        assert_eq!(s.diagnostics().record(unsafe_source), RecordStatus::Invalid);
    }
    let page = events(s, &item);
    assert_eq!(page["events"].as_array().unwrap().len(), 1);
    assert!(!page.to_string().contains("private secret body"));
    assert!(!page.to_string().contains("private node body"));
    assert!(!page.to_string().contains("private epoch body"));
    assert!(page["coverage"]["instance_dropped"].as_u64().unwrap() >= 4);
    assert_eq!(page["coverage"]["delivery"], "best_effort");
    assert_eq!(page["coverage"]["unobserved_gaps"], "unknown");
}

#[test]
fn card_transitions_record_only_committed_changes_without_copying_card_content() {
    let (t, item, session) = setup();
    let s = &t.store;
    let spec = NewDecision {
        workspace_id: WS.into(),
        item_id: Some(item.clone()),
        session_id: Some(session),
        kind: "ask".into(),
        subject_type: "question".into(),
        subject_id: item.clone(),
        question: "private question body".into(),
        ..Default::default()
    };
    let d = s.decisions().create(&spec).unwrap().unwrap();
    s.decisions()
        .resolve(
            &d.id,
            &json!({"answer":"private answer body"}),
            "resolved",
            &Value::Null,
        )
        .unwrap();
    s.decisions()
        .resolve(&d.id, &json!({}), "resolved", &Value::Null)
        .unwrap();
    s.decisions().reopen(&d.id).unwrap();
    s.decisions().reopen(&d.id).unwrap();
    let rolled_back: Result<(), steve_store::StoreError> = s.transaction(|tx| {
        steve_store::decision_create_in(tx, &spec)?;
        Err(steve_store::StoreError::Conflict("rollback fixture".into()))
    });
    assert!(rolled_back.is_err());
    let page = events(s, &item);
    let kinds: Vec<_> = page["events"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["kind"].as_str().unwrap())
        .collect();
    assert_eq!(
        kinds,
        ["decision.created", "decision.resolved", "decision.reopened"]
    );
    assert!(!page.to_string().contains("private question body"));
    assert!(!page.to_string().contains("private answer body"));
    assert_eq!(s.decisions().get(&d.id).unwrap().unwrap().status, "open");

    // Refuse diagnostic persistence on the worker's writer connection, without
    // breaking the business table or requiring a full disk to reproduce the failure.
    s.with_conn(|c| {
        c.execute_batch(
            "CREATE TEMP TRIGGER reject_diagnostic_insert BEFORE INSERT ON main.diagnostic_events
             BEGIN SELECT RAISE(ABORT, 'diagnostic write failure fixture'); END;",
        )?;
        Ok(())
    })
    .unwrap();
    let next = s
        .decisions()
        .create(&NewDecision {
            subject_id: format!("{item}-write-failure"),
            question: "private question after diagnostic failure".into(),
            ..spec
        })
        .unwrap()
        .unwrap();
    let after_failure = events(s, &item);
    assert_eq!(after_failure["events"].as_array().unwrap().len(), 3);
    assert!(
        after_failure["coverage"]["instance_dropped"]
            .as_u64()
            .unwrap()
            >= 1
    );
    assert!(after_failure["gaps"].as_array().unwrap().iter().any(|gap| {
        gap["reason"] == "diagnostic_queue_or_write_loss" && gap["scope"] == "instance"
    }));
    assert_eq!(s.decisions().get(&next.id).unwrap().unwrap().status, "open");
    assert!(
        !after_failure
            .to_string()
            .contains("private question after diagnostic failure")
    );
    s.with_conn(|c| {
        c.execute_batch("DROP TRIGGER reject_diagnostic_insert")?;
        Ok(())
    })
    .unwrap();
}
