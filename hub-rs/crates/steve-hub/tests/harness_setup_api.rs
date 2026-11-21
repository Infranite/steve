//! Controlled harness setup API: instance-owner authorization, a fixed registry id and an
//! argument-free body. A disconnected execution host is unknown even if a local file exists.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;

use serde_json::Value;
use steve_hub::api::AppState;
use steve_hub::bus::Bus;
use steve_hub::config::Config;

struct Env {
    _home: tempfile::TempDir,
    base: String,
    http: reqwest::Client,
    cookie: String,
    db: std::path::PathBuf,
    catalogue: Arc<steve_hub::harnesses::HubHarnesses>,
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

async fn start() -> Env {
    start_with_node(None).await
}

async fn start_with_node(node: Option<Arc<dyn steve_node_api::Node>>) -> Env {
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
        ("STEVE_NODE_MODE", "off".into()),
        ("STEVE_NODE_LISTEN", "0".into()),
        (
            "STEVE_ADAPTER_PI",
            h.join("steve/adapters/node_modules/@razorbackdb/pi-acp/dist/index.js")
                .display()
                .to_string(),
        ),
        ("CLAUDE_CODE_EXECUTABLE", "/usr/bin/false".into()),
        ("STEVE_ADAPTER_CLAUDE", "/usr/bin/false".into()),
        ("STEVE_ADAPTER_CODEX", "/usr/bin/false".into()),
        ("STEVE_WEB_DIR", h.join("no-web").display().to_string()),
        ("STEVE_DISK_FLOOR_GB", "7".into()),
        ("STEVE_DISK_LOW_GB", "9".into()),
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
    st.runtimes.set_harnesses(w.harnesses.clone());
    if let Some(node) = node {
        st.runtimes.set_node(node);
    }
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
        db: cfg.db_path(),
        catalogue: w.harnesses.clone(),
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn disconnect_after_ready_stays_unknown_in_state_and_model_catalogue() {
    use serde_json::json;
    use steve_node_api::{ApiError, ErrorCode, HarnessCapability};

    let ready: HarnessCapability = serde_json::from_value(json!({
        "name": "pi", "version": "1.5.2", "sessions": true, "jobs": true,
        "models": ["fixture/pi-test"], "resume": true, "cancel": true,
        "permissions": false, "images": true, "enforcement": [],
        "adapter": {
            "title": "Pi", "builtin": true, "status": "ready", "load_session": true,
            "list_sessions": false, "mcp_http": true, "embedded_context": false,
            "auth": [], "system_prompt": false,
            "session": {
                "modes": ["full_access"], "models": [{"id": "fixture/pi-test"}],
                "default_model": "fixture/pi-test", "efforts": ["off"]
            }
        },
        "setup": {"state": "ready", "components": [], "can_install": true}
    }))
    .unwrap();
    let node = steve_node_api::testkit::FakeNode::new("local");
    node.set_harnesses(vec![ready]);
    let e = start_with_node(Some(node.clone())).await;
    let pi = |v: &Value| {
        v["runtimes"]
            .as_array()
            .unwrap()
            .iter()
            .find(|r| r["id"] == "pi")
            .unwrap()
            .clone()
    };
    for code in [ErrorCode::Offline, ErrorCode::Internal] {
        let (status, devices) = e.call("GET", "/api/devices?fresh=1", None).await;
        assert_eq!(status, 200, "{devices}");
        assert_eq!(pi(&devices)["ok"], true);
        // Fill both the runtime cache and the two-second /api/state response cache.
        let (status, state) = e.call("GET", "/api/state", None).await;
        assert_eq!(status, 200, "{state}");
        assert_eq!(pi(&state)["setup"]["state"], "ready");
        let (_, models) = e.call("GET", "/api/system/models?harness=pi", None).await;
        assert_eq!(models["models"][0]["id"], "fixture/pi-test", "{models}");

        // The Node remains disconnected for both Devices and the following state read.
        for _ in 0..2 {
            node.refuse_next("probe", ApiError::new(code, "probe unavailable"));
        }
        let (status, devices) = e.call("GET", "/api/devices?fresh=1", None).await;
        assert_eq!(status, 200, "{devices}");
        assert_eq!(pi(&devices)["setup"]["state"], "unknown");
        let (status, state) = e.call("GET", "/api/state", None).await;
        assert_eq!(status, 200, "{state}");
        assert_eq!(pi(&state)["ok"], false, "{state}");
        assert_eq!(pi(&state)["setup"]["state"], "unknown");
        assert_eq!(
            pi(&state)["modes"],
            json!(["full_access"]),
            "declared native execution limits remain known while offline"
        );
        assert_eq!(pi(&state)["models"], json!([]));
        let (_, models) = e.call("GET", "/api/system/models?harness=pi", None).await;
        assert_eq!(models["models"], json!([]), "{models}");
    }
    // A successful reconnect restores the same selectors immediately, even within STATE_TTL.
    let (_, devices) = e.call("GET", "/api/devices?fresh=1", None).await;
    assert_eq!(pi(&devices)["setup"]["state"], "ready");
    let (_, state) = e.call("GET", "/api/state", None).await;
    assert_eq!(pi(&state)["ok"], true);
}

#[tokio::test(flavor = "multi_thread")]
async fn installation_api_rejects_nonowners_arbitrary_targets_and_arguments() {
    use serde_json::json;
    let e = start().await;
    let store = steve_store::Store::open(&e.db, steve_store::FnPublisher(|_| {})).unwrap();
    let member = store
        .users()
        .create("fixture-member", "Member", None, "member")
        .unwrap();
    let token = "harness-setup-fixture-token";
    store
        .auth_sessions()
        .create(&member, token, 3_600_000)
        .unwrap();
    let r = e
        .http
        .post(format!("{}/api/system/harnesses/pi/install", e.base))
        .header("cookie", format!("steve_session={token}"))
        .json(&json!({}))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status().as_u16(), 403);
    for id in ["unknown-package", "my-custom-agent"] {
        let (code, _) = e
            .call(
                "POST",
                &format!("/api/system/harnesses/{id}/install"),
                Some(json!({})),
            )
            .await;
        assert_eq!(code, 400);
    }
    for body in [
        json!({"package":"arbitrary"}),
        json!({"path":"/outside"}),
        json!({"command":"anything"}),
        json!({"version":"1"}),
        json!([]),
    ] {
        let (code, _) = e
            .call("POST", "/api/system/harnesses/pi/install", Some(body))
            .await;
        assert_eq!(code, 400);
    }
    let (code, v) = e
        .call("POST", "/api/system/harnesses/pi/install", Some(json!({})))
        .await;
    assert_eq!(code, 503, "no local Node must not accept an install: {v}");
    let (code, v) = e.call("GET", "/api/devices?fresh=1", None).await;
    assert_eq!(code, 200, "{v}");
    let pi = v["runtimes"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["id"] == "pi")
        .unwrap();
    assert_eq!(pi["ok"], false);
    assert_eq!(pi["setup"]["state"], "unknown");
    assert_eq!(pi["setup"]["can_install"], false);
    assert_eq!(pi["setup"]["components"].as_array().unwrap().len(), 2);
}

mod setup_node {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/support/setup_node.rs"
    ));
}

