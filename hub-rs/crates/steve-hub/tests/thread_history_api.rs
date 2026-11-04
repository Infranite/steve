//! The conversation's older history and search over HTTP (ST-50): `/api/state` carries the latest 200 messages;
//! `GET /api/thread/history` pages back before them (or fills the gap down to a search hit) and `GET
//! /api/thread/search` finds what was said, both without the messages the conversation hides.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;

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
async fn thread_history_and_search() {
    let e = start().await;
    let (_, me) = e.call("GET", "/api/me", None).await;
    let ws = me["workspace"].as_str().expect("workspace").to_string();
    {
        let c = rusqlite::Connection::open(&e.db).expect("db");
        for i in 0..260i64 {
            let (text, item, meta) = match i {
                5 => ("讨论对话页的历史定位", None, None),
                6 => (
                    "这条被收进事项卡片了",
                    None,
                    Some(json!({ "hidden": true }).to_string()),
                ),
                7 => ("交给 ST-9 去做", Some("ST-9"), None),
                8 => ("百分号 100% 完成", None, None),
                _ => ("普通消息", None, None),
            };
            c.execute(
                "INSERT INTO messages (id, workspace_id, space, author, text, item_id, meta, created) VALUES (?, ?, 'steve', ?, ?, ?, ?, ?)",
                rusqlite::params![format!("m{i:03}"), ws, if i % 2 == 0 { "me" } else { "steve" }, text, item, meta, 1_000 + i],
            )
            .expect("insert");
        }
    }
    let (_, st) = e.call("GET", "/api/state", None).await;
    let thread = st["thread"].as_array().cloned().unwrap_or_default();
    assert_eq!(thread.len(), 200);
    let oldest = thread[0]["created"].as_i64().expect("created");
    assert_eq!(oldest, 1_060);

    /* paging back: everything before the window, oldest first, the hidden one left out */
    let (s, page) = e
        .call(
            "GET",
            &format!("/api/thread/history?before={oldest}&limit=100"),
            None,
        )
        .await;
    assert_eq!(s, 200, "{page}");
    let ids: Vec<&str> = page["messages"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| m["id"].as_str().unwrap())
        .collect();
    assert_eq!(ids.len(), 59);
    assert_eq!(ids[0], "m000");
    assert!(!ids.contains(&"m006"));
    assert_eq!(page["has_more"], json!(false));
    let (_, page) = e
        .call(
            "GET",
            &format!("/api/thread/history?before={oldest}&limit=10"),
            None,
        )
        .await;
    let ids: Vec<&str> = page["messages"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| m["id"].as_str().unwrap())
        .collect();
    assert_eq!(ids.first(), Some(&"m050"));
    assert_eq!(ids.last(), Some(&"m059"));
    assert_eq!(page["has_more"], json!(true));
    assert_eq!(page["oldest"], json!(1_050));

    /* filling the gap down to a hit: from it up to the window, and whether there is more before it */
    let (_, fill) = e
        .call(
            "GET",
            &format!("/api/thread/history?before={oldest}&after=1005&limit=2000"),
            None,
        )
        .await;
    let ids: Vec<&str> = fill["messages"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| m["id"].as_str().unwrap())
        .collect();
    assert_eq!(ids.first(), Some(&"m005"));
    assert_eq!(ids.len(), 54);
    assert_eq!(fill["has_more"], json!(true));

    /* search: text and item number, newest first; the hidden message and LIKE wildcards stay out */
    let (_, hit) = e
        .call(
            "GET",
            "/api/thread/search?q=%E5%8E%86%E5%8F%B2%E5%AE%9A%E4%BD%8D",
            None,
        )
        .await;
    assert_eq!(hit["messages"].as_array().unwrap().len(), 1);
    assert_eq!(hit["messages"][0]["id"], json!("m005"));
    let (_, hit) = e.call("GET", "/api/thread/search?q=st-9", None).await;
    assert_eq!(hit["messages"][0]["id"], json!("m007"));
    let (_, hit) = e
        .call(
            "GET",
            "/api/thread/search?q=%E6%94%B6%E8%BF%9B%E4%BA%8B%E9%A1%B9",
            None,
        )
        .await;
    assert_eq!(hit["messages"].as_array().unwrap().len(), 0);
    let (_, hit) = e.call("GET", "/api/thread/search?q=0%25", None).await;
    assert_eq!(
        hit["messages"]
            .as_array()
            .unwrap()
            .iter()
            .map(|m| m["id"].as_str().unwrap())
            .collect::<Vec<_>>(),
        vec!["m008"]
    );
    let (_, hit) = e
        .call(
            "GET",
            "/api/thread/search?q=%E6%99%AE%E9%80%9A&limit=3",
            None,
        )
        .await;
    let ids: Vec<&str> = hit["messages"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| m["id"].as_str().unwrap())
        .collect();
    assert_eq!(ids, vec!["m259", "m258", "m257"]);
    let (s, _) = e.call("GET", "/api/thread/search?q=%20", None).await;
    assert_eq!(s, 400);
}
