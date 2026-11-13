//! 能力: the library (MCP servers, skills, remote A2A agents) and per-Bot assignment through
//! `/capabilities/*` and `/bots/:id/capabilities`: validation, the vault keeping every secret value
//! (never a row, an answer or the audit), what a session's Node may take, the limits of one Bot,
//! and a remote agent's card fetched and its task sent and answered.

use std::sync::Arc;

use axum::Router;
use serde_json::{Value, json};
use steve_modules::caps::mcp::secret_id;
use steve_modules::caps::{Capabilities, routes};
use steve_modules::testkit::{ME, TestEnv, WS, WS2, call, caller};
use steve_modules::vault::Vault;
use steve_modules::vault::keystore::FileKeyStore;

struct H {
    env: TestEnv,
    _keys: tempfile::TempDir,
    svc: Capabilities,
    api: Router,
}

fn setup() -> H {
    let env = TestEnv::new(|_| {});
    let keys = tempfile::tempdir().unwrap();
    let vault = Vault::new(
        env.store().clone(),
        Arc::new(FileKeyStore::open(keys.path()).unwrap()),
    );
    vault.init(WS, ME).unwrap();
    env.store()
        .with_conn(|c| {
            c.execute_batch(
                "INSERT OR IGNORE INTO bots (id, workspace_id, name, harness, updated) VALUES ('dev', 'default', '开发', 'claude-code', 1), ('doc', 'default', '文档', 'codex', 1);
                 INSERT INTO bot_workspaces (bot_id, workspace_id, created) VALUES ('dev', 'default', 1), ('doc', 'default', 1);
                 INSERT INTO sessions (id, workspace_id, bot_id, harness, cwd, status, created, updated) VALUES ('s_dev', 'default', 'dev', 'claude-code', '.', 'active', 1, 1), ('s_doc', 'default', 'doc', 'codex', '.', 'active', 1, 1);",
            )?;
            Ok(())
        })
        .unwrap();
    let svc = Capabilities::new(vault);
    let api = routes(svc.clone()).with_state(());
    H {
        env,
        _keys: keys,
        svc,
        api,
    }
}

async fn req(h: &H, method: &str, path: &str, body: Option<Value>) -> (u16, Value) {
    call(&h.api, Some(&caller(WS, ME)), method, path, body.as_ref()).await
}

fn q<T: rusqlite::types::FromSql>(h: &H, sql: &str) -> T {
    h.env
        .store()
        .reader(|c| Ok::<_, steve_store::StoreError>(c.query_row(sql, [], |r| r.get(0))?))
        .unwrap()
}

fn secret(h: &H, session: &str, id: &str) -> Option<String> {
    h.svc
        .mcp_secret(session, id)
        .ok()
        .map(|v| String::from_utf8(v.to_vec()).unwrap())
}

