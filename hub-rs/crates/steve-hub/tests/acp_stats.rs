//! Agent turns into the statistics tables on the wired Hub (stats.md ST1), with the scripted ACP agent of
//! steve-acp (`fake-acp.js`) as the claude-code adapter: one row per turn, per model token rows, a
//! queued follow-up as its own turn, and the daily cube. Temp STEVE_HOME, no real agent or host.
#![allow(clippy::type_complexity)]

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
    #[allow(dead_code)]
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

    async fn session(&self, id: &str) -> Value {
        let (s, v) = self.call("GET", &format!("/api/sessions/{id}"), None).await;
        assert_eq!(s, 200, "{v}");
        v
    }

    fn db(&self) -> rusqlite::Connection {
        let c = rusqlite::Connection::open(&self.db).expect("db");
        c.busy_timeout(Duration::from_secs(5)).expect("timeout");
        c
    }

    fn count(&self, sql: &str) -> i64 {
        self.db().query_row(sql, [], |r| r.get(0)).expect(sql)
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
            Some(json!({"goal": goal, "title": "统计测试", "bot": bot, "project": null})),
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
async fn turns_and_tokens_land_in_the_stats_tables() {
    let e = start().await;
    let sid = item(
        &e,
        "做 @tool execute Bash @usage 10 100 0.5 claude-opus-5-5 @tally 1 2 3 4 @quota claude-opus-5-5:10:20:30:40,claude-haiku-5-5:1:2:3:4 @sleep 1500",
    )
    .await;
    let (s, v) = e
        .call(
            "POST",
            &format!("/api/sessions/{sid}/messages"),
            Some(json!({"text": "再来 @tally 5 6 7 8 @usage 10 100 0.8", "mode": "排队"})),
        )
        .await;
    assert_eq!(s, 200, "{v}");
    assert!(
        until(20, || async {
            e.count("SELECT count(*) FROM agent_turns WHERE ended IS NOT NULL") == 2
        })
        .await,
        "two turns end"
    );
    assert!(until(10, || async { e.session(&sid).await["busy"] == false }).await);
    let c = e.db();
    let turns: Vec<(Option<String>, Option<i64>, i64, String, i64, String)> = c
        .prepare("SELECT item_id, user_seq, attempt, stop_reason, tools, tool_kinds FROM agent_turns WHERE session_id = ? ORDER BY started, rowid")
        .unwrap()
        .query_map([&sid], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?)))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    assert_eq!(turns.len(), 2, "{turns:?}");
    assert!(
        turns
            .iter()
            .all(|t| t.0.is_some() && t.1.is_some() && t.2 == 0 && t.3 == "end_turn")
    );
    assert!(
        turns[1].1 > turns[0].1,
        "each turn points at its own user event"
    );
    assert_eq!(turns[0].4, 1);
    assert_eq!(turns[0].5, r#"{"execute":1,"tool:Bash":1}"#);
    let rows: Vec<(String, Option<i64>, Option<i64>, Option<i64>, Option<i64>, Option<f64>, String)> = c
        .prepare("SELECT model, input, output, cache_read, cache_write, cost_reported, detail FROM usage_records WHERE session_id = ? ORDER BY id")
        .unwrap()
        .query_map([&sid], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?, r.get(6)?)))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    assert_eq!(rows.len(), 3, "{rows:?}");
    assert_eq!(rows[0].0, "claude-opus-5-5");
    assert_eq!(
        (rows[0].1, rows[0].2, rows[0].3, rows[0].4),
        (Some(10), Some(20), Some(30), Some(40))
    );
    assert!((rows[0].5.unwrap() - 0.5).abs() < 1e-9);
    assert_eq!(rows[0].6, r#"{"scope":"all"}"#);
    assert_eq!((rows[1].0.as_str(), rows[1].5), ("claude-haiku-5-5", None));
    // the follow-up has no model_usage: main loop, last model seen in the session
    assert_eq!(
        (rows[2].1, rows[2].3, rows[2].6.as_str()),
        (Some(5), Some(7), r#"{"scope":"main"}"#)
    );
    assert!((rows[2].5.unwrap() - 0.3).abs() < 1e-9);
    // the cube equals the details
    let (tin, tcr, turns_n): (i64, i64, i64) = c
        .query_row(
            "SELECT sum(input), sum(cache_read), sum(turns) FROM stat_daily WHERE source = 'agent'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .unwrap();
    assert_eq!((tin, tcr, turns_n), (16, 40, 2));
    // the turns flagged the item's stats; the Hub's refresher (every 2 s) may already have recomputed them
    let flagged = e.count(
        "SELECT count(*) FROM item_stats WHERE dirty = 1 OR updated >= (SELECT max(ended) FROM agent_turns)",
    );
    assert_eq!(flagged, 1);
}
