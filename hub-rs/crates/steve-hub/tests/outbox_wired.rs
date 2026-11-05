//! The first structural slice end to end, as wired in the binary: finishing an item through the API
//! commits `work.item.done` with it, the outbox job (started after the flow scheduler recovered) hands
//! it to `automation.item_trigger` and `work.tidy`, an item-triggered flow runs once (the old in-memory
//! `on_finished` path is gone), and healthz reports the outbox. On a temp STEVE_HOME; no agent and no
//! external host is contacted.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use serde_json::{Value, json};
use steve_hub::api::AppState;
use steve_hub::bus::Bus;
use steve_hub::config::Config;

struct Env {
    domain: Arc<dyn steve_hub::backend::Domain>,
    store: steve_store::Store,
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

    fn one(&self, sql: &str) -> i64 {
        let c = rusqlite::Connection::open(&self.db).expect("db");
        c.busy_timeout(Duration::from_secs(5)).expect("timeout");
        c.query_row(sql, [], |r| r.get(0)).expect("sql")
    }

    fn one_text(&self, sql: &str) -> String {
        let c = rusqlite::Connection::open(&self.db).expect("db");
        c.busy_timeout(Duration::from_secs(5)).expect("timeout");
        c.query_row(sql, [], |r| r.get(0)).expect("sql")
    }

    fn exec(&self, sql: &str, p: &[&dyn rusqlite::ToSql]) {
        let c = rusqlite::Connection::open(&self.db).expect("db");
        c.busy_timeout(Duration::from_secs(5)).expect("timeout");
        c.execute(sql, p).expect("sql");
    }
}

