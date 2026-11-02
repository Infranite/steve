//! `/api/stats/*` on the wired Hub (stats.md ST5): parameter checks, the previous range, the price
//! table (validation, audit, revisions), breakdowns adding up to the summary, and the coalesced
//! `stats` SSE frames. Temp STEVE_HOME, adapters `/usr/bin/false`, no real host.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use futures::StreamExt;
use serde_json::{Value, json};
use steve_hub::api::AppState;
use steve_hub::bus::Bus;
use steve_hub::config::Config;

struct Env {
    _home: tempfile::TempDir,
    base: String,
    http: reqwest::Client,
    cookie: String,
    db: std::path::PathBuf,
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

    fn db(&self) -> rusqlite::Connection {
        let c = rusqlite::Connection::open(&self.db).expect("db");
        c.busy_timeout(Duration::from_secs(5)).expect("timeout");
        c
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
        base,
        http,
        cookie,
        db: cfg.db_path(),
    }
}

fn days(from: &str, to: &str) -> usize {
    steve_stats::views::days(from, to).len()
}

fn prices(input: f64) -> Value {
    json!({"prices": [{"match": "claude-opus-5-5", "input": input, "output": 20, "cache_read": 0.2, "cache_write": 5}]})
}

#[tokio::test(flavor = "multi_thread")]
async fn stats_routes() {
    let e = start().await;
    let today = steve_stats::day(steve_store::now(), None);
    // two cube rows today, before anything is cached: 1M opus input = $4, and an unpriced model
    {
        let c = e.db();
        c.execute_batch(&format!(
            "INSERT INTO stat_daily (workspace_id, day, source, purpose, harness, model, input, output, cache_read, cache_write, hit_base, calls, turns, turn_ms, tools, tool_kinds)
             VALUES ('default', '{today}', 'agent', 'turn', 'claude-code', 'claude-opus-5-5', 1000000, 0, 3000000, 0, 4000000, 1, 2, 60000, 3, '{{\"execute\":2,\"mcp:steve_deliver\":1,\"tool:Bash\":2}}'),
                    ('default', '{today}', 'gateway', 'route', 'claude-cli', 'gpt-9', 100, 10, 0, 0, 0, 1, 0, 0, 0, '{{}}');
             INSERT INTO workspaces (id, name, created) VALUES ('elsewhere', '别处', 1);
             INSERT INTO projects (id, workspace_id, name, path, created, updated) VALUES ('prj_other', 'elsewhere', '别处', '/nonexistent', 1, 1);"
        ))
        .expect("seed");
    }

    /* the summary and its previous range */
    let (s, v) = e.call("GET", "/api/stats/summary", None).await;
    assert_eq!(s, 200, "{v}");
    let (from, to) = (
        v["range"]["from"].as_str().unwrap_or_default(),
        v["range"]["to"].as_str().unwrap_or_default(),
    );
    assert_eq!((days(from, to), to), (7, today.as_str()));
    let (pf, pt) = (
        v["prev"]["range"]["from"].as_str().unwrap_or_default(),
        v["prev"]["range"]["to"].as_str().unwrap_or_default(),
    );
    assert_eq!(days(pf, pt), 7);
    assert_eq!(days(pt, from), 2, "prev ends the day before");
    assert!(
        (v["usage"]["est"].as_f64().unwrap_or_default() - 4.6).abs() < 1e-9,
        "{}",
        v["usage"]
    );
    assert_eq!(v["usage"]["hit_rate"], json!(0.75));
    assert_eq!(v["spend"]["unpriced_tokens"], json!(110));
    assert_eq!(v["agents"]["tools"], json!(3));
    for r in ["today", "30d"] {
        let (s, v) = e
            .call("GET", &format!("/api/stats/summary?range={r}"), None)
            .await;
        assert_eq!(s, 200, "{v}");
        let (f, t) = (
            v["range"]["from"].as_str().unwrap_or_default(),
            v["range"]["to"].as_str().unwrap_or_default(),
        );
        assert_eq!(days(f, t), if r == "today" { 1 } else { 30 });
    }

    /* parameter checks */
    for (q, field) in [
        ("range=custom&to=2026-11-01", "from"),
        ("range=custom&from=2026-11-01", "to"),
        ("range=custom&from=2025-10-31&to=2026-11-01", "from"),
        ("range=90d", "range"),
        ("source=nope", "source"),
    ] {
        let (s, v) = e
            .call("GET", &format!("/api/stats/summary?{q}"), None)
            .await;
        assert_eq!(
            (s, v),
            (400, json!({"error": format!("参数不对：{field}")})),
            "{q}"
        );
    }
    let (s, v) = e
        .call(
            "GET",
            "/api/stats/summary?range=custom&from=2025-11-01&to=2026-11-01",
            None,
        )
        .await;
    assert_eq!(s, 200, "366 days: {v}");
    let (s, v) = e
        .call("GET", "/api/stats/summary?project=prj_other", None)
        .await;
    assert_eq!((s, v), (404, json!({"error": "没有这个项目"})));
    let (s, v) = e.call("GET", "/api/stats/summary?bot=nobody", None).await;
    assert_eq!((s, v), (404, json!({"error": "没有这个 Bot"})));
    let (s, _) = e.call("GET", "/api/stats/items/ST-999", None).await;
    assert_eq!(s, 404);
    let (s, _) = e.call("GET", "/api/stats/sessions/ses_nope", None).await;
    assert_eq!(s, 404);

    /* breakdowns add up to the summary */
    for by in ["model", "source", "purpose", "harness", "bot", "project"] {
        let (s, b) = e
            .call("GET", &format!("/api/stats/breakdown?by={by}"), None)
            .await;
        assert_eq!(s, 200, "{b}");
        let rows = b["rows"].as_array().cloned().unwrap_or_default();
        let est: f64 = rows
            .iter()
            .map(|r| r["est"].as_f64().unwrap_or_default())
            .sum();
        let total: i64 = rows
            .iter()
            .map(|r| r["total"].as_i64().unwrap_or_default())
            .sum();
        assert!((est - 4.6).abs() < 1e-9 && total == 4_000_110, "{by}: {b}");
    }
    let (_, b) = e.call("GET", "/api/stats/breakdown?by=model", None).await;
    assert_eq!(b["rows"][0]["key"], "claude-opus-5-5");
    assert_eq!(b["rows"][1]["priced"], json!(false));
    let (_, b) = e.call("GET", "/api/stats/breakdown?by=tool", None).await;
    let tools: Vec<(String, String, i64)> = b["rows"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|r| {
            (
                r["key"].as_str().unwrap_or_default().into(),
                r["dim"].as_str().unwrap_or_default().into(),
                r["count"].as_i64().unwrap_or_default(),
            )
        })
        .collect();
    assert!(
        tools.contains(&("tool:Bash".into(), "tool".into(), 2))
            && tools.contains(&("mcp:steve_deliver".into(), "mcp".into(), 1)),
        "{tools:?}"
    );
    let (s, _) = e.call("GET", "/api/stats/breakdown?by=day", None).await;
    assert_eq!(s, 400);

    /* series */
    let (s, v) = e
        .call(
            "GET",
            "/api/stats/series?metric=tokens&group=token_type",
            None,
        )
        .await;
    assert_eq!(s, 200, "{v}");
    assert_eq!(v["buckets"].as_array().map(Vec::len), Some(7));
    let keys: Vec<&str> = v["series"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|l| l["key"].as_str())
        .collect();
    assert_eq!(keys, ["input", "output", "cache_read", "cache_write"]);
    assert_eq!(v["series"][2]["values"][6], json!(3_000_000.0));
    let (s, v) = e
        .call("GET", "/api/stats/series?metric=hit_rate", None)
        .await;
    assert_eq!(
        (s, &v["series"][0]["values"][6]),
        (200, &json!(0.75)),
        "{v}"
    );
    for q in [
        "metric=wip&group=model",
        "metric=nope",
        "metric=tokens&bucket=week",
    ] {
        let (s, _) = e.call("GET", &format!("/api/stats/series?{q}"), None).await;
        assert_eq!(s, 400, "{q}");
    }
    let (s, v) = e.call("GET", "/api/stats/items?limit=101", None).await;
    assert_eq!((s, v), (400, json!({"error": "参数不对：limit"})));
    let (s, v) = e.call("GET", "/api/stats/items", None).await;
    assert_eq!((s, &v["total"]), (200, &json!(0)), "{v}");

    /* the price table */
    let (s, v) = e.call("GET", "/api/stats/prices", None).await;
    assert_eq!((s, &v["custom"]), (200, &json!(false)), "{v}");
    assert_eq!(v["unpriced"], json!([{"model": "gpt-9", "tokens": 110}]));
    let (s, v) = e
        .call("PUT", "/api/stats/prices", Some(json!({"prices": [{"match": "a", "input": -1, "output": 1, "cache_read": 0, "cache_write": 0}]})))
        .await;
    assert_eq!((s, v), (400, json!({"error": "参数不对：prices[0].input"})));
    let (s, v) = e.call("PUT", "/api/stats/prices", Some(prices(8.0))).await;
    assert_eq!(s, 200, "{v}");
    // the cached summary is gone: 1M input at $8
    let (_, v) = e.call("GET", "/api/stats/summary", None).await;
    assert!(
        (v["usage"]["est"].as_f64().unwrap_or_default() - 8.6).abs() < 1e-9,
        "{}",
        v["usage"]
    );
    let (s, _) = e
        .call("PUT", "/api/stats/prices", Some(json!({"prices": null})))
        .await;
    assert_eq!(s, 200);
    let (_, v) = e.call("GET", "/api/stats/prices", None).await;
    assert_eq!(v["custom"], json!(false));
    {
        let c = e.db();
        let audits: i64 = c
            .query_row(
                "SELECT COUNT(*) FROM audit WHERE action = 'stats.prices'",
                [],
                |r| r.get(0),
            )
            .expect("audit");
        let versions: Vec<i64> = c
            .prepare("SELECT version FROM revisions WHERE object_type = 'stats_prices' ORDER BY id")
            .expect("prep")
            .query_map([], |r| r.get(0))
            .expect("q")
            .collect::<Result<_, _>>()
            .expect("rows");
        assert_eq!((audits, versions), (2, vec![1, 2]));
    }
    let (s, _) = e
        .call("PUT", "/api/stats/tz", Some(json!({"tz": "Mars/Base"})))
        .await;
    assert_eq!(s, 400);
    let (s, v) = e
        .call("PUT", "/api/stats/tz", Some(json!({"tz": "Asia/Shanghai"})))
        .await;
    assert_eq!(s, 200, "{v}");
    let (s, _) = e.call("POST", "/api/stats/rebuild", None).await;
    assert_eq!(s, 202);

    /* SSE: four changes within a second reach the browser as one or two frames (the first at once if the
     * workspace was quiet, the rest merged at the window's end), never two within 5 s */
    tokio::time::sleep(Duration::from_secs(6)).await; // the rebuild's and the tz change's windows end
    let r = e
        .http
        .get(format!("{}/api/stream", e.base))
        .header("cookie", &e.cookie)
        .send()
        .await
        .expect("stream");
    let mut body = r.bytes_stream();
    tokio::time::sleep(Duration::from_millis(200)).await;
    for i in 0..4 {
        let (s, _) = e
            .call("PUT", "/api/stats/prices", Some(prices(4.0 + i as f64)))
            .await;
        assert_eq!(s, 200);
    }
    let start = tokio::time::Instant::now();
    let deadline = start + Duration::from_millis(11_000);
    let mut at = vec![];
    let mut buf = String::new();
    while let Ok(Some(Ok(chunk))) = tokio::time::timeout_at(deadline, body.next()).await {
        buf.push_str(&String::from_utf8_lossy(&chunk));
        while let Some(i) = buf.find("\n\n") {
            let frame: String = buf.drain(..i + 2).collect();
            if let Some(v) = frame
                .strip_prefix("data: ")
                .and_then(|d| serde_json::from_str::<Value>(d.trim()).ok())
                && v["type"] == "stats"
            {
                assert_eq!(v["ws"], "default");
                at.push(start.elapsed());
            }
        }
    }
    assert!((1..=2).contains(&at.len()), "{at:?}");
    assert!(
        at.windows(2)
            .all(|w| w[1] - w[0] >= Duration::from_millis(4500)),
        "{at:?}"
    );
}
