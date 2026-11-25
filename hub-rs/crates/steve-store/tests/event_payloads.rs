use steve_store::Views;
use steve_store::testkit::SqliteFixtures;
mod common;
use common::fresh;
use rusqlite::Connection;
use serde_json::{Value, json};
use steve_node_api::{Action, CallId, Part, ToolCategory, ToolOrigin, ToolPhase, ToolView};
use steve_store::{NewSession, NoPublisher, Store, WS, db};

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
    s.fixture_write(
        |c| Ok(c.query_row(&format!("SELECT count(*) FROM {table}"), [], |r| r.get(0))?),
    )
    .unwrap()
}
fn tool_event(result: Vec<Part>, action: Option<Action>, error: Option<String>) -> Value {
    let tool = ToolView {
        id: CallId::new("tool1").unwrap(),
        parent: None,
        origin: ToolOrigin::Harness,
        name: "Bash".into(),
        category: ToolCategory::Exec,
        title: "Run".into(),
        description: None,
        target: None,
        phase: ToolPhase::Succeeded,
        action,
        evidence: None,
        result,
        error,
        duration_ms: None,
    };
    json!({ "tool": tool })
}
fn payload() -> Value {
    let large = "Abc123".repeat(32_000);
    tool_event(
        vec![Part::Text {
            text: large.clone(),
        }],
        Some(Action::Tool {
            name: "steve_deliver".into(),
            args: json!({"weird/~key": large}),
        }),
        Some(large),
    )
}

