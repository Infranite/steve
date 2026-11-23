use serde_json::json;
use steve_store::{NewSession, NoPublisher, Store, Views, WS};

#[test]
fn transcript_cursors_cover_sparse_rows_and_keep_merged_prompts_and_tools_together() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path().join("db"), NoPublisher).unwrap();
    store.ensure_workspace().unwrap();
    let session = || {
        store
            .sessions()
            .create(&NewSession {
                workspace_id: WS.into(),
                harness: "test".into(),
                ..Default::default()
            })
            .unwrap()
            .id
    };
    let (id, other) = (session(), session());
    let add = |kind, data| {
        store.events().add(&other, "system", json!({})).unwrap();
        store.events().add(&id, kind, data).unwrap()
    };
    let hello = add("system", json!({"text":"start"}));
    let first = add("user", json!({"text":"first"}));
    let first_answer = add("text", json!({"text":"done"}));
    let second = add("user", json!({"text":"next"}));
    let merged = add("user", json!({"text":"also"}));
    let tool_start = add("tool", json!({"tool":{"id":"a","phase":"running"}}));
    let tool_end = add("tool", json!({"tool":{"id":"a","phase":"succeeded"}}));
    let seqs = |p: steve_store::views::TranscriptPage| {
        p.events.into_iter().map(|e| e.seq).collect::<Vec<_>>()
    };
    store
        .read::<_, steve_store::StoreError>(|r| {
            let mut t = r.transcript();
            assert!(t.exists(WS, &id)?);
            assert!(!t.exists("another", &id)?);
            let notices = t.before(&other, None, 2)?;
            assert_eq!(notices.events.len(), 2);
            assert_eq!(notices.next, Some(notices.events[0].seq));
            let page = t.before(&id, None, 1)?;
            assert_eq!(page.next, Some(second));
            assert_eq!(seqs(page), [second, merged, tool_start, tool_end]);
            let previous = t.before(&id, Some(second), 1)?;
            assert_eq!(previous.next, Some(first));
            assert_eq!(seqs(previous), [first, first_answer]);
            let beginning = t.before(&id, Some(first), 1)?;
            assert_eq!(beginning.next, None);
            assert_eq!(seqs(beginning), [hello]);
            assert_eq!(
                t.marks(&id, 2)?.marks,
                [first, second, tool_start, tool_end]
            );
            let page = t.after(&id, first, Some(second), 1)?;
            assert_eq!(page.next, Some(first_answer));
            assert_eq!(seqs(page), [first_answer]);
            let page = t.after(&id, first_answer, Some(second), 1)?;
            assert_eq!(page.next, None);
            assert_eq!(seqs(page), [second]);
            Ok(())
        })
        .unwrap();
}

#[test]
fn transcript_pages_keep_unknown_format_cursors_and_budget_hydrated_payloads() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path().join("db"), NoPublisher).unwrap();
    store.ensure_workspace().unwrap();
    let id = store
        .sessions()
        .create(&NewSession {
            workspace_id: WS.into(),
            harness: "test".into(),
            ..Default::default()
        })
        .unwrap()
        .id;
    let unknown = store
        .write(|w| {
            let tx = steve_store::testkit::sqlite_writer(w);
            tx.execute(
                "INSERT INTO events(session_id, ts, kind, data, format)
                 VALUES (?, 1, 'agent_message_chunk', '{\"text\":\"not decoded\"}', 'future.v2')",
                [&id],
            )?;
            Ok::<_, steve_store::StoreError>(tx.last_insert_rowid())
        })
        .unwrap();
    let data = json!({"content":[{"type":"image","data":"Abc123".repeat(32_000)}]});
    let first = store
        .events()
        .add(&id, "tool_call_update", data.clone())
        .unwrap();
    let last = store
        .events()
        .add(&id, "tool_call_update", data.clone())
        .unwrap();
    store
        .read::<_, steve_store::StoreError>(|r| {
            let mut transcript = r.transcript();
            let page = transcript.after(&id, 0, None, 1)?;
            assert_eq!(page.next, Some(unknown));
            let event = &page.events[0];
            assert_eq!(event.kind, "system");
            assert_eq!(event.data["code"], "unsupported_format");
            assert_eq!(event.data["format"], "future.v2");
            assert!(!event.data.to_string().contains("not decoded"));

            let page = transcript.after(&id, unknown, None, 400)?;
            assert_eq!(page.next, Some(first));
            assert_eq!(page.events.len(), 1);
            assert_eq!(page.events[0].data, data);

            let page = transcript.after(&id, first, None, 400)?;
            assert_eq!(page.next, None);
            assert_eq!(page.events.len(), 1);
            assert_eq!(page.events[0].seq, last);
            assert_eq!(page.events[0].data, data);
            Ok(())
        })
        .unwrap();
}