async fn until(secs: u64, f: impl Fn() -> bool) -> bool {
    let end = tokio::time::Instant::now() + Duration::from_secs(secs);
    while tokio::time::Instant::now() < end {
        if f() {
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
    let db = cfg.data_dir.join("steve.sqlite3");
    let bus = Bus::new();
    let w = steve_hub::wire::wire_with(&cfg, &bus, Arc::new(get.clone())).expect("wire");
    let domain = w.domain.clone();
    let store = w.modules.as_ref().unwrap().store.clone();
    let st = AppState::new(cfg.clone(), w.backend, w.domain, bus)
        .with_modules(w.modules)
        .with_node(w.node);
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
        domain,
        store,
        _home: home,
        base,
        http,
        cookie,
        db,
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn finishing_through_the_api_runs_an_item_flow_once_through_the_outbox() {
    let e = start().await;
    // both consumers were registered while wiring, from the beginning of the topic
    assert_eq!(
        e.one(
            "SELECT count(*) FROM outbox_consumers WHERE topic = 'work.item.done' AND since_seq = 0"
        ),
        2
    );
    let (s, v) = e
        .call(
            "POST",
            "/api/flows",
            Some(json!({"name": "完成后记一笔", "graph": {
                "nodes": [{"id": "t", "kind": "trigger", "config": {"source": "item", "types": ["item.done"]}},
                          {"id": "d", "kind": "action", "config": {"action": "digest", "text": "{{event.payload.item_id}} 做完了"}}],
                "edges": [{"from": "t", "to": "d"}]}})),
        )
        .await;
    assert_eq!(s, 200, "{v}");
    let fid = v["id"].as_str().expect("flow id").to_string();
    let (s, v) = e
        .call(
            "PATCH",
            &format!("/api/flows/{fid}"),
            Some(json!({"enabled": true})),
        )
        .await;
    assert_eq!(s, 200, "{v}");
    // an item in progress, without a session (no agent runs here)
    let (_, state) = e.call("GET", "/api/state", None).await;
    let bot = state["bots"][0]["id"].as_str().expect("a bot").to_string();
    e.exec(
        "INSERT INTO items (id, workspace_id, no, title, status, stage, bot_id, created, updated) VALUES ('ST-90', 'default', 90, '写周报', 'run', '执行', ?, 1, 1)",
        &[&bot],
    );
    let (s, v) = e
        .call(
            "POST",
            "/api/items/ST-90/actions",
            Some(json!({"action": "finish"})),
        )
        .await;
    assert_eq!(s, 200, "{v}");
    // the request returns at once; with the modules' hooks wired the close commits once they answered
    assert_eq!(v["phase"], "closing", "{v}");
    assert!(
        until(10, || e.one(
            "SELECT count(*) FROM items WHERE id = 'ST-90' AND phase = 'closed' AND status = 'done'"
        ) == 1)
        .await,
        "closed"
    );
    assert_eq!(
        e.one("SELECT count(*) FROM outbox_events WHERE topic = 'work.item.done'"),
        1
    );
    assert!(
        until(10, || e.one("SELECT count(*) FROM outbox_receipts") == 2).await,
        "both consumers received it"
    );
    assert!(
        until(10, || e
            .one("SELECT count(*) FROM flow_runs WHERE status = 'done'")
            == 1)
        .await,
        "the flow ran"
    );
    // give a stray old path the time it would need, then: one event, one run
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert_eq!(
        e.one("SELECT count(*) FROM ext_events WHERE type = 'item.done'"),
        1
    );
    assert_eq!(e.one("SELECT count(*) FROM flow_runs"), 1);
    let (s, h) = e.call("GET", "/healthz", None).await;
    assert_eq!(s, 200);
    assert_eq!(h["ok"], true);
    assert_eq!(
        (
            h["outbox"]["pending"].as_i64(),
            h["outbox"]["dead"].as_i64()
        ),
        (Some(0), Some(0)),
        "{h}"
    );
}

/// healthz's `node` block without a Node (STEVE_NODE_MODE unset): off, nothing reconciled, and the
/// store's counts as they are: Node-bound sessions with queued messages are parked, and legacy
/// sessions that could still run (and those with work in hand) are counted for the runner's removal.
#[tokio::test(flavor = "multi_thread")]
async fn healthz_reports_the_node_path_without_a_node() {
    let e = start().await;
    let node = || async { e.call("GET", "/healthz", None).await.1["node"].clone() };
    let before = node().await;
    assert_eq!(
        (
            &before["enabled"],
            &before["remote"],
            &before["reconciled"],
            &before["stopped"],
            &before["fallbacks"]
        ),
        (
            &json!(false),
            &json!(false),
            &Value::Null,
            &json!(false),
            &json!(0)
        ),
        "{before}"
    );
    let n = |v: &Value, k: &str| v.pointer(k).and_then(Value::as_i64).expect(k);
    let ws = e.one_text("SELECT id FROM workspaces LIMIT 1");
    let session = |id: &str, status: &str, readonly: i64| {
        e.exec(
            "INSERT INTO sessions (id, workspace_id, harness, cwd, status, readonly, created, updated) VALUES (?, ?, 'claude', '', ?, ?, 0, 0)",
            &[&id, &ws, &status, &readonly],
        )
    };
    let queue = |id: &str| {
        e.exec(
            "INSERT INTO queued (id, session_id, text, via, created) VALUES (?, ?, 'x', 'web', 0)",
            &[&format!("q-{id}"), &id],
        )
    };
    let bind = |id: &str, node: &str, phase: &str| {
        e.exec(
            "INSERT INTO runtime_sessions (session_id, node_id, workspace_ref, node_session_id, config_revision, phase, updated) VALUES (?, ?, 'w', ?, 'r', ?, 0)",
            &[&id, &node, &id, &phase],
        )
    };
    // legacy: idle, with a queued message, with a submitted dispatch, ended with a pending resume, read-only
    session("hz-idle", "idle", 0);
    session("hz-queued", "idle", 0);
    queue("hz-queued");
    session("hz-submitted", "running", 0);
    e.exec(
        "INSERT INTO session_dispatch (id, session_id, text, via, created, state, updated) VALUES ('d1', 'hz-submitted', 'x', 'web', 0, 'submitted', 0)",
        &[],
    );
    session("hz-ended", "ended", 0);
    e.exec(
        "INSERT INTO session_resume (session_id, created) VALUES ('hz-ended', 0)",
        &[],
    );
    session("hz-readonly", "idle", 1);
    // Node-bound: one on the local Node with a message waiting, one released on a remote Node whose
    // last attempt nobody can tell the end of
    session("hz-local", "idle", 0);
    bind("hz-local", "local", "ready");
    queue("hz-local");
    session("hz-remote", "idle", 0);
    bind("hz-remote", "remote-1", "released");
    e.exec(
        "INSERT INTO runtime_turns (turn_id, session_id, state, created, updated) VALUES ('t1', 'hz-remote', 'unknown', 0, 0)",
        &[],
    );
    e.exec(
        "INSERT INTO runtime_attempts (attempt_id, turn_id, ordinal, node_id, operation_id, execution, spec, digest, state, created, updated) VALUES ('a1', 't1', 0, 'remote-1', 'op1', 'e', '{}', 'd', 'unknown', 0, 0)",
        &[],
    );
    let after = node().await;
    let delta = |k: &str| n(&after, k) - n(&before, k);
    assert_eq!(
        [
            "/sessions",
            "/open",
            "/running",
            "/unknown",
            "/parked",
            "/legacy/sessions",
            "/legacy/busy"
        ]
        .map(delta),
        [2, 1, 1, 1, 1, 3, 3],
        "{before} -> {after}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn upgrade_drain_blocks_new_mutations_and_waits_for_existing_external_work() {
    let e = start().await;
    let permit = e.store.try_activity("mail_send").unwrap();
    let token = "synthetic-supervisor-token-for-test-00000000000000000";
    let controls = e.db.parent().unwrap().parent().unwrap().join("updates");
    std::fs::create_dir_all(&controls).unwrap();
    std::fs::write(
        controls.join("control-token.json"),
        serde_json::to_vec(token).unwrap(),
    )
    .unwrap();
    let response = e
        .http
        .post(format!("{}/internal/upgrade/drain", e.base))
        .bearer_auth(token)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status().as_u16(), 200);
    let status: Value = response.json().await.unwrap();
    assert_eq!(status["maintenance"], true);
    assert_eq!(status["ready"], false);
    assert_eq!(status["running"]["mail_send"], 1);
    let (status, _) = e
        .call("POST", "/api/workspaces", Some(json!({"name":"blocked"})))
        .await;
    assert_eq!(status, 503);
    let (status, _) = e.call("GET", "/internal/upgrade/drain", None).await;
    assert_eq!(
        status, 404,
        "owner cookie is not supervisor control authorization"
    );
    drop(permit);
    let mut ready = false;
    for _ in 0..30 {
        let status = e.domain.upgrade_drain("status").await.unwrap();
        if status["ready"] == true {
            ready = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(ready);
    assert!(
        e.store.continuing_activity("late_mcp", || true).is_none(),
        "ready seals even stale running-token admission"
    );
    e.domain.upgrade_drain("start").await.unwrap();
    assert!(
        e.store
            .continuing_activity("retried_start", || true)
            .is_none()
    );
    e.domain.upgrade_drain("cancel").await.unwrap();
    assert!(!e.store.maintenance());
    assert!(e.store.try_activity("mail_send").is_some());
}

/// Taking a drafted reply out of 待处理 discards the draft (it would otherwise wait in its
/// conversation for good); undo puts it back; a second dismissal is refused.
#[tokio::test(flavor = "multi_thread")]
async fn dismissing_a_draft_reply_discards_it() {
    let e = start().await;
    // a draft of a space this test does not set up: rows only, without their references
    let c = rusqlite::Connection::open(&e.db).expect("db");
    c.busy_timeout(Duration::from_secs(5)).expect("timeout");
    c.execute_batch(
        "PRAGMA foreign_keys = OFF;
         INSERT INTO chan_messages (id, workspace_id, space_id, direction, text, status, decision_id, created)
         VALUES ('cm_d1', 'default', 'sp_x', 'out', '要我处理吗？', 'draft', 'dc_d1', 1);
         INSERT INTO decisions (id, workspace_id, kind, subject_type, subject_id, question, options, status, requested_by, created)
         VALUES ('dc_d1', 'default', 'approve', 'chan_message', 'cm_d1', '以机器人发到「群」', '[\"yes\",\"no\"]', 'open', 'steve', 1);",
    )
    .expect("rows");
    drop(c);
    let status = || e.one_text("SELECT status FROM chan_messages WHERE id = 'cm_d1'");
    let (s, v) = e.call("POST", "/api/decisions/dc_d1/dismiss", None).await;
    assert_eq!(s, 200, "{v}");
    assert_eq!(v["result"]["choice"], "no", "{v}");
    assert_eq!(status(), "discarded");
    let (s, v) = e.call("POST", "/api/decisions/dc_d1/dismiss", None).await;
    assert_eq!(s, 409, "{v}");
    let (s, v) = e.call("POST", "/api/decisions/dc_d1/undo", None).await;
    assert_eq!(s, 200, "{v}");
    assert_eq!(status(), "draft");
}
