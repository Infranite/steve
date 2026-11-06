//! /api/updates and the page's update notice over HTTP: owner only, paths and runner details hidden from
//! everyone else, the notice in /api/state. Temp STEVE_HOME; no GitHub call (the check is only read from cache).

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;

use reqwest::Method;
use serde_json::{Value, json};
use steve_hub::api::AppState;
use steve_hub::bus::Bus;
use steve_hub::config::Config;

struct Env {
    _home: tempfile::TempDir,
    cfg: Arc<Config>,
    base: String,
    http: reqwest::Client,
    cookie: String,
}

impl Env {
    fn req(&self, method: reqwest::Method, path: &str) -> reqwest::RequestBuilder {
        self.http
            .request(method, format!("{}{path}", self.base))
            .header("cookie", &self.cookie)
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
        cfg,
        base,
        http,
        cookie,
    }
}

fn seed_check(cfg: &Config, status: &str) {
    let dir = cfg.home.join("updates");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("check.json"),
        serde_json::to_vec(&json!({
            "repository": "Infranite/steve",
            "current_hub": cfg.version,
            "current_web": steve_hub::web::web_version(cfg),
            "result": {"status": status, "checked_at": 4_102_444_800_000u64, "version": "99.0.0", "release_url": null, "notes": null, "error": null}
        }))
        .unwrap(),
    )
    .unwrap();
}

async fn json_of(r: reqwest::RequestBuilder) -> (u16, Value) {
    let r = r.send().await.expect("send");
    (r.status().as_u16(), r.json().await.unwrap_or(Value::Null))
}

#[tokio::test(flavor = "multi_thread")]
async fn updates_are_for_the_instance_owner_and_the_notice_is_in_the_state() {
    let e = start().await;
    seed_check(&e.cfg, "available");
    let (s, v) = json_of(e.req(Method::GET, "/api/updates")).await;
    assert_eq!(s, 200, "{v}");
    assert_eq!(v["capability"]["owner"], true);
    assert_eq!(v["check"]["status"], "available");
    assert!(
        v["install"]["home"].is_string(),
        "the owner gets this machine's paths"
    );
    let (_, state) = json_of(e.req(Method::GET, "/api/state")).await;
    assert_eq!(state["update"], json!({"version": "99.0.0", "owner": true}));
    // tags only: anything but a stable version is refused before any work
    for version in ["main", "../../etc", "99.0.0-rc.1"] {
        let (s, _) = json_of(
            e.req(Method::POST, "/api/updates")
                .json(&json!({"version": version, "idempotency_key": format!("k-{version}")})),
        )
        .await;
        assert_eq!(s, 400, "{version}");
    }

    // the same person as a workspace member: no check, no upgrade, no paths
    let db = rusqlite::Connection::open(e.cfg.db_path()).unwrap();
    db.execute("UPDATE users SET role = 'member'", []).unwrap();
    drop(db);
    let (s, _) = json_of(e.req(Method::POST, "/api/updates/check").json(&json!({}))).await;
    assert_eq!(s, 403, "a check spends the instance's GitHub allowance");
    let (s, _) = json_of(
        e.req(Method::POST, "/api/updates")
            .json(&json!({"version": "99.0.0", "idempotency_key": "member"})),
    )
    .await;
    assert_eq!(s, 403);
    let (_, v) = json_of(e.req(Method::GET, "/api/updates")).await;
    assert_eq!(v["capability"]["owner"], false);
    assert_eq!(v["capability"]["can_execute"], false);
    assert!(v["install"]["home"].is_null());
    seed_check(&e.cfg, "up_to_date");
    tokio::time::sleep(std::time::Duration::from_millis(2100)).await;
    let (_, state) = json_of(e.req(Method::GET, "/api/state")).await;
    assert!(state.get("update").is_none(), "{}", state["update"]);
}
