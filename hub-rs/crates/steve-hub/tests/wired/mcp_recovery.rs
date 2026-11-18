//! Acceptance on an isolated wired Hub and real LocalNode. The ACP fixture skips attachment,
//! then its replacement uses MCP over HTTP to list tools, update the item and deliver.
use super::*;

async fn acceptance(mode: &str) {
    let adapter = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/mcp-recovery.mjs"
    )
    .to_string();
    let e = start_with(&[
        ("STEVE_ADAPTER_CLAUDE", adapter.clone()),
        ("STEVE_ADAPTER_CODEX", adapter),
        ("STEVE_NO_RESUME", "0".into()),
        ("STEVE_NODE_MODE", "local".into()),
    ])
    .await;
    let (status, item) = e.call("POST","/api/items",Some(json!({"title":format!("MCP recovery acceptance {mode}"),"goal":format!("MCP recovery acceptance {mode}"),"bot":"steve","project":null}))).await;
    assert_eq!(status, 200, "{item}");
    let id = item["id"].as_str().unwrap();
    let store = steve_store::Store::open(&e.db, steve_store::NoPublisher).unwrap();
    let mut checked_watchdog = false;
    let mut injected = false;
    let mut result = None;
    for _ in 0..600 {
        let sessions = store.sessions().of_item(id).unwrap();
        if mode == "missing-attempt"
            && !injected
            && let Some(s) = sessions.first()
        {
            let ready = store
                .events()
                .after(&s.id, 0, 100)
                .unwrap()
                .iter()
                .any(|ev| {
                    steve_transcript::assistant_text(&ev.kind, &ev.data).is_some_and(|text| {
                        text.contains("Waiting for isolated attempt loss injection")
                    })
                });
            if ready {
                let node =
                    rusqlite::Connection::open(e.db.parent().unwrap().join("node/node.sqlite3"))
                        .unwrap();
                node.busy_timeout(Duration::from_secs(5)).unwrap();
                assert_eq!(
                    node.execute(
                        "UPDATE sessions SET view=json_set(view,'$.active',NULL) WHERE id=?",
                        [&s.id]
                    )
                    .unwrap(),
                    1
                );
                std::fs::write(
                    std::path::Path::new(&s.cwd).join("attempt-cleared.txt"),
                    "isolated fixture",
                )
                .unwrap();
                injected = true;
            }
        }
        if mode == "missing-attempt"
            && injected
            && !checked_watchdog
            && let Some(s) = sessions.first()
        {
            if store
                .sessions()
                .mcp_attachment(&s.id)
                .unwrap()
                .is_some_and(|a| a.fault.as_deref() == Some("missing_attempt"))
            {
                let core = e.tools.core.clone();
                tokio::task::spawn_blocking(move || core.check_stalls())
                    .await
                    .unwrap()
                    .unwrap();
                checked_watchdog = true;
            }
        }
        if mode == "startup-watchdog"
            && !checked_watchdog
            && let Some(s) = sessions.first()
        {
            let dispatched = store
                .events()
                .after(&s.id, 0, 100)
                .unwrap()
                .iter()
                .any(|ev| steve_transcript::assistant_text(&ev.kind, &ev.data).is_some());
            if dispatched {
                e.sql(
                    "UPDATE events SET ts=ts-61000 WHERE session_id=? AND kind='user'",
                    &[&s.id],
                );
                let core = e.tools.core.clone();
                tokio::task::spawn_blocking(move || core.check_stalls())
                    .await
                    .unwrap()
                    .unwrap();
                checked_watchdog = true;
            }
        }
        if sessions.len() == 2 {
            let events = store.events().after(&sessions[1].id, 0, 100).unwrap();
            if events
                .iter()
                .any(|ev| ev.kind == "steve_tool_receipt" && ev.data["asked"] == false)
                && events.iter().any(|ev| ev.kind == "turn_end")
            {
                result = Some(sessions);
                break;
            }
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let sessions = result.unwrap_or_else(|| {
        panic!(
            "no recovered delivery; sessions={:?}; events={:?}",
            store.sessions().of_item(id).unwrap(),
            store
                .sessions()
                .of_item(id)
                .unwrap()
                .iter()
                .map(|s| store
                    .events()
                    .after(&s.id, 0, 100)
                    .unwrap()
                    .into_iter()
                    .filter(|ev| ev.kind != "queued" && ev.kind != "user")
                    .collect::<Vec<_>>())
                .collect::<Vec<_>>()
        )
    });
    assert_eq!(sessions[0].status, "ended");
    assert!(
        !store
            .sessions()
            .mcp_attachment(&sessions[0].id)
            .unwrap()
            .unwrap()
            .ready
    );
    assert!(
        store
            .sessions()
            .mcp_attachment(&sessions[1].id)
            .unwrap()
            .unwrap()
            .ready
    );
    let first_user = store
        .events()
        .after(&sessions[1].id, 0, 100)
        .unwrap()
        .into_iter()
        .find(|ev| ev.kind == "user")
        .unwrap()
        .seq;
    assert!(
        store
            .sessions()
            .mcp_attachment(&sessions[1].id)
            .unwrap()
            .unwrap()
            .seq
            < first_user,
        "replacement attached during session/new, before any attempt was dispatched"
    );
    assert_eq!(
        sessions[0].cwd, sessions[1].cwd,
        "same workspace and existing file"
    );
    assert!(
        std::fs::read_to_string(
            std::path::Path::new(&sessions[1].cwd).join("mcp-first-session.txt")
        )
        .unwrap()
        .contains("survive")
    );
    let timeline = store.timeline().list(id).unwrap();
    assert!(
        store
            .items()
            .get(id)
            .unwrap()
            .unwrap()
            .plan
            .iter()
            .any(|p| p.step == "恢复 MCP 工具挂载" && p.done)
    );
    assert!(
        timeline
            .iter()
            .any(|t| t.text.contains(if mode == "missing-attempt" {
                "工具无法关联运行尝试"
            } else {
                "没有挂上 Steve 工具，自动重开"
            }))
    );
    assert!(store.queued().list(&sessions[0].id).unwrap().is_empty());
    assert!(store.reader::<_,steve_store::StoreError>(|c|Ok(c.query_row("SELECT EXISTS(SELECT 1 FROM node_commands WHERE method='release_session' AND json_extract(request,'$.body')=? AND state='finished' AND json_extract(receipt,'$.success')=1)",[&sessions[0].id],|r|r.get::<_,bool>(0))?)).unwrap());
    assert!(
        !timeline.iter().any(|t| t.text.starts_with("Steve 接手：")),
        "never nag the broken session"
    );
    if mode == "startup-watchdog" || mode == "missing-attempt" {
        assert!(checked_watchdog)
    }
    retire(&e, &store, &sessions[1].id).await;
    eprintln!(
        "{mode}: missing MCP -> new session -> real tools/list + steve_update + steve_deliver; old harness released, workspace retained"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn stopped_agent_without_mcp_gets_a_working_replacement() {
    acceptance("stopped").await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn startup_watchdog_replaces_a_running_agent_without_mcp() {
    acceptance("startup-watchdog").await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_live_harness_that_lost_its_attempt_keeps_discovery_but_is_replaced_safely() {
    acceptance("missing-attempt").await
}

async fn retire(e: &Env, store: &steve_store::Store, sid: &str) {
    let sid_ref = sid;
    let core = e.tools.core.clone();
    let sid = sid.to_string();
    tokio::task::spawn_blocking(move || core.end_session(&sid))
        .await
        .unwrap()
        .unwrap();
    let mut released = false;
    for _ in 0..200 {
        released = store
            .reader::<_, steve_store::StoreError>(|c| {
                Ok(c.query_row(
                    "SELECT phase='released' FROM runtime_sessions WHERE session_id=?",
                    [sid_ref],
                    |r| r.get::<_, bool>(0),
                )?)
            })
            .unwrap();
        if released {
            break;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    assert!(
        released,
        "the test's harness must stop before its runtime shuts down"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn provider_interruption_keeps_mcp_usable_on_the_next_attempt_in_the_same_session() {
    let adapter = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/mcp-recovery.mjs"
    )
    .to_string();
    let e = start_with(&[
        ("STEVE_ADAPTER_CLAUDE", adapter.clone()),
        ("STEVE_ADAPTER_CODEX", adapter),
        ("STEVE_NO_RESUME", "0".into()),
        ("STEVE_NODE_MODE", "local".into()),
    ])
    .await;
    let (status,item)=e.call("POST","/api/items",Some(json!({"title":"MCP recovery acceptance provider-retry","goal":"MCP recovery acceptance provider-retry","bot":"steve","project":null}))).await;
    assert_eq!(status, 200, "{item}");
    let id = item["id"].as_str().unwrap();
    let store = steve_store::Store::open(&e.db, steve_store::NoPublisher).unwrap();
    let mut delivered = false;
    for _ in 0..1200 {
        let sessions = store.sessions().of_item(id).unwrap();
        assert!(
            sessions.len() <= 1,
            "a successful MCP followed by a provider error must keep the session"
        );
        if let Some(s) = sessions.first() {
            let events = store.events().after(&s.id, 0, 200).unwrap();
            delivered = events
                .iter()
                .any(|ev| ev.kind == "steve_tool_receipt" && ev.data["asked"] == false)
                && events.iter().any(|ev| ev.kind == "turn_end");
            if delivered {
                break;
            }
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(
        delivered,
        "provider retry did not finish: {:?}",
        store.items().get(id).unwrap()
    );
    let sessions = store.sessions().of_item(id).unwrap();
    assert_eq!(sessions.len(), 1);
    assert_eq!(store.reader::<_,steve_store::StoreError>(|c|Ok(c.query_row("SELECT COUNT(*) FROM runtime_attempts a JOIN runtime_turns t USING(turn_id) WHERE t.session_id=?",[&sessions[0].id],|r|r.get::<_,i64>(0))?)).unwrap(),2);
    assert!(store.items().plan_rounds(id).unwrap().iter().any(|round| {
        round
            .steps
            .iter()
            .any(|p| p.step == "provider retry initial MCP success" && p.done)
    }));
    retire(&e, &store, &sessions[0].id).await;
    eprintln!(
        "provider-retry: real MCP call -> 503 -> new attempt in same session -> real MCP delivery"
    );
}
