#![allow(dead_code)]
//! Notes on an item's outputs over HTTP on the wired Hub (ST-38), with the scripted ACP agent of steve-acp
//! (`fake-acp.js`, run by node) as the claude-code adapter: drafts are kept and edited, a send reaches the
//! item once as the user's message with each output's version and the places marked, the same send repeated
//! sends nothing, and records stay apart from results. Temp STEVE_HOME, no real agent or host.

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
async fn drafts_one_send_and_the_item_line() {
    let e = start().await;
    let sid = item(&e, "做一下 @sleep 3000").await;
    let iid = e.session(&sid).await["item_id"]
        .as_str()
        .expect("item")
        .to_string();
    {
        let c = rusqlite::Connection::open(&e.db).expect("db");
        c.execute_batch(&format!(
            "INSERT INTO artifacts (id, item_id, kind, title, location, created) VALUES
               ('art_a', '{iid}', '文件', '截图', '/tmp/shots', 1), ('art_b', '{iid}', '文件', '截图', '/tmp/shots', 2);
             INSERT INTO artifact_roles (artifact_id, role) VALUES ('art_a', 'record');"
        ))
        .expect("artifacts");
    }
    let path = format!("/api/items/{iid}/annotations");
    let (s, v) = e
        .call("POST", &path, Some(json!({"art": "art_b", "path": "a.png", "anchor": {"type": "region", "x": 0.1, "y": 0.2, "w": 0.5, "h": 0.25}, "note": "按钮被裁掉了"})))
        .await;
    assert_eq!(s, 200, "{v}");
    let one = v["id"].as_str().unwrap().to_string();
    let (s, v) = e
        .call("POST", &path, Some(json!({"art": "art_b", "anchor": {"type": "region", "x": 0.9, "y": 0, "w": 0.5, "h": 0.1}, "note": "x"})))
        .await;
    assert_eq!((s, v["error"].as_str()), (400, Some("参数不对：anchor")));
    let (s, _) = e
        .call(
            "POST",
            &path,
            Some(json!({"art": "art_x", "anchor": {"type": "file"}, "note": "x"})),
        )
        .await;
    assert_eq!(s, 404);
    let (s, v) = e
        .call("POST", &path, Some(json!({"art": "art_b", "anchor": {"type": "text", "quote": "旧文案", "line": 3}, "note": "要删"})))
        .await;
    assert_eq!(s, 200, "{v}");
    let two = v["id"].as_str().unwrap().to_string();
    let (s, v) = e
        .call(
            "PATCH",
            &format!("{path}/{one}"),
            Some(json!({"note": "按钮被裁掉了，挪上去"})),
        )
        .await;
    assert_eq!((s, v["note"].as_str()), (200, Some("按钮被裁掉了，挪上去")));

    let (_, d) = e.call("GET", &format!("/api/items/{iid}"), None).await;
    assert_eq!(d["artifactRoles"], json!({"art_a": "record"}));
    assert_eq!(d["annotations"].as_array().map(Vec::len), Some(2));
    assert!(
        d["annotations"]
            .as_array()
            .unwrap()
            .iter()
            .all(|a| a["status"] == "draft")
    );

    // two clicks of one send, and one from a second tab: the item hears it once
    let body = json!({"ids": [&one, &two], "key": "send-0001"});
    let send = format!("{path}/send");
    let (a, b, c) = tokio::join!(
        e.call("POST", &send, Some(body.clone())),
        e.call("POST", &send, Some(body.clone())),
        e.call(
            "POST",
            &send,
            Some(json!({"ids": [&one, &two], "key": "send-0002"}))
        ),
    );
    let firsts: Vec<usize> = [&a, &b, &c]
        .iter()
        .enumerate()
        .filter(|(_, r)| r.0 == 200 && r.1["again"] == false)
        .map(|(n, _)| n)
        .collect();
    assert_eq!(firsts.len(), 1, "{a:?} {b:?} {c:?}");
    let won = if firsts[0] == 2 {
        "send-0002"
    } else {
        "send-0001"
    };
    let (_, d) = e.call("GET", &format!("/api/items/{iid}"), None).await;
    let lines: Vec<&Value> = d["timeline"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|t| {
            t["kind"] == "me"
                && t["text"]
                    .as_str()
                    .is_some_and(|x| x.starts_with("对产出的批注"))
        })
        .collect();
    assert_eq!(lines.len(), 1, "{}", d["timeline"]);
    assert_eq!(
        lines[0]["text"],
        "对产出的批注（2 条）：\n\n**截图**（文件 · 第 2 版） `/tmp/shots`\n1. `a.png` · 图片区域（左 10%，上 20%，宽 50%，高 25%）：按钮被裁掉了，挪上去\n2. 选中文字「旧文案」（第 3 行）：要删"
    );
    let anns = d["annotations"].as_array().unwrap();
    assert!(
        anns.iter()
            .all(|a| a["status"] == "sent" && a["timeline_id"] == lines[0]["id"]),
        "{anns:?}"
    );

    // sent notes stay as they were sent; the same send again is answered without sending
    let (s, _) = e
        .call(
            "PATCH",
            &format!("{path}/{one}"),
            Some(json!({"note": "x"})),
        )
        .await;
    assert_eq!(s, 409);
    let (s, _) = e.call("DELETE", &format!("{path}/{one}"), None).await;
    assert_eq!(s, 409);
    let (s, v) = e
        .call(
            "POST",
            &format!("{path}/send"),
            Some(json!({"ids": [one, two], "key": won})),
        )
        .await;
    assert_eq!(
        (s, v["again"].as_bool(), v["pending"].as_bool()),
        (200, Some(true), Some(false))
    );
    assert!(
        until(10, || async {
            e.prompts()
                .iter()
                .any(|p| p.contains("对产出的批注（2 条）"))
        })
        .await,
        "the agent hears the notes: {:?}",
        e.prompts()
    );
    assert_eq!(
        e.prompts()
            .iter()
            .filter(|p| p.contains("对产出的批注"))
            .count(),
        1
    );
}