/// Every file of the database holds none of `needles`.
fn nowhere_on_disk(h: &H, needles: &[&str]) {
    h.env
        .store()
        .with_conn(|c| {
            c.execute_batch("PRAGMA wal_checkpoint(TRUNCATE);")?;
            Ok(())
        })
        .unwrap();
    for f in std::fs::read_dir(h.env.dir.path()).unwrap().flatten() {
        let bytes = std::fs::read(f.path()).unwrap();
        for n in needles {
            assert!(
                !bytes.windows(n.len()).any(|w| w == n.as_bytes()),
                "{} holds {n}",
                f.path().display()
            );
        }
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_server_is_defined_once_and_its_secrets_go_to_the_bots_that_use_it() {
    let h = setup();
    let (s, v) = req(
        &h,
        "POST",
        "/capabilities/mcp-servers",
        Some(
            json!({"name": "github", "transport": "stdio", "command": "npx",
            "args": ["-y", "@modelcontextprotocol/server-github"],
            "env": [{"name": "LOG", "value": "debug"},
                    {"name": "GITHUB_TOKEN", "value": "ghp-SECRET-one", "secret": true}],
            "bot": "dev"}),
        ),
    )
    .await;
    assert_eq!(s, 200, "{v}");
    let id = v["id"].as_str().unwrap().to_string();
    assert_eq!(
        v["env"],
        json!([{"name": "LOG", "value": "debug"}, {"name": "GITHUB_TOKEN", "secret": true}])
    );
    // the quick add assigned it to dev
    assert_eq!(v["bots"], json!([{"id": "dev", "enabled": true}]));
    let sid = secret_id(&id, "env:GITHUB_TOKEN");
    assert_eq!(secret(&h, "s_dev", &sid).as_deref(), Some("ghp-SECRET-one"));
    // a Bot that does not use it gets nothing, nor a secret it does not have
    assert_eq!(secret(&h, "s_doc", &sid), None);
    assert_eq!(secret(&h, "s_dev", &secret_id(&id, "env:LOG")), None);
    // doc is given it too, then switched off for doc alone
    let (s, _) = req(
        &h,
        "PUT",
        &format!("/bots/doc/capabilities/mcp/{id}"),
        Some(json!({})),
    )
    .await;
    assert_eq!(s, 200);
    assert_eq!(secret(&h, "s_doc", &sid).as_deref(), Some("ghp-SECRET-one"));
    let (s, _) = req(
        &h,
        "PUT",
        &format!("/bots/doc/capabilities/mcp/{id}"),
        Some(json!({"enabled": false})),
    )
    .await;
    assert_eq!(s, 200);
    assert_eq!(secret(&h, "s_doc", &sid), None);
    assert_eq!(secret(&h, "s_dev", &sid).as_deref(), Some("ghp-SECRET-one"));
    let (_, mine) = req(&h, "GET", "/bots/doc/capabilities", None).await;
    assert_eq!(mine["mcp"][0]["enabled"], false, "{mine}");
    assert_eq!(
        mine["mcp"][0]["env"][1],
        json!({"name": "GITHUB_TOKEN", "secret": true})
    );
    let (_, list) = req(&h, "GET", "/capabilities/mcp-servers", None).await;
    assert_eq!(
        list[0]["bots"],
        json!([{"id": "dev", "enabled": true}, {"id": "doc", "enabled": false}])
    );

    // saved again without the value: the vault's stays; a new one replaces it
    let (s, v) = req(
        &h,
        "PATCH",
        &format!("/capabilities/mcp-servers/{id}"),
        Some(json!({"args": ["-y", "server-github@2"],
            "env": [{"name": "GITHUB_TOKEN", "secret": true}, {"name": "LOG", "value": "info"}]})),
    )
    .await;
    assert_eq!(s, 200, "{v}");
    assert_eq!(secret(&h, "s_dev", &sid).as_deref(), Some("ghp-SECRET-one"));
    let (s, _) = req(
        &h,
        "PATCH",
        &format!("/capabilities/mcp-servers/{id}"),
        Some(json!({"env": [{"name": "GITHUB_TOKEN", "value": "ghp-SECRET-two", "secret": true}]})),
    )
    .await;
    assert_eq!(s, 200);
    assert_eq!(secret(&h, "s_dev", &sid).as_deref(), Some("ghp-SECRET-two"));
    assert_eq!(
        q::<i64>(
            &h,
            "SELECT COUNT(*) FROM vault_items WHERE status = 'active' AND template = 'connection_credential'"
        ),
        1,
        "the replaced item is gone"
    );

    // off a Bot: it stays in the library
    let (s, _) = req(
        &h,
        "DELETE",
        &format!("/bots/doc/capabilities/mcp/{id}"),
        None,
    )
    .await;
    assert_eq!(s, 200);
    assert_eq!(q::<i64>(&h, "SELECT COUNT(*) FROM mcp_servers"), 1);
    let actions: String = q(
        &h,
        &format!(
            "SELECT group_concat(action, ',') FROM (SELECT action FROM audit WHERE target = '{id}' ORDER BY id)"
        ),
    );
    assert_eq!(
        actions,
        "mcp_server.create,bot_capability.assign,bot_capability.assign,bot_capability.disable,mcp_server.update,mcp_server.update,bot_capability.unassign"
    );

    // deleted: the row, its vault item and every assignment
    let (s, _) = req(
        &h,
        "DELETE",
        &format!("/capabilities/mcp-servers/{id}"),
        None,
    )
    .await;
    assert_eq!(s, 200);
    assert_eq!(q::<i64>(&h, "SELECT COUNT(*) FROM mcp_servers"), 0);
    assert_eq!(q::<i64>(&h, "SELECT COUNT(*) FROM bot_capabilities"), 0);
    assert_eq!(
        q::<i64>(
            &h,
            "SELECT COUNT(*) FROM vault_items WHERE status = 'active' AND template = 'connection_credential'"
        ),
        0
    );
    assert_eq!(secret(&h, "s_dev", &sid), None);
    nowhere_on_disk(&h, &["ghp-SECRET-one", "ghp-SECRET-two"]);
}

#[tokio::test(flavor = "multi_thread")]
async fn what_cannot_be_given_to_a_harness_is_refused() {
    let h = setup();
    let post = |b: Value| {
        let h = &h;
        async move { req(h, "POST", "/capabilities/mcp-servers", Some(b)).await }
    };
    for (body, field) in [
        (
            json!({"name": "a b", "transport": "stdio", "command": "x"}),
            "name",
        ),
        (
            json!({"name": "Steve", "transport": "stdio", "command": "x"}),
            "name",
        ),
        (
            json!({"name": "a", "transport": "ws", "command": "x"}),
            "transport",
        ),
        (json!({"name": "a", "transport": "stdio"}), "command"),
        (
            json!({"name": "a", "transport": "http", "url": "file:///etc"}),
            "url",
        ),
        (
            json!({"name": "a", "transport": "stdio", "command": "x", "env": [{"name": "X", "value": "v"}, {"name": "X", "value": "w"}]}),
            "env.1.name",
        ),
        (
            json!({"name": "a", "transport": "http", "url": "https://x.test", "headers": [{"name": "A", "value": "a\r\nB: b"}]}),
            "headers.0.value",
        ),
        (json!({"transport": "stdio", "command": "x"}), "name"),
    ] {
        let (s, v) = post(body.clone()).await;
        assert_eq!(s, 400, "{body} → {v}");
        assert!(
            v["error"].as_str().unwrap_or("").contains(field),
            "{body} → {v}, wanted {field}"
        );
    }
    let (s, _) = post(json!({"name": "a", "transport": "stdio", "command": "x", "env": [{"name": "K", "secret": true}]})).await;
    assert_eq!(s, 400);
    // one name per workspace, in any case
    let (_, files) = post(json!({"name": "files", "transport": "stdio", "command": "x"})).await;
    assert_eq!(
        post(json!({"name": "FILES", "transport": "stdio", "command": "y"}))
            .await
            .0,
        409
    );
    // another workspace neither sees nor assigns this one
    let id = files["id"].as_str().unwrap();
    let (_, other) = call(
        &h.api,
        Some(&caller(WS2, ME)),
        "GET",
        "/capabilities/mcp-servers",
        None,
    )
    .await;
    assert_eq!(other, json!([]));
    let (s, _) = call(
        &h.api,
        Some(&caller(WS2, ME)),
        "DELETE",
        &format!("/capabilities/mcp-servers/{id}"),
        None,
    )
    .await;
    assert_eq!(s, 404);
    let (s, _) = req(
        &h,
        "PUT",
        "/bots/dev/capabilities/mcp/bmcp_nope",
        Some(json!({})),
    )
    .await;
    assert_eq!(s, 404);
    let (s, _) = req(
        &h,
        "PUT",
        &format!("/bots/nobody/capabilities/mcp/{id}"),
        Some(json!({})),
    )
    .await;
    assert_eq!(s, 404);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_bots_skills_stay_within_what_its_node_is_given() {
    let h = setup();
    let mut ids = vec![];
    for (n, kib) in [
        ("one", 31),
        ("two", 31),
        ("three", 31),
        ("four", 31),
        ("five", 4),
        ("six", 1),
    ] {
        let (s, v) = req(
            &h,
            "POST",
            "/capabilities/skills",
            Some(json!({"name": n, "description": format!("Use for {n}"), "content": "x".repeat(kib * 1024)})),
        )
        .await;
        assert_eq!(s, 200, "{v}");
        ids.push(v["id"].as_str().unwrap().to_string());
    }
    // 4 × 31 KiB + 4 KiB is exactly what a Bot may have on
    for id in &ids[..5] {
        let (s, v) = req(
            &h,
            "PUT",
            &format!("/bots/dev/capabilities/skill/{id}"),
            Some(json!({})),
        )
        .await;
        assert_eq!(s, 200, "{v}");
    }
    // one more does not fit; switched off it is kept
    let (s, v) = req(
        &h,
        "PUT",
        &format!("/bots/dev/capabilities/skill/{}", ids[5]),
        Some(json!({})),
    )
    .await;
    assert_eq!(s, 400, "{v}");
    let (s, _) = req(
        &h,
        "PUT",
        &format!("/bots/dev/capabilities/skill/{}", ids[5]),
        Some(json!({"enabled": false})),
    )
    .await;
    assert_eq!(s, 200);
    // a body that grows past it for a Bot that has it on is refused; one that is off may grow
    let (s, _) = req(
        &h,
        "PATCH",
        &format!("/capabilities/skills/{}", ids[4]),
        Some(json!({"content": "y".repeat(5 * 1024)})),
    )
    .await;
    assert_eq!(s, 400);
    let (s, _) = req(
        &h,
        "PATCH",
        &format!("/capabilities/skills/{}", ids[5]),
        Some(json!({"content": "y".repeat(32 * 1024)})),
    )
    .await;
    assert_eq!(s, 200);
    let (_, mine) = req(&h, "GET", "/bots/dev/capabilities", None).await;
    let names: Vec<&str> = mine["skills"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["five", "four", "one", "six", "three", "two"]);
    let (_, bad) = req(
        &h,
        "POST",
        "/capabilities/skills",
        Some(json!({"name": "Bad Name", "description": "d"})),
    )
    .await;
    assert!(
        bad["error"].as_str().unwrap_or("").contains("name"),
        "{bad}"
    );
    // two skills of one name cannot both be a Bot's: the library has one name once
    let (s, _) = req(
        &h,
        "POST",
        "/capabilities/skills",
        Some(json!({"name": "one", "description": "d"})),
    )
    .await;
    assert_eq!(s, 409);
}

/// A remote agent on this machine: its card at the well-known path, a task it finishes on the
/// second look, and the header it wants.
async fn fake_agent() -> (String, Arc<std::sync::Mutex<Vec<Value>>>) {
    use axum::routing::{get, post};
    let seen: Arc<std::sync::Mutex<Vec<Value>>> = Arc::default();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let card = json!({
        "protocolVersion": "0.3.0", "name": "Trip Planner", "description": "Plans trips.",
        "url": format!("{base}/a2a"), "version": "1.2.0",
        "skills": [{"id": "plan", "name": "Plan a trip", "description": "Itineraries by day", "tags": ["travel"]}],
        "capabilities": {}, "defaultInputModes": ["text/plain"], "defaultOutputModes": ["text/plain"]
    });
    let log = seen.clone();
    let app = Router::new()
        .route("/.well-known/agent-card.json", get(move || async move { axum::Json(card) }))
        .route(
            "/a2a",
            post(move |headers: axum::http::HeaderMap, axum::Json(b): axum::Json<Value>| {
                let log = log.clone();
                async move {
                    let key = headers.get("x-api-key").and_then(|v| v.to_str().ok()).unwrap_or("");
                    log.lock().unwrap().push(json!({"key": key, "body": b.clone()}));
                    if key != "agent-KEY" {
                        return axum::Json(json!({"jsonrpc": "2.0", "id": b["id"], "error": {"code": -32001, "message": "unauthorized"}}));
                    }
                    let result = match b["method"].as_str() {
                        Some("message/send") => json!({"kind": "task", "id": "t1", "contextId": "c1", "status": {"state": "working"}}),
                        _ => json!({"kind": "task", "id": "t1", "contextId": "c1", "status": {"state": "completed"},
                            "artifacts": [{"name": "itinerary", "parts": [{"kind": "text", "text": "Day 1: Kyoto"}]}]}),
                    };
                    axum::Json(json!({"jsonrpc": "2.0", "id": b["id"], "result": result}))
                }
            }),
        );
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (base, seen)
}

#[tokio::test(flavor = "multi_thread")]
async fn a_remote_agent_is_added_by_its_card_and_sent_a_task() {
    let h = setup();
    let (base, seen) = fake_agent().await;
    let (s, p) = req(
        &h,
        "POST",
        "/capabilities/a2a-agents/preview",
        Some(json!({"card_url": base})),
    )
    .await;
    assert_eq!(s, 200, "{p}");
    assert_eq!(p["card"]["name"], "Trip Planner");
    assert_eq!(p["card_url"], format!("{base}/.well-known/agent-card.json"));
    // a secret is needed for header auth
    let (s, _) = req(
        &h,
        "POST",
        "/capabilities/a2a-agents",
        Some(json!({"card_url": base, "auth": {"scheme": "header", "header": "X-Api-Key"}})),
    )
    .await;
    assert_eq!(s, 400);
    let (s, a) = req(
        &h,
        "POST",
        "/capabilities/a2a-agents",
        Some(json!({"card_url": base, "auth": {"scheme": "header", "header": "X-Api-Key"}, "secret": "agent-KEY", "bot": "dev"})),
    )
    .await;
    assert_eq!(s, 200, "{a}");
    assert_eq!(a["name"], "trip_planner");
    assert_eq!(a["tool"], "steve_a2a_trip_planner");
    assert_eq!(a["skills"][0]["name"], "Plan a trip");
    assert_eq!(a["has_secret"], true);
    assert!(!a.to_string().contains("agent-KEY"));
    // dev's sessions get the tool; doc's do not
    let tools = h.svc.a2a_tools("s_dev");
    assert_eq!(tools.len(), 1);
    assert_eq!(tools[0]["name"], "steve_a2a_trip_planner");
    assert!(
        tools[0]["description"]
            .as_str()
            .unwrap()
            .contains("Plan a trip: Itineraries by day")
    );
    assert!(h.svc.a2a_tools("s_doc").is_empty());
    assert!(
        h.svc
            .a2a_call("s_doc", "steve_a2a_trip_planner", &json!({"message": "hi"}))
            .await
            .is_err()
    );
    // the task: sent, still working, looked at again until it is done
    let out = h
        .svc
        .a2a_call(
            "s_dev",
            "steve_a2a_trip_planner",
            &json!({"message": "Plan 2 days in Kyoto"}),
        )
        .await
        .unwrap();
    assert!(
        out.contains("completed") && out.contains("Day 1: Kyoto") && out.contains("task_id t1"),
        "{out}"
    );
    let calls = seen.lock().unwrap().clone();
    assert_eq!(calls[0]["key"], "agent-KEY");
    assert_eq!(calls[0]["body"]["method"], "message/send");
    assert_eq!(
        calls[0]["body"]["params"]["message"]["parts"][0]["text"],
        "Plan 2 days in Kyoto"
    );
    assert_eq!(calls[1]["body"]["method"], "tasks/get");
    // renamed, the tool follows; the secret stays
    let id = a["id"].as_str().unwrap();
    let (s, v) = req(
        &h,
        "PATCH",
        &format!("/capabilities/a2a-agents/{id}"),
        Some(json!({"name": "trips", "refresh": true})),
    )
    .await;
    assert_eq!(s, 200, "{v}");
    assert_eq!(h.svc.a2a_tools("s_dev")[0]["name"], "steve_a2a_trips");
    assert!(
        h.svc
            .a2a_call("s_dev", "steve_a2a_trips", &json!({"task_id": "t1"}))
            .await
            .unwrap()
            .contains("Day 1")
    );
    // a page that is not a card
    let (s, _) = req(
        &h,
        "POST",
        "/capabilities/a2a-agents/preview",
        Some(json!({"card_url": format!("{base}/nothing.json")})),
    )
    .await;
    assert_eq!(s, 400);
    let (s, _) = req(
        &h,
        "DELETE",
        &format!("/capabilities/a2a-agents/{id}"),
        None,
    )
    .await;
    assert_eq!(s, 200);
    assert!(h.svc.a2a_tools("s_dev").is_empty());
    nowhere_on_disk(&h, &["agent-KEY"]);
}
