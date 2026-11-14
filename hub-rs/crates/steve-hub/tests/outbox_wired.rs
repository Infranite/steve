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
    channels: steve_channels::Channels,
    outbox: Arc<steve_channels::Outbox>,
    fake: Arc<steve_channels::fake::channel::FakeChannel>,
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

struct NotificationFake(Arc<steve_channels::fake::channel::FakeChannel>);
impl steve_hub::extension::Extension for NotificationFake {
    fn id(&self) -> &str {
        "notification.fake"
    }
    fn declare(
        &self,
        d: &mut steve_hub::extension::Declare<'_>,
    ) -> Result<(), steve_hub::extension::ExtensionError> {
        use steve_channels::ChannelAdapter;
        use steve_channels::ext::{ChannelDecl, ChannelSide};
        d.channel(
            ChannelSide::new(
                ChannelDecl::new(steve_channels::fake::channel::FakeChannel::ID)
                    .handle_system("slack")
                    .features(self.0.features()),
                self.0.clone(),
            )
            .adapter(self.0.clone()),
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
    let fake = Arc::new(steve_channels::fake::channel::FakeChannel::new());
    let assembly = steve_hub::Assembly::builtin().with(NotificationFake(fake.clone()));
    let w =
        steve_hub::wire::wire_assembly(&cfg, &bus, Arc::new(get.clone()), &assembly).expect("wire");
    let channels = w.seams.channels.clone();
    let outbox = w.tools.outbox.clone();
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
        channels,
        outbox,
        fake,
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
    // all three consumers were registered while wiring, from the beginning of the topic
    assert_eq!(
        e.one(
            "SELECT count(*) FROM outbox_consumers WHERE topic = 'work.item.done' AND since_seq = 0"
        ),
        3
    );
    // An explicit personal log policy exercises real completion -> wired dispatcher -> inbox.
    let (s,v)=e.call("PUT","/api/me/notification-policy",Some(json!({
        "enabled":true,"mode":"log","party_id":null,"space_id":null,"timezone":"UTC","quiet_start":null,"quiet_end":null
    }))).await;
    assert_eq!(s, 200, "{v}");
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
        until(10, || e.one("SELECT count(*) FROM outbox_receipts") == 3).await,
        "all three consumers received it"
    );
    assert!(
        until(10, || e
            .one("SELECT count(*) FROM flow_runs WHERE status = 'done'")
            == 1)
        .await,
        "the flow ran"
    );
    assert!(
        until(10, || e.one("SELECT count(*) FROM personal_notifications")
            == 1)
        .await
    );
    let (s, inbox) = e.call("GET", "/api/me/notifications", None).await;
    assert_eq!(s, 200, "{inbox}");
    assert_eq!(inbox[0]["mode"], "log");
    assert!(inbox[0]["body"].as_str().unwrap().contains("写周报"));
    let id = inbox[0]["id"].as_str().unwrap();
    let (s, v) = e
        .call("POST", &format!("/api/me/notifications/{id}/seen"), None)
        .await;
    assert_eq!(s, 200, "{v}");
    assert_eq!(e.one("SELECT count(*) FROM output_deliveries"), 0);
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

/// healthz's `node` block without a Node (a STEVE_NO_RESUME copy, STEVE_NODE_MODE unset): off, nothing reconciled, and the
/// store's counts as they are: Node-bound sessions with queued messages are parked.
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
        ["/sessions", "/open", "/running", "/unknown", "/parked"].map(delta),
        [2, 1, 1, 1, 1],
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

/// Audit #5: the dead deliveries are listed for the instance owner, grouped by consumer, and revived.
#[tokio::test(flavor = "multi_thread")]
async fn dead_deliveries_are_listed_by_consumer_and_revived() {
    let e = start().await;
    let c = rusqlite::Connection::open(&e.db).expect("db");
    c.busy_timeout(Duration::from_secs(5)).expect("timeout");
    // a consumer this Hub does not run, so nothing delivers the revived rows meanwhile
    c.execute_batch(
        "INSERT INTO outbox_events (id, workspace_id, topic, version, payload, created) VALUES
           ('evt_d1', 'default', 'test.topic', 1, '{}', 1), ('evt_d2', 'default', 'test.topic', 1, '{}', 2);
         INSERT INTO outbox_leases (event_seq, consumer_id, attempts, last_error, next_at, dead, updated)
           SELECT seq, 'test.gone', 3, 'GitHub 503', 0, 1, created FROM outbox_events WHERE id IN ('evt_d1', 'evt_d2');",
    )
    .expect("rows");
    drop(c);
    let (s, v) = e.call("GET", "/api/system/outbox", None).await;
    assert_eq!(s, 200, "{v}");
    assert_eq!(v["dead"], 2, "{v}");
    assert_eq!(v["groups"].as_array().map(Vec::len), Some(1), "{v}");
    assert_eq!(v["groups"][0]["consumer"], "test.gone");
    assert_eq!(v["groups"][0]["count"], 2);
    assert_eq!(v["groups"][0]["last_error"], "GitHub 503");
    let (s, v) = e
        .call(
            "POST",
            "/api/system/outbox/revive",
            Some(json!({ "consumer": 7 })),
        )
        .await;
    assert_eq!(s, 400, "{v}");
    let (s, v) = e
        .call(
            "POST",
            "/api/system/outbox/revive",
            Some(json!({ "consumer": "test.gone" })),
        )
        .await;
    assert_eq!(s, 200, "{v}");
    assert_eq!(v["revived"], 2);
    assert_eq!(
        e.one("SELECT count(*) FROM outbox_leases WHERE dead = 0 AND attempts = 0"),
        2
    );
    let (_, v) = e.call("GET", "/api/system/outbox", None).await;
    assert_eq!(v["groups"], json!([]), "{v}");
}

#[tokio::test(flavor = "multi_thread")]
async fn real_completion_notification_reaches_e1_and_existing_c6_fake_adapter() {
    use steve_channels::fake::channel::FakeChannel;
    let e = start().await;
    let user = e.one_text("SELECT id FROM users WHERE subject='dev:local'");
    let who = steve_modules::Caller {
        ws: "default".into(),
        me: user,
    };
    FakeChannel::seed_connection(&e.store, &who, Some("u-notify-self")).unwrap();
    let (conn, _) = FakeChannel::seed_connection(&e.store, &who, None).unwrap();
    e.store
        .transaction(|tx| {
            steve_channels::spaces::set_account_rules(
                tx,
                &steve_store::base::Viewer::bare(&who.ws, &who.me),
                &conn,
                &json!([{"when":{"kind":"all"},"act":"record"}]),
                e.channels.now(),
            )
        })
        .unwrap();
    e.fake.space(
        "D_NOTIFICATION_SELF",
        "",
        steve_channels::SpaceKind::Dm,
        false,
        &[("u-notify-self", "Self")],
    );
    let inbound = e
        .channels
        .on_message(
            "default",
            &conn,
            FakeChannel::message(
                "D_NOTIFICATION_SELF",
                "notification-source",
                "u-notify-self",
                "record",
            ),
        )
        .await
        .unwrap();
    let party = e.one_text(
        "SELECT id FROM parties WHERE user_id=(SELECT id FROM users WHERE subject='dev:local')",
    );
    let (s,v)=e.call("PUT","/api/me/notification-policy",Some(json!({
        "enabled":true,"mode":"now","party_id":party,"space_id":inbound.space_id,"timezone":"UTC","quiet_start":null,"quiet_end":null
    }))).await;
    assert_eq!(s, 200, "{v}");
    let (_, state) = e.call("GET", "/api/state", None).await;
    let bot = state["bots"][0]["id"].as_str().unwrap();
    e.exec("INSERT INTO items(id,workspace_id,no,title,status,stage,bot_id,created,updated) VALUES('ST-91','default',91,'Real completion notification','run','执行',?,1,1)",&[&bot]);
    let (s, v) = e
        .call(
            "POST",
            "/api/items/ST-91/actions",
            Some(json!({"action":"finish"})),
        )
        .await;
    assert_eq!(s, 200, "{v}");
    assert!(
        until(15, || e.one(
            "SELECT count(*) FROM personal_notifications WHERE delivery_id IS NOT NULL"
        ) == 1)
        .await
    );
    assert_eq!(
        e.one("SELECT count(*) FROM outbox_events WHERE topic='work.item.done'"),
        1
    );
    assert_eq!(
        e.one("SELECT count(*) FROM outbox_receipts WHERE consumer_id='notifications.work_done'"),
        1
    );
    assert_eq!(e.one("SELECT count(*) FROM output_deliveries"), 1);
    // The test has no model backend: the unchanged disclosure contract leaves a draft. Explicit
    // existing C6 approval, not a notification bypass, then sends through the fake adapter.
    let status = e.one_text(
        "SELECT m.status FROM output_deliveries d JOIN chan_messages m ON m.id=d.message_id",
    );
    if status == "draft" {
        let card=e.one_text("SELECT m.decision_id FROM output_deliveries d JOIN chan_messages m ON m.id=d.message_id");
        let (s, v) = e
            .call(
                "POST",
                &format!("/api/decisions/{card}"),
                Some(json!({"choice":"yes","text":""})),
            )
            .await;
        assert_eq!(s, 200, "{v}");
        e.exec(
            "UPDATE decisions SET result=json_set(result,'$.send_at',0) WHERE id=?",
            &[&card],
        );
        e.outbox.send_due(&e.channels).await;
    }
    assert_eq!(e.fake.sent.lock().unwrap().len(), 1);
    assert!(
        e.fake.sent.lock().unwrap()[0]
            .1
            .text
            .contains("Real completion notification")
    );
    steve_channels::notifications::pump(&e.channels, &e.outbox)
        .await
        .unwrap();
    assert_eq!(e.fake.sent.lock().unwrap().len(), 1);
    assert_eq!(
        e.one("SELECT count(*) FROM output_deliveries WHERE attempts=1"),
        1
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn notification_targets_http_uses_live_caller_not_owner_or_query_identity() {
    use steve_channels::{SpaceKind, fake::channel::FakeChannel};
    use steve_modules::Caller;
    async fn get(e: &Env, cookie: Option<&str>, ws: &str) -> (u16, Value) {
        let mut req = e
            .http
            .get(format!(
                "{}/api/me/notification-targets?user_id=ignored&party_id=ignored",
                e.base
            ))
            .header("x-steve-ws", ws);
        if let Some(cookie) = cookie {
            req = req.header("cookie", cookie);
        }
        let response = req.send().await.unwrap();
        let status = response.status().as_u16();
        (status, response.json().await.unwrap())
    }
    let e = start().await;
    let owner = e.one_text("SELECT id FROM users WHERE subject='dev:local'");
    let other = e
        .store
        .users()
        .create("notification-target:other", "Other", None, "member")
        .unwrap();
    e.store
        .auth_sessions()
        .create(&other, "notification-target-member", 3_600_000)
        .unwrap();
    let other_cookie = "steve_session=notification-target-member";
    let me = Caller {
        ws: "default".into(),
        me: owner.clone(),
    };
    let them = Caller {
        ws: "default".into(),
        me: other.clone(),
    };
    FakeChannel::seed_connection(&e.store, &me, Some("target-owner")).unwrap();
    FakeChannel::seed_connection(&e.store, &them, Some("target-member")).unwrap();
    let (conn, _) = FakeChannel::seed_connection(&e.store, &me, None).unwrap();
    e.store
        .transaction(|tx| {
            steve_channels::spaces::set_account_rules(
                tx,
                &steve_store::base::Viewer::bare(&me.ws, &me.me),
                &conn,
                &json!([{"when":{"kind":"all"},"act":"record"}]),
                e.channels.now(),
            )
        })
        .unwrap();
    let mut expected = Vec::new();
    for (who, handle, ext) in [
        (&me, "target-owner", "D_TARGET_OWNER"),
        (&them, "target-member", "D_TARGET_MEMBER"),
    ] {
        e.fake.space(
            ext,
            "Personal DM",
            SpaceKind::Dm,
            false,
            &[(handle, "Self")],
        );
        let inbound = e
            .channels
            .on_message(
                "default",
                &conn,
                FakeChannel::message(ext, ext, handle, "record only"),
            )
            .await
            .unwrap();
        let party: String = e
            .store
            .reader(|c| -> steve_modules::ApiResult<String> {
                Ok(c.query_row(
                    "SELECT id FROM parties WHERE workspace_id=? AND user_id=?",
                    [&who.ws, &who.me],
                    |r| r.get(0),
                )?)
            })
            .unwrap();
        expected.push((party, inbound.space_id));
    }
    assert_eq!(get(&e, None, "default").await.0, 401);
    for (cookie, (party, space)) in [(&*e.cookie, &expected[0]), (other_cookie, &expected[1])] {
        let before = e.one("SELECT count(*) FROM audit");
        let (s, v) = get(&e, Some(cookie), "default").await;
        assert_eq!(s, 200, "{v}");
        assert_eq!(v["targets"].as_array().unwrap().len(), 1, "{v}");
        let t = &v["targets"][0];
        assert_eq!(t["party_id"], *party);
        assert_eq!(t["space_id"], *space);
        assert_eq!(t["channel"], "slack");
        assert_eq!(
            t.as_object().unwrap().len(),
            5,
            "no roster, account or credential fields"
        );
        assert_eq!(e.one("SELECT count(*) FROM audit"), before);
    }
    assert_eq!(e.one("SELECT count(*) FROM notification_policies"), 0);
    assert_eq!(e.one("SELECT count(*) FROM output_deliveries"), 0);
    assert!(e.fake.sent.lock().unwrap().is_empty());
    // Same caller, another live workspace: no cross-workspace choices or owner fallback.
    e.exec(
        "INSERT INTO workspaces(id,name,created) VALUES('notification-empty','Empty',1)",
        &[],
    );
    e.exec("INSERT INTO workspace_members(workspace_id,user_id,role,created) VALUES('notification-empty',?,'owner',1)", &[&owner]);
    let (s, v) = get(&e, Some(&e.cookie), "notification-empty").await;
    assert_eq!(s, 200, "{v}");
    assert_eq!(v, json!({"targets":[]}));
    assert_eq!(
        get(&e, Some(other_cookie), "notification-empty").await.0,
        409
    );
    // Losing the user link immediately empties just that caller's choices, without seeding a
    // replacement party or borrowing the workspace owner's route.
    let parties = e.one("SELECT count(*) FROM parties");
    e.exec(
        "UPDATE parties SET user_id=NULL WHERE id=?",
        &[&expected[1].0],
    );
    let (s, v) = get(&e, Some(other_cookie), "default").await;
    assert_eq!(s, 200, "{v}");
    assert_eq!(v, json!({"targets":[]}));
    assert_eq!(e.one("SELECT count(*) FROM parties"), parties);
    // A still-valid session is not membership. The HTTP Caller layer refuses the stale scope.
    e.exec(
        "DELETE FROM workspace_members WHERE workspace_id='default' AND user_id=?",
        &[&other],
    );
    assert_eq!(get(&e, Some(other_cookie), "default").await.0, 409);
    let (s, v) = get(&e, Some(&e.cookie), "default").await;
    assert_eq!(s, 200, "{v}");
    assert_eq!(v["targets"][0]["party_id"], expected[0].0);
}
