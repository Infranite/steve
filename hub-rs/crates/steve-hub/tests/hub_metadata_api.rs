//! Shared Hub metadata through the authenticated HTTP router, with Nodes disabled.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;

use serde_json::{Value, json};
use steve_hub::api::AppState;
use steve_hub::bus::Bus;
use steve_hub::config::Config;

struct Env {
    _home: tempfile::TempDir,
    db: std::path::PathBuf,
    base: String,
    http: reqwest::Client,
}

impl Env {
    async fn call(
        &self,
        method: &str,
        path: &str,
        auth: Option<&str>,
        body: Option<Value>,
    ) -> (u16, Value) {
        let m = reqwest::Method::from_bytes(method.as_bytes()).expect("method");
        let mut req = self.http.request(m, format!("{}{path}", self.base));
        if let Some(a) = auth {
            req = req.header("authorization", format!("Bearer {a}"));
        }
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
    fn store(&self) -> steve_store::Store {
        steve_store::Store::open(&self.db, steve_store::FnPublisher(|_| {})).expect("store")
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
        ("STEVE_NODE_LISTEN", "off".into()),
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
    let bus = Bus::new(std::sync::Arc::new(steve_runtime::cache::Caches::new(
        Default::default(),
    )));
    let w = steve_hub::wire::wire_with(&cfg, &bus, Arc::new(get.clone())).expect("wire");
    let st = AppState::new(cfg.clone(), w.backend, w.domain, bus)
        .with_modules(w.modules)
        .with_oidc(w.oidc.clone());
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
    // the development login makes the owner
    http.get(format!("{base}/auth/dev"))
        .send()
        .await
        .expect("dev login");
    Env {
        _home: home,
        db: cfg.db_path(),
        base,
        http,
    }
}

#[tokio::test]
async fn shared_info_http_permissions_cas_validation_and_node_off() {
    let e = start().await;
    assert_eq!(e.call("GET", "/api/hub-metadata", None, None).await.0, 401);
    let store = e.store();
    let owner = store.access().owner().unwrap().unwrap();
    let (_, token) = store
        .access()
        .token_create(&owner.id, "metadata-test", None)
        .unwrap();
    let (s, login) = e.call("POST", "/auth/token", None, Some(json!({"user":owner.name,"token":token,"client":{"kind":"web","name":"Metadata test"}}))).await;
    assert_eq!(s, 200, "{login}");
    let cred = login["credential"].as_str().unwrap();
    let (s, first) = e.call("GET", "/api/hub-metadata", Some(cred), None).await;
    assert_eq!(s, 200, "{first}");
    assert_eq!(first["can_edit"], true);
    // G12: where the Hub is reached (here: this machine only), on reads and on saves
    let port = e.base.rsplit(':').next().unwrap().to_string();
    assert_eq!(
        first["addresses"],
        json!([{"url": format!("http://127.0.0.1:{port}"), "page": format!("http://127.0.0.1:{port}"), "kind": "loopback"}])
    );
    let id = first["metadata"]["hub_id"].clone();
    assert_eq!(id, steve_hub::nodes::hub_id(&store).unwrap());
    let change = json!({"revision":0,"display_name":"  Shared 😀  ","purpose_note":"one\r\ntwo"});
    let (s, saved) = e
        .call("PUT", "/api/hub-metadata", Some(cred), Some(change.clone()))
        .await;
    assert_eq!(s, 200, "{saved}");
    assert_eq!(saved["metadata"]["display_name"], "Shared 😀");
    assert_eq!(saved["metadata"]["purpose_note"], "one\ntwo");
    assert_eq!(saved["metadata"]["revision"], 1);
    assert_eq!(saved["metadata"]["hub_id"], id);
    assert_eq!(saved["addresses"], first["addresses"]);
    assert_eq!(
        e.call("PUT", "/api/hub-metadata", Some(cred), Some(change))
            .await
            .0,
        409
    );
    for body in [
        json!({"revision":1,"display_name":" ","purpose_note":""}),
        json!({"revision":1,"display_name":"a","purpose_note":"x".repeat(501)}),
        json!({"revision":1,"display_name":"a","purpose_note":"","hub_id":"fake"}),
    ] {
        let status = e
            .call("PUT", "/api/hub-metadata", Some(cred), Some(body))
            .await
            .0;
        assert!((400..500).contains(&status));
    }
    assert_eq!(
        e.call("GET", "/api/hub-metadata", Some(cred), None).await.1["metadata"],
        saved["metadata"]
    );
    steve_store::testkit::hub::hub_fixtures::set_users_role_member(&store, &owner.id).unwrap();
    assert_eq!(
        e.call("GET", "/api/hub-metadata", Some(cred), None).await.1["can_edit"],
        false
    );
    assert_eq!(
        e.call(
            "PUT",
            "/api/hub-metadata",
            Some(cred),
            Some(json!({"revision":1,"display_name":"Denied","purpose_note":""}))
        )
        .await
        .0,
        403
    );
    assert_eq!(
        e.call("GET", "/api/hub-metadata", Some(cred), None).await.1["metadata"],
        saved["metadata"]
    );
}
