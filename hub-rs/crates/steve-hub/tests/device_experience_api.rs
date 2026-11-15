//! Device facts and configuration against the real isolated HTTP service.
use std::collections::HashMap;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

use serde_json::{Value, json};
use steve_hub::api::AppState;
use steve_hub::bus::Bus;
use steve_hub::config::Config;
use steve_store::Store;

struct Env {
    _home: tempfile::TempDir,
    db: PathBuf,
    base: String,
    http: reqwest::Client,
    owner_cookie: String,
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

async fn start(local: bool) -> Env {
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
    vars.insert(
        "STEVE_NODE_MODE".into(),
        if local { "local" } else { "off" }.into(),
    );
    if local {
        vars.remove("STEVE_NO_RESUME");
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
        .with_node(w.node.clone())
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
    assert!(store.access().owner().unwrap().is_some());
    Env {
        _home: home,
        db: cfg.db_path(),
        base,
        http,
        owner_cookie,
    }
}

async fn patch(e: &Env, cookie: &str, body: Value) -> (u16, Value) {
    let r = e
        .http
        .patch(format!("{}/api/devices/hub", e.base))
        .header("cookie", cookie)
        .json(&body)
        .send()
        .await
        .unwrap();
    (r.status().as_u16(), r.json().await.unwrap())
}

#[tokio::test(flavor = "multi_thread")]
async fn device_config_requires_owner_revision_and_returns_committed_receipt() {
    let e = start(true).await;
    for _ in 0..100 {
        let (_, r) = e.get("/api/devices", Some(&e.owner_cookie), None).await;
        if r["devices"][0]["connected"] == true {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    let (status, list) = e.get("/api/devices", Some(&e.owner_cookie), None).await;
    assert_eq!(status, 200);
    let d = &list["devices"][0];
    assert_eq!(d["id"], "hub");
    assert_eq!(d["node_id"], "local");
    assert_eq!(d["can_configure"], true);
    assert_eq!(d["eligibility"]["state"], "unknown");
    assert!(d["occupancy"].is_null());
    let revision = d["revision"].as_i64().unwrap();
    assert_eq!(
        patch(&e, &e.owner_cookie, json!({"status":"暂停接活"}))
            .await
            .0,
        400
    );
    let (status, receipt) = patch(
        &e,
        &e.owner_cookie,
        json!({"revision":revision,"status":"暂停接活","max_sessions":2}),
    )
    .await;
    assert_eq!(status, 200, "{receipt}");
    assert_eq!(receipt["device"]["revision"], revision + 1);
    assert_eq!(receipt["device"]["status"], "暂停接活");
    assert_eq!(
        patch(
            &e,
            &e.owner_cookie,
            json!({"revision":revision,"max_sessions":3})
        )
        .await
        .0,
        409
    );
    assert_eq!(
        patch(
            &e,
            &e.owner_cookie,
            json!({"revision":revision+1,"max_sessions":0})
        )
        .await
        .0,
        400
    );
    assert_eq!(e.store().devices().list().unwrap()[0].max_sessions, 2);
    let store = e.store();
    let member = store
        .users()
        .create("member-sub", "Member", None, "member")
        .unwrap();
    store
        .auth_sessions()
        .create(&member, "device-member-token", 86400000)
        .unwrap();
    let cookie = "steve_session=device-member-token";
    assert_eq!(
        patch(&e, &cookie, json!({"revision":revision+1,"max_sessions":3}))
            .await
            .0,
        403
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn device_role_or_name_never_establishes_local_binding_when_node_is_off() {
    let e = start(false).await;
    let (_, list) = e.get("/api/devices", Some(&e.owner_cookie), None).await;
    let d = &list["devices"][0];
    assert!(d["node_id"].is_null());
    assert_eq!(d["can_configure"], false);
    assert_eq!(
        patch(&e, &e.owner_cookie, json!({"revision":0,"max_sessions":3}))
            .await
            .0,
        409
    );
}
