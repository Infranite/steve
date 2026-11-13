//! A Bot's MCP servers through `/bots/:id/mcp-servers`: validation, the vault keeping every secret
//! value (never the row, an answer or the audit), kept and replaced secrets, the switch, deletion,
//! and what a session's Node may take.

use std::sync::Arc;

use axum::Router;
use serde_json::{Value, json};
use steve_modules::bot_mcp::{BotMcp, routes, secret_id};
use steve_modules::testkit::{ME, TestEnv, WS, WS2, call, caller};
use steve_modules::vault::Vault;
use steve_modules::vault::keystore::FileKeyStore;

struct H {
    env: TestEnv,
    _keys: tempfile::TempDir,
    svc: BotMcp,
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
    let svc = BotMcp::new(vault);
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
        .secret(session, id)
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
async fn servers_are_kept_with_their_secrets_in_the_vault() {
    let h = setup();
    let (s, v) = req(
        &h,
        "POST",
        "/bots/dev/mcp-servers",
        Some(
            json!({"name": "github", "transport": "stdio", "command": "npx",
            "args": ["-y", "@modelcontextprotocol/server-github"],
            "env": [{"name": "LOG", "value": "debug"},
                    {"name": "GITHUB_TOKEN", "value": "ghp-SECRET-one", "secret": true}]}),
        ),
    )
    .await;
    assert_eq!(s, 200, "{v}");
    let id = v["id"].as_str().unwrap().to_string();
    assert_eq!(
        v["env"],
        json!([{"name": "LOG", "value": "debug"}, {"name": "GITHUB_TOKEN", "secret": true}])
    );
    assert_eq!(v["enabled"], true);
    let sid = secret_id(&id, "env:GITHUB_TOKEN");
    assert_eq!(secret(&h, "s_dev", &sid).as_deref(), Some("ghp-SECRET-one"));
    // only the session of its own Bot, only a secret it has
    assert_eq!(secret(&h, "s_doc", &sid), None);
    assert_eq!(secret(&h, "s_dev", &secret_id(&id, "env:LOG")), None);
    // the use is in the vault's log, under the server
    assert_eq!(
        q::<i64>(
            &h,
            &format!("SELECT COUNT(*) FROM secret_uses WHERE actor_id = '{id}'")
        ),
        1
    );
    let (_, list) = req(&h, "GET", "/bots/dev/mcp-servers", None).await;
    assert_eq!(list.as_array().unwrap().len(), 1);
    assert_eq!(
        list[0]["env"][1],
        json!({"name": "GITHUB_TOKEN", "secret": true})
    );

    // saved again without the value: the vault's stays
    let (s, v) = req(
        &h,
        "PATCH",
        &format!("/bots/dev/mcp-servers/{id}"),
        Some(json!({"args": ["-y", "server-github@2"],
            "env": [{"name": "GITHUB_TOKEN", "secret": true}, {"name": "LOG", "value": "info"}]})),
    )
    .await;
    assert_eq!(s, 200, "{v}");
    assert_eq!(v["args"], json!(["-y", "server-github@2"]));
    assert_eq!(secret(&h, "s_dev", &sid).as_deref(), Some("ghp-SECRET-one"));
    // a new value replaces it
    let (s, _) = req(
        &h,
        "PATCH",
        &format!("/bots/dev/mcp-servers/{id}"),
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

    // the switch: a disabled server gives nothing
    let (s, v) = req(
        &h,
        "PATCH",
        &format!("/bots/dev/mcp-servers/{id}"),
        Some(json!({"enabled": false})),
    )
    .await;
    assert_eq!((s, v["enabled"].clone()), (200, json!(false)));
    assert_eq!(secret(&h, "s_dev", &sid), None);
    req(
        &h,
        "PATCH",
        &format!("/bots/dev/mcp-servers/{id}"),
        Some(json!({"enabled": true})),
    )
    .await;

    // to http: the stdio fields and the env secret go, a header secret comes
    let (s, v) = req(
        &h,
        "PATCH",
        &format!("/bots/dev/mcp-servers/{id}"),
        Some(json!({"transport": "http", "url": "https://api.githubcopilot.com/mcp/",
            "headers": [{"name": "Authorization", "value": "Bearer SECRET-three", "secret": true}]})),
    )
    .await;
    assert_eq!(s, 200, "{v}");
    assert_eq!(v["command"], Value::Null);
    assert_eq!(v["env"], json!([]));
    assert_eq!(secret(&h, "s_dev", &sid), None);
    assert_eq!(
        secret(&h, "s_dev", &secret_id(&id, "header:Authorization")).as_deref(),
        Some("Bearer SECRET-three")
    );

    let actions: String = q(
        &h,
        &format!(
            "SELECT group_concat(action, ',') FROM (SELECT action FROM audit WHERE target = '{id}' ORDER BY id)"
        ),
    );
    assert_eq!(
        actions,
        "bot_mcp.create,bot_mcp.update,bot_mcp.update,bot_mcp.disable,bot_mcp.enable,bot_mcp.update"
    );

    // deleted: the row and its vault item
    let (s, _) = req(&h, "DELETE", &format!("/bots/dev/mcp-servers/{id}"), None).await;
    assert_eq!(s, 200);
    assert_eq!(q::<i64>(&h, "SELECT COUNT(*) FROM bot_mcp_servers"), 0);
    assert_eq!(
        q::<i64>(
            &h,
            "SELECT COUNT(*) FROM vault_items WHERE status = 'active' AND template = 'connection_credential'"
        ),
        0
    );
    assert_eq!(
        secret(&h, "s_dev", &secret_id(&id, "header:Authorization")),
        None
    );
    nowhere_on_disk(&h, &["ghp-SECRET-one", "ghp-SECRET-two", "SECRET-three"]);
}

#[tokio::test(flavor = "multi_thread")]
async fn what_cannot_be_given_to_a_harness_is_refused() {
    let h = setup();
    let post = |b: Value| {
        let h = &h;
        async move { req(h, "POST", "/bots/dev/mcp-servers", Some(b)).await }
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
        (json!({"name": "a", "transport": "sse"}), "url"),
        (
            json!({"name": "a", "transport": "stdio", "command": "x", "env": [{"name": "1X", "value": "v"}]}),
            "env.0.name",
        ),
        (
            json!({"name": "a", "transport": "stdio", "command": "x", "env": [{"name": "X", "value": "v"}, {"name": "X", "value": "w"}]}),
            "env.1.name",
        ),
        (
            json!({"name": "a", "transport": "stdio", "command": "x", "env": [{"name": "X"}]}),
            "env.0.value",
        ),
        (
            json!({"name": "a", "transport": "http", "url": "https://x.test", "headers": [{"name": "A", "value": "a\r\nB: b"}]}),
            "headers.0.value",
        ),
        (
            json!({"name": "a", "transport": "http", "url": "https://x.test", "headers": [{"name": "Bad Header", "value": "v"}]}),
            "headers.0.name",
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
    // a secret needs its value the first time
    let (s, v) = post(json!({"name": "a", "transport": "stdio", "command": "x", "env": [{"name": "K", "secret": true}]})).await;
    assert_eq!(s, 400, "{v}");
    // one name per Bot, in any case; another Bot may use it
    assert_eq!(
        post(json!({"name": "files", "transport": "stdio", "command": "x"}))
            .await
            .0,
        200
    );
    assert_eq!(
        post(json!({"name": "FILES", "transport": "stdio", "command": "y"}))
            .await
            .0,
        409
    );
    let (s, _) = req(
        &h,
        "POST",
        "/bots/doc/mcp-servers",
        Some(json!({"name": "files", "transport": "sse", "url": "http://127.0.0.1:3000/sse"})),
    )
    .await;
    assert_eq!(s, 200);
    // a Bot or server of another workspace, or another Bot's server, is not there
    let (s, _) = call(
        &h.api,
        Some(&caller(WS2, ME)),
        "GET",
        "/bots/dev/mcp-servers",
        None,
    )
    .await;
    assert_eq!(s, 404);
    let (_, list) = req(&h, "GET", "/bots/doc/mcp-servers", None).await;
    let doc_id = list[0]["id"].as_str().unwrap();
    let (s, _) = req(
        &h,
        "DELETE",
        &format!("/bots/dev/mcp-servers/{doc_id}"),
        None,
    )
    .await;
    assert_eq!(s, 404);
    assert_eq!(q::<i64>(&h, "SELECT COUNT(*) FROM bot_mcp_servers"), 2);
}