#[tokio::test(flavor = "multi_thread")]
async fn accepted_installation_cannot_restore_old_devices_state_or_model_readiness() {
    use serde_json::json;
    let base = steve_node_api::testkit::FakeNode::new("local");
    base.set_harnesses(vec![
        serde_json::from_value(json!({
            "name":"pi","version":"1","sessions":true,"jobs":true,"models":[],
            "resume":true,"cancel":true,"permissions":false,"images":true,"enforcement":[],
            "adapter":{"title":"Pi","builtin":true,"status":"ready","session":{
                "modes":["full_access"],"models":[{"id":"fixture/test"}]
            }}, "setup":{"state":"ready","components":[],"can_install":true}
        }))
        .unwrap(),
    ]);
    let node = setup_node::Delayed::new(base);
    let e = Arc::new(start_with_node(Some(node.clone())).await);
    let pi = |v: &Value| {
        v["runtimes"]
            .as_array()
            .unwrap()
            .iter()
            .find(|r| r["id"] == "pi")
            .unwrap()
            .clone()
    };
    assert_eq!(
        pi(&e.call("GET", "/api/devices?fresh=1", None).await.1)["ok"],
        true
    );
    // Populate the response cache before installation, within its two-second TTL.
    e.call("GET", "/api/state", None).await;
    node.arm();
    let copy = e.clone();
    let task = tokio::spawn(async move { copy.call("GET", "/api/devices?fresh=1", None).await });
    node.entered.notified().await;
    let (status, result) = e
        .call("POST", "/api/system/harnesses/pi/install", Some(json!({})))
        .await;
    assert_eq!(status, 200, "{result}");
    node.gate.add_permits(1);
    assert_eq!(pi(&task.await.unwrap().1)["setup"]["state"], "installing");
    for path in ["/api/devices", "/api/state", "/api/state"] {
        let (status, body) = e.call("GET", path, None).await;
        assert_eq!(status, 200, "{body}");
        assert_eq!(pi(&body)["setup"]["state"], "installing", "{path}");
        assert_eq!(pi(&body)["ok"], false, "{path}");
    }
    let (_, models) = e.call("GET", "/api/system/models?harness=pi", None).await;
    assert_eq!(models["models"], json!([]), "{models}");
}

