//! The new modules as mounted in the binary: behind the login with a `Caller`, their SSE events scoped to
//! the workspace, the `tz` setting, and module config drafts dispatched to steve-modules. Runs the real
//! binary on a temp STEVE_HOME (file keystore, fake claude CLI, no agents).

use std::net::TcpListener;
use std::process::{Child, Command};
use std::time::Duration;

use futures::StreamExt;

/// The event modules, in process (S1/S2, G2, E1, C1/F1/F5/F6).
mod wired;
use serde_json::{Value, json};

struct Hub {
    child: Child,
    base: String,
    home: tempfile::TempDir,
}

impl Drop for Hub {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn free_port() -> u16 {
    TcpListener::bind("127.0.0.1:0")
        .and_then(|l| l.local_addr())
        .map(|a| a.port())
        .expect("port")
}

async fn start() -> Hub {
    let home = tempfile::tempdir().expect("tmp");
    let web = home.path().join("no-web");
    start_in(home, &web).await
}

#[allow(clippy::zombie_processes)] // Hub::drop kills and waits
async fn start_in(home: tempfile::TempDir, web_dir: &std::path::Path) -> Hub {
    // a free port can be taken by another test's Hub before this one binds it: that Hub would answer
    // our requests. Ours prints its address once it has bound; a Hub that could not bind exits, and
    // the start is tried again on another port.
    let mut last = String::new();
    for _ in 0..5 {
        let port = free_port();
        assert_ne!(port, 8440);
        let (out, log) = (home.path().join("hub.out"), home.path().join("hub.log"));
        let mut child = spawn_hub(&home, web_dir, port, &out, &log);
        let base = format!("http://127.0.0.1:{port}");
        let bound = format!("on {base}");
        // 30 s: a loaded machine (load average ~100) has taken more than 10
        for _ in 0..600 {
            if child.try_wait().ok().flatten().is_some() {
                break;
            }
            if std::fs::read_to_string(&out).is_ok_and(|o| o.contains(&bound))
                && reqwest::get(format!("{base}/healthz"))
                    .await
                    .is_ok_and(|r| r.status().is_success())
            {
                return Hub { child, base, home };
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        let _ = child.kill();
        let _ = child.wait();
        last = std::fs::read_to_string(&log).unwrap_or_default();
        if !last.contains("Address already in use") {
            break;
        }
    }
    let tail: Vec<&str> = last.lines().rev().take(40).collect();
    panic!(
        "hub did not start; its log ends:\n{}",
        tail.into_iter().rev().collect::<Vec<_>>().join("\n")
    );
}

fn spawn_hub(
    home: &tempfile::TempDir,
    web_dir: &std::path::Path,
    port: u16,
    out: &std::path::Path,
    log: &std::path::Path,
) -> Child {
    let fake = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/contract/fake-claude.mjs"
    );
    Command::new(env!("CARGO_BIN_EXE_steve-hub"))
        .env("HOME", home.path())
        .env("STEVE_HOME", home.path().join("steve"))
        .env("STEVE_PORT", port.to_string())
        .env("STEVE_HOST", "127.0.0.1")
        .env("STEVE_PUBLIC_URL", format!("http://127.0.0.1:{port}"))
        .env("STEVE_DEV_LOGIN", "1")
        .env("STEVE_REPO", "/nonexistent")
        .env("STEVE_VAULT_KEYSTORE", "file")
        .env("STEVE_FORBID_REAL_HOSTS", "1")
        .env("STEVE_NO_RESUME", "1")
        .env("STEVE_NODE_MODE", "off")
        .env("CLAUDE_CODE_EXECUTABLE", fake)
        .env("STEVE_ADAPTER_CLAUDE", fake)
        .env("STEVE_ADAPTER_CODEX", fake)
        .env("CODEX_EXECUTABLE", home.path().join("codex"))
        .env_remove("CLAUDE_CONFIG_DIR")
        .env_remove("CODEX_HOME")
        .env("STEVE_WEB_DIR", web_dir)
        .env_remove("STEVE_OIDC_ISSUER")
        // the machine's own knowledge store (STEVE_KB_STORE=openviking on a Steve host) is not the test's
        .env_remove("STEVE_KB_STORE")
        .env_remove("STEVE_OPENVIKING_KEY_FILE")
        .env_remove("STEVE_OPENVIKING_URL")
        .env_remove("STEVE_OPENVIKING_ACCOUNT")
        .stdout(std::fs::File::create(out).expect("out"))
        .stderr(std::fs::File::create(log).expect("log"))
        .spawn()
        .expect("spawn hub")
}

struct Client {
    http: reqwest::Client,
    base: String,
    cookie: String,
}

impl Client {
    async fn login(hub: &Hub) -> Client {
        let http = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .expect("client");
        let r = http
            .get(format!("{}/auth/dev", hub.base))
            .send()
            .await
            .expect("dev login");
        let cookie = r
            .headers()
            .get_all("set-cookie")
            .iter()
            .filter_map(|v| v.to_str().ok())
            .find(|c| c.starts_with("steve_session="))
            .map(|c| c.split(';').next().unwrap_or_default().to_string())
            .expect("session cookie");
        Client {
            http,
            base: hub.base.clone(),
            cookie,
        }
    }

    async fn call(
        &self,
        method: &str,
        path: &str,
        body: Option<Value>,
        ws: Option<&str>,
    ) -> (u16, Value) {
        let m = reqwest::Method::from_bytes(method.as_bytes()).expect("method");
        let mut req = self
            .http
            .request(m, format!("{}{path}", self.base))
            .header("cookie", &self.cookie);
        if let Some(w) = ws {
            req = req.header("x-steve-ws", w);
        }
        if let Some(b) = body {
            req = req.json(&b);
        }
        let r = req.send().await.expect("send");
        let status = r.status().as_u16();
        let text = r.text().await.unwrap_or_default();
        (
            status,
            serde_json::from_str(&text).unwrap_or(Value::String(text)),
        )
    }

    /// Frames of `/api/stream` for `ws` while `during` runs.
    async fn frames<F: std::future::Future<Output = ()>>(&self, ws: &str, during: F) -> Vec<Value> {
        let r = self
            .http
            .get(format!("{}/api/stream", self.base))
            .header("cookie", &self.cookie)
            .header("x-steve-ws", ws)
            .send()
            .await
            .expect("stream");
        let mut body = r.bytes_stream();
        tokio::time::sleep(Duration::from_millis(200)).await;
        during.await;
        let mut buf = String::new();
        let deadline = tokio::time::Instant::now() + Duration::from_millis(800);
        while let Ok(Some(Ok(chunk))) = tokio::time::timeout_at(deadline, body.next()).await {
            buf.push_str(&String::from_utf8_lossy(&chunk));
        }
        buf.split("\n\n")
            .filter_map(|f| f.strip_prefix("data: "))
            .filter_map(|d| serde_json::from_str(d).ok())
            .collect()
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn modules_are_mounted_behind_the_login() {
    let hub = start().await;
    let anon = reqwest::get(format!("{}/api/contacts", hub.base))
        .await
        .expect("anon");
    assert_eq!(
        anon.status().as_u16(),
        401,
        "module routes need the session"
    );

    let c = Client::login(&hub).await;
    for path in [
        "/api/contacts",
        "/api/systems",
        "/api/accounts",
        "/api/schedules",
        "/api/vault/status",
        "/api/teams",
        "/api/me/contact",
    ] {
        let (s, v) = c.call("GET", path, None, None).await;
        assert!(s == 200, "{path}: {s} {v}");
    }
    let (s, v) = c
        .call(
            "POST",
            "/api/contacts",
            Some(json!({ "name": "张三" })),
            None,
        )
        .await;
    assert_eq!(s, 200, "{v}");
    let (_, list) = c.call("GET", "/api/contacts", None, None).await;
    assert!(list.to_string().contains("张三"), "{list}");
}

#[tokio::test(flavor = "multi_thread")]
async fn module_events_stay_in_their_workspace() {
    let hub = start().await;
    let c = Client::login(&hub).await;
    let (_, w) = c
        .call(
            "POST",
            "/api/workspaces",
            Some(json!({ "name": "另一个" })),
            None,
        )
        .await;
    let ws2 = w["id"].as_str().expect("ws id").to_string();

    let mine = c
        .frames("default", async {
            c.call(
                "POST",
                "/api/contacts",
                Some(json!({ "name": "李四" })),
                None,
            )
            .await;
        })
        .await;
    assert!(
        mine.iter()
            .any(|e| e["type"] == "party" && e["ws"] == "default"),
        "{mine:?}"
    );
    let other = c
        .frames(&ws2, async {
            c.call(
                "POST",
                "/api/contacts",
                Some(json!({ "name": "王五" })),
                None,
            )
            .await;
        })
        .await;
    assert!(
        !other.iter().any(|e| e["type"] == "party"),
        "a module event leaked into another workspace: {other:?}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn tz_setting() {
    let hub = start().await;
    let c = Client::login(&hub).await;
    let (_, schema) = c.call("GET", "/api/settings/schema", None, None).await;
    let tz = schema["general"]
        .as_array()
        .and_then(|g| g.iter().find(|s| s["key"] == "tz"))
        .cloned()
        .expect("tz in general");
    assert_eq!(tz["def"], "Asia/Taipei");
    assert_eq!(
        c.call("PATCH", "/api/settings", Some(json!({ "tz": "UTC" })), None)
            .await
            .0,
        200
    );
    assert_eq!(
        c.call(
            "PATCH",
            "/api/settings",
            Some(json!({ "tz": "Mars/Base" })),
            None
        )
        .await,
        (400, json!({ "error": "设置项不对：tz" }))
    );
    let (_, st) = c.call("GET", "/api/state", None, None).await;
    assert_eq!(st["settings"]["tz"], "UTC");
}

#[tokio::test(flavor = "multi_thread")]
async fn model_settings() {
    let hub = start().await;
    let c = Client::login(&hub).await;
    let (code, v) = c.call("GET", "/api/settings/models", None, None).await;
    assert_eq!(code, 200, "{v}");
    let ps = v["purposes"].as_array().expect("purposes");
    let keys: Vec<&str> = ps.iter().filter_map(|p| p["key"].as_str()).collect();
    assert_eq!(
        keys,
        [
            "route",
            "chat",
            "research",
            "title",
            "extract",
            "disclosure",
            "yolo",
            "draft",
            "flow",
            "mail",
            "compose"
        ]
    );
    assert_eq!(ps[0]["label"], "分流");
    // Steve is a bot: the seeded Steve bot (Claude Code · claude-sonnet-5-5) makes the judgments;
    // routing keeps its fast list as a recommended override written once at boot
    assert_eq!(v["steve"]["runs_on"], "claude-code");
    assert_eq!(v["steve"]["model"], "claude-sonnet-5-5");
    assert_eq!(
        ps[0]["value"],
        json!({"harness": "claude-code", "models": ["claude-haiku-5-5", "claude-sonnet-5-5"]})
    );
    assert_eq!(ps[0]["follows"], false);
    assert_eq!(ps[0]["recommended"], ps[0]["value"]);
    assert_eq!(ps[2]["follows"], true);
    assert_eq!(
        ps[2]["effective"],
        json!({"harness": "claude-code", "models": ["claude-sonnet-5-5", "claude-opus-5-5"]})
    );
    assert_eq!(v["agents"][0]["default"], "claude-opus-5-5");
    assert_eq!(v["efforts"][4]["key"], "max");
    assert_eq!(v["harnesses"][1]["id"], "codex");
    let (code, v) = c
        .call(
            "PATCH",
            "/api/settings/models",
            Some(json!({"purposes": {"route": {"harness": "codex", "models": ["gpt-6-sol"], "effort": "low"}}, "agents": {"codex": "gpt-6-sol"}})),
            None,
        )
        .await;
    assert_eq!(code, 200, "{v}");
    assert_eq!(
        v["purposes"][0]["effective"],
        json!({"harness": "codex", "models": ["gpt-6-sol"], "effort": "low"})
    );
    assert_eq!(v["agents"][1]["value"], "gpt-6-sol");
    assert_eq!(
        c.call(
            "PATCH",
            "/api/settings/models",
            Some(json!({"purposes": {"route": {"models": ["a b"]}}})),
            None
        )
        .await,
        (400, json!({"error": "分流 的模型名不对：a b"}))
    );
    assert_eq!(
        c.call(
            "PATCH",
            "/api/settings/models",
            Some(json!({"purposes": {"nope": null}})),
            None
        )
        .await
        .0,
        400
    );
    // in the workspace settings (bootstrap) too, so every client sees the change
    let (_, st) = c.call("GET", "/api/state", None, None).await;
    assert_eq!(
        st["settings"]["models.route"]["models"],
        json!(["gpt-6-sol"])
    );
    // the Steve bot's harness and model move every purpose that follows it
    let (code, _) = c
        .call(
            "PATCH",
            "/api/bots/steve",
            Some(json!({"harness": "codex", "model": "gpt-6-luna"})),
            None,
        )
        .await;
    assert_eq!(code, 200);
    let (_, v) = c.call("GET", "/api/settings/models", None, None).await;
    assert_eq!(
        v["purposes"][1]["effective"],
        json!({"harness": "codex", "models": ["gpt-6-luna"]})
    );
}

/// `GET /api/system/models`: Claude Code's list is the user's model picker, read without the key
/// helper or keys; Codex's is its catalogue. A harness without a list is refused.
#[tokio::test(flavor = "multi_thread")]
async fn system_models_lists_what_each_harness_offers_and_never_the_keys() {
    let home = tempfile::tempdir().expect("tmp");
    let h = home.path();
    std::fs::create_dir_all(h.join(".claude")).unwrap();
    let helper = h.join("key-helper.sh");
    std::fs::write(
        &helper,
        format!(
            "#!/bin/sh\ntouch '{}'\necho sk-ant-HELPER\n",
            h.join("helper-ran").display()
        ),
    )
    .unwrap();
    std::fs::write(
        h.join(".claude/settings.json"),
        json!({
            "apiKeyHelper": helper.display().to_string(),
            "env": {"ANTHROPIC_AUTH_TOKEN": "sk-ant-ENVTOKEN"},
            "modelPicker": {"options": [
                {"model": "claude-sonnet-5-5", "label": "claude-sonnet-5-5"},
                {"model": "kiro/claude-opus-5-5", "label": "kiro/claude-opus-5-5"},
                {"model": "gemini-3-flash", "label": "gemini-3-flash"},
            ]},
        })
        .to_string(),
    )
    .unwrap();
    std::fs::create_dir_all(h.join(".codex")).unwrap();
    std::fs::write(h.join(".codex/config.toml"), "model = \"gpt-6-luna\"\n").unwrap();
    std::fs::write(
        h.join("codex-models.json"),
        json!({"models": [{"slug": "gpt-6-luna", "display_name": "GPT-6-Luna", "visibility": "list", "priority": 1}]}).to_string(),
    )
    .unwrap();
    std::fs::write(
        h.join("codex"),
        format!(
            "#!/bin/sh\n[ \"$1 $2\" = 'debug models' ] && cat '{}'\n",
            h.join("codex-models.json").display()
        ),
    )
    .unwrap();
    {
        use std::os::unix::fs::PermissionsExt;
        for f in ["codex", "key-helper.sh"] {
            std::fs::set_permissions(h.join(f), std::fs::Permissions::from_mode(0o755)).unwrap();
        }
    }
    let web = h.join("no-web");
    let hub = start_in(home, &web).await;
    let c = Client::login(&hub).await;
    let (code, v) = c
        .call("GET", "/api/system/models?harness=claude-code", None, None)
        .await;
    assert_eq!(code, 200, "{v}");
    let ids: Vec<&str> = v["models"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|m| m["id"].as_str())
        .collect();
    assert_eq!(
        ids,
        [
            "claude-sonnet-5-5",
            "kiro/claude-opus-5-5",
            "gemini-3-flash"
        ]
    );
    assert_eq!(
        v["families"],
        json!(["Claude Opus", "Claude Sonnet", "Gemini"])
    );
    let text = v.to_string();
    for secret in ["sk-ant", "key-helper", "apiKeyHelper", "ENVTOKEN"] {
        assert!(!text.contains(secret), "{secret}: {text}");
    }
    let (code, v) = c
        .call("GET", "/api/system/models?harness=codex", None, None)
        .await;
    assert_eq!(code, 200, "{v}");
    assert_eq!(v["models"][0]["id"], "gpt-6-luna");
    assert_eq!(v["default"], "gpt-6-luna");
    assert_eq!(
        c.call("GET", "/api/system/models?harness=gemini", None, None)
            .await
            .0,
        400
    );
    // nothing ran the key helper
    assert!(!hub.home.path().join("helper-ran").exists());
}

#[tokio::test(flavor = "multi_thread")]
async fn module_config_drafts_go_to_the_modules() {
    let hub = start().await;
    let c = Client::login(&hub).await;
    // `schedule` is a module target kind: accepted by validation and drafted by steve-modules
    let (s, d) = c
        .call(
            "POST",
            "/api/config/draft",
            Some(json!({ "target": { "kind": "schedule" }, "text": "每天早上九点给我汇报" })),
            None,
        )
        .await;
    assert_eq!(s, 200, "{d}");
    let id = d["id"].as_str().expect("draft id").to_string();
    // the stored changeset answers apply (not core's in-memory "这份草稿已经失效" 500)
    let (s, v) = c
        .call("POST", &format!("/api/config/{id}/apply"), None, None)
        .await;
    assert!(s == 200 || s == 409 || s == 400, "{s} {v}");
    // a module draft is dropped once; a core draft id is dropped even when it is already gone
    let (s, d) = c
        .call(
            "POST",
            "/api/config/draft",
            Some(json!({ "target": { "kind": "schedule" }, "text": "每天早上九点给我汇报" })),
            None,
        )
        .await;
    assert_eq!(s, 200, "{d}");
    let drop = format!("/api/config/{}/drop", d["id"].as_str().expect("draft id"));
    assert_eq!(
        c.call("POST", &drop, None, None).await,
        (200, json!({ "ok": true }))
    );
    assert_eq!(c.call("POST", &drop, None, None).await.0, 409);
    assert_eq!(
        c.call("POST", "/api/config/chg_0000000000000000/drop", None, None)
            .await,
        (200, json!({ "ok": true }))
    );
    // TS kinds still go to the core
    let (s, v) = c
        .call("POST", "/api/config/chg_0000000000000000/apply", None, None)
        .await;
    assert_eq!(
        (s, v),
        (500, json!({ "error": "这份草稿已经失效，请重新说一次" }))
    );
    // contacts registered its targets (and merge its decision handlers)
    let (s, v) = c
        .call(
            "POST",
            "/api/config/draft",
            Some(json!({ "target": { "kind": "contact" }, "text": "加一个联系人叫钱七" })),
            None,
        )
        .await;
    assert_ne!(v, json!({ "error": "参数不对：target.kind" }), "{s} {v}");
    // an unknown kind is still a validation error
    assert_eq!(
        c.call(
            "POST",
            "/api/config/draft",
            Some(json!({ "target": { "kind": "nope" }, "text": "x" })),
            None
        )
        .await,
        (400, json!({ "error": "参数不对：target.kind" }))
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_module_decision_is_undone_through_the_route() {
    let hub = start().await;
    let c = Client::login(&hub).await;
    let db =
        rusqlite::Connection::open(hub.home.path().join("steve/data/steve.sqlite3")).expect("db");
    db.busy_timeout(Duration::from_secs(5)).expect("timeout");
    db.execute_batch(
        "INSERT INTO attr_defs (id, workspace_id, applies, key, label, type, options, default_level, source, status, created)
           VALUES ('ad_undo', 'default', 'contact', 'x_undo', '撤销测试', 'text', '[]', 'fact', 'agent', 'proposed', 1);
         INSERT INTO decisions (id, workspace_id, kind, subject_type, subject_id, question, options, status, requested_by, created)
           VALUES ('dec_undo', 'default', 'confirm', 'attr_def', 'ad_undo', '加这个属性吗？', '[\"yes\",\"no\"]', 'open', 'steve', 1);",
    )
    .expect("seed");
    let def = || -> String {
        db.query_row(
            "SELECT status FROM attr_defs WHERE id = 'ad_undo'",
            [],
            |r| r.get(0),
        )
        .expect("def")
    };
    let (s, d) = c
        .call(
            "POST",
            "/api/decisions/dec_undo",
            Some(json!({ "choice": "yes" })),
            None,
        )
        .await;
    assert_eq!(s, 200, "{d}");
    assert_eq!(d["undo"], json!({ "module": "attr_def" }));
    assert_eq!(def(), "active");
    let (s, d) = c
        .call("POST", "/api/decisions/dec_undo/undo", None, None)
        .await;
    assert_eq!(s, 200, "{d}");
    assert_eq!(d["status"], json!("open"));
    assert_eq!(def(), "proposed", "the handler's before_undo ran");
    assert_eq!(
        c.call("POST", "/api/decisions/dec_undo/undo", None, None)
            .await,
        (500, json!({ "error": "这个决定不能撤销" }))
    );
}

/// An item whose session gets the bearer token `test-token`; returns an MCP caller with that token.
/// No Node runs here (`STEVE_NODE_MODE=off`), so the new session is refused and ended at once;
/// it is made idle again, which is all its Steve tools need.
async fn agent_session(
    hub: &Hub,
    c: &Client,
) -> impl Fn(Value) -> std::pin::Pin<Box<dyn std::future::Future<Output = (u16, String)> + Send>> {
    let (_, item) = c
        .call(
            "POST",
            "/api/items",
            Some(json!({ "goal": "x", "title": "x", "bot": "steve", "project": null })),
            None,
        )
        .await;
    let id = item["id"].as_str().expect("item").to_string();
    let mut sid = String::new();
    for _ in 0..100 {
        let (_, i) = c.call("GET", &format!("/api/items/{id}"), None, None).await;
        if let Some(s) = i["sessionIds"][0].as_str() {
            sid = s.to_string();
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(!sid.is_empty(), "no session");
    let db =
        rusqlite::Connection::open(hub.home.path().join("steve/data/steve.sqlite3")).expect("db");
    db.busy_timeout(Duration::from_secs(5)).expect("timeout");
    let hash = hex::encode(<sha2::Sha256 as sha2::Digest>::digest(b"test-token"));
    db.execute(
        "UPDATE sessions SET token_hash = ?, status = 'idle' WHERE id = ?",
        [&hash, &sid],
    )
    .expect("token");
    let (http, url) = (c.http.clone(), format!("{}/mcp", c.base));
    move |body: Value| {
        let (http, url) = (http.clone(), url.clone());
        Box::pin(async move {
            let r = http
                .post(url)
                .header("authorization", "Bearer test-token")
                .header("accept", "application/json, text/event-stream")
                .json(&body)
                .send()
                .await
                .expect("mcp");
            (r.status().as_u16(), r.text().await.unwrap_or_default())
        })
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn contacts_tools_are_served_over_mcp() {
    let hub = start().await;
    let c = Client::login(&hub).await;
    c.call(
        "POST",
        "/api/contacts",
        Some(json!({ "name": "赵六" })),
        None,
    )
    .await;
    let mcp = agent_session(&hub, &c).await;
    let (s, list) = mcp(json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/list" })).await;
    assert_eq!(s, 200, "{list}");
    for t in [
        "steve_update",
        "steve_deliver",
        "steve_contacts_find",
        "steve_contacts_remember",
        "steve_im_read",
    ] {
        assert!(list.contains(t), "{t} missing: {list}");
    }
    // the chat reading tool is routed: a conversation the session cannot see is an error it reads
    let (s, out) = mcp(json!({ "jsonrpc": "2.0", "id": 4, "method": "tools/call", "params": { "name": "steve_im_read", "arguments": { "space": "spc_nope" } } })).await;
    assert_eq!(s, 200, "{out}");
    assert!(
        out.contains("\"isError\":true") && out.contains("spc_nope"),
        "{out}"
    );
    let (s, found) = mcp(json!({ "jsonrpc": "2.0", "id": 2, "method": "tools/call", "params": { "name": "steve_contacts_find", "arguments": { "query": "赵六" } } })).await;
    assert_eq!(s, 200, "{found}");
    assert!(found.contains("赵六"), "{found}");
    // without the bearer: refused like the Steve tools
    let r = c.http.post(format!("{}/mcp", c.base)).json(&json!({ "jsonrpc": "2.0", "id": 3, "method": "tools/call", "params": { "name": "steve_contacts_find", "arguments": { "query": "x" } } })).send().await.expect("mcp");
    assert_eq!(r.status().as_u16(), 401);
}

#[tokio::test(flavor = "multi_thread")]
async fn panels_and_mentions() {
    let hub = start().await;
    let c = Client::login(&hub).await;
    let (_, item) = c.call("POST", "/api/items", Some(json!({ "goal": "x", "title": "x", "bot": "steve", "project": null, "mode": "propose" })), None).await;
    let id = item["id"].as_str().expect("item").to_string();
    assert_eq!(
        c.call("GET", &format!("/api/items/{id}/panels"), None, None)
            .await,
        (200, json!([]))
    );
    assert_eq!(
        c.call("GET", "/api/items/ST-999/panels", None, None).await,
        (404, json!({ "error": "没有这个事项" }))
    );
    let (s, v) = c
        .call(
            "POST",
            &format!("/api/items/{id}/panels/github.pr:x/actions/merge"),
            None,
            None,
        )
        .await;
    assert_eq!(s, 404, "{v}");
    let (s, v) = c
        .call(
            "POST",
            "/api/thread",
            Some(json!({ "text": "问问 @张三", "mentions": [{ "kind": "robot", "id": "" }] })),
            None,
        )
        .await;
    assert_eq!(s, 400, "{v}");
    assert!(
        v["error"]
            .as_str()
            .unwrap_or_default()
            .starts_with("参数不对：mentions.0.kind"),
        "{v}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn knowledge_is_mounted_and_scoped() {
    let hub = start().await;
    let c = Client::login(&hub).await;
    for path in [
        "/api/kb/sources",
        "/api/kb/docs",
        "/api/kb/mems",
        "/api/kb/search?q=x",
    ] {
        let (s, v) = c.call("GET", path, None, None).await;
        assert!(s == 200 || s == 400, "{path}: {s} {v}");
    }
    let (_, w) = c
        .call(
            "POST",
            "/api/workspaces",
            Some(json!({ "name": "别处" })),
            None,
        )
        .await;
    let ws2 = w["id"].as_str().expect("ws").to_string();
    let note = json!({ "title": "部署说明", "body": "先跑测试再发布" });
    let mine = c
        .frames("default", async {
            assert_eq!(
                c.call("POST", "/api/kb/notes", Some(note.clone()), None)
                    .await
                    .0,
                200
            );
        })
        .await;
    assert!(
        mine.iter()
            .any(|e| e["type"] == "kb" && e["ws"] == "default"),
        "{mine:?}"
    );
    let other = c
        .frames(&ws2, async {
            c.call("POST", "/api/kb/notes", Some(note.clone()), None)
                .await;
        })
        .await;
    assert!(!other.iter().any(|e| e["type"] == "kb"), "{other:?}");
}

#[tokio::test(flavor = "multi_thread")]
async fn knowledge_note_is_found_over_http_and_mcp() {
    let hub = start().await;
    let c = Client::login(&hub).await;
    let note = json!({ "title": "发布流程", "body": "发布前先跑灰度验证，再全量上线。" });
    let (s, doc) = c.call("POST", "/api/kb/notes", Some(note), None).await;
    assert_eq!(s, 200, "{doc}");
    let (s, found) = c
        .call("GET", "/api/kb/search?q=%E7%81%B0%E5%BA%A6", None, None)
        .await;
    assert_eq!(s, 200, "{found}");
    let hits = found["hits"].as_array().expect("hits");
    assert!(
        hits.iter().any(|h| h.to_string().contains("灰度")),
        "{found}"
    );

    let mcp = agent_session(&hub, &c).await;
    let (s, list) = mcp(json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/list" })).await;
    assert_eq!(s, 200, "{list}");
    for t in [
        "steve_update",
        "steve_contacts_find",
        "steve_knowledge_search",
        "steve_knowledge_read",
        "steve_remember",
    ] {
        assert!(list.contains(t), "{t} missing: {list}");
    }
    // the item was raised by the logged-in user, so the session sees their private note
    let (s, out) = mcp(json!({ "jsonrpc": "2.0", "id": 2, "method": "tools/call", "params": { "name": "steve_knowledge_search", "arguments": { "query": "灰度验证" } } })).await;
    assert_eq!(s, 200, "{out}");
    assert!(out.contains("发布流程") && out.contains("灰度"), "{out}");
    assert!(!out.contains("\"isError\":true"), "{out}");
    let (s, out) = mcp(json!({ "jsonrpc": "2.0", "id": 3, "method": "tools/call", "params": { "name": "steve_remember", "arguments": { "text": "发布要先灰度", "kind": "rule" } } })).await;
    assert_eq!(s, 200, "{out}");
    assert!(
        out.contains("Saved as a candidate; the user will confirm it."),
        "{out}"
    );
}

/// Steve's reply to the `n`-th user message (counting from 1), from `/api/state`. The item's own
/// cards (卡住了, 完成 …) are posts of their own, not replies: the fake adapter fails at once, and on a
/// busy machine its card can land before the reply.
async fn steve_reply(c: &Client, n: usize) -> Value {
    for _ in 0..200 {
        let (_, st) = c.call("GET", "/api/state", None, None).await;
        let replies: Vec<Value> = st["thread"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|m| {
                m["author"] == "steve" && !m["item_id"].is_null() && m["meta"]["card_only"] != true
            })
            .cloned()
            .collect();
        if replies.len() >= n {
            return replies[n - 1].clone();
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("no reply");
}

#[tokio::test(flavor = "multi_thread")]
async fn thread_links_mentions_and_infers_people() {
    let hub = start().await;
    let c = Client::login(&hub).await;
    let (_, zhang) = c
        .call(
            "POST",
            "/api/contacts",
            Some(json!({ "name": "张三" })),
            None,
        )
        .await;
    let zid = zhang["id"].as_str().expect("contact").to_string();
    c.call(
        "POST",
        "/api/contacts",
        Some(json!({ "name": "李雷" })),
        None,
    )
    .await;

    let (s, v) = c
        .call(
            "POST",
            "/api/thread",
            Some(json!({ "text": "请 @张三 核对报价", "mentions": [{ "kind": "party", "id": zid }] })),
            None,
        )
        .await;
    assert_eq!(s, 200, "{v}");
    let r = steve_reply(&c, 1).await;
    assert!(
        r["text"]
            .as_str()
            .unwrap_or_default()
            .ends_with(" 关联了 张三。"),
        "{r}"
    );
    let item = r["item_id"].as_str().expect("item").to_string();
    let (_, people) = c
        .call("GET", &format!("/api/items/{item}/people"), None, None)
        .await;
    assert!(people.to_string().contains("张三"), "{people}");
    assert!(!people.to_string().contains("李雷"), "{people}");

    // no mentions: the names in the text are inferred, and the reply says nothing about it
    c.call(
        "POST",
        "/api/thread",
        Some(json!({ "text": "让李雷整理一下合同" })),
        None,
    )
    .await;
    let r = steve_reply(&c, 2).await;
    assert!(
        !r["text"].as_str().unwrap_or_default().contains("关联了"),
        "{r}"
    );
    let item = r["item_id"].as_str().expect("item").to_string();
    let (_, people) = c
        .call("GET", &format!("/api/items/{item}/people"), None, None)
        .await;
    assert!(people.to_string().contains("李雷"), "{people}");
}

#[tokio::test(flavor = "multi_thread")]
async fn connections_passkeys_and_github_are_mounted() {
    let hub = start().await;
    let c = Client::login(&hub).await;
    for path in [
        "/api/connections",
        "/api/connections/adapters",
        "/api/passkeys",
    ] {
        let (s, v) = c.call("GET", path, None, None).await;
        assert_eq!(s, 200, "{path}: {v}");
    }
    let (_, adapters) = c.call("GET", "/api/connections/adapters", None, None).await;
    // the GitHub App connects through its manifest flow, so only the token form is listed
    assert!(adapters.to_string().contains("github_pat"), "{adapters}");
    // 接入: every registered adapter under its provider, no provider without one, no connection yet
    let (s, ig) = c
        .call("GET", "/api/connections/integrations", None, None)
        .await;
    assert_eq!(s, 200, "{ig}");
    let ps = ig["providers"].as_array().expect("providers");
    let ids: Vec<_> = ps.iter().filter_map(|p| p["id"].as_str()).collect();
    assert_eq!(ids, ["feishu", "github", "mail", "ext_ws"], "{ig}");
    let status = |id: &str| {
        ps.iter()
            .find(|p| p["id"] == id)
            .map(|p| p["status"].clone())
    };
    assert_eq!(status("feishu"), Some(json!("supported")));
    assert_eq!(status("mail"), Some(json!("supported")));
    assert!(ps.iter().all(|p| p.get("planned").is_none()), "{ig}");
    let feishu = &ps[0];
    let adapters: Vec<_> = feishu["adapters"]
        .as_array()
        .expect("adapters")
        .iter()
        .filter_map(|a| a["id"].as_str())
        .collect();
    assert_eq!(adapters, ["feishu_app", "feishu_user"]);
    // each with what its channel side declares and whom it pairs with
    let user = &feishu["adapters"][1];
    assert_eq!(user["identity"], "self");
    assert_eq!(user["pairing"]["with"], json!(["feishu_app"]));
    assert_eq!(user["channel"]["mode"], "self");
    assert_eq!(user["channel"]["receive"], "poll");
    assert_eq!(
        user["channel"]["start"],
        json!([{"kind": "oauth"}, {"kind": "device"}])
    );
    let app = &feishu["adapters"][0];
    assert_eq!(app["channel"]["receive"], "long_connection");
    assert_eq!(app["channel"]["start"], json!([{"kind": "form"}]));
    assert_eq!(app["pairing"], Value::Null);
    // the external channel: you, through a channel service, made from its credential form
    let ext = ps.iter().find(|p| p["id"] == "ext_ws").expect("ext_ws");
    let side = &ext["adapters"][0];
    assert_eq!(side["id"], "ext_ws");
    assert_eq!(side["identity"], "self");
    assert_eq!(side["pairing"], Value::Null);
    assert_eq!(side["channel"]["mode"], "self");
    assert_eq!(side["channel"]["receive"], "long_connection");
    assert_eq!(side["channel"]["message_ids"], "none");
    assert_eq!(side["channel"]["start"], json!([{"kind": "form"}]));
    assert!(
        feishu["areas"]
            .to_string()
            .contains(r#"{"area":"minutes","state":"planned"}"#)
    );
    assert!(ps.iter().all(|p| p["connections"] == json!([])));
    assert!(
        ig["callback_urls"][0]
            .as_str()
            .is_some_and(|u| u.ends_with("/auth/connect/callback"))
    );
    let anon = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .expect("client");
    let get = |path: &str| anon.get(format!("{}{path}", hub.base)).send();
    // behind the login like every /api route
    assert_eq!(get("/api/connections").await.expect("get").status(), 401);
    // browser navigations: no session is a redirect with an error, not a 401
    for (path, to) in [
        (
            "/auth/connect/callback?code=x&state=y",
            "/#/accounts?error=",
        ),
        (
            "/api/github/app/manifest/callback?code=x&state=y",
            "/#/github?error=",
        ),
        (
            "/api/github/app/installed?installation_id=1",
            "/#/github?error=",
        ),
    ] {
        let r = get(path).await.expect("get");
        assert_eq!(r.status(), 302, "{path}");
        let loc = r
            .headers()
            .get("location")
            .and_then(|v| v.to_str().ok())
            .unwrap_or_default();
        assert!(loc.starts_with(to), "{path}: {loc}");
    }
    // POST stays login-only
    let r = anon
        .post(format!("{}/api/github/app/manifest", hub.base))
        .json(&json!({}))
        .send()
        .await
        .expect("post");
    assert_eq!(r.status(), 401);
    // the auth routes next to the callback still answer
    assert_eq!(get("/auth/dev").await.expect("dev").status(), 302);
}

#[tokio::test(flavor = "multi_thread")]
async fn web_dir_is_read_per_request() {
    let home = tempfile::tempdir().expect("tmp");
    let web = home.path().join("steve/web");
    let current = web.join("current");
    let hub = start_in(home, &current).await;
    let http = reqwest::Client::new();
    let get = |path: &str| http.get(format!("{}{path}", hub.base)).send();
    let header = |r: &reqwest::Response, h: &str| {
        r.headers()
            .get(h)
            .and_then(|v| v.to_str().ok())
            .unwrap_or_default()
            .to_string()
    };
    let health = |v: Value| v["web_version"].as_str().map(str::to_string);

    // no web installed yet
    let r = get("/").await.expect("get");
    assert_eq!(
        (r.status().as_u16(), header(&r, "cache-control")),
        (503, "no-store".into())
    );
    assert_eq!(
        r.text().await.unwrap_or_default(),
        "Steve web is not installed"
    );
    let v: Value = get("/healthz")
        .await
        .expect("h")
        .json()
        .await
        .expect("json");
    assert_eq!(health(v), Some(String::new()));

    // a release: web/<ver> and the current link
    for (ver, js) in [("w1", "one"), ("w2", "two")] {
        let d = web.join(ver);
        std::fs::create_dir_all(d.join("assets")).expect("mkdir");
        std::fs::write(d.join("index.html"), format!("<html>{ver}</html>")).expect("w");
        std::fs::write(d.join(format!("assets/app-{ver}.js")), js).expect("w");
        std::fs::write(d.join("release.json"), format!(r#"{{"version":"{ver}"}}"#)).expect("w");
    }
    std::os::unix::fs::symlink(web.join("w1"), &current).expect("link");
    let r = get("/").await.expect("get");
    assert_eq!(
        (r.status().as_u16(), header(&r, "cache-control")),
        (200, "no-store".into())
    );
    assert_eq!(r.text().await.unwrap_or_default(), "<html>w1</html>");
    let r = get("/items/ST-1").await.expect("spa");
    assert_eq!(header(&r, "cache-control"), "no-store");
    assert_eq!(r.text().await.unwrap_or_default(), "<html>w1</html>");
    let r = get("/assets/app-w1.js").await.expect("asset");
    assert_eq!(
        (r.status().as_u16(), header(&r, "cache-control")),
        (200, "public, max-age=31536000, immutable".into())
    );
    assert_eq!(
        get("/release.json").await.expect("rel").status().as_u16(),
        404
    );
    let v: Value = get("/healthz")
        .await
        .expect("h")
        .json()
        .await
        .expect("json");
    assert_eq!(health(v), Some("w1".into()));

    // switched while the Hub runs (atomic rename of a new link, like release-lib.sh)
    let next = web.join("current.new");
    std::os::unix::fs::symlink(web.join("w2"), &next).expect("link");
    std::fs::rename(&next, &current).expect("switch");
    assert_eq!(
        get("/")
            .await
            .expect("get")
            .text()
            .await
            .unwrap_or_default(),
        "<html>w2</html>"
    );
    // a tab on the old build: its hashed asset is gone, and that is a 404, not index.html
    let r = get("/assets/app-w1.js").await.expect("gone");
    assert_eq!(r.status().as_u16(), 404);
    assert_eq!(r.text().await.unwrap_or_default(), "not found");
    let v: Value = get("/healthz")
        .await
        .expect("h")
        .json()
        .await
        .expect("json");
    assert_eq!(health(v), Some("w2".into()));
    let c = Client::login(&hub).await;
    let (_, me) = c.call("GET", "/api/me", None, None).await;
    assert_eq!(me["web_version"], "w2");
    let (_, st) = c.call("GET", "/api/state", None, None).await;
    assert_eq!(st["web_version"], "w2");
}

/// Ownership: “我” owns Steve's bots and the Hub device once created, a new Bot is
/// owned by its creator, a 负责人 set over REST is undone through `/api/config/:id/undo`, and the
/// ownership tools are served over MCP.
#[tokio::test(flavor = "multi_thread")]
async fn ownership_routes_and_tools() {
    let hub = start().await;
    let c = Client::login(&hub).await;
    let (s, me) = c.call("GET", "/api/me/contact", None, None).await;
    assert_eq!(s, 200, "{me}");
    let (s, v) = c.call("GET", "/api/stewards/bot/steve", None, None).await;
    assert_eq!(s, 200, "{v}");
    assert_eq!(v["stewards"][0]["person_id"], me["id"], "{v}");
    // the Hub device registers in the background at boot, before or after “我”
    let mut v = Value::Null;
    for _ in 0..100 {
        let (s, x) = c.call("GET", "/api/stewards/device/hub", None, None).await;
        v = x;
        if s == 200 && v["stewards"][0]["person_id"] == me["id"] {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert_eq!(v["stewards"][0]["person_id"], me["id"], "{v}");

    let (s, bot) = c
        .call(
            "POST",
            "/api/bots",
            Some(json!({ "name": "值班", "harness": "claude-code" })),
            None,
        )
        .await;
    assert_eq!(s, 200, "{bot}");
    let bid = bot["id"].as_str().expect("bot id");
    let (_, v) = c
        .call("GET", &format!("/api/stewards/bot/{bid}"), None, None)
        .await;
    assert_eq!(v["stewards"][0]["person_id"], me["id"], "{v}");

    let (_, zhang) = c
        .call(
            "POST",
            "/api/contacts",
            Some(json!({ "name": "张三" })),
            None,
        )
        .await;
    let (s, v) = c
        .call(
            "PUT",
            &format!("/api/stewards/bot/{bid}"),
            Some(json!({ "person_id": zhang["id"] })),
            None,
        )
        .await;
    assert_eq!(s, 200, "{v}");
    assert_eq!(v["stewards"][0]["person_name"], "张三");
    let (s, h) = c
        .call(
            "GET",
            &format!("/api/people/{}/holdings", zhang["id"].as_str().unwrap()),
            None,
            None,
        )
        .await;
    assert_eq!(
        (s, &h["responsible"][0]["object"]["object_id"]),
        (200, &json!(bid)),
        "{h}"
    );
    let undo = v["undo"].as_str().expect("undo");
    let (s, u) = c
        .call("POST", &format!("/api/config/{undo}/undo"), None, None)
        .await;
    assert_eq!(s, 200, "{u}");
    let (_, v) = c
        .call("GET", &format!("/api/stewards/bot/{bid}"), None, None)
        .await;
    assert_eq!(v["stewards"][0]["person_id"], me["id"], "{v}");
    let (s, v) = c.call("GET", "/api/stewards/unowned", None, None).await;
    assert_eq!(s, 200, "{v}");
    assert!(v["objects"].is_array(), "{v}");

    let mcp = agent_session(&hub, &c).await;
    let (s, list) = mcp(json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/list" })).await;
    assert_eq!(s, 200, "{list}");
    for t in [
        "steve_whois",
        "steve_person_holdings",
        "steve_steward_propose",
    ] {
        assert!(list.contains(t), "{t} missing: {list}");
    }
    let (s, out) = mcp(json!({ "jsonrpc": "2.0", "id": 2, "method": "tools/call", "params": { "name": "steve_whois", "arguments": { "query": "值班" } } })).await;
    assert_eq!(s, 200, "{out}");
    assert!(
        out.contains("Bot「值班」") && out.contains("负责人 (owner)"),
        "{out}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn conversation_preferences_and_pagination_are_wired() {
    let hub = start().await;
    let c = Client::login(&hub).await;
    let db = rusqlite::Connection::open(hub.home.path().join("steve/data/steve.sqlite3")).unwrap();
    db.busy_timeout(Duration::from_secs(5)).unwrap();
    db.execute_batch("INSERT INTO spaces (id, workspace_id, system_id, kind, external_id, name, created, updated) VALUES ('spc_pref', 'default', 'feishu', 'group', 'pref', 'Conversation preference test', 1, 1);
        INSERT INTO messages (id, workspace_id, space, author, text, created) VALUES ('msg_pref', 'default', 'chan:spc_pref', 'me', 'keep this history', 1);").unwrap();
    let path = "/api/conversations/chan%3Aspc_pref/preferences";
    let anon = reqwest::Client::new()
        .patch(format!("{}{path}", hub.base))
        .json(&json!({"closed": true}))
        .send()
        .await
        .unwrap();
    assert_eq!(anon.status().as_u16(), 401);
    let (status, v) = c
        .call(
            "GET",
            "/api/conversations?q=preference&state=open&limit=50&offset=0",
            None,
            None,
        )
        .await;
    assert_eq!(status, 200, "{v}");
    assert_eq!(v["conversations"][0]["space"], "chan:spc_pref");
    assert_eq!(v["conversations"][0]["closed"], false);
    assert_eq!(
        (v["total"].clone(), v["has_more"].clone()),
        (json!(1), json!(false))
    );
    let (status, v) = c
        .call("PATCH", path, Some(json!({"closed": true})), None)
        .await;
    assert_eq!(status, 200, "{v}");
    assert_eq!(v["ok"], true);
    let (_, v) = c
        .call("GET", "/api/conversations?state=open&limit=50", None, None)
        .await;
    assert_eq!(v["conversations"], json!([]));
    let (_, v) = c
        .call(
            "GET",
            "/api/conversations?state=closed&limit=50",
            None,
            None,
        )
        .await;
    assert_eq!(v["conversations"][0]["closed"], true);
    assert_eq!(
        c.call("PATCH", path, Some(json!({"closed": false})), None)
            .await
            .0,
        200
    );
    // Closed state remains a strict boolean; malformed input is rejected by the HTTP extractor.
    assert_eq!(
        c.call("PATCH", path, Some(json!({"closed": "true"})), None)
            .await
            .0,
        422
    );
    assert_eq!(c.call("PATCH", path, Some(json!({})), None).await.0, 422);
    assert_eq!(
        c.call(
            "PATCH",
            "/api/conversations/chan%3Amissing/preferences",
            Some(json!({"closed": true})),
            None
        )
        .await
        .0,
        404
    );
    assert_eq!(
        c.call("GET", "/api/conversations?limit=201", None, None)
            .await
            .0,
        400
    );
    let (_, v) = c
        .call(
            "GET",
            "/api/conversations/chan%3Aspc_pref/messages",
            None,
            None,
        )
        .await;
    assert_eq!(v["messages"][0]["text"], "keep this history");
}
