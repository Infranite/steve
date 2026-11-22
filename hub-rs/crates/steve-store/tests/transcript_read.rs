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
        .read(|r| {
            let mut t = r.transcript();
            assert!(t.exists(WS, &id)?);
            assert!(!t.exists("another", &id)?);
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
