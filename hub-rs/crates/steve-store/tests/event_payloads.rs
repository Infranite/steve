mod common;
use common::fresh;
use serde_json::{Value, json};
use steve_store::{NewSession, NoPublisher, Store, WS};

fn session(s: &Store) -> String {
    s.sessions()
        .create(&NewSession {
            workspace_id: WS.into(),
            harness: "fake".into(),
            cwd: "/tmp".into(),
            title: "test".into(),
            token: steve_store::uid("token"),
            ..Default::default()
        })
        .unwrap()
        .id
}
fn count(s: &Store, table: &str) -> i64 {
    s.with_conn(|c| Ok(c.query_row(&format!("SELECT count(*) FROM {table}"), [], |r| r.get(0))?))
        .unwrap()
}
fn payload() -> Value {
    let image = "Abc123".repeat(32_000);
    json!({"toolCallId":"tool1", "status":"completed", "content":[{"type":"image", "data":image}], "rawOutput":{"weird/~key":image}, "_meta":{"output":image}})
}

#[test]
fn lossless_dedup_reopen_and_session_isolation() {
    let t = fresh();
    let s = &t.store;
    let id = session(s);
    let data = payload();
    let first = s
        .events()
        .add(&id, "tool_call_update", data.clone())
        .unwrap();
    let last = s
        .events()
        .add(&id, "tool_call_update", data.clone())
        .unwrap();
    assert_eq!(count(s, "event_payloads"), 1);
    assert_eq!(count(s, "event_payload_refs"), 6);
    assert_eq!(
        s.events()
            .after(&id, 0, 10)
            .unwrap()
            .iter()
            .map(|e| &e.data)
            .collect::<Vec<_>>(),
        vec![&data, &data]
    );
    assert_eq!(s.events().up_to(&id, first).unwrap()[0].data, data);
    let reopened = Store::open(s.path(), NoPublisher).unwrap();
    assert_eq!(reopened.events().up_to(&id, last).unwrap()[1].data, data);
    let other = session(s);
    s.events()
        .add(&other, "tool_call_update", data.clone())
        .unwrap();
    assert_eq!(
        count(s, "event_payloads"),
        2,
        "never share payload ownership across sessions"
    );
    s.sessions().remove(&id).unwrap();
    assert_eq!(count(s, "event_payloads"), 1);
    assert_eq!(s.events().after(&other, 0, 10).unwrap()[0].data, data);
    s.sessions().remove(&other).unwrap();
    assert_eq!(count(s, "event_payloads"), 0);
    assert_eq!(count(s, "event_payload_refs"), 0);
}

#[test]
fn mixed_legacy_rows_and_reference_lookalikes_are_not_interpreted() {
    let t = fresh();
    let s = &t.store;
    let id = session(s);
    let legacy =
        json!({"content":{"$steve_event_payload":"guess","hash":"whatever"},"text":"small"});
    s.events()
        .add(&id, "tool_call_update", legacy.clone())
        .unwrap();
    assert_eq!(s.events().after(&id, 0, 1).unwrap()[0].data, legacy);
    assert_eq!(count(s, "event_payload_refs"), 0);
    let long = "legacy".repeat(20_000);
    let data = json!({"content":long});
    s.with_conn(|c| {
        c.execute(
            "INSERT INTO events(session_id,ts,kind,data) VALUES (?,1,'tool_call_update',?)",
            rusqlite::params![id, serde_json::to_string(&data).unwrap()],
        )?;
        Ok(())
    })
    .unwrap();
    assert_eq!(s.events().after(&id, 0, 10).unwrap()[1].data, data);
}

#[test]
fn event_and_payload_write_roll_back_together() {
    let t = fresh();
    let s = &t.store;
    let id = session(s);
    s.with_conn(|c| { c.execute_batch("CREATE TRIGGER reject_payload BEFORE INSERT ON event_payload_refs BEGIN SELECT RAISE(ABORT, 'test rollback'); END;")?; Ok(()) }).unwrap();
    assert!(s.events().add(&id, "tool_call_update", payload()).is_err());
    for table in ["events", "event_payloads", "event_payload_refs"] {
        assert_eq!(count(s, table), 0);
    }
}