#[tokio::test(flavor = "multi_thread")]
async fn persistent_disconnect_revokes_independent_follower_through_http() {
    use serde_json::json;
    use steve_node_api::ErrorCode;
    let base = steve_node_api::testkit::FakeNode::new("local");
    let ready = serde_json::from_value(json!({
        "name":"pi","version":"1","sessions":true,"jobs":true,"models":[],
        "resume":true,"cancel":true,"permissions":false,"images":true,"enforcement":[],
        "adapter":{"title":"Pi","builtin":true,"status":"ready","session":{
            "modes":["full_access"],"models":[{"id":"fixture/test"}]
        }},"setup":{"state":"ready","components":[],"can_install":true}
    }))
    .unwrap();
    base.set_harnesses(vec![ready]);
    let node = setup_node::Delayed::new(base);
    let e = start_with_node(Some(node.clone())).await;
    let pi = |v: &Value| {
        v["runtimes"]
            .as_array()
            .unwrap()
            .iter()
            .find(|r| r["id"] == "pi")
            .unwrap()
            .clone()
    };
    for code in [ErrorCode::Offline, ErrorCode::Internal] {
        assert_eq!(
            pi(&e.call("GET", "/api/devices?fresh=1", None).await.1)["ok"],
            true
        );
        assert_eq!(pi(&e.call("GET", "/api/state", None).await.1)["ok"], true);
        assert_eq!(
            e.call("GET", "/api/system/models?harness=pi", None).await.1["models"][0]["id"],
            "fixture/test"
        );
        node.arm();
        let c = e.catalogue.clone();
        let n = node.clone();
        let old = tokio::spawn(async move { c.refresh(n.as_ref(), false).await });
        node.entered.notified().await;
        // All reads and retries remain Offline, including after the old follower is released.
        node.disconnect(code);
        // A pending refresh has invalidated the sole report: synchronous readers are
        // unknown immediately. A second probe shares that slot until it is released.
        assert_eq!(
            e.call("GET", "/api/system/models?harness=pi", None).await.1["models"],
            json!([])
        );
        node.gate.add_permits(1);
        old.await.unwrap();
        for path in ["/api/devices?fresh=1", "/api/state"] {
            let (status, body) = e.call("GET", path, None).await;
            assert_eq!(status, 200);
            assert_eq!(pi(&body)["ok"], false);
            assert_eq!(pi(&body)["setup"]["state"], "unknown");
            assert_eq!(pi(&body)["models"], json!([]));
        }
        assert_eq!(
            e.call("GET", "/api/system/models?harness=pi", None).await.1["models"],
            json!([])
        );
        assert_eq!(
            e.call("GET", "/api/system/models?harness=pi", None).await.1["models"],
            json!([])
        );
        assert_eq!(pi(&e.call("GET", "/api/state", None).await.1)["ok"], false);
        // Explicitly reconnect, preserving the configured capabilities.
        node.reconnect();
        assert_eq!(
            pi(&e.call("GET", "/api/devices?fresh=1", None).await.1)["ok"],
            true
        );
        assert_eq!(pi(&e.call("GET", "/api/state", None).await.1)["ok"], true);
        assert_eq!(
            e.call("GET", "/api/system/models?harness=pi", None).await.1["models"][0]["id"],
            "fixture/test"
        );
    }
}
