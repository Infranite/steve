//! `/api/stats/*` on the wired Hub: parameter checks, the previous range, the price
//! table (validation, audit, revisions), breakdowns adding up to the summary, and the coalesced
//! `stats` SSE frames, and the Models.dev price sync against a catalog served on loopback. Temp
//! STEVE_HOME, adapters `/usr/bin/false`, no real host.

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
    start_with(&[]).await
}

async fn start_with(extra: &[(&str, String)]) -> Env {
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
    for (k, v) in extra {
        vars.insert((*k).into(), v.clone());
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
    // agent working time and time waiting on you, per day (the daily chart's third line)
    let (s, v) = e
        .call("GET", "/api/stats/series?metric=turn_ms", None)
        .await;
    assert_eq!(
        (s, &v["series"][0]["values"][6]),
        (200, &json!(60_000.0)),
        "{v}"
    );
    let (s, v) = e
        .call("GET", "/api/stats/series?metric=wait_ms", None)
        .await;
    assert_eq!(
        (s, v["buckets"].as_array().map(Vec::len)),
        (200, Some(7)),
        "{v}"
    );
    for q in [
        "metric=wip&group=model",
        "metric=wait_ms&group=model",
        "metric=turn_ms&group=token_type",
        "metric=nope",
        "metric=tokens&bucket=week",
    ] {
        let (s, _) = e.call("GET", &format!("/api/stats/series?{q}"), None).await;
        assert_eq!(s, 400, "{q}");
    }
    /* hours of a one-day range (ST-43) */
    let (s, v) = e
        .call(
            "GET",
            "/api/stats/series?metric=done&range=today&bucket=hour",
            None,
        )
        .await;
    assert_eq!((s, &v["bucket"]), (200, &json!("hour")), "{v}");
    assert_eq!(v["buckets"][0], json!(format!("{today}T00:00")));
    assert_eq!(v["buckets"][23], json!(format!("{today}T23:00")));
    assert_eq!(v["spans"].as_array().map(Vec::len), Some(24));
    assert!(v["tz"].as_str().is_some_and(|z| !z.is_empty()), "{v}");
    let cur = v["current"].as_u64().expect("now is in today") as usize;
    let vals = v["series"][0]["values"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    assert!(vals[..=cur].iter().all(|x| x == &json!(0.0)), "{v}");
    assert!(vals[cur + 1..].iter().all(Value::is_null), "{v}");
    let (_, v) = e.call("GET", "/api/stats/series?metric=done", None).await;
    assert_eq!(
        (&v["bucket"], v.get("spans"), v.get("current")),
        (&json!("day"), None, None)
    );
    for (q, field) in [
        ("series?metric=done&bucket=hour", "bucket"),
        ("series?metric=done&range=today&bucket=minute", "bucket"),
        ("items?hour=3", "hour"),
        ("items?range=today&hour=24", "hour"),
        ("breakdown?by=tool&range=today&hour=3", "hour"),
    ] {
        let (s, v) = e.call("GET", &format!("/api/stats/{q}"), None).await;
        assert_eq!(
            (s, v),
            (400, json!({"error": format!("参数不对：{field}")})),
            "{q}"
        );
    }
    for q in [
        "items?range=today&hour=3&only=done",
        "breakdown?by=model&range=today&hour=3",
        "series?metric=tokens&group=model&range=custom&from=2026-11-01&to=2026-11-01&bucket=hour",
    ] {
        let (s, v) = e.call("GET", &format!("/api/stats/{q}"), None).await;
        assert_eq!(s, 200, "{q}: {v}");
    }
    let (s, v) = e.call("GET", "/api/stats/items?limit=101", None).await;
    assert_eq!((s, v), (400, json!({"error": "参数不对：limit"})));
    let (s, v) = e.call("GET", "/api/stats/items", None).await;
    assert_eq!((s, &v["total"]), (200, &json!(0)), "{v}");
    let (s, v) = e.call("GET", "/api/stats/items?only=nope", None).await;
    assert_eq!((s, v), (400, json!({"error": "参数不对：only"})));
    let (s, v) = e.call("GET", "/api/stats/items?only=done", None).await;
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

    /* SSE: a change reaches the browser as a `stats` frame (the throttle's timing is the stream's own
     * unit test's business, not this one's) */
    let r = e
        .http
        .get(format!("{}/api/stream", e.base))
        .header("cookie", &e.cookie)
        .send()
        .await
        .expect("stream");
    let mut body = r.bytes_stream();
    let (s, _) = e.call("PUT", "/api/stats/prices", Some(prices(4.0))).await;
    assert_eq!(s, 200);
    let deadline = tokio::time::Instant::now() + Duration::from_secs(11);
    let mut got = false;
    let mut buf = String::new();
    while !got && let Ok(Some(Ok(chunk))) = tokio::time::timeout_at(deadline, body.next()).await {
        buf.push_str(&String::from_utf8_lossy(&chunk));
        while let Some(i) = buf.find("\n\n") {
            let frame: String = buf.drain(..i + 2).collect();
            if let Some(v) = frame
                .strip_prefix("data: ")
                .and_then(|d| serde_json::from_str::<Value>(d.trim()).ok())
                && v["type"] == "stats"
            {
                assert_eq!(v["ws"], "default");
                got = true;
            }
        }
    }
    assert!(got, "no stats frame");
}

/// A Models.dev stand-in on loopback. `mode` 0 serves v1 (etag `"v1"`), 1 answers 500, 2 serves v2
/// (GPT-5 input 1.25 → 3); a request carrying the current etag gets 304. Returns the URL and the
/// `If-None-Match` values seen.
async fn catalog_server(
    mode: Arc<std::sync::atomic::AtomicUsize>,
) -> (String, Arc<std::sync::Mutex<Vec<String>>>) {
    use axum::http::{HeaderMap, StatusCode, header};
    use axum::response::IntoResponse;
    let seen: Arc<std::sync::Mutex<Vec<String>>> = Arc::default();
    let s2 = seen.clone();
    let app = axum::Router::new().route(
        "/api.json",
        axum::routing::get(move |h: HeaderMap| {
            let (mode, seen) = (mode.clone(), s2.clone());
            async move {
                let inm = h
                    .get(header::IF_NONE_MATCH)
                    .and_then(|v| v.to_str().ok())
                    .unwrap_or_default()
                    .to_string();
                seen.lock().unwrap().push(inm.clone());
                let (etag, gpt) = match mode.load(std::sync::atomic::Ordering::SeqCst) {
                    1 => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
                    2 => ("\"v2\"", 3.0),
                    _ => ("\"v1\"", 1.25),
                };
                if inm == etag {
                    return StatusCode::NOT_MODIFIED.into_response();
                }
                let body = json!({
                    "openai": {"id": "openai", "models": {
                        "gpt-5": {"name": "GPT-5", "cost": {"input": gpt, "output": 10, "cache_read": 0.125}}
                    }},
                    "anthropic": {"id": "anthropic", "models": {
                        "claude-opus-5-5": {"name": "Claude Opus 5.5", "cost": {"input": 99, "output": 99, "cache_read": 9, "cache_write": 9}}
                    }}
                });
                ([(header::ETAG, etag)], axum::Json(body)).into_response()
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let port = listener.local_addr().expect("addr").port();
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    (format!("http://127.0.0.1:{port}/api.json"), seen)
}

#[tokio::test(flavor = "multi_thread")]
async fn models_dev_prices() {
    let mode = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let (url, seen) = catalog_server(mode.clone()).await;
    let e = start_with(&[("STEVE_PRICE_CATALOG_URL", url)]).await;
    let now = steve_store::now();
    let today = steve_stats::day(now, None);
    let yesterday = steve_stats::day(now - 86_400_000, None);
    let tomorrow = steve_stats::day(now + 86_400_000, None);
    // 1M GPT-5 input yesterday and today, 1M Opus input today (the table has it: $4), an unknown model
    {
        let c = e.db();
        c.execute_batch(&format!(
            "INSERT INTO stat_daily (workspace_id, day, source, purpose, harness, model, input, output, cache_read, cache_write, hit_base, calls, turns, turn_ms, tools, tool_kinds)
             VALUES ('default', '{yesterday}', 'agent', 'turn', 'codex', 'gpt-5', 1000000, 0, 0, 0, 0, 1, 1, 0, 0, '{{}}'),
                    ('default', '{today}', 'agent', 'turn', 'codex', 'gpt-5', 1000000, 0, 0, 0, 0, 1, 1, 0, 0, '{{}}'),
                    ('default', '{today}', 'agent', 'turn', 'claude-code', 'claude-opus-5-5', 1000000, 0, 0, 0, 0, 1, 1, 0, 0, '{{}}'),
                    ('default', '{today}', 'agent', 'turn', 'claude-code', 'mystery-1', 500, 0, 0, 0, 0, 1, 1, 0, 0, '{{}}');"
        ))
        .expect("seed");
    }
    let est = |v: &Value| v["usage"]["est"].as_f64().unwrap_or(-1.0);

    /* before any sync: GPT-5 is unpriced, not free */
    let (_, v) = e.call("GET", "/api/stats/summary", None).await;
    assert!((est(&v) - 4.0).abs() < 1e-9, "{}", v["spend"]);
    assert_eq!(
        v["spend"]["unpriced"],
        json!({"gpt-5": 2_000_000, "mystery-1": 500})
    );
    let (_, p) = e.call("GET", "/api/stats/prices", None).await;
    assert_eq!(
        (&p["catalog"]["enabled"], &p["catalog"]["synced"]),
        (&json!(true), &Value::Null)
    );

    /* the first sync prices GPT-5 on every day; the table keeps Opus at its own price */
    let (s, r) = e
        .call("POST", "/api/stats/prices/sync", Some(json!({})))
        .await;
    assert_eq!(
        (s, &r["models"], &r["changed"]),
        (200, &json!(2), &json!(2)),
        "{r}"
    );
    let (_, v) = e.call("GET", "/api/stats/summary", None).await;
    assert!((est(&v) - (4.0 + 2.5)).abs() < 1e-9, "{}", v["spend"]);
    assert_eq!(v["spend"]["unpriced"], json!({"mystery-1": 500}));
    assert_eq!(
        v["spend"]["priced_by"],
        json!({"models_dev": 2_000_000, "table": 1_000_000})
    );
    assert!(v["spend"]["catalog"]["synced"].as_i64().is_some());
    let (_, p) = e.call("GET", "/api/stats/prices", None).await;
    let model = |p: &Value, m: &str| {
        p["models"]
            .as_array()
            .into_iter()
            .flatten()
            .find(|x| x["model"] == m)
            .cloned()
            .unwrap_or(Value::Null)
    };
    let gpt = model(&p, "gpt-5");
    assert_eq!(gpt["price"]["source"], "models_dev");
    assert_eq!(gpt["price"]["match"], "openai/gpt-5");
    // cache writes are not listed: priced as input, never as free
    assert_eq!(gpt["price"]["cache_write"], json!(1.25));
    assert_eq!(model(&p, "claude-opus-5-5")["price"]["source"], "table");
    assert_eq!(
        p["unpriced"],
        json!([{"model": "mystery-1", "tokens": 500}])
    );
    let (_, b) = e.call("GET", "/api/stats/breakdown?by=model", None).await;
    let row = |key: &str| {
        b["rows"]
            .as_array()
            .into_iter()
            .flatten()
            .find(|r| r["key"] == key)
            .cloned()
            .unwrap_or(Value::Null)
    };
    assert_eq!(
        (&row("gpt-5")["priced"], &row("gpt-5")["price_source"]),
        (&json!(true), &json!("models_dev"))
    );
    assert_eq!(row("mystery-1")["priced"], json!(false));

    /* a failed sync keeps the last prices and says why */
    mode.store(1, std::sync::atomic::Ordering::SeqCst);
    let (s, r) = e
        .call("POST", "/api/stats/prices/sync", Some(json!({})))
        .await;
    assert_eq!(s, 502, "{r}");
    assert!(
        r["error"]
            .as_str()
            .unwrap_or_default()
            .contains("继续使用上次同步的价格"),
        "{r}"
    );
    let (_, p) = e.call("GET", "/api/stats/prices", None).await;
    assert!(
        p["catalog"]["error"]
            .as_str()
            .unwrap_or_default()
            .contains("500"),
        "{p}"
    );
    assert!(p["catalog"]["synced"].as_i64().is_some());
    let (_, v) = e.call("GET", "/api/stats/summary", None).await;
    assert!((est(&v) - 6.5).abs() < 1e-9, "{}", v["spend"]);

    /* unchanged upstream: a 304 on the stored etag, the error is cleared */
    mode.store(0, std::sync::atomic::Ordering::SeqCst);
    let (s, r) = e
        .call("POST", "/api/stats/prices/sync", Some(json!({})))
        .await;
    assert_eq!((s, &r["unchanged"]), (200, &json!(true)), "{r}");
    assert_eq!(
        seen.lock().unwrap().last().map(String::as_str),
        Some("\"v1\"")
    );
    let (_, p) = e.call("GET", "/api/stats/prices", None).await;
    assert_eq!(p["catalog"]["error"], Value::Null);

    /* a price change applies from tomorrow: the days already counted keep their estimate */
    mode.store(2, std::sync::atomic::Ordering::SeqCst);
    let (s, r) = e
        .call("POST", "/api/stats/prices/sync", Some(json!({})))
        .await;
    assert_eq!((s, &r["changed"]), (200, &json!(1)), "{r}");
    let (_, v) = e.call("GET", "/api/stats/summary", None).await;
    assert!((est(&v) - 6.5).abs() < 1e-9, "{}", v["spend"]);
    {
        let c = e.db();
        let book = steve_stats::PriceBook::new(vec![]).with_catalog(&c, None);
        let at = |d: &str| book.quote("gpt-5", d).map(|q| q.price.input);
        assert_eq!(
            (at(&yesterday), at(&today), at(&tomorrow)),
            (Some(1.25), Some(1.25), Some(3.0))
        );
    }

    /* a row of the table overrides the catalog, on every day */
    let (s, _) = e
        .call(
            "PUT",
            "/api/stats/prices",
            Some(json!({"prices": [
                {"match": "claude-opus-5-5", "input": 4, "output": 20, "cache_read": 0.2, "cache_write": 5},
                {"match": "gpt-5", "input": 0.5, "output": 4, "cache_read": 0.05, "cache_write": 0.5, "note": "代理折扣"}
            ]})),
        )
        .await;
    assert_eq!(s, 200);
    let (_, v) = e.call("GET", "/api/stats/summary", None).await;
    assert!((est(&v) - (4.0 + 1.0)).abs() < 1e-9, "{}", v["spend"]);
    assert_eq!(v["spend"]["priced_by"], json!({"table": 3_000_000}));
}