#[test]
fn search_sees_text_after_preview_and_obeys_workspace() {
    let t = fresh();
    let s = &t.store;
    let id = session(s);
    let text = format!("{}needle-after-preview", "a".repeat(100_000));
    s.events()
        .add(&id, "tool_call_update", json!({"rawOutput":text}))
        .unwrap();
    assert_eq!(
        s.sessions().search(WS, "needle-after-preview").unwrap(),
        vec![id]
    );
    assert!(
        s.sessions()
            .search("other", "needle-after-preview")
            .unwrap()
            .is_empty()
    );
}

#[test]
fn byte_pages_and_row_pages_make_progress_without_loss() {
    let t = fresh();
    let s = &t.store;
    let id = session(s);
    let data = json!({"rawOutput":"x".repeat(1_100_000)});
    let a = s
        .events()
        .add(&id, "tool_call_update", data.clone())
        .unwrap();
    let b = s
        .events()
        .add(&id, "tool_call_update", data.clone())
        .unwrap();
    let c = s
        .events()
        .add(
            &id,
            "tool_call_update",
            json!({"rawOutput":"x".repeat(3_000_000)}),
        )
        .unwrap();
    let mut page = vec![];
    s.events().visit_page(&id, 0, |e| page.push(e)).unwrap();
    assert_eq!(page.len(), 1);
    assert_eq!(page[0].seq, a);
    assert_eq!(page[0].data, data);
    let mut got = vec![];
    let mut after = 0;
    loop {
        let mut n = 0;
        s.events()
            .visit_page(&id, after, |e| {
                after = e.seq;
                got.push(e.seq);
                n += 1;
            })
            .unwrap();
        if n == 0 {
            break;
        }
    }
    assert_eq!(got, vec![a, b, c]);
    let many = session(s);
    for i in 0..503 {
        s.events()
            .add(&many, "agent_message_chunk", json!({"text":i.to_string()}))
            .unwrap();
    }
    let mut first = vec![];
    s.events().visit_page(&many, 0, |e| first.push(e)).unwrap();
    assert_eq!(first.len(), 500);
    let mut rest = vec![];
    s.events()
        .visit_page(&many, first.last().unwrap().seq, |e| rest.push(e))
        .unwrap();
    assert_eq!(rest.len(), 3);
}

#[test]
fn synthetic_total_storage_reduces_with_all_payload_tables_counted() {
    let t = fresh();
    let s = &t.store;
    let id = session(s);
    let data = payload();
    let raw = serde_json::to_string(&data).unwrap();
    for _ in 0..30 {
        s.events()
            .add(&id, "tool_call_update", data.clone())
            .unwrap();
    }
    let stored: i64=s.with_conn(|c|Ok(c.query_row("SELECT (SELECT SUM(length(CAST(data AS BLOB))) FROM events) + (SELECT SUM(length(CAST(data AS BLOB))) FROM event_payloads) + (SELECT SUM(length(path)+length(hash)+length(session_id)+8) FROM event_payload_refs)",[],|r|r.get(0))?)).unwrap();
    let baseline = raw.len() * 30;
    println!(
        "synthetic events=30 original_json_bytes={baseline} normalized_total_payload_bytes={stored} ratio={:.4}",
        stored as f64 / baseline as f64
    );
    assert!(stored < baseline as i64 / 8);
    let baseline_store = fresh();
    let bid = session(&baseline_store.store);
    baseline_store
        .store
        .with_conn(|c| {
            let tx = c.unchecked_transaction()?;
            for _ in 0..30 {
                tx.execute(
                    "INSERT INTO events(session_id,ts,kind,data) VALUES (?,1,'tool_call_update',?)",
                    rusqlite::params![bid, raw],
                )?;
            }
            tx.commit()?;
            Ok(())
        })
        .unwrap();
    let pages = |store: &Store| {
        store
            .with_conn(|c| {
                Ok(c.query_row(
                    "SELECT page_count * page_size FROM pragma_page_count(),pragma_page_size()",
                    [],
                    |r| r.get::<_, i64>(0),
                )?)
            })
            .unwrap()
    };
    let before = pages(&baseline_store.store);
    let after = pages(s);
    println!(
        "synthetic total_sqlite_pages_bytes baseline={before} normalized={after} ratio={:.4}",
        after as f64 / before as f64
    );
    assert!(
        after < before / 3,
        "total DB including every table and index shrinks"
    );
    // Every event, including all repeated strings, still reads back intact.
    assert!(
        s.events()
            .up_to(&id, i64::MAX)
            .unwrap()
            .iter()
            .all(|e| e.data == data)
    );
}

