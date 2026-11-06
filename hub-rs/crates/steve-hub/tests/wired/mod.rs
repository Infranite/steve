//! (part of `tests/modules.rs`) The event modules as wired in the binary (S1/S2 signals and flows, G2 GitHub, E1 mail, C1/F1/F5/F6
//! channels and Feishu): their routes are mounted behind the login, and an event handed to each
//! producer's wired seam comes out of `GET /api/signals`. In process (`wire_with` on a variable map),
//! on a temp STEVE_HOME with a file keystore; no agent and no external host is ever contacted (the
//! test connections point their API base at a closed local port).

use std::collections::HashMap;
use std::net::SocketAddr;
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
    db: std::path::PathBuf,
    seams: steve_hub::wire::modules::Seams,
    tools: steve_hub::wire::modules::Tools,
    vars: Arc<HashMap<String, String>>,
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

    fn sql(&self, sql: &str, params: &[&dyn rusqlite::ToSql]) {
        let c = rusqlite::Connection::open(&self.db).expect("db");
        c.busy_timeout(Duration::from_secs(5)).expect("timeout");
        c.execute(sql, params).expect("sql");
    }

    async fn signals(&self) -> Vec<Value> {
        let (s, v) = self.call("GET", "/api/signals", None).await;
        assert_eq!(s, 200, "{v}");
        v["items"].as_array().cloned().unwrap_or_default()
    }

    /// A connection of adapter `adapter` owned by `me` (on “我”), as the connect flow would leave it.
    fn connection(
        &self,
        me: &str,
        me_party: &str,
        adapter: &str,
        system: &str,
        handle: &str,
    ) -> String {
        let (acc, con) = (format!("acc_{adapter}"), format!("con_{adapter}"));
        self.sql(
            "INSERT INTO accounts (id, workspace_id, system_id, family, party_id, kind, handle, handle_display, source, created, updated)
             VALUES (?1, 'default', ?2, ?2, ?3, 'personal', ?4, ?4, 'connect', 1, 1)",
            &[&acc, &system, &me_party, &handle],
        );
        self.sql(
            "INSERT INTO connections (id, workspace_id, account_id, adapter, auth, caps, status, config, created_by, created, updated)
             VALUES (?1, 'default', ?2, ?3, 'credential', '[\"events\"]', 'ok', '{\"api_base\":\"http://127.0.0.1:9\"}', ?4, 1, 1)",
            &[&con, &acc, &adapter, &me],
        );
        con
    }
}

async fn start() -> Env {
    start_with(&[]).await
}

async fn start_with(extra: &[(&str, String)]) -> Env {
    // no real provider host, whatever a test connection says (STEVE_FORBID_REAL_HOSTS=1 in process)
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
        db: cfg.db_path(),
        _home: home,
        base,
        http,
        cookie,
        seams: w.seams,
        tools: w.tools,
        vars,
    }
}

