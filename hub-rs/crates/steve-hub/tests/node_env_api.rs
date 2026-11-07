#![allow(dead_code)]
//! A user's own environment variables on a Node (`node_env`) over HTTP on the wired Hub, with the
//! scripted ACP agent (`fake-acp.js`) as the claude-code adapter: one's own are saved, Steve's keys
//! and other people's are refused, and an item the user asks for gets them in its harness on the
//! local Node. Temp STEVE_HOME, no real agent or host.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use serde_json::{Value, json};
use steve_hub::api::AppState;
use steve_hub::bus::Bus;
use steve_hub::config::Config;

struct Env {
    _home: tempfile::TempDir,
    base: String,
    http: reqwest::Client,
    cookie: String,
    log: PathBuf,
    db: PathBuf,
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

    /// Prompt texts the fake agent received, in order.
    fn prompts(&self) -> Vec<String> {
        std::fs::read_to_string(&self.log)
            .unwrap_or_default()
            .lines()
            .filter_map(|l| serde_json::from_str::<Value>(l).ok())
            .filter(|m| m["method"] == "session/prompt")
            .map(|m| {
                m["params"]["prompt"]
                    .as_array()
                    .map(|ps| {
                        ps.iter()
                            .filter_map(|p| p["text"].as_str())
                            .collect::<Vec<_>>()
                            .join("\n")
                    })
                    .unwrap_or_default()
            })
            .collect()
    }

    /// The open decision `id` as `/api/state` lists it.
    async fn open_decision(&self, id: &str) -> Value {
        let (_, st) = self.call("GET", "/api/state", None).await;
        st["decisions"]
            .as_array()
            .and_then(|ds| ds.iter().find(|d| d["id"] == id))
            .cloned()
            .unwrap_or(Value::Null)
    }

    async fn session(&self, id: &str) -> Value {
        let (s, v) = self.call("GET", &format!("/api/sessions/{id}"), None).await;
        assert_eq!(s, 200, "{v}");
        v
    }

    async fn queued(&self, id: &str) -> Vec<Value> {
        let (s, v) = self
            .call("GET", &format!("/api/sessions/{id}/queued"), None)
            .await;
        assert_eq!(s, 200, "{v}");
        v.as_array().cloned().unwrap_or_default()
    }

    async fn kinds(&self, id: &str) -> Vec<String> {
        let (_, v) = self
            .call("GET", &format!("/api/sessions/{id}/events"), None)
            .await;
        v.as_array()
            .or_else(|| v["events"].as_array())
            .cloned()
            .unwrap_or_default()
            .iter()
            .filter_map(|e| e["kind"].as_str().map(String::from))
            .collect()
    }
}