#[test]
fn corruption_does_not_return_another_sessions_payload_or_silent_preview() {
    let t = fresh();
    let s = &t.store;
    let a = session(s);
    let b = session(s);
    let seq = s.events().add(&a, "tool_call_update", payload()).unwrap();
    s.events().add(&b, "tool_call_update", payload()).unwrap();
    s.with_conn(|c| {
        c.execute(
            "UPDATE event_payload_refs SET session_id=? WHERE event_seq=?",
            rusqlite::params![b, seq],
        )?;
        Ok(())
    })
    .unwrap();
    assert!(
        s.events()
            .after(&a, 0, 10)
            .unwrap_err()
            .to_string()
            .contains("session mismatch")
    );
    assert!(
        s.events()
            .visit_page(&a, 0, |_| panic!("must not expose event"))
            .is_err()
    );
}

#[test]
fn event_only_retention_reclaims_last_reference() {
    let t = fresh();
    let s = &t.store;
    let id = session(s);
    let a = s.events().add(&id, "tool_call_update", payload()).unwrap();
    let b = s.events().add(&id, "tool_call_update", payload()).unwrap();
    s.with_conn(|c| {
        c.execute("DELETE FROM events WHERE seq=?", [a])?;
        Ok(())
    })
    .unwrap();
    assert_eq!(count(s, "event_payloads"), 1);
    s.with_conn(|c| {
        c.execute("DELETE FROM events WHERE seq=?", [b])?;
        Ok(())
    })
    .unwrap();
    assert_eq!(count(s, "event_payloads"), 0);
    assert_eq!(count(s, "event_payload_refs"), 0);
}

#[test]
fn standalone_database_snapshot_contains_every_payload() {
    let t = fresh();
    let s = &t.store;
    let id = session(s);
    let data = payload();
    s.events()
        .add(&id, "tool_call_update", data.clone())
        .unwrap();
    let snapshot = t.dir.path().join("snapshot.sqlite3");
    s.with_conn(|c| {
        c.execute("VACUUM INTO ?", [snapshot.to_str().unwrap()])?;
        Ok(())
    })
    .unwrap();
    let restored = Store::open(snapshot, NoPublisher).unwrap();
    assert_eq!(
        restored.events().up_to(&id, i64::MAX).unwrap()[0].data,
        data
    );
}

#[test]
fn compaction_drops_images_and_big_outputs_and_their_payloads_once() {
    let t = fresh();
    let s = &t.store;
    let id = session(s);
    let big = format!("{}{}", "a".repeat(5000), "b".repeat(200_000));
    s.events()
        .add(
            &id,
            "tool_call_update",
            json!({"rawOutput": big, "status": "completed"}),
        )
        .unwrap();
    s.events()
        .add(&id, "user", json!({"text": "截图", "images": [{"type": "image", "data": "QUJD".repeat(1000), "mimeType": "image/png"}]}))
        .unwrap();
    s.events()
        .add(&id, "agent_message_chunk", json!({"text": "完成了"}))
        .unwrap();
    assert!(count(s, "event_payloads") >= 1);
    assert!(!s.events().compacted(&id).unwrap());
    assert_eq!(s.events().compact(&id).unwrap(), 2);
    assert!(s.events().compacted(&id).unwrap());
    assert_eq!(
        count(s, "event_payloads"),
        0,
        "the big output's payload went with its last ref"
    );
    assert_eq!(count(s, "event_payload_refs"), 0);
    let ev = s.events().after(&id, 0, 10).unwrap();
    let out = ev[0].data["rawOutput"].as_str().unwrap();
    assert!(
        out.starts_with(&"a".repeat(4000)) && out.ends_with(&"b".repeat(2000)),
        "{}",
        out.len()
    );
    assert!(out.len() < 7000);
    assert_eq!(ev[0].data["status"], "completed");
    assert_eq!(ev[1].data["images"][0]["type"], "image");
    assert!(ev[1].data["images"][0].get("data").is_none());
    assert_eq!(ev[1].data["text"], "截图");
    assert_eq!(
        ev[2].data["text"], "完成了",
        "conversation text is untouched"
    );
    assert_eq!(
        s.events().compact(&id).unwrap(),
        0,
        "a second pass changes nothing"
    );
}
