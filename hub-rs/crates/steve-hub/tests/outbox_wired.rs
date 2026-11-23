//! The first structural slice end to end, as wired in the binary: finishing an item through the API
//! commits `work.item.done` with it, the outbox job (started after the flow scheduler recovered) hands
//! it to `automation.item_trigger` and `work.tidy`, an item-triggered flow runs once (the old in-memory
//! `on_finished` path is gone), and healthz reports the outbox. On a temp STEVE_HOME; no agent and no
//! external host is contacted.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;
use steve_runtime::testkit::eventually_sync;
use steve_store::testkit::hub::outbox_checks;

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
    let bus = Bus::new(std::sync::Arc::new(steve_runtime::cache::Caches::new(
        Default::default(),
    )));
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
        outbox_checks::outbox_consumers_count_work_item_done_with_since_seq(&e.db).unwrap(),
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
    let (_, state) = e.call("GET", "/api/shell", None).await;
    let bot = state["bots"][0]["id"].as_str().expect("a bot").to_string();
    outbox_checks::seed_finish_item(&e.db, &bot).unwrap();
    let (s, v) = e
        .call(
            "POST",
            "/api/items/ST-90/lifecycle",
            Some(json!({"command": "finish", "expected_cycle": 1, "expected_revision": 0})),
        )
        .await;
    assert_eq!(s, 200, "{v}");
    assert_eq!(v["item"]["phase"], "closed", "{v}");
    eventually_sync(
        "The outbox completion closes the delivered work item.",
        || outbox_checks::items_count_st_90_closed_done(&e.db).unwrap() == 1,
    )
    .await;
    assert_eq!(
        outbox_checks::outbox_events_count_by_topic(&e.db, "work.item.done").unwrap(),
        1
    );
    eventually_sync("All three outbox consumers save their receipts.", || {
        outbox_checks::outbox_receipts_count(&e.db).unwrap() == 3
    })
    .await;
    eventually_sync("The flow run started by the outbox completes.", || {
        outbox_checks::flow_runs_count_by_status(&e.db, "done").unwrap() == 1
    })
    .await;
    eventually_sync(
        "The outbox creates exactly one personal notification.",
        || outbox_checks::personal_notifications_count(&e.db).unwrap() == 1,
    )
    .await;
    let (s, inbox) = e.call("GET", "/api/me/notifications", None).await;
    assert_eq!(s, 200, "{inbox}");
    assert_eq!(inbox[0]["mode"], "log");
    assert!(inbox[0]["body"].as_str().unwrap().contains("写周报"));
    let id = inbox[0]["id"].as_str().unwrap();
    let (s, v) = e
        .call("POST", &format!("/api/me/notifications/{id}/seen"), None)
        .await;
    assert_eq!(s, 200, "{v}");
    assert_eq!(outbox_checks::output_deliveries_count(&e.db).unwrap(), 0);
    // give a stray old path the time it would need, then: one event, one run
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert_eq!(
        outbox_checks::ext_events_count_by_type(&e.db, "item.done").unwrap(),
        1
    );
    assert_eq!(outbox_checks::flow_runs_count(&e.db).unwrap(), 1);
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
    let ws = outbox_checks::workspaces_id(&e.db).unwrap();
    let session = |id: &str, status: &str, readonly: i64| {
        outbox_checks::seed_health_session(&e.db, id, &ws, status, readonly).unwrap()
    };
    let queue = |id: &str| outbox_checks::seed_health_queue(&e.db, id).unwrap();
    let bind = |id: &str, node: &str, phase: &str| {
        outbox_checks::seed_health_binding(&e.db, id, node, phase).unwrap()
    };
    // Node-bound: one on the local Node with a message waiting, one released on a remote Node whose
    // last attempt nobody can tell the end of
    session("hz-local", "idle", 0);
    bind("hz-local", "local", "ready");
    queue("hz-local");
    session("hz-remote", "idle", 0);
    bind("hz-remote", "remote-1", "released");
    outbox_checks::seed_unknown_turn(&e.db).unwrap();
    outbox_checks::seed_unknown_attempt(&e.db).unwrap();
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
    let ready = steve_runtime::testkit::eventually(
        "The upgrade drain seals after external work ends.",
        || async {
            let status = e.domain.upgrade_drain("status").await.unwrap();
            status["ready"] == true
        },
    )
    .await;
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
    outbox_checks::seed_orphan_draft(&e.db).unwrap();
    let status = || outbox_checks::chan_messages_status_by_id(&e.db, "cm_d1").unwrap();
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