#[test]
fn lossless_dedup_reopen_and_session_isolation() {
    let t = fresh();
    let s = &t.store;
    let id = session(s);
    let data = payload();
    let first = s.events().add(&id, "tool", data.clone()).unwrap();
    let last = s.events().add(&id, "tool", data.clone()).unwrap();
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
    s.events().add(&other, "tool", data.clone()).unwrap();
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
fn inline_rows_and_reference_lookalikes_are_not_interpreted() {
    let t = fresh();
    let s = &t.store;
    let id = session(s);
    let lookalike = tool_event(
        vec![Part::Text {
            text: "small".into(),
        }],
        Some(Action::Tool {
            name: "steve_deliver".into(),
            args: json!({"$steve_event_payload": "guess", "hash": "whatever"}),
        }),
        None,
    );
    s.events().add(&id, "tool", lookalike.clone()).unwrap();
    assert_eq!(s.events().after(&id, 0, 1).unwrap()[0].data, lookalike);
    assert_eq!(count(s, "event_payload_refs"), 0);
    let long = "inline".repeat(20_000);
    let data = tool_event(vec![Part::Text { text: long }], None, None);
    s.fixture_write(|c| {
        c.execute(
            "INSERT INTO events(session_id,ts,kind,data,format) VALUES (?,1,'tool',?,'steve.v1')",
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
    s.fixture_write(|c| { c.execute_batch("CREATE TRIGGER reject_payload BEFORE INSERT ON event_payload_refs BEGIN SELECT RAISE(ABORT, 'test rollback'); END;")?; Ok(()) }).unwrap();
    assert!(s.events().add(&id, "tool", payload()).is_err());
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
        .add(
            &id,
            "tool",
            tool_event(vec![Part::Text { text }], None, None),
        )
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
    let data = tool_event(
        vec![Part::Text {
            text: "x".repeat(1_100_000),
        }],
        None,
        None,
    );
    let a = s.events().add(&id, "tool", data.clone()).unwrap();
    let b = s.events().add(&id, "tool", data.clone()).unwrap();
    let c = s
        .events()
        .add(
            &id,
            "tool",
            tool_event(
                vec![Part::Text {
                    text: "x".repeat(3_000_000),
                }],
                None,
                None,
            ),
        )
        .unwrap();
    let page = s.read(|r| r.transcript().after(&id, 0, None, 500)).unwrap();
    assert_eq!(page.events.len(), 1);
    assert_eq!(page.events[0].seq, a);
    assert_eq!(page.events[0].data, data);
    let page = s
        .read(|r| r.transcript().after(&id, page.next.unwrap(), None, 500))
        .unwrap();
    assert_eq!(page.events[0].seq, b);
    let page = s
        .read(|r| r.transcript().after(&id, page.next.unwrap(), None, 500))
        .unwrap();
    assert_eq!(page.events[0].seq, c);
    assert!(page.next.is_none());
}

#[test]
fn synthetic_total_storage_reduces_with_all_payload_tables_counted() {
    let t = fresh();
    let s = &t.store;
    let id = session(s);
    let data = payload();
    let raw = serde_json::to_string(&data).unwrap();
    for _ in 0..30 {
        s.events().add(&id, "tool", data.clone()).unwrap();
    }
    let stored: i64=s.fixture_write(|c|Ok(c.query_row("SELECT (SELECT SUM(length(CAST(data AS BLOB))) FROM events) + (SELECT SUM(length(CAST(data AS BLOB))) FROM event_payloads) + (SELECT SUM(length(path)+length(hash)+length(session_id)+8) FROM event_payload_refs)",[],|r|r.get(0))?)).unwrap();
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
        .fixture_write(|c| {
            let tx = c.unchecked_transaction()?;
            for _ in 0..30 {
                tx.execute(
                    "INSERT INTO events(session_id,ts,kind,data,format) VALUES (?,1,'tool',?,'steve.v1')",
                    rusqlite::params![bid, raw],
                )?;
            }
            tx.commit()?;
            Ok(())
        })
        .unwrap();
    let pages = |store: &Store| {
        store
            .fixture_write(|c| {
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
    let seq = s.events().add(&a, "tool", payload()).unwrap();
    s.events().add(&b, "tool", payload()).unwrap();
    s.fixture_write(|c| {
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
    assert!(s.read(|r| r.transcript().after(&a, 0, None, 500)).is_err());
}

#[test]
fn event_only_retention_reclaims_last_reference() {
    let t = fresh();
    let s = &t.store;
    let id = session(s);
    let a = s.events().add(&id, "tool", payload()).unwrap();
    let b = s.events().add(&id, "tool", payload()).unwrap();
    s.fixture_write(|c| {
        c.execute("DELETE FROM events WHERE seq=?", [a])?;
        Ok(())
    })
    .unwrap();
    assert_eq!(count(s, "event_payloads"), 1);
    s.fixture_write(|c| {
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
    s.events().add(&id, "tool", data.clone()).unwrap();
    let snapshot_dir = t.dir.path().join("snapshot");
    std::fs::create_dir(&snapshot_dir).unwrap();
    let snapshot = snapshot_dir.join("steve.sqlite3");
    s.fixture_write(|c| {
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
            "tool",
            tool_event(vec![Part::Text { text: big }], None, None),
        )
        .unwrap();
    s.events()
        .add(&id, "user", json!({"text": "截图", "images": [{"type": "image", "data": "QUJD".repeat(1000), "mimeType": "image/png"}]}))
        .unwrap();
    s.events()
        .add(
            &id,
            "text",
            json!({"role": "assistant", "message": "m1", "text": "完成了"}),
        )
        .unwrap();
    assert!(count(s, "event_payloads") >= 1);
    assert_eq!(s.events().compacted_upto(&id).unwrap(), 0);
    let last = s.events().last_seq(&id).unwrap();
    assert_eq!(s.events().compact(&id, last, || Some(())).unwrap(), Some(2));
    assert_eq!(s.events().compacted_upto(&id).unwrap(), last);
    assert_eq!(
        count(s, "event_payloads"),
        0,
        "the big output's payload went with its last ref"
    );
    assert_eq!(count(s, "event_payload_refs"), 0);
    let ev = s.events().after(&id, 0, 10).unwrap();
    let out = ev[0].data["tool"]["result"][0]["text"].as_str().unwrap();
    assert!(
        out.starts_with(&"a".repeat(4000)) && out.ends_with(&"b".repeat(2000)),
        "{}",
        out.len()
    );
    assert!(out.len() < 7000);
    assert_eq!(ev[0].data["tool"]["phase"], "succeeded");
    assert_eq!(ev[1].data["images"][0]["type"], "image");
    assert!(ev[1].data["images"][0].get("data").is_none());
    assert_eq!(ev[1].data["text"], "截图");
    assert_eq!(
        ev[2].data["text"], "完成了",
        "conversation text is untouched"
    );
    assert_eq!(
        s.events().compact(&id, last, || Some(())).unwrap(),
        Some(0),
        "a second pass changes nothing"
    );
}

/// Audit: a compaction pass stops when the session stops being due between pages, and leaves alone
/// what was appended after the seq it was found due at; a later pass takes up only what came after.
#[test]
fn compaction_stops_when_no_longer_due_and_stays_within_its_seq() {
    let t = fresh();
    let s = &t.store;
    let id = session(s);
    let shot = || json!({"text": "截图", "images": [{"type": "image", "data": "QUJD".repeat(1000), "mimeType": "image/png"}]});
    for _ in 0..450 {
        s.events().add(&id, "user", shot()).unwrap();
    }
    let has_image = |e: &steve_store::Event| e.data["images"][0].get("data").is_some();
    let all = |s: &Store| s.events().up_to(&id, i64::MAX).unwrap();
    // due for the first page only: the pass stops, records nothing, and leaves the rest whole
    let last = s.events().last_seq(&id).unwrap();
    let mut pages = 0;
    let stopped = s
        .events()
        .compact(&id, last, || {
            pages += 1;
            (pages == 1).then_some(())
        })
        .unwrap();
    assert_eq!(stopped, None);
    assert_eq!(s.events().compacted_upto(&id).unwrap(), 0);
    let ev = all(s);
    let changed = ev.iter().filter(|e| !has_image(e)).count();
    assert!((1..=200).contains(&changed), "one bounded slice written");
    assert!(ev[changed..].iter().all(has_image));
    // events appended after the seq the pass was given are not touched
    let upto = s.events().last_seq(&id).unwrap();
    s.events().add(&id, "user", shot()).unwrap();
    assert_eq!(
        s.events().compact(&id, upto, || Some(())).unwrap(),
        Some(450 - changed)
    );
    assert_eq!(s.events().compacted_upto(&id).unwrap(), upto);
    let ev = all(s);
    assert!(ev[..450].iter().all(|e| !has_image(e)));
    assert!(has_image(&ev[450]), "appended after the pass was due");
    // a later pass (archived again) takes up only what came after
    let last = s.events().last_seq(&id).unwrap();
    assert_eq!(s.events().compact(&id, last, || Some(())).unwrap(), Some(1));
    assert_eq!(s.events().compacted_upto(&id).unwrap(), last);
    assert!(all(s).iter().all(|e| !has_image(e)));
}

#[test]
fn migration_127_removes_legacy_acp_events_and_backfills_first_tool_at() {
    let c = Connection::open_in_memory().unwrap();
    c.execute_batch(
        "CREATE TABLE schema_version(v INTEGER NOT NULL); INSERT INTO schema_version VALUES(126);
         CREATE TABLE schema_compat(min_reader INTEGER NOT NULL); INSERT INTO schema_compat VALUES(126);",
    )
    .unwrap();
    for (i, m) in db::MIGRATIONS[..126].iter().enumerate() {
        if i == 119 {
            c.execute_batch(
                "ALTER TABLE bots ADD COLUMN reasoning_effort TEXT;
                 ALTER TABLE sessions ADD COLUMN reasoning_effort TEXT;",
            )
            .unwrap();
        }
        if i == 121 {
            c.execute_batch(db::ITEM_LIFECYCLE).unwrap();
        }
        c.execute_batch(m).unwrap();
    }
    c.execute_batch(
        "PRAGMA foreign_keys=ON;
         INSERT INTO workspaces(id,name,created) VALUES('w','w',1);
         INSERT INTO sessions(id,workspace_id,harness,cwd,status,created,updated) VALUES('s1','w','fake','/tmp','idle',10,10);
         INSERT INTO events(session_id,ts,kind,data,format) VALUES
           ('s1',11,'tool_call','{}','legacy.acp'),
           ('s1',12,'agent_message_chunk','{}','legacy.acp'),
           ('s1',13,'tool_call_update','{}','steve.v1'),
           ('s1',14,'text','{\"role\":\"assistant\",\"message\":\"m1\",\"text\":\"hi\"}','steve.v1'),
           ('s1',20,'tool','{\"tool\":{\"id\":\"t1\"}}','steve.v1'),
           ('s1',30,'tool','{\"tool\":{\"id\":\"t2\"}}','steve.v1');",
    )
    .unwrap();
    db::migrate(&c).unwrap();
    let remaining: Vec<String> = c
        .prepare("SELECT kind FROM events WHERE session_id='s1' ORDER BY seq")
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap();
    assert_eq!(remaining, vec!["text", "tool", "tool"]);
    let first_tool: Option<i64> = c
        .query_row(
            "SELECT first_tool_at FROM sessions WHERE id='s1'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(first_tool, Some(20));
}
