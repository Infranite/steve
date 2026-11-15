//! `GET /api/sessions/{id}/principal-state`: instance-owner inspection of a session's recorded
//! Node binding and `STEVE_PRINCIPAL_STATE` directory without touching the filesystem or leaking
//! sessions across workspaces.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

use rusqlite::params;
use serde_json::{Value, json};
use steve_hub::api::AppState;
use steve_hub::bus::Bus;
use steve_hub::config::Config;
use steve_store::{NewSession, NewWorkspace, Store, WS};

struct Env {
    _home: tempfile::TempDir,
    db: PathBuf,
    data_dir: PathBuf,
    base: String,
    http: reqwest::Client,
    owner_cookie: String,
    owner_id: String,
}

impl Env {
    fn store(&self) -> Store {
        Store::open(&self.db, steve_store::FnPublisher(|_| {})).expect("store")
    }

    async fn get(&self, path: &str, cookie: Option<&str>, ws: Option<&str>) -> (u16, Value) {
        let mut req = self.http.get(format!("{}{path}", self.base));
        if let Some(c) = cookie {
            req = req.header("cookie", c);
        }
        if let Some(w) = ws {
            req = req.header("x-steve-ws", w);
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
    let st = AppState::new(cfg.clone(), w.backend, w.domain, bus)
        .with_modules(w.modules)
        .with_oidc(w.oidc.clone());
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
    let owner_cookie = r
        .headers()
        .get_all("set-cookie")
        .iter()
        .filter_map(|v| v.to_str().ok())
        .find(|c| c.starts_with("steve_session="))
        .map(|c| c.split(';').next().unwrap_or_default().to_string())
        .expect("owner session cookie");
    let store = Store::open(cfg.db_path(), steve_store::FnPublisher(|_| {})).expect("store");
    let owner = store.access().owner().expect("owner query").expect("owner");
    Env {
        _home: home,
        db: cfg.db_path(),
        data_dir: cfg.data_dir.clone(),
        base,
        http,
        owner_cookie,
        owner_id: owner.id,
    }
}

fn new_session(store: &Store, ws: &str, harness: &str) -> String {
    store
        .sessions()
        .create(&NewSession {
            workspace_id: ws.into(),
            item_id: None,
            bot_id: None,
            harness: harness.into(),
            model: None,
            reasoning_effort: None,
            cwd: "/tmp".into(),
            title: "principal-state test".into(),
            token: format!("tok_{}", steve_store::uid("t")),
        })
        .expect("create session")
        .id
}

fn bind_runtime_session(
    store: &Store,
    session_id: &str,
    node_id: &str,
    principal: Option<&str>,
    state_dir: Option<&str>,
) {
    store
        .with_conn(|c| {
            c.execute(
                "INSERT INTO runtime_sessions
                   (session_id, node_id, workspace_ref, node_session_id, config_revision, phase, updated, principal, state_dir)
                 VALUES (?1, ?2, '{}', ?1, 'r1', 'ready', 1, ?3, ?4)",
                params![session_id, node_id, principal, state_dir],
            )?;
            Ok(())
        })
        .expect("bind runtime session");
}

#[tokio::test(flavor = "multi_thread")]
async fn principal_state_reports_recorded_paths_and_enforces_owner_and_workspace_boundaries() {
    let e = start().await;
    let store = e.store();

    // 1. Local-Node session with recorded state_dir -> exact state_dir, harness_rel_dir, and local_harness_dir.
    let s_local = new_session(&store, WS, "codex");
    let recorded_local = "default-a1b2c3d4/usr_owner-11223344/none-55667788/codex-99aabbcc";
    bind_runtime_session(
        &store,
        &s_local,
        "local",
        Some(&e.owner_id),
        Some(recorded_local),
    );

    let (status, v) = e
        .get(
            &format!("/api/sessions/{s_local}/principal-state"),
            Some(&e.owner_cookie),
            Some(WS),
        )
        .await;
    assert_eq!(status, 200, "{v}");
    let expected_local_abs = e
        .data_dir
        .join("node")
        .join("state")
        .join(recorded_local)
        .join("harness")
        .to_string_lossy()
        .into_owned();
    assert_eq!(
        v,
        json!({
            "bound": true,
            "session_id": s_local,
            "harness": "codex",
            "node_id": "local",
            "principal": e.owner_id,
            "independent": true,
            "state_dir": recorded_local,
            "harness_rel_dir": format!("state/{recorded_local}/harness"),
            "local_harness_dir": expected_local_abs,
        })
    );

    // 2. Remote-Node session with recorded state_dir -> local_harness_dir is null, harness_rel_dir is valid.
    let s_remote = new_session(&store, WS, "claude-code");
    let recorded_remote =
        "default-a1b2c3d4/svc_default-deadbeef/bot_1-cafebabe/claude-code-01020304";
    bind_runtime_session(
        &store,
        &s_remote,
        "node_remote_alpha",
        Some("svc:default"),
        Some(recorded_remote),
    );

    let (status, v) = e
        .get(
            &format!("/api/sessions/{s_remote}/principal-state"),
            Some(&e.owner_cookie),
            Some(WS),
        )
        .await;
    assert_eq!(status, 200, "{v}");
    assert_eq!(
        v,
        json!({
            "bound": true,
            "session_id": s_remote,
            "harness": "claude-code",
            "node_id": "node_remote_alpha",
            "principal": "svc:default",
            "independent": true,
            "state_dir": recorded_remote,
            "harness_rel_dir": format!("state/{recorded_remote}/harness"),
            "local_harness_dir": Value::Null,
        })
    );

    // 3a. Bound session with NULL state_dir -> independent: false, state_dir: null (never guessed).
    let s_null = new_session(&store, WS, "codex");
    bind_runtime_session(&store, &s_null, "local", Some(&e.owner_id), None);
    let (status, v) = e
        .get(
            &format!("/api/sessions/{s_null}/principal-state"),
            Some(&e.owner_cookie),
            Some(WS),
        )
        .await;
    assert_eq!(status, 200, "{v}");
    assert_eq!(
        v,
        json!({
            "bound": true,
            "session_id": s_null,
            "harness": "codex",
            "node_id": "local",
            "principal": e.owner_id,
            "independent": false,
            "state_dir": Value::Null,
            "harness_rel_dir": Value::Null,
            "local_harness_dir": Value::Null,
        })
    );

    // 3b. Bound session with empty/whitespace state_dir -> independent: false, state_dir: null.
    let s_empty = new_session(&store, WS, "codex");
    bind_runtime_session(&store, &s_empty, "local", Some(&e.owner_id), Some("   "));
    let (status, v) = e
        .get(
            &format!("/api/sessions/{s_empty}/principal-state"),
            Some(&e.owner_cookie),
            Some(WS),
        )
        .await;
    assert_eq!(status, 200, "{v}");
    assert_eq!(v["bound"], true);
    assert_eq!(v["independent"], false);
    assert_eq!(v["state_dir"], Value::Null);
    assert_eq!(v["harness_rel_dir"], Value::Null);
    assert_eq!(v["local_harness_dir"], Value::Null);

    // 3c. Unbound session (no runtime_sessions row) -> bound: false, independent: false, state_dir: null.
    let s_unbound = new_session(&store, WS, "custom-acp");
    let (status, v) = e
        .get(
            &format!("/api/sessions/{s_unbound}/principal-state"),
            Some(&e.owner_cookie),
            Some(WS),
        )
        .await;
    assert_eq!(status, 200, "{v}");
    assert_eq!(
        v,
        json!({
            "bound": false,
            "session_id": s_unbound,
            "harness": "custom-acp",
            "node_id": Value::Null,
            "principal": Value::Null,
            "independent": false,
            "state_dir": Value::Null,
            "harness_rel_dir": Value::Null,
            "local_harness_dir": Value::Null,
        })
    );

    // 4. Non-owner workspace member -> 403 Forbidden on workspace sessions, 404 on unknown sessions.
    let member_id = store
        .users()
        .create("sub-member-1", "Member One", None, "member")
        .expect("create member");
    let member_token = "member_session_secret_token";
    store
        .auth_sessions()
        .create(&member_id, member_token, 3_600_000)
        .expect("member auth session");
    let member_cookie = format!("steve_session={member_token}");

    let (status, v) = e
        .get(
            &format!("/api/sessions/{s_local}/principal-state"),
            Some(&member_cookie),
            Some(WS),
        )
        .await;
    assert_eq!(status, 403, "{v}");
    assert_eq!(
        v["error"],
        "Only the instance owner can inspect principal state directories."
    );

    let (status, _) = e
        .get(
            "/api/sessions/ses_missing_does_not_exist/principal-state",
            Some(&member_cookie),
            Some(WS),
        )
        .await;
    assert_eq!(
        status, 404,
        "unknown session must 404 before 403 so existence is not leaked"
    );

    // 5. Session belonging to a different workspace -> 404 Not Found when queried from another workspace.
    let ws_other = store
        .workspaces()
        .create(
            &NewWorkspace {
                name: "Other".into(),
                description: None,
                item_prefix: Some("OT".into()),
            },
            &e.owner_id,
        )
        .expect("create other workspace");
    let s_other = new_session(&store, &ws_other.id, "codex");
    bind_runtime_session(
        &store,
        &s_other,
        "local",
        Some(&e.owner_id),
        Some(recorded_local),
    );

    // Owner asking in workspace `default` gets 404 for `s_other`.
    let (status, v) = e
        .get(
            &format!("/api/sessions/{s_other}/principal-state"),
            Some(&e.owner_cookie),
            Some(WS),
        )
        .await;
    assert_eq!(status, 404, "{v}");

    // Member of `default` (not in `ws_other`) also gets 404 when querying in `default`.
    let (status, v) = e
        .get(
            &format!("/api/sessions/{s_other}/principal-state"),
            Some(&member_cookie),
            Some(WS),
        )
        .await;
    assert_eq!(status, 404, "{v}");

    // Owner switching `x-steve-ws` to `ws_other` gets 200.
    let (status, v) = e
        .get(
            &format!("/api/sessions/{s_other}/principal-state"),
            Some(&e.owner_cookie),
            Some(&ws_other.id),
        )
        .await;
    assert_eq!(status, 200, "{v}");
    assert_eq!(v["session_id"], s_other);
    assert_eq!(v["independent"], true);

    // Unauthenticated request -> 401.
    let (status, _) = e
        .get(
            &format!("/api/sessions/{s_local}/principal-state"),
            None,
            None,
        )
        .await;
    assert_eq!(status, 401);

    // 6. `/api/sessions/:id/resume`: owner sees isolated principal state env; non-owner member and
    // remote-Node sessions never leak internal `state/.../harness` paths or check Hub-local transcripts.
    store
        .with_conn(|c| {
            c.execute(
                "UPDATE sessions SET acp_session_id = 'acp-123' WHERE id IN (?1, ?2)",
                params![s_local, s_remote],
            )?;
            Ok(())
        })
        .expect("set acp_session_id");

    let (status, r_owner) = e
        .get(
            &format!("/api/sessions/{s_local}/resume"),
            Some(&e.owner_cookie),
            Some(WS),
        )
        .await;
    assert_eq!(status, 200, "{r_owner}");
    let owner_cmd = r_owner["command"].as_str().unwrap_or_default();
    assert!(
        owner_cmd.contains(&expected_local_abs),
        "owner resume command should include isolated local harness dir: {owner_cmd}"
    );
    assert_eq!(r_owner["transcript_checked"], true);

    let (status, r_member) = e
        .get(
            &format!("/api/sessions/{s_local}/resume"),
            Some(&member_cookie),
            Some(WS),
        )
        .await;
    assert_eq!(status, 200, "{r_member}");
    let member_cmd = r_member["command"].as_str().unwrap_or_default();
    assert!(
        !member_cmd.contains(recorded_local) && !member_cmd.contains("node/state"),
        "non-owner resume command must not leak principal state path: {member_cmd}"
    );
    assert_eq!(r_member["transcript"], Value::Null);
    assert_eq!(r_member["transcript_checked"], false);

    let (status, r_remote) = e
        .get(
            &format!("/api/sessions/{s_remote}/resume"),
            Some(&e.owner_cookie),
            Some(WS),
        )
        .await;
    assert_eq!(status, 200, "{r_remote}");
    let remote_cmd = r_remote["command"].as_str().unwrap_or_default();
    assert!(
        !remote_cmd.contains(recorded_remote) && !remote_cmd.contains("node/state"),
        "remote-Node resume command must not point at Hub-local state dir: {remote_cmd}"
    );
    assert_eq!(r_remote["transcript"], Value::Null);
    assert_eq!(r_remote["transcript_checked"], false);
}

#[tokio::test(flavor = "multi_thread")]
async fn principal_state_returns_503_when_modules_store_is_absent() {
    steve_modules::net::forbid_in_this_process();
    let home = tempfile::tempdir().expect("tmp");
    let h = home.path().to_path_buf();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let port = listener.local_addr().expect("addr").port();
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
    // Deliberately omit `.with_modules(...)`.
    let st = AppState::new(cfg.clone(), w.backend, w.domain, bus);
    steve_hub::app::boot(&st).await.expect("boot");
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
    let owner_cookie = r
        .headers()
        .get_all("set-cookie")
        .iter()
        .filter_map(|v| v.to_str().ok())
        .find(|c| c.starts_with("steve_session="))
        .map(|c| c.split(';').next().unwrap_or_default().to_string())
        .expect("owner session cookie");
    let store = Store::open(cfg.db_path(), steve_store::FnPublisher(|_| {})).expect("store");
    let sid = new_session(&store, WS, "codex");

    let res = http
        .get(format!("{base}/api/sessions/{sid}/principal-state"))
        .header("cookie", owner_cookie)
        .header("x-steve-ws", WS)
        .send()
        .await
        .expect("request");
    assert_eq!(res.status().as_u16(), 503);
}