/// A decision about a chat message carries `open_url`, the AppLink that opens its chat in the
/// Feishu client (“在飞书中打开”), on the brand of the connection; other decisions carry none.
#[tokio::test(flavor = "multi_thread")]
async fn a_chat_message_decision_opens_its_chat_in_the_feishu_client() {
    use steve_channels::fake::channel::FakeChannel;
    let e = start().await;
    let user = outbox_checks::users_id_by_subject(&e.db, "dev:local").unwrap();
    let who = steve_modules::Caller {
        ws: "default".into(),
        me: user,
    };
    let (conn, _) = FakeChannel::seed_connection(&e.store, &who, None).unwrap();
    outbox_checks::set_lark_connection(&e.db, &conn).unwrap();
    outbox_checks::seed_chat_space(&e.db, &conn).unwrap();
    outbox_checks::seed_chat_decisions(&e.db).unwrap();
    let (status, open) = e
        .call("GET", "/api/chan-messages/cm_open/open-url", None)
        .await;
    assert_eq!(status, 200, "{open}");
    assert_eq!(
        open["url"],
        "https://applink.larksuite.com/client/chat/open?openChatId=oc_0000test0002"
    );
    let (_, other) = e
        .call("GET", "/api/chan-messages/cm_other/open-url", None)
        .await;
    assert!(other["url"].is_null(), "{other}");
}

/// Audit #5: the dead deliveries are listed for the instance owner, grouped by consumer, and revived.
#[tokio::test(flavor = "multi_thread")]
async fn dead_deliveries_are_listed_by_consumer_and_revived() {
    let e = start().await;
    outbox_checks::seed_dead_deliveries(&e.db).unwrap();
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
        outbox_checks::outbox_leases_count_with_dead_with_attempts(&e.db).unwrap(),
        2
    );
    let (_, v) = e.call("GET", "/api/system/outbox", None).await;
    assert_eq!(v["groups"], json!([]), "{v}");
}