async fn until_signal(e: &Env, summary: &str) -> Value {
    for _ in 0..100 {
        if let Some(s) = e
            .signals()
            .await
            .into_iter()
            .find(|s| s["summary"] == summary)
        {
            return s;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let c = rusqlite::Connection::open(&e.db).expect("db");
    let rows: Vec<(String, String, String)> = c
        .prepare("SELECT summary, visibility, coalesce(outcome, '') FROM ext_events")
        .and_then(|mut st| {
            st.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
                .collect()
        })
        .unwrap_or_default();
    panic!(
        "no signal “{summary}”: {:?}; ext_events: {rows:?}",
        e.call("GET", "/api/signals", None).await
    );
}

/// The dev user and their “我” contact.
async fn owner(e: &Env) -> (String, String) {
    let (_, me) = e.call("GET", "/api/me", None).await;
    let me_id = me["user"]["id"].as_str().expect("me").to_string();
    let (_, mine) = e.call("GET", "/api/me/contact", None).await;
    let me_party = mine["id"]
        .as_str()
        .or_else(|| mine["party"]["id"].as_str())
        .expect("me contact")
        .to_string();

    (me_id, me_party)
}

/// S1/S2, G2, E1, C1/F1/F6: routes behind the login; the providers and sources are registered.
#[tokio::test(flavor = "multi_thread")]
async fn signal_modules_mount_their_routes() {
    let e = start().await;
    // routes of every module, behind the login
    for path in [
        "/api/signals",
        "/api/sources",
        "/api/flows",
        "/api/mail/accounts",
        "/api/spaces",
        "/api/github",
        "/api/vault/uses",
        "/api/personas",
        "/api/connections/adapters",
    ] {
        let (s, v) = e.call("GET", path, None).await;
        assert_eq!(s, 200, "{path}: {v}");
    }
    let (_, adapters) = e.call("GET", "/api/connections/adapters", None).await;
    for a in ["feishu_app", "feishu_user", "imap", "github_pat"] {
        assert!(adapters.to_string().contains(a), "{a}: {adapters}");
    }
    let (_, sources) = e.call("GET", "/api/sources", None).await;
    assert!(
        sources.to_string().contains("github.check_failed"),
        "{sources}"
    );
    // nothing connected yet: no mailbox, no space
    assert_eq!(e.call("GET", "/api/mail/accounts", None).await.1, json!([]));
    let anon = reqwest::Client::new();
    let r = anon
        .get(format!("{}/api/signals", e.base))
        .send()
        .await
        .expect("anon");
    assert_eq!(r.status().as_u16(), 401);
}

/// G2 → S1.
#[tokio::test(flavor = "multi_thread")]
async fn github_signals_reach_the_signals_page() {
    let e = start().await;
    // the signals page lists recent events (`range` defaults to the last days)
    let now = steve_store::now();
    let (me_id, me_party) = owner(&e).await;
    // G2: a GitHub signal through the wired sink
    let gh = e.connection(&me_id, &me_party, "github_pat", "github", "octo");
    e.seams
        .github
        .ingest(
            "default",
            &gh,
            steve_github::events::SignalEvent {
                type_key: "github.check_failed".into(),
                external_id: "check:1".into(),
                occurred: now,
                thread_key: Some("pr:ghp_1".into()),
                group_key: Some("repo:ghr_1".into()),
                scope_ext_id: Some("octo/app".into()),
                actor: Some(steve_github::events::Actor {
                    login: "octo".into(),
                    kind: "User".into(),
                }),
                summary: "octo/app · PR #1 检查失败：build".into(),
                payload: json!({"repo": "octo/app", "number": 1}),
            },
        )
        .await
        .expect("github ingest");
    let s = until_signal(&e, "octo/app · PR #1 检查失败：build").await;
    assert_eq!(
        (s["source"].as_str(), s["type"].as_str()),
        (Some("github"), Some("github.check_failed"))
    );
}

/// E1 → S1, live and with a fixed outcome.
#[tokio::test(flavor = "multi_thread")]
async fn mail_events_reach_the_signals_page() {
    let e = start().await;
    // the signals page lists recent events (`range` defaults to the last days)
    let now = steve_store::now();
    let (me_id, me_party) = owner(&e).await;
    // E1: live mail, and mail of a muted thread
    let mail = e.connection(&me_id, &me_party, "imap", "mail", "me@example.test");
    let ev = |id: &str, summary: &str| steve_mail::MailEvent {
        type_key: "mail.new".into(),
        also: vec![],
        external_id: id.into(),
        occurred: now,
        thread_key: Some("mt_1".into()),
        group_key: Some("example.test".into()),
        scope_ext_id: Some("INBOX".into()),
        actor: Some(steve_mail::events::Actor {
            family: "email".into(),
            handle: "sarah@example.test".into(),
            display_name: Some("Sarah".into()),
            signature_org: None,
            account_id: None,
            party_id: None,
        }),
        summary: summary.into(),
        payload: json!({"subject": "Friday"}),
    };
    let id = e
        .seams
        .mail
        .deliver(
            "default",
            &mail,
            ev("ml_1", "Sarah：Friday review"),
            steve_mail::Delivery::Ingest,
        )
        .await
        .expect("mail");
    assert!(id.is_some_and(|i| i.starts_with("evt_")));
    let s = until_signal(&e, "Sarah：Friday review").await;
    assert_eq!(s["source"], "mail");
    e.seams
        .mail
        .deliver(
            "default",
            &mail,
            ev("ml_2", "Sarah：静音的往来"),
            steve_mail::Delivery::muted(),
        )
        .await
        .expect("muted");
    let c = rusqlite::Connection::open(&e.db).expect("db");
    let (outcome, label): (String, String) = c
        .query_row(
            "SELECT outcome, outcome_label FROM ext_events WHERE external_id = 'ml_2'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .expect("muted row");
    assert_eq!(
        (outcome.as_str(), label.as_str()),
        ("ignored", "这串往来已静音")
    );
}

/// C1 → S1; `/hooks/*` refuse unsigned deliveries.
#[tokio::test(flavor = "multi_thread")]
async fn channel_messages_reach_the_signals_page_and_hooks_verify() {
    let e = start().await;
    // the signals page lists recent events (`range` defaults to the last days)
    let now = steve_store::now();
    let (me_id, me_party) = owner(&e).await;
    let gh = e.connection(&me_id, &me_party, "github_pat", "github", "octo");
    let anon = reqwest::Client::new();
    // C1: a Feishu message through the wired channels (its space lookup fails on the closed port)
    let fs = e.connection(&me_id, &me_party, "feishu_app", "feishu_app", "cli_test");
    use steve_channels::MessageSink;
    let msg = steve_channels::InboundMessage::text(
        "oc_chat1",
        "om_1",
        steve_channels::HandleRef::new("feishu", "on_alice"),
        "周五的评审改到两点",
        now,
    );
    e.seams
        .channels
        .inbound("default", &fs, msg)
        .await
        .expect("inbound");
    let im = e.signals().await.into_iter().find(|s| s["source"] == "im");
    assert!(im.is_some(), "{:?}", e.signals().await);

    // the hooks: unknown connection and an unsigned GitHub delivery are both 401, nothing written
    let before = e.signals().await.len();
    for path in ["/hooks/con_nope", &format!("/hooks/{gh}"), "/hooks/github"] {
        let r = anon
            .post(format!("{}{path}", e.base))
            .body("{}")
            .send()
            .await
            .expect("hook");
        assert_eq!(r.status().as_u16(), 401, "{path}");
        assert!(r.text().await.unwrap_or_default().is_empty());
    }
    assert_eq!(e.signals().await.len(), before);
}

/// Polls `f` for up to `secs` seconds.
async fn until<F: std::future::Future<Output = bool>>(secs: u64, mut f: impl FnMut() -> F) -> bool {
    for _ in 0..secs * 20 {
        if f().await {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    false
}

fn one<T: rusqlite::types::FromSql>(
    db: &std::path::Path,
    sql: &str,
    p: &[&dyn rusqlite::ToSql],
) -> Option<T> {
    use rusqlite::OptionalExtension;
    let c = rusqlite::Connection::open(db).ok()?;
    c.busy_timeout(Duration::from_secs(5)).ok()?;
    c.query_row(sql, p, |r| r.get(0)).optional().ok().flatten()
}

/// E1/E3: a mailbox connected over the API (fake IMAP/SMTP with a test CA), a draft written and
/// sent through its approval decision, and the mail arrives at the fake SMTP server.
#[tokio::test(flavor = "multi_thread")]
async fn a_mail_draft_goes_out_through_its_approval() {
    use steve_mail::fake::{FakeImap, FakeSmtp, ImapConfig, SmtpConfig, TestCa, TlsMode};
    let ca = TestCa::generate().expect("ca");
    let imap = FakeImap::start(ImapConfig {
        ca: Some(ca.clone()),
        ..Default::default()
    })
    .await
    .expect("imap");
    let smtp = FakeSmtp::start(SmtpConfig {
        ca: Some(ca.clone()),
        ..Default::default()
    })
    .await
    .expect("smtp");
    let ca_dir = tempfile::tempdir().expect("tmp");
    let pem = ca_dir.path().join("ca.pem");
    std::fs::write(&pem, ca.ca_pem()).expect("pem");
    let e = start_with(&[("STEVE_MAIL_TEST_CA", pem.display().to_string())]).await;
    assert!(e.vars.contains_key("STEVE_MAIL_TEST_CA"));
    assert_eq!(e.call("POST", "/api/vault/init", None).await.0, 200);
    let sec = |m: TlsMode| {
        if m == TlsMode::StartTls {
            "starttls"
        } else {
            "tls"
        }
    };
    let (s, c) = e
        .call(
            "POST",
            "/api/connections",
            Some(json!({"adapter": "imap", "fields": {
                "address": "me@example.com", "password": "app-password",
                "imap_host": imap.host(), "imap_port": imap.port().to_string(), "imap_security": sec(imap.tls_mode()),
                "smtp_host": smtp.host(), "smtp_port": smtp.port().to_string(), "smtp_security": sec(smtp.tls_mode()),
            }})),
        )
        .await;
    assert_eq!(s, 200, "{c}");
    let conn = c["id"].as_str().expect("connection").to_string();
    let (s, v) = e
        .call(
            "PATCH",
            &format!("/api/mail/accounts/{conn}"),
            Some(json!({"send_delay_s": 0})),
        )
        .await;
    assert_eq!(s, 200, "{v}");
    let (s, d) = e
        .call(
            "POST",
            "/api/mail/drafts",
            Some(json!({"connection_id": conn, "mode": "new", "to": [{"addr": "alice@acme.example", "name": "Alice"}],
                        "subject": "周五评审", "body": "改到两点可以吗？"})),
        )
        .await;
    assert_eq!(s, 200, "{d}");
    let id = d["id"].as_str().expect("draft").to_string();
    let (s, d) = e
        .call("POST", &format!("/api/mail/drafts/{id}/send"), None)
        .await;
    assert_eq!(s, 200, "{d}");
    assert!(d["decision_id"].as_str().is_some(), "{d}");
    assert!(
        until(15, || async { !smtp.sent().is_empty() }).await,
        "nothing reached the fake SMTP server"
    );
    let sent = smtp.sent();
    assert_eq!(sent.len(), 1);
    // the triage template was installed for the new mailbox
    assert!(
        until(5, || async {
            one::<String>(
                &e.db,
                "SELECT id FROM flows WHERE origin = 'template:mail-triage'",
                &[],
            )
            .is_some()
        })
        .await
    );
}

/// V4: `steve_secret_use` from a session opens a `secret_use` decision; answering it over the API
/// runs the command with the value injected, and the use is recorded.
#[tokio::test(flavor = "multi_thread")]
async fn a_secret_use_runs_after_its_decision() {
    let e = start().await;
    assert_eq!(e.call("POST", "/api/vault/init", None).await.0, 200);
    let (_, vaults) = e.call("GET", "/api/vaults", None).await;
    let shared = vaults
        .as_array()
        .into_iter()
        .flatten()
        .find(|v| v["scope"] == "shared")
        .and_then(|v| v["id"].as_str())
        .expect("shared vault")
        .to_string();
    let (s, item) = e
        .call(
            "POST",
            "/api/vault/items",
            Some(json!({"vault_id": shared, "template": "api_credential", "title": "测试令牌",
                        "fields": [{"label": "令牌", "type": "password", "secret": true, "value": "tok_wired_0123456789"}]})),
        )
        .await;
    assert_eq!(s, 200, "{item}");
    let (_, it) = e
        .call(
            "POST",
            "/api/items",
            Some(json!({"goal": "x", "title": "用令牌", "bot": "steve", "project": null})),
        )
        .await;
    let work = it["id"].as_str().expect("item").to_string();
    let mut sid = String::new();
    for _ in 0..100 {
        let (_, i) = e.call("GET", &format!("/api/items/{work}"), None).await;
        if let Some(s) = i["sessionIds"][0].as_str() {
            sid = s.to_string();
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(!sid.is_empty(), "no session");
    let (_, me) = e.call("GET", "/api/me", None).await;
    let cx = steve_modules::vault::UseCx {
        ws: "default".into(),
        user: me["user"]["id"].as_str().expect("me").into(),
        bot_id: "steve".into(),
        item_id: Some(work.clone()),
        session_id: Some(sid.clone()),
    };
    let out = steve_modules::vault::mcp::call(
        &e.tools.uses,
        &cx,
        "steve_secret_use",
        &json!({"item": "测试令牌", "purpose": "测试", "command": "test \"$T\" = tok_wired_0123456789", "env": {"T": "令牌"}}),
    )
    .await
    .expect("tool");
    assert!(out.awaiting_approval, "{}", out.text);
    let dec: String = one(
        &e.db,
        "SELECT id FROM decisions WHERE subject_type = 'secret_use' AND status = 'open'",
        &[],
    )
    .expect("secret_use decision");
    let (s, v) = e
        .call(
            "POST",
            &format!("/api/decisions/{dec}"),
            Some(json!({"choice": "once"})),
        )
        .await;
    assert_eq!(s, 200, "{v}");
    assert!(
        until(15, || async {
            one::<String>(
                &e.db,
                "SELECT status FROM secret_uses WHERE status IN ('ok', 'failed', 'error') LIMIT 1",
                &[],
            )
            .is_some()
        })
        .await
    );
    let (status, code): (Option<String>, Option<i64>) = (
        one(
            &e.db,
            "SELECT status FROM secret_uses ORDER BY created DESC LIMIT 1",
            &[],
        ),
        one(
            &e.db,
            "SELECT exit_code FROM secret_uses ORDER BY created DESC LIMIT 1",
            &[],
        ),
    );
    assert_eq!((status.as_deref(), code), (Some("ok"), Some(0)));
}

/// The paste guard: a pasted password or token is replaced before the message is stored, the rest of
/// the message goes on, and Steve's note says where to keep the secret; `/api/config/draft` refuses it.
#[tokio::test(flavor = "multi_thread")]
async fn the_paste_guard_keeps_secrets_out_of_the_thread() {
    let e = start().await;
    let token = "ghp_abcdefghijklmnopqrstuvwxyz0123456789";
    let (s, v) = e
        .call(
            "POST",
            "/api/thread",
            Some(json!({"text": format!("这是我的令牌 {token} 帮我记一下")})),
        )
        .await;
    assert_eq!(s, 200, "{v}");
    let (_, st) = e.call("GET", "/api/state", None).await;
    let thread = st["thread"].to_string();
    assert!(!thread.contains(token), "{thread}");
    assert!(thread.contains(steve_vault::guard::REPLACEMENT), "{thread}");
    assert!(thread.contains(steve_vault::guard::ROUTED), "{thread}");
    // the message itself went on: stored as the person's, with the rest of the text
    assert!(thread.contains("帮我记一下"), "{thread}");
    let (s, v) = e
        .call(
            "POST",
            "/api/config/draft",
            Some(json!({"target": {"kind": "global"}, "text": format!("把令牌改成 {token}")})),
        )
        .await;
    assert_eq!(
        (s, v["error"].as_str()),
        (400, Some(steve_vault::guard::REPLY))
    );
}

/// C1–C6 on the fake Feishu: a bot account connected over `/api/channels`, a DM over its long
/// connection, the default rule replies, the disclosure check passes, and the fake receives exactly
/// one message. Your own account only ever drafts. Disconnecting the bot keeps its spaces.
#[tokio::test(flavor = "multi_thread")]
async fn a_channel_round_trip_on_the_fake_feishu() {
    use steve_channels::fake::feishu::{FakeFeishu, WsClientConfig, fixture, ids};
    let fake = FakeFeishu::start().await.expect("fake feishu");
    fake.ws_set_client_config(WsClientConfig {
        reconnect_count: -1,
        reconnect_interval: 1,
        reconnect_nonce: 0,
        ping_interval: 1,
    });
    let model = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fake-claude-chan.mjs");
    let e = start_with(&[
        ("CLAUDE_CODE_EXECUTABLE", model.to_string()),
        ("STEVE_FEISHU_API_BASE", fake.base_url().to_string()),
    ])
    .await;
    assert_eq!(e.call("POST", "/api/vault/init", None).await.0, 200);

    // the bot: connected, its long connection up through the runtime (no restart)
    let (s, a) = e
        .call(
            "POST",
            "/api/channels",
            Some(json!({"adapter": "feishu_app", "fields": {"App ID": ids::APP_ID, "App Secret": ids::APP_SECRET},
                        "config": {"api_base": fake.base_url()}})),
        )
        .await;
    assert_eq!(s, 200, "{a}");
    let bot = a["connection_id"].as_str().expect("connection").to_string();
    assert_eq!(
        a["rules"],
        json!([{"when": {"kind": "mention_or_dm"}, "act": "reply"}])
    );
    assert!(
        fake.ws_wait_connections(1, Duration::from_secs(10)).await,
        "long connection up"
    );
    let before = fake.sent().len();
    fake.ws_push_envelope(fixture("message_p2p_text").expect("fixture"));
    if !until(20, || async { fake.sent().len() > before }).await {
        let c = rusqlite::Connection::open(&e.db).expect("db");
        let dump = |sql: &str| -> Vec<String> {
            let mut st = c.prepare(sql).expect("sql");
            let n = st.column_count();
            st.query_map([], |r| {
                Ok((0..n)
                    .map(|i| {
                        format!(
                            "{:?}",
                            r.get_ref(i).map(|v| v
                                .as_str()
                                .map(String::from)
                                .unwrap_or_else(|_| format!("{v:?}")))
                        )
                    })
                    .collect::<Vec<_>>()
                    .join(" | "))
            })
            .expect("q")
            .collect::<Result<_, _>>()
            .expect("rows")
        };
        panic!(
            "no reply reached the fake; messages {:?}; decisions {:?}; attempts {}",
            dump("SELECT direction, status, text FROM chan_messages"),
            dump("SELECT subject_type, status, question, draft FROM decisions"),
            fake.send_attempts().len()
        );
    }
    tokio::time::sleep(Duration::from_millis(500)).await;
    let sent = fake.sent();
    assert_eq!(sent.len(), before + 1, "exactly one message");
    assert_eq!(
        sent.last().and_then(|m| m.text()).as_deref(),
        Some("按计划进行，我稍后同步进展。")
    );
    let out: Option<String> = one(
        &e.db,
        "SELECT id FROM chan_messages WHERE direction = 'out' AND status = 'sent'",
        &[],
    );
    let out = out.expect("the sent message is stored");
    let (s, b) = e
        .call("GET", &format!("/api/chan-messages/{out}/basis"), None)
        .await;
    assert_eq!(s, 200, "{b}");
    // “收到” went on first and was swapped for “完成” once the reply was out
    let m = "om_p2ptext00000000000000000000001";
    assert!(
        until(10, || async { fake.reactions_on(m) == ["DONE"] }).await,
        "{:?}",
        fake.reactions()
    );
    let r = fake.reactions();
    assert_eq!(r[0].emoji_type, "OnIt");
    assert!(
        r[0].deleted && r[0].as_user.is_none(),
        "as the bot, taken off"
    );
    // The DM is a conversation of its own, with the turn and Steve's answer
    let (s, l) = e.call("GET", "/api/conversations", None).await;
    assert_eq!(s, 200, "{l}");
    let c = &l["conversations"][0];
    assert_eq!(c["dm"], json!(true), "{l}");
    let space = c["space"].as_str().expect("space").to_string();
    let (s, v) = e
        .call("GET", &format!("/api/conversations/{space}/messages"), None)
        .await;
    assert_eq!(s, 200, "{v}");
    let ms = v["messages"].as_array().expect("messages");
    assert_eq!(ms.len(), 2, "{v}");
    assert_eq!(ms[0]["author"], "me");
    assert_eq!(ms[0]["reaction"]["state"], "done");
    assert_eq!(ms[1]["text"], "按计划进行，我稍后同步进展。");
    assert_eq!(ms[1]["meta"]["chan_out"]["status"], "sent");
    // and not in your web conversation
    let (_, st) = e.call("GET", "/api/state", None).await;
    assert!(!st["thread"].to_string().contains("按计划进行"));

    // you: authorized through the fake's consent page; a rule may not even say reply (400), it
    // drafts, and the draft waits for you
    let (s, v) = e
        .call(
            "POST",
            "/api/channels",
            Some(json!({"adapter": "feishu_user"})),
        )
        .await;
    assert_eq!(s, 200, "{v}");
    let url = v["url"].as_str().expect("url");
    assert!(url.starts_with(fake.base_url()), "{url}");
    let r = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .expect("client")
        .get(url)
        .send()
        .await
        .expect("consent");
    let back = reqwest::Url::parse(r.headers()["location"].to_str().expect("loc")).expect("url");
    let r = e
        .http
        .get(format!(
            "{}{}?{}",
            e.base,
            back.path(),
            back.query().unwrap_or_default()
        ))
        .header("cookie", &e.cookie)
        .send()
        .await
        .expect("callback");
    assert_eq!(r.status().as_u16(), 302);
    let to = r.headers()["location"]
        .to_str()
        .unwrap_or_default()
        .to_string();
    assert!(
        to.starts_with("/#/accounts/") && !to.contains("error"),
        "{to}"
    );
    let me: String = one(
        &e.db,
        "SELECT id FROM connections WHERE adapter = 'feishu_user'",
        &[],
    )
    .expect("your connection");
    let (s, v) = e
        .call(
            "PATCH",
            &format!("/api/channels/{me}"),
            Some(json!({"rules": [{"when": {"kind": "all"}, "act": "reply"}]})),
        )
        .await;
    assert_eq!(
        (s, v["error"].as_str()),
        (400, Some("以你本人接入时不能直接回复，只能起草"))
    );
    let (s, v) = e
        .call(
            "PATCH",
            &format!("/api/channels/{me}"),
            Some(json!({"rules": [{"when": {"kind": "all"}, "act": "draft"}]})),
        )
        .await;
    assert_eq!(s, 200, "{v}");
    let sent_before = fake.sent().len();
    use steve_channels::MessageSink;
    let mut msg = steve_channels::InboundMessage::text(
        "oc_selfgroup0000000000000000001",
        "om_self_0001",
        steve_channels::HandleRef::new("feishu", "on_bob0000000000000000000000000001"),
        "周五的发布要不要推迟？",
        steve_store::now(),
    );
    // it @s you: only addressed messages are acted on
    msg.mentions.push(steve_channels::Mention {
        handle: None,
        name: "你".into(),
        is_connection: true,
    });
    let got = e
        .seams
        .channels
        .inbound("default", &me, msg)
        .await
        .expect("inbound");
    assert!(!got.duplicate);
    assert!(
        until(20, || async {
            one::<String>(
                &e.db,
                "SELECT id FROM decisions WHERE subject_type = 'chan_message' AND status = 'open'",
                &[],
            )
            .is_some()
        })
        .await,
        "your account's reply waits for you"
    );
    assert_eq!(fake.sent().len(), sent_before, "nothing sent as you");

    // disconnecting the bot keeps its spaces (left, without a connection)
    let spaces: i64 = one(
        &e.db,
        "SELECT count(*) FROM spaces WHERE connection_id = ?",
        &[&bot],
    )
    .unwrap_or(0);
    assert!(spaces >= 1);
    let (s, v) = e
        .call("DELETE", &format!("/api/connections/{bot}"), None)
        .await;
    assert_eq!(s, 200, "{v}");
    let left: i64 = one(
        &e.db,
        "SELECT count(*) FROM spaces WHERE connection_id IS NULL AND status IN ('left', 'archived')",
        &[],
    )
    .unwrap_or(0);
    assert_eq!(left, spaces);
    let (s, list) = e.call("GET", "/api/spaces", None).await;
    assert_eq!(s, 200, "{list}");
    assert!(
        list.as_array().is_some_and(|l| l.len() as i64 >= spaces),
        "{list}"
    );
}

/// K5, C8, E6, G5: their draft targets are reachable through `/api/config/draft` (the kind passes
/// the Hub's validation and reaches the module, which may ask for an id; the model call fails here).
#[tokio::test(flavor = "multi_thread")]
async fn module_draft_kinds_are_reachable() {
    let e = start().await;
    for kind in [
        "kb_source",
        "knowledge",
        "space",
        "persona",
        "channel",
        "mail_account",
        "gh_repo",
    ] {
        let (s, v) = e
            .call(
                "POST",
                "/api/config/draft",
                Some(json!({"target": {"kind": kind}, "text": "把它改一下"})),
            )
            .await;
        assert!(
            !v["error"]
                .as_str()
                .unwrap_or_default()
                .starts_with("参数不对：target.kind"),
            "{kind}: {s} {v}"
        );
    }
    let (s, v) = e
        .call(
            "POST",
            "/api/config/draft",
            Some(json!({"target": {"kind": "nope"}, "text": "x"})),
        )
        .await;
    assert_eq!(s, 400, "{v}");
    assert!(
        v["error"]
            .as_str()
            .unwrap_or_default()
            .starts_with("参数不对：target.kind"),
        "{v}"
    );
}
