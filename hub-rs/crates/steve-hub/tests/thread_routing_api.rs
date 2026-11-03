//! Thread routing over HTTP on the wired Hub (thread-routing R3, §7.2): a clear change becomes an item,
//! “改成在这里回答” takes it back, an answer's offer is taken once (then 409), and the new endpoints check
//! their input. The model is a stand-in `claude` script; no agent runs (adapters are `/usr/bin/false`).

use std::collections::HashMap;
use std::net::SocketAddr;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use serde_json::{Value, json};
use steve_hub::api::AppState;
use steve_hub::bus::Bus;
use steve_hub::config::Config;

struct Env {
    _home: tempfile::TempDir,
    db: std::path::PathBuf,
    base: String,
    http: reqwest::Client,
    cookie: String,
}

impl Env {
    async fn call(&self, method: &str, path: &str, body: Option<Value>) -> (u16, Value) {
        let m = reqwest::Method::from_bytes(method.as_bytes()).expect("method");
        let mut req = self
            .http
            .request(m, format!("{}{path}", self.base))
            .header("cookie", &self.cookie);
        if let Some(b) = body {
            req = req.json(&b);
        }
        let r = req.send().await.expect("request");
        let status = r.status().as_u16();
        let text = r.text().await.unwrap_or_default();
        (
            status,
            serde_json::from_str(&text).unwrap_or(Value::String(text)),
        )
    }

    async fn thread(&self) -> Vec<Value> {
        let (_, st) = self.call("GET", "/api/state", None).await;
        st["thread"].as_array().cloned().unwrap_or_default()
    }

    /// Wait (≤10 s) until the conversation has `n` messages.
    async fn settle(&self, n: usize) -> Vec<Value> {
        for _ in 0..200 {
            let t = self.thread().await;
            if t.len() >= n {
                return t;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        panic!("waited for {n}: {:#?}", self.thread().await);
    }
}

/// A `claude` that routes by the message and answers with an offer.
fn fake_claude(dir: &Path) -> std::path::PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let p = dir.join("claude");
    let item = r#"{\"kind\":\"item\",\"confidence\":0.95,\"signals\":[\"change\"],\"title\":\"改按钮\",\"goal\":\"把 X 改成 Y\",\"bot\":\"steve\"}"#;
    let answer = r#"{\"kind\":\"answer\",\"confidence\":0.9}"#;
    let reply = r#"{\"thought\":\"\",\"answer\":\"评论是纯文本。\",\"offer\":{\"title\":\"修评论\",\"goal\":\"让评论支持 markdown\"}}"#;
    let body = format!(
        r#"#!/bin/sh
# the router's rules come as --system-prompt, the message with -p
all="$*"
case "$all" in
  *"You are the router"*'User message: "帮我把'*|*'User message: "帮我把'*"You are the router"*) r='{item}' ;;
  *"You are the router"*) r='{answer}' ;;
  *) r='{reply}' ;;
esac
printf '{{"type":"result","is_error":false,"result":"%s"}}\n' "$r"
"#
    );
    std::fs::write(&p, body).expect("write claude");
    std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).expect("chmod");
    p
}

async fn start() -> Env {
    steve_modules::net::forbid_in_this_process();
    let home = tempfile::tempdir().expect("tmp");
    let h = home.path().to_path_buf();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let port = listener.local_addr().expect("addr").port();
    assert_ne!(port, 8440);
    let claude = fake_claude(&h);
    let mut vars: HashMap<String, String> = HashMap::new();
    for (k, v) in [
        ("HOME", h.display().to_string()),
        ("STEVE_HOME", h.join("steve").display().to_string()),
        ("STEVE_PORT", port.to_string()),
        ("STEVE_HOST", "127.0.0.1".into()),
        ("STEVE_PUBLIC_URL", format!("http://127.0.0.1:{port}")),
        ("STEVE_DEV_LOGIN", "1".into()),
        ("STEVE_REPO", "/nonexistent".into()),
        ("STEVE_VAULT_KEYSTORE", "file".into()),
        ("STEVE_FORBID_REAL_HOSTS", "1".into()),
        ("STEVE_NO_RESUME", "1".into()),
        ("CLAUDE_CODE_EXECUTABLE", claude.display().to_string()),
        ("STEVE_MODELS_FAST", "claude-haiku-5-5".into()),
        ("STEVE_MODELS_SMART", "claude-sonnet-5-5".into()),
        ("STEVE_ADAPTER_CLAUDE", "/usr/bin/false".into()),
        ("STEVE_ADAPTER_CODEX", "/usr/bin/false".into()),
        ("STEVE_WEB_DIR", h.join("no-web").display().to_string()),
    ] {
        vars.insert(k.into(), v);
    }
    let vars = Arc::new(vars);
    let get = {
        let vars = vars.clone();
        move |k: &str| vars.get(k).cloned()
    };
    let cfg = Arc::new(Config::from_vars(&get).expect("config"));
    for d in [&cfg.data_dir, &cfg.worktrees, &cfg.scratch] {
        std::fs::create_dir_all(d).expect("dirs");
    }
    let bus = Bus::new();
    let w = steve_hub::wire::wire_with(&cfg, &bus, Arc::new(get.clone())).expect("wire");
    let st = AppState::new(cfg.clone(), w.backend, w.domain, bus).with_modules(w.modules);
    steve_hub::app::boot(&st).await.expect("boot");
    for job in w.jobs {
        tokio::spawn(job);
    }
    let app = steve_hub::app::app(st, w.auth, w.mcp, w.module_routes, w.root_routes);
    tokio::spawn(async move {
        let _ = axum::serve(
            listener,
            app.into_make_service_with_connect_info::<SocketAddr>(),
        )
        .await;
    });
    let base = format!("http://127.0.0.1:{port}");
    let http = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .expect("client");
    let r = http
        .get(format!("{base}/auth/dev"))
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
    Env {
        _home: home,
        db: cfg.db_path(),
        base,
        http,
        cookie,
    }
}

