//! Agent turns into the statistics tables on the wired Hub, with the scripted ACP agent of
//! steve-acp (`fake-acp.js`) as the claude-code adapter: one row per turn, per model token rows, a
//! queued follow-up as its own turn, and the daily cube. Temp STEVE_HOME, no real agent or host.
#![allow(clippy::type_complexity)]

use std::collections::HashMap;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use steve_runtime::testkit::eventually;

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
    let bus = Bus::new(std::sync::Arc::new(steve_runtime::cache::Caches::new(
        Default::default(),
    )));
    let w = steve_hub::wire::wire_with(&cfg, &bus, Arc::new(get.clone())).expect("wire");
    let st = AppState::new(cfg.clone(), w.backend, w.domain, bus).with_modules(w.modules);
    steve_hub::app::boot(&st).await.expect("boot");
    for job in w.jobs {
        job.start(&st.jobs);
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
    item_on(e, goal, &bot).await
}

async fn item_on(e: &Env, goal: &str, bot: &str) -> String {
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
    eventually("The session reports busy.", || async {
        e.session(&sid).await["busy"] == true
    })
    .await;
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
    // the first turn is under way on the Node (a message queued before it starts goes with it)
    eventually(
        "The first agent turn is recorded in the statistics table.",
        || async { e.count("SELECT count(*) FROM agent_turns") == 1 },
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
    let diagnostic = steve_runtime::testkit::on_timeout(|| async {
        format!("Both completed turns are recorded with their end times; two turns end: {:?} {:?}",
        e.db()
            .prepare("SELECT id, started, ended, stop_reason, node_attempt_id FROM agent_turns")
            .unwrap()
            .query_map([], |r| Ok(format!("{:?}/{:?}/{:?}/{:?}/{:?}", r.get::<_, String>(0)?, r.get::<_, i64>(1)?, r.get::<_, Option<i64>>(2)?, r.get::<_, Option<String>>(3)?, r.get::<_, Option<String>>(4)?)))
            .unwrap()
            .map(Result::unwrap)
            .collect::<Vec<_>>(),
        e.db()
            .prepare("SELECT kind, substr(data,1,200) FROM events WHERE kind<>'agent_message_chunk' ORDER BY seq")
            .unwrap()
            .query_map([], |r| Ok(format!("{}:{}", r.get::<_, String>(0)?, r.get::<_, String>(1)?)))
            .unwrap()
            .map(Result::unwrap)
            .collect::<Vec<_>>()
    )
    });
    eventually(diagnostic, || async {
        e.count("SELECT count(*) FROM agent_turns WHERE ended IS NOT NULL") == 2
    })
    .await;
    eventually("The session reports that it is no longer busy.", || async {
        e.session(&sid).await["busy"] == false
    })
    .await;
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
    // the Node names the tool kind by its category
    assert_eq!(turns[0].5, r#"{"exec":1,"tool:Bash":1}"#);
    let rows: Vec<(String, Option<i64>, Option<i64>, Option<i64>, Option<i64>, Option<f64>, String)> = c
        .prepare("SELECT model, input, output, cache_read, cache_write, cost_reported, detail FROM usage_records WHERE session_id = ? ORDER BY id")
        .unwrap()
        .query_map([&sid], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?, r.get(6)?)))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    // Billing tokens and context-meter dollars keep their independent epoch attribution. The
    // context counter is rebased from process .5 -> .8 to attempt increments .5 and .3.
    let token_rows: Vec<_> = rows
        .iter()
        .filter(|r| r.1.is_some() || r.2.is_some())
        .collect();
    assert_eq!(token_rows.len(), 3, "{rows:?}");
    let opus = token_rows
        .iter()
        .find(|r| r.0 == "claude-opus-5-5")
        .unwrap();
    assert_eq!(
        (opus.1, opus.2, opus.3, opus.4),
        (Some(10), Some(20), Some(30), Some(40))
    );
    assert_eq!(
        serde_json::from_str::<Value>(&opus.6).unwrap()["scope"],
        "all"
    );
    assert!(token_rows.iter().any(|r| r.0 == "claude-haiku-5-5"));
    let main = token_rows
        .iter()
        .find(|r| serde_json::from_str::<Value>(&r.6).unwrap()["scope"] == "main")
        .unwrap();
    assert_eq!((main.1, main.3), (Some(5), Some(7)));
    let mut dollars: Vec<f64> = rows.iter().filter_map(|r| r.5).collect();
    dollars.sort_by(f64::total_cmp);
    assert_eq!(dollars.len(), 2, "{rows:?}");
    assert!(
        (dollars[0] - 0.3).abs() < 1e-9 && (dollars[1] - 0.5).abs() < 1e-9,
        "{dollars:?}"
    );
    assert!((dollars.iter().sum::<f64>() - 0.8).abs() < 1e-9);
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

    // Real collection through the scripted ACP process, the Node's journal and the Hub's inbox.
    // Deliberately huge final usage must not replace or spike the live text estimate.
    // The earlier no-text, positive billing attempts are correctly unavailable coverage for
    // ten seconds. Give the stream its own Bot and exercise that real filter as well.
    e.db().execute_batch("INSERT INTO bots(id,workspace_id,name,harness,enabled,updated) VALUES ('realtime-bot','default','stream','claude-code',1,1);
        INSERT INTO bot_workspaces(bot_id,workspace_id,created) VALUES ('realtime-bot','default',1);").unwrap();
    let streaming = item_on(
        &e,
        "@drip 20 200 @tally 1 900000 0 0 @sleep 1000",
        "realtime-bot",
    )
    .await;
    eventually(
        "Realtime statistics include the streaming output estimate.",
        || async {
            let (s, v) = e
                .call("GET", "/api/stats/realtime?bot=realtime-bot", None)
                .await;
            s == 200
                && v["state"] == "estimated"
                && v["output_sessions"] == 1
                && e.session(&streaming).await["busy"] == true
        },
    )
    .await;
    eventually("The session reports that it is no longer busy.", || async {
        e.session(&streaming).await["busy"] == false
    })
    .await;
    eventually(
        "Every completed output session receives its end sample.",
        || async { e.count("SELECT COUNT(*) FROM realtime_output_samples WHERE kind='end'") == 3 },
    )
    .await;
    // The API's clock is rounded down to the current second. Settlement can leave >3s
    // since the final text: a read point just before the durable end is correctly partial.
    // Wait for that same clock to include the end; do not change source times or TPS units.
    let ended: i64 = e
        .db()
        .query_row(
            "SELECT MAX(at) FROM realtime_output_samples WHERE kind='end'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    let (_, before) = e
        .call("GET", "/api/stats/realtime?bot=realtime-bot", None)
        .await;
    eventually(
        "Realtime statistics advance beyond the final turn timestamp.",
        || async {
            let (s, v) = e
                .call("GET", "/api/stats/realtime?bot=realtime-bot", None)
                .await;
            s == 200 && v["at"].as_i64().is_some_and(|at| at >= ended)
        },
    )
    .await;
    let (status, realtime) = e
        .call(
            "GET",
            "/api/stats/realtime?bot=realtime-bot&range=custom&from=bad&to=bad",
            None,
        )
        .await;
    let receipt = json!({"persisted_end_at":ended,"before":before,"after":realtime});
    eprintln!("SETTLEMENT_CLOCK_RECEIPT={receipt}");
    if let Ok(out) = std::env::var("ST117_OUTPUTS") {
        std::fs::write(
            PathBuf::from(out).join("settlement-clock-receipt.json"),
            serde_json::to_vec_pretty(&receipt).unwrap(),
        )
        .unwrap();
    }
    assert_eq!(status, 200, "{realtime}");
    let at = realtime["at"].as_i64().unwrap();
    let units: i64 = e
        .db()
        .query_row(
            "SELECT COALESCE(SUM(units),0) FROM realtime_output_samples WHERE at>? AND at<=?",
            [at - 10_000, at],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(units, 70, "20 streamed text pieces have 70 ASCII scalars");
    assert_eq!(realtime["tps"], json!(units as f64 / 40.0), "{realtime}");
    assert_eq!(realtime["state"], "estimated");
    assert_eq!(realtime["coverage"]["measured"], 0);
    assert_eq!(realtime["series"].as_array().unwrap().len(), 60);
}