#[tokio::test(flavor = "multi_thread")]
async fn real_completion_notification_reaches_e1_and_existing_c6_fake_adapter() {
    use steve_channels::fake::channel::FakeChannel;
    let e = start().await;
    let user = outbox_checks::users_id_by_subject(&e.db, "dev:local").unwrap();
    let who = steve_modules::Caller {
        ws: "default".into(),
        me: user,
    };
    FakeChannel::seed_connection(&e.store, &who, Some("u-notify-self")).unwrap();
    let (conn, _) = FakeChannel::seed_connection(&e.store, &who, None).unwrap();
    e.store
        .write(|tx| {
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
    let party = outbox_checks::parties_id_dev_local_for_selected_row(&e.db).unwrap();
    let (s,v)=e.call("PUT","/api/me/notification-policy",Some(json!({
        "enabled":true,"mode":"now","party_id":party,"space_id":inbound.space_id,"timezone":"UTC","quiet_start":null,"quiet_end":null
    }))).await;
    assert_eq!(s, 200, "{v}");
    let (_, state) = e.call("GET", "/api/shell", None).await;
    let bot = state["bots"][0]["id"].as_str().unwrap();
    outbox_checks::seed_notification_item(&e.db, bot).unwrap();
    let (s, v) = e
        .call(
            "POST",
            "/api/items/ST-91/lifecycle",
            Some(json!({"command": "finish", "expected_cycle": 1, "expected_revision": 0})),
        )
        .await;
    assert_eq!(s, 200, "{v}");
    eventually_sync(
        "The personal notification receives its durable delivery binding.",
        || outbox_checks::personal_notifications_count_with_value(&e.db).unwrap() == 1,
    )
    .await;
    assert_eq!(
        outbox_checks::outbox_events_count_by_topic(&e.db, "work.item.done").unwrap(),
        1
    );
    assert_eq!(
        outbox_checks::outbox_receipts_count_by_consumer(&e.db, "notifications.work_done").unwrap(),
        1
    );
    assert_eq!(outbox_checks::output_deliveries_count(&e.db).unwrap(), 1);
    // The test has no model backend: the unchanged disclosure contract leaves a draft. Explicit
    // existing C6 approval, not a notification bypass, then sends through the fake adapter.
    let delivery_status = || outbox_checks::output_deliveries_m_status(&e.db).unwrap();
    if delivery_status() == "draft" {
        let card = outbox_checks::output_deliveries_m_decision_id(&e.db).unwrap();
        let (s, v) = e
            .call(
                "POST",
                &format!("/api/decisions/{card}"),
                Some(json!({"choice":"yes","text":""})),
            )
            .await;
        assert_eq!(s, 200, "{v}");
        outbox_checks::make_reply_due(&e.db, &card).unwrap();
        e.outbox.send_due(&e.channels).await;
    }
    // The background queue can claim the message before the explicit pass returns.
    eventually_sync("The fake adapter send is recorded on the delivery.", || {
        delivery_status() == "sent"
    })
    .await;
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
        outbox_checks::output_deliveries_count_with_attempts(&e.db).unwrap(),
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
    let owner = outbox_checks::users_id_by_subject(&e.db, "dev:local").unwrap();
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
        .write(|tx| {
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
        let party = outbox_checks::notification_party(&e.store, &who.ws, &who.me).unwrap();
        expected.push((party, inbound.space_id));
    }
    assert_eq!(get(&e, None, "default").await.0, 401);
    for (cookie, (party, space)) in [(&*e.cookie, &expected[0]), (other_cookie, &expected[1])] {
        let before = outbox_checks::audit_count(&e.db).unwrap();
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
        assert_eq!(outbox_checks::audit_count(&e.db).unwrap(), before);
    }
    assert_eq!(
        outbox_checks::notification_policies_count(&e.db).unwrap(),
        0
    );
    assert_eq!(outbox_checks::output_deliveries_count(&e.db).unwrap(), 0);
    assert!(e.fake.sent.lock().unwrap().is_empty());
    // Same caller, another live workspace: no cross-workspace choices or owner fallback.
    outbox_checks::seed_notification_workspace(&e.db).unwrap();
    outbox_checks::seed_notification_member(&e.db, &owner).unwrap();
    let (s, v) = get(&e, Some(&e.cookie), "notification-empty").await;
    assert_eq!(s, 200, "{v}");
    assert_eq!(v, json!({"targets":[]}));
    assert_eq!(
        get(&e, Some(other_cookie), "notification-empty").await.0,
        409
    );
    // Losing the user link immediately empties just that caller's choices, without seeding a
    // replacement party or borrowing the workspace owner's route.
    let parties = outbox_checks::parties_count(&e.db).unwrap();
    outbox_checks::detach_party_user(&e.db, &expected[1].0).unwrap();
    let (s, v) = get(&e, Some(other_cookie), "default").await;
    assert_eq!(s, 200, "{v}");
    assert_eq!(v, json!({"targets":[]}));
    assert_eq!(outbox_checks::parties_count(&e.db).unwrap(), parties);
    // A still-valid session is not membership. The HTTP Caller layer refuses the stale scope.
    outbox_checks::remove_notification_member(&e.db, &other).unwrap();
    assert_eq!(get(&e, Some(other_cookie), "default").await.0, 409);
    let (s, v) = get(&e, Some(&e.cookie), "default").await;
    assert_eq!(s, 200, "{v}");
    assert_eq!(v["targets"][0]["party_id"], expected[0].0);
}