#[tokio::test]
async fn thread_routing_api() {
    let e = start().await;
    /* a clear change becomes an item */
    let (s, _) = e
        .call(
            "POST",
            "/api/thread",
            Some(json!({"text": "帮我把 X 改成 Y"})),
        )
        .await;
    assert_eq!(s, 200);
    let t = e.settle(2).await;
    let reply = t[1].clone();
    let item = reply["item_id"].as_str().expect("an item").to_string();
    assert_eq!(reply["meta"]["steps"]["route"]["kind"], "item");

    /* input checks */
    let (s, v) = e
        .call(
            "POST",
            &format!("/api/thread/{}/reroute", reply["id"].as_str().unwrap()),
            Some(json!({"to": "nowhere"})),
        )
        .await;
    assert_eq!((s, v["error"].as_str()), (400, Some("参数不对：to")));
    let (s, v) = e
        .call(
            "POST",
            "/api/thread/msg_nope/reroute",
            Some(json!({"to": "item"})),
        )
        .await;
    assert_eq!((s, v["error"].as_str()), (404, Some("没有这条消息")));
    let (s, _) = e
        .call(
            "POST",
            &format!("/api/thread/{}/offer", reply["id"].as_str().unwrap()),
            Some(json!({})),
        )
        .await;
    assert_eq!(s, 400);

    /* “改成在这里回答”: the item is dropped, the message answered here */
    let (s, v) = e
        .call(
            "POST",
            &format!("/api/thread/{}/reroute", reply["id"].as_str().unwrap()),
            Some(json!({"to": "inline"})),
        )
        .await;
    assert_eq!(s, 200, "{v}");
    assert_eq!(v["item"]["status"], "drop");
    let t = e.settle(3).await;
    assert_eq!(t[2]["meta"]["reply_to"], reply["id"]);
    let (s, v) = e.call("GET", &format!("/api/items/{item}"), None).await;
    assert_eq!((s, v["status"].as_str()), (200, Some("drop")));

    /* a question is answered with an offer; taking it twice: the second is 409 with the same item */
    e.call(
        "POST",
        "/api/thread",
        Some(json!({"text": "评论支持 markdown 吗"})),
    )
    .await;
    let t = e.settle(5).await;
    let ans = t[4].clone();
    assert_eq!(ans["meta"]["offer"]["state"], "open", "{ans}");
    let path = format!("/api/thread/{}/offer", ans["id"].as_str().unwrap());
    let (s, v) = e.call("POST", &path, Some(json!({"take": true}))).await;
    assert_eq!(s, 200, "{v}");
    let made = v["item"]["id"].as_str().unwrap().to_string();
    assert_eq!(v["item"]["title"], "修评论");
    let (s, v) = e.call("POST", &path, Some(json!({"take": true}))).await;
    assert_eq!((s, v["error"].as_str()), (409, Some("已经处理过了")));
    assert_eq!(v["item"]["id"], json!(made));

    /* not a card */
    let (s, v) = e
        .call(
            "POST",
            &format!("/api/thread/{}/suggest", ans["id"].as_str().unwrap()),
            Some(json!({"choice": "here"})),
        )
        .await;
    assert_eq!((s, v["error"].as_str()), (400, Some("这条消息不是建议卡")));
    let (s, _) = e
        .call(
            "POST",
            &format!("/api/thread/{}/suggest", ans["id"].as_str().unwrap()),
            Some(json!({"choice": "x"})),
        )
        .await;
    assert_eq!(s, 400);

    /* runs and follow */
    let (s, v) = e.call("POST", "/api/thread/runs/tr_nope/stop", None).await;
    assert_eq!((s, v["ok"].as_bool()), (200, Some(false)));
    let (s, v) = e.call("DELETE", "/api/thread/runs/tr_nope", None).await;
    assert_eq!((s, v["ok"].as_bool()), (200, Some(false)));
    let (s, _) = e
        .call(
            "PATCH",
            &format!("/api/items/{made}/thread"),
            Some(json!({"follow": false})),
        )
        .await;
    assert_eq!(s, 200);
    let (s, _) = e
        .call(
            "PATCH",
            "/api/items/ST-999/thread",
            Some(json!({"follow": false})),
        )
        .await;
    assert_eq!(s, 404);
}

