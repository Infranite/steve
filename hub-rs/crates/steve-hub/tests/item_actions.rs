//! The item page's quick actions over HTTP on the wired Hub (pause/resume, mark done and its undo, drop,
//! reopen), with the scripted ACP agent of steve-acp (`fake-acp.js`, run by node) as the claude-code
//! adapter. Temp STEVE_HOME, no real agent or host.

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

    async fn session(&self, id: &str) -> Value {
        let (s, v) = self.call("GET", &format!("/api/sessions/{id}"), None).await;
        assert_eq!(s, 200, "{v}");
        v
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
        ("STEVE_REPO", "/nonexistent".into()),
        ("STEVE_VAULT_KEYSTORE", "file".into()),
        ("STEVE_FORBID_REAL_HOSTS", "1".into()),
        ("STEVE_NO_RESUME", "1".into()),
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
            Some(json!({"goal": goal, "title": "快捷操作", "bot": bot, "project": null})),
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

async fn act(e: &Env, iid: &str, action: &str) -> (u16, Value) {
    e.call(
        "POST",
        &format!("/api/items/{iid}/actions"),
        Some(json!({ "action": action })),
    )
    .await
}

async fn item_of(e: &Env, sid: &str) -> String {
    e.session(sid).await["item_id"]
        .as_str()
        .expect("item id")
        .to_string()
}

#[tokio::test(flavor = "multi_thread")]
async fn pause_finish_undo_drop_and_reopen() {
    let e = start().await;
    let sid = item(&e, "慢慢做 @sleep 20000").await;
    let iid = item_of(&e, &sid).await;

    // mark done in the middle of a turn: the turn stops, the item is done
    let (s, v) = act(&e, &iid, "finish").await;
    assert_eq!(s, 200, "{v}");
    assert_eq!(
        (v["status"].as_str(), v["now"].as_str()),
        (Some("done"), Some("你标记了完成"))
    );
    assert!(
        until(10, || async { e.session(&sid).await["busy"] == false }).await,
        "the running turn is cancelled"
    );
    let (s, v) = act(&e, &iid, "finish").await;
    assert_eq!(s, 409, "a finished item can't be finished again: {v}");

    // the toast's undo: back in progress, the cut-off turn continues
    let before = e.prompts().len();
    let (s, v) = act(&e, &iid, "unfinish").await;
    assert_eq!(s, 200, "{v}");
    assert_eq!(v["status"], "run", "{v}");
    assert!(
        until(10, || async { e.prompts().len() > before }).await,
        "the agent is told to go on"
    );
    assert!(e.prompts().last().unwrap().contains("继续这个事项。"));
    let (s, v) = act(&e, &iid, "unfinish").await;
    assert_eq!(s, 409, "only right after a finish: {v}");

    // pause and resume
    let (s, v) = act(&e, &iid, "pause").await;
    assert_eq!((s, v["status"].as_str()), (200, Some("pause")), "{v}");
    let (s, v) = act(&e, &iid, "resume").await;
    assert_eq!((s, v["status"].as_str()), (200, Some("run")), "{v}");

    // drop, then reopen
    let (s, v) = act(&e, &iid, "drop").await;
    assert_eq!((s, v["status"].as_str()), (200, Some("drop")), "{v}");
    let (s, v) = act(&e, &iid, "finish").await;
    assert_eq!(s, 409, "a dropped item can't be marked done: {v}");
    let before = e.prompts().len();
    let (s, v) = act(&e, &iid, "reopen").await;
    assert_eq!((s, v["status"].as_str()), (200, Some("run")), "{v}");

    assert!(
        until(10, || async { e.prompts().len() > before }).await,
        "reopening goes on"
    );

    // a paused item marked done comes back paused on undo, without prodding the agent
    let (_, _) = act(&e, &iid, "pause").await;
    let n = e.prompts().len();
    let (s, v) = act(&e, &iid, "finish").await;
    assert_eq!((s, v["status"].as_str()), (200, Some("done")), "{v}");
    let (s, v) = act(&e, &iid, "unfinish").await;
    assert_eq!((s, v["status"].as_str()), (200, Some("pause")), "{v}");
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(e.prompts().len(), n);
    let (_, tl) = e.call("GET", &format!("/api/items/{iid}"), None).await;
    let texts: Vec<&str> = tl["timeline"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|t| t["text"].as_str())
        .collect();
    assert!(
        texts.contains(&"标记完成") && texts.contains(&"撤销了标记完成"),
        "{texts:?}"
    );

    workspace_is_named_never_guessed(&e).await;
}

/// The page says which workspace a request is for (`x-steve-ws`). A workspace the user is not a member of
/// is refused, never swapped for another; a browser write without it (a page loaded before) is refused
/// with "refresh"; a script without the header keeps the cookie's workspace.
async fn workspace_is_named_never_guessed(e: &Env) {
    let send = |method: &'static str, headers: Vec<(&'static str, String)>| {
        let http = e.http.clone();
        let path = if method == "GET" {
            "/api/me"
        } else {
            "/api/tags"
        };
        let (url, cookie) = (format!("{}{path}", e.base), e.cookie.clone());
        async move {
            let m = reqwest::Method::from_bytes(method.as_bytes()).unwrap();
            let mut r = http.request(m, url).header("cookie", cookie);
            if method == "POST" {
                r = r.json(&json!({"name": format!("t{}", rand_suffix()), "cat": "主题", "description": "测试"}));
            }
            for (k, v) in headers {
                r = r.header(k, v);
            }
            let res = r.send().await.expect("request");
            let s = res.status().as_u16();
            (s, res.json::<Value>().await.unwrap_or(Value::Null))
        }
    };
    let (_, me) = e.call("GET", "/api/me", None).await;
    let ws = me["workspace"]["id"]
        .as_str()
        .or_else(|| me["ws"].as_str())
        .unwrap_or("default")
        .to_string();
    // named and a member: fine, read or write
    let (s, v) = send("GET", vec![("x-steve-ws", ws.clone())]).await;
    assert_eq!(s, 200, "{v}");
    let (s, v) = send(
        "POST",
        vec![
            ("x-steve-ws", ws.clone()),
            ("sec-fetch-site", "same-origin".into()),
        ],
    )
    .await;
    assert_eq!(s, 200, "{v}");
    // a workspace that is not theirs: refused, not another one
    let (s, v) = send("GET", vec![("x-steve-ws", "ws_ffffffffffffffff".into())]).await;
    assert_eq!(
        (s, v["error"].as_str()),
        (409, Some(steve_hub::api::WS_GONE)),
        "{v}"
    );
    // a browser page that doesn't say: may read, may not write
    let (s, _) = send("GET", vec![("sec-fetch-site", "same-origin".into())]).await;
    assert_eq!(s, 200);
    let (s, v) = send("POST", vec![("sec-fetch-site", "same-origin".into())]).await;
    assert_eq!(
        (s, v["error"].as_str()),
        (409, Some(steve_hub::api::WS_RELOAD)),
        "{v}"
    );
    // what can't send a header (an EventSource, an <img>) names it as ?ws=, under the same rule
    let (s, _) = e.call("GET", &format!("/api/me?ws={ws}"), None).await;
    assert_eq!(s, 200);
    let (s, v) = e.call("GET", "/api/me?ws=ws_ffffffffffffffff", None).await;
    assert_eq!(
        (s, v["error"].as_str()),
        (409, Some(steve_hub::api::WS_GONE)),
        "{v}"
    );
    // a script (no Fetch Metadata) keeps the cookie's workspace
    let (s, v) = send("POST", vec![]).await;
    assert_eq!(s, 200, "{v}");
}

fn rand_suffix() -> String {
    format!(
        "{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
            % 1_000_000
    )
}