async fn until<F: std::future::Future<Output = bool>>(secs: u64, mut f: impl FnMut() -> F) -> bool {
    let end = tokio::time::Instant::now() + Duration::from_secs(secs);
    while tokio::time::Instant::now() < end {
        if f().await {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    false
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
    let fake = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../steve-acp/tests/fake-acp.js");
    let log = h.join("acp.log");
    // the adapter inherits the process env: the log path goes there
    // SAFETY: this file has one test, so one Hub per process sets it, before any adapter starts
    unsafe { std::env::set_var("FAKE_ACP_LOG", &log) };
    let mut vars: HashMap<String, String> = HashMap::new();
    for (k, v) in [
        ("HOME", h.display().to_string()),
        ("STEVE_HOME", h.join("steve").display().to_string()),
        ("STEVE_PORT", port.to_string()),
        ("STEVE_HOST", "127.0.0.1".into()),
        ("STEVE_PUBLIC_URL", format!("http://127.0.0.1:{port}")),
        ("STEVE_DEV_LOGIN", "1".into()),
        // the fake agent's stops reach the card as they did; Steve's own continues are tested in steve-core
        ("STEVE_TAKEOVER_CONTINUES", "0".into()),
        ("STEVE_REPO", "/nonexistent".into()),
        ("STEVE_VAULT_KEYSTORE", "file".into()),
        ("STEVE_FORBID_REAL_HOSTS", "1".into()),
        // the local Node runs the sessions (the Hub runs none itself)
        ("STEVE_NODE_MODE", "local".into()),
        ("CLAUDE_CODE_EXECUTABLE", "/usr/bin/false".into()),
        ("STEVE_ADAPTER_CLAUDE", fake.display().to_string()),
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
        base,
        http,
        cookie,
        log,
        db: cfg.db_path(),
    }
}

/// An item whose first turn runs `goal` on the fake agent; returns its session id.
async fn item(e: &Env, goal: &str) -> String {
    let (_, state) = e.call("GET", "/api/state", None).await;
    let bot = state["bots"]
        .as_array()
        .and_then(|bs| bs.iter().find(|b| b["harness"] == "claude-code"))
        .and_then(|b| b["id"].as_str())
        .expect("a claude-code bot")
        .to_string();
    let (s, i) = e
        .call(
            "POST",
            "/api/items",
            Some(json!({"goal": goal, "title": "排队测试", "bot": bot, "project": null})),
        )
        .await;
    assert_eq!(s, 200, "{i}");
    let iid = i["id"].as_str().expect("item id").to_string();
    let (_, ss) = e.call("GET", "/api/sessions", None).await;
    let sid = ss
        .as_array()
        .and_then(|ss| ss.iter().find(|s| s["item_id"] == iid.as_str()))
        .and_then(|s| s["id"].as_str())
        .unwrap_or_else(|| panic!("no session of {iid} in {ss}"))
        .to_string();
    assert!(
        until(10, || async { e.session(&sid).await["busy"] == true }).await,
        "the first turn runs"
    );
    sid
}

#[tokio::test(flavor = "multi_thread")]
async fn ones_own_variables_reach_the_items_harness() {
    let e = start().await;
    let (s, me) = e.call("GET", "/api/me", None).await;
    assert_eq!(s, 200, "{me}");
    // one's own, on the local Node
    let (s, v) = e
        .call(
            "PUT",
            "/api/node-env/local/NPM_TOKEN",
            Some(json!({"value": "tok-e2e", "note": "registry"})),
        )
        .await;
    assert_eq!(s, 200, "{v}");
    let (s, v) = e.call("GET", "/api/node-env/local", None).await;
    assert_eq!(s, 200, "{v}");
    assert_eq!(v["env"][0]["key"], "NPM_TOKEN");
    assert_eq!(v["env"][0]["value"], "tok-e2e");
    // Steve's keys, someone else's variables, a Node nobody paired
    let (s, v) = e
        .call(
            "PUT",
            "/api/node-env/local/HOME",
            Some(json!({"value": "/x"})),
        )
        .await;
    assert_eq!(s, 400, "{v}");
    let (s, v) = e
        .call(
            "PUT",
            "/api/node-env/local/X",
            Some(json!({"value": "1", "principal": "someone-else"})),
        )
        .await;
    assert_eq!(s, 403, "{v}");
    let (s, v) = e
        .call(
            "PUT",
            "/api/node-env/n_nowhere/X",
            Some(json!({"value": "1"})),
        )
        .await;
    assert_eq!(s, 403, "{v}");
    let (s, v) = e
        .call("GET", "/api/node-env/local?principal=someone-else", None)
        .await;
    assert_eq!(s, 403, "{v}");

    // an item asked for over the API runs with the asker's variables
    item(&e, "环境变量 @say ok").await;
    assert!(
        until(20, || async {
            std::fs::read_to_string(&e.log)
                .unwrap_or_default()
                .contains("\"spawn\"")
        })
        .await,
        "the adapter started"
    );
    let env = std::fs::read_to_string(&e.log)
        .unwrap_or_default()
        .lines()
        .filter_map(|l| serde_json::from_str::<Value>(l).ok())
        .find_map(|m| m["spawn"]["env"].as_object().cloned())
        .unwrap_or_else(|| {
            panic!(
                "the adapter's spawn line: {}",
                std::fs::read_to_string(&e.log)
                    .unwrap_or_default()
                    .chars()
                    .take(1500)
                    .collect::<String>()
            )
        });
    assert_eq!(env["NPM_TOKEN"], "tok-e2e", "{env:?}");

    let (s, _) = e
        .call("DELETE", "/api/node-env/local/NPM_TOKEN", None)
        .await;
    assert_eq!(s, 200);
    let (_, v) = e.call("GET", "/api/node-env/local", None).await;
    assert_eq!(v["env"], json!([]));
}