/// A research run's token sees only the four read-only tools (§7.4); a session token or none gets nothing.
#[tokio::test]
async fn research_runs_get_read_only_mcp() {
    let e = start().await;
    e.call(
        "POST",
        "/api/thread",
        Some(json!({"text": "帮我把 X 改成 Y"})),
    )
    .await;
    let t = e.settle(2).await;
    let item = t[1]["item_id"].as_str().unwrap().to_string();
    let store = steve_store::Store::open(&e.db, steve_store::NoPublisher).unwrap();
    let th = store.thread();
    let run = th
        .run_add(&steve_store::NewThreadRun {
            workspace_id: "default".into(),
            message_id: "msg_x".into(),
            ..Default::default()
        })
        .unwrap();
    let token = "research-token-1";
    th.run_patch(
        &run.id,
        &steve_store::RunPatch {
            status: Some("running".into()),
            token_hash: Some(Some(steve_store::sha(token))),
            ..Default::default()
        },
    )
    .unwrap();
    let rpc = |body: Value, tok: &'static str| {
        let http = e.http.clone();
        let url = format!("{}/mcp", e.base);
        async move {
            let r = http
                .post(url)
                .header("authorization", format!("Bearer {tok}"))
                .header("accept", "application/json, text/event-stream")
                .json(&body)
                .send()
                .await
                .unwrap();
            let s = r.status().as_u16();
            (s, r.json::<Value>().await.unwrap_or(Value::Null))
        }
    };
    let (s, v) = rpc(json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {"protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": {"name": "t", "version": "1"}}}), token).await;
    assert_eq!(s, 200, "{v}");
    assert_eq!(v["result"]["serverInfo"]["name"], "steve");
    let (_, v) = rpc(
        json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list"}),
        token,
    )
    .await;
    let names: Vec<&str> = v["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap())
        .collect();
    assert_eq!(
        names,
        [
            "steve_knowledge_search",
            "steve_knowledge_read",
            "steve_items_search",
            "steve_item_read"
        ]
    );
    for tool in v["result"]["tools"].as_array().unwrap() {
        assert_eq!(tool["annotations"]["readOnlyHint"], true);
        assert_eq!(tool["annotations"]["destructiveHint"], false);
    }
    let (_, v) = rpc(json!({"jsonrpc": "2.0", "id": 3, "method": "tools/call", "params": {"name": "steve_deliver", "arguments": {"summary": "x", "outputs": [], "checks": []}}}), token).await;
    assert_eq!(v["result"]["isError"], true);
    assert_eq!(
        v["result"]["content"][0]["text"],
        "This tool is not available in a read-only research run."
    );
    let (_, v) = rpc(json!({"jsonrpc": "2.0", "id": 4, "method": "tools/call", "params": {"name": "steve_items_search", "arguments": {"query": "改按钮"}}}), token).await;
    assert!(
        v["result"]["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains(&item),
        "{v}"
    );
    let (_, v) = rpc(json!({"jsonrpc": "2.0", "id": 5, "method": "tools/call", "params": {"name": "steve_item_read", "arguments": {"id": item}}}), token).await;
    assert!(
        v["result"]["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("目标：把 X 改成 Y"),
        "{v}"
    );
    let (_, v) = rpc(json!({"jsonrpc": "2.0", "id": 6, "method": "tools/call", "params": {"name": "steve_item_read", "arguments": {"id": "ST-999"}}}), token).await;
    assert_eq!(v["result"]["isError"], true);
    /* the run ended: the token is dead */
    th.run_patch(
        &run.id,
        &steve_store::RunPatch {
            status: Some("done".into()),
            token_hash: Some(None),
            ..Default::default()
        },
    )
    .unwrap();
    let (s, _) = rpc(
        json!({"jsonrpc": "2.0", "id": 7, "method": "tools/list"}),
        token,
    )
    .await;
    assert_eq!(s, 401);
}
