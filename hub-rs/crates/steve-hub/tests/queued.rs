//! Queued session messages over HTTP on the wired Hub, with the scripted ACP agent of steve-acp
//! (`fake-acp.js`, run by node) as the claude-code adapter: edit, withdraw, send now during a slow
//! turn, and the 409 for a row a turn already took. Temp STEVE_HOME, no real agent or host.

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
        // on the Node: its turn is under way (not only the session opening)
        until(10, || async {
            let s = e.session(&sid).await;
            s["busy"] == true && s["status"] == "running"
        })
        .await,
        "the first turn runs"
    );
    sid
}

async fn say(e: &Env, sid: &str, text: &str) -> String {
    let (s, v) = e
        .call(
            "POST",
            &format!("/api/sessions/{sid}/messages"),
            Some(json!({"text": text, "mode": "排队"})),
        )
        .await;
    assert_eq!(s, 200, "{v}");
    v["result"].as_str().unwrap_or_default().to_string()
}

#[tokio::test(flavor = "multi_thread")]
async fn edit_withdraw_and_send_now_during_a_slow_turn() {
    let e = start().await;
    let sid = item(&e, "慢慢做 @sleep 20000").await;
    let (_, st) = e.call("GET", "/api/state", None).await;
    assert_eq!(st["hold"], Value::Null, "no release hold");

    // A persisted recovery receipt is already delivered, even though it is visible in queued.
    // All public mutations, including the legacy DELETE endpoint, must reject it.
    {
        let c = rusqlite::Connection::open(&e.db).expect("db");
        c.busy_timeout(Duration::from_secs(5)).unwrap();
        c.execute("INSERT INTO queued (id,session_id,text,via,created) VALUES ('q_recovery',?1,'already delivered','resumed:me',1)", [&sid]).unwrap();
    }
    let events_before = e.kinds(&sid).await;
    for (method, path, body) in [
        (
            "PATCH",
            format!("/api/sessions/{sid}/queued/q_recovery"),
            Some(json!({"text":"must not rewrite delivered work"})),
        ),
        (
            "DELETE",
            format!("/api/sessions/{sid}/queued/q_recovery"),
            None,
        ),
        (
            "POST",
            format!("/api/sessions/{sid}/queued/q_recovery/send-now"),
            None,
        ),
        ("DELETE", "/api/queued/q_recovery".into(), None),
    ] {
        let (status, result) = e.call(method, &path, body).await;
        assert_eq!(status, 409, "{method} {path}: {result}");
    }
    let recovery = e.queued(&sid).await;
    assert_eq!(recovery.len(), 1);
    assert_eq!(recovery[0]["text"], "already delivered");
    assert_eq!(recovery[0]["via"], "resumed:me");
    let queued_events = |events: Vec<String>| {
        events
            .into_iter()
            .filter(|kind| kind.starts_with("queued_"))
            .collect::<Vec<_>>()
    };
    assert_eq!(
        queued_events(e.kinds(&sid).await),
        queued_events(events_before),
        "rejected mutations publish no queue events"
    );
    assert_eq!(
        e.session(&sid).await["busy"],
        true,
        "send-now did not cancel the running turn"
    );
    // Remove only this synthetic fixture before exercising the existing ordinary-queue scenario.
    rusqlite::Connection::open(&e.db)
        .unwrap()
        .execute("DELETE FROM queued WHERE id='q_recovery'", [])
        .unwrap();

    // an approval the agent asked for from inside its turn: the card says the agent is still finishing
    {
        let c = rusqlite::Connection::open(&e.db).expect("db");
        c.busy_timeout(Duration::from_secs(5)).expect("timeout");
        c.execute(
            "INSERT INTO decisions (id, workspace_id, kind, subject_type, subject_id, item_id, session_id, question, status, requested_by, created)
             SELECT 'dec_midturn', workspace_id, 'approve', 'action', item_id, item_id, id, '可以推送吗？', 'open', 'steve', 1 FROM sessions WHERE id = ?1",
            [&sid],
        )
        .expect("decision");
    }

    for t in ["一", "二", "三"] {
        assert_eq!(say(&e, &sid, t).await, "queued");
    }
    // (the row was written behind the store's back; queueing published, so the state is fresh)
    assert_eq!(e.open_decision("dec_midturn").await["agent_busy"], true);
    let q = e.queued(&sid).await;
    let ids: Vec<String> = q
        .iter()
        .map(|x| x["id"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(
        q.iter()
            .map(|x| x["text"].as_str().unwrap())
            .collect::<Vec<_>>(),
        ["一", "二", "三"]
    );

    // edit
    let (s, v) = e
        .call(
            "PATCH",
            &format!("/api/sessions/{sid}/queued/{}", ids[1]),
            Some(json!({"text": "二（改过）"})),
        )
        .await;
    assert_eq!((s, v["text"].as_str()), (200, Some("二（改过）")), "{v}");
    let (s, _) = e
        .call(
            "PATCH",
            &format!("/api/sessions/{sid}/queued/{}", ids[1]),
            Some(json!({"text": "  "})),
        )
        .await;
    assert_eq!(s, 400, "blank text is refused");

    // withdraw: the row comes back for the composer
    let (s, v) = e
        .call(
            "DELETE",
            &format!("/api/sessions/{sid}/queued/{}", ids[0]),
            None,
        )
        .await;
    assert_eq!((s, v["text"].as_str()), (200, Some("一")), "{v}");
    let (s, _) = e
        .call(
            "DELETE",
            &format!("/api/sessions/{sid}/queued/{}", ids[0]),
            None,
        )
        .await;
    assert_eq!(s, 409, "already gone");

    // another workspace's view is a 404 (a made-up session)
    let (s, _) = e
        .call(
            "DELETE",
            &format!("/api/sessions/ses_nope/queued/{}", ids[2]),
            None,
        )
        .await;
    assert_eq!(s, 404);

    // send now: the slow turn is interrupted, the queue runs next with 三 first
    let (s, v) = e
        .call(
            "POST",
            &format!("/api/sessions/{sid}/queued/{}/send-now", ids[2]),
            None,
        )
        .await;
    assert_eq!((s, v["result"].as_str()), (200, Some("interrupted")), "{v}");
    assert!(
        until(10, || async { e.prompts().len() == 2 }).await,
        "the next turn starts: {:?}",
        e.prompts()
    );
    assert_eq!(e.prompts()[1], "三\n\n二（改过）");
    assert!(e.queued(&sid).await.is_empty());

    // the rows are gone with the turn: edit and send-now are 409s, nothing is resent
    for (m, p, b) in [
        (
            "PATCH",
            format!("/api/sessions/{sid}/queued/{}", ids[2]),
            Some(json!({"text": "晚了"})),
        ),
        (
            "POST",
            format!("/api/sessions/{sid}/queued/{}/send-now", ids[2]),
            None,
        ),
    ] {
        let (s, v) = e.call(m, &p, b).await;
        assert_eq!(s, 409, "{m} {p}: {v}");
        assert_eq!(v["error"], "这条消息已经发出，不能再改");
    }
    assert!(until(10, || async { e.session(&sid).await["busy"] == false }).await);
    assert_eq!(e.prompts().len(), 2);
    // the turn ended: the hint is gone (the session change refreshes the state)
    assert!(
        until(5, || async {
            e.open_decision("dec_midturn")
                .await
                .get("agent_busy")
                .is_none()
        })
        .await,
        "{}",
        e.open_decision("dec_midturn").await
    );
    let k = e.kinds(&sid).await;
    for x in [
        "queued",
        "queued_edited",
        "queued_removed",
        "queued_sent_now",
        "turn_end",
    ] {
        assert!(k.contains(&x.to_string()), "{x} in {k:?}");
    }
}
