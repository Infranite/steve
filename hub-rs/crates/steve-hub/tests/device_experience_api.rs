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

#[path = "support/local_node.rs"]
mod local_node;

struct Env {
    _home: tempfile::TempDir,
    _local_node: Option<local_node::AttachedLocalNode>,
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
    if local {
        vars.insert("STEVE_LOCAL_NODE_PORT".into(), "0".into());
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
    let bus = Bus::new(std::sync::Arc::new(steve_runtime::cache::Caches::new(
        Default::default(),
    )));
    let w = steve_hub::wire::wire_with(&cfg, &bus, Arc::new(get.clone())).expect("wire");
    let local_node = local_node::attach(&w, &cfg, &vars).await;
    let st = AppState::new(cfg.clone(), w.backend, w.domain, bus)
        .with_node(w.node.clone())
        .with_modules(w.modules)
        .with_oidc(w.oidc.clone());
    steve_hub::app::boot(&st).await.expect("boot");
    local_node::start_jobs(w.jobs, &st.jobs);
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
        _local_node: local_node,
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
async fn harness_reports_require_login_and_an_inert_copy_stays_offline() {
    let e = start(false).await;
    let (status, _) = e.get("/api/node-harnesses", None, None).await;
    assert_eq!(status, 401);
    let (status, report) = e
        .get("/api/node-harnesses", Some(&e.owner_cookie), None)
        .await;
    assert_eq!(status, 200);
    assert_eq!(report["nodes"]["local"]["state"], "offline");
    assert!(report["nodes"]["local"]["checked_at"].is_null());
    let (status, report) = e
        .get(
            "/api/node-harnesses/local?fresh=1",
            Some(&e.owner_cookie),
            None,
        )
        .await;
    assert_eq!(status, 200);
    assert_eq!(report["report"]["state"], "offline");
    assert!(report["report"]["harnesses"].as_array().unwrap().is_empty());
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
    for revision in [Value::Null, json!(-1), json!(0.5)] {
        let (code, error) = patch(
            &e,
            &e.owner_cookie,
            json!({"revision":revision,"status":"暂停接活"}),
        )
        .await;
        assert_eq!(code, 400);
        assert_eq!(
            error["error"],
            "device_revision_required_refresh_or_upgrade"
        );
    }
    let (code, error) = patch(&e, &e.owner_cookie, json!({"status":"暂停接活"})).await;
    assert_eq!(code, 400);
    assert_eq!(
        error["error"],
        "device_revision_required_refresh_or_upgrade"
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
async fn device_role_or_name_never_establishes_local_binding_on_a_diagnostic_hub() {
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

async fn ready(e: &Env) -> Value {
    for _ in 0..200 {
        let (_, list) = e.get("/api/devices", Some(&e.owner_cookie), None).await;
        if list["devices"][0]["connected"] == true {
            return list["devices"][0].clone();
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    panic!("local node did not connect");
}

#[tokio::test(flavor = "multi_thread")]
async fn revoked_owner_waiting_for_sqlite_writer_cannot_patch_any_device_field() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let e = start(true).await;
    ready(&e).await;
    let owner = e.store().access().owner().unwrap().unwrap().id;
    // Each field independently, twice: status/name must not bypass max_sessions protection.
    for field in ["status", "name", "max_sessions"]
        .into_iter()
        .cycle()
        .take(6)
    {
        let (_, before) = e.get("/api/devices", Some(&e.owner_cookie), None).await;
        let before = before["devices"][0].clone();
        let mut body = json!({"revision":before["revision"]});
        body[field] = match field {
            "status" => json!("暂停接活"),
            "name" => json!("Revoked write"),
            _ => json!(32),
        };
        let body = body.to_string();
        let revocation =
            steve_store::testkit::hub::api::PendingOwnerRevocation::begin(&e.db, &owner).unwrap();
        let addr = e.base.strip_prefix("http://").unwrap();
        let mut stream = tokio::net::TcpStream::connect(addr).await.unwrap();
        stream.write_all(format!("PATCH /api/devices/hub HTTP/1.1\r\nHost: {addr}\r\nCookie: {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nExpect: 100-continue\r\nConnection: close\r\n\r\n", e.owner_cookie, body.len()).as_bytes()).await.unwrap();
        // Hyper only emits 100 Continue when the handler polls Bytes. require_user and
        // the Ctx parts extractor have therefore already read the still-committed owner.
        let mut interim = Vec::new();
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            while !interim.ends_with(b"\r\n\r\n") {
                interim.push(stream.read_u8().await.unwrap());
            }
        })
        .await
        .unwrap();
        assert_eq!(
            String::from_utf8(interim).unwrap(),
            "HTTP/1.1 100 Continue\r\n\r\n"
        );
        stream.write_all(body.as_bytes()).await.unwrap();
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(200), stream.read_u8())
                .await
                .is_err(),
            "must wait for writer"
        );
        revocation.commit().unwrap();
        let mut response = String::new();
        tokio::time::timeout(
            std::time::Duration::from_secs(5),
            stream.read_to_string(&mut response),
        )
        .await
        .unwrap()
        .unwrap();
        assert!(response.starts_with("HTTP/1.1 403"), "{field}: {response}");
        let (_, after) = e.get("/api/devices", Some(&e.owner_cookie), None).await;
        let after = &after["devices"][0];
        for key in ["revision", "status", "name", "max_sessions"] {
            assert_eq!(after[key], before[key], "{field}: {key}");
        }
        assert_eq!(after["can_configure"], false);
        steve_store::testkit::hub::api::restore_owner(&e.db, &owner).unwrap();
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn full_range_roundtrip_pause_resume_validation_and_concurrent_cas() {
    let e = start(true).await;
    let mut d = ready(&e).await;
    // Persisted 32 is a supported existing installation, not a value to clamp on read/save.
    steve_store::testkit::hub::hub_fixtures::set_devices_max_sessions_32(&e.store()).unwrap();
    d["max_sessions"] = json!(32);
    for (status, limit) in [("暂停接活", 32), ("正常", 32), ("正常", 256), ("正常", 32)] {
        let (code, r) = patch(
            &e,
            &e.owner_cookie,
            json!({"revision":d["revision"],"status":status,"max_sessions":limit}),
        )
        .await;
        assert_eq!(code, 200, "{r}");
        assert_eq!(r["device"]["max_sessions"], limit);
        assert_eq!(r["device"]["can_configure"], true);
        let (_, read) = e.get("/api/devices", Some(&e.owner_cookie), None).await;
        assert_eq!(read["devices"][0]["revision"], r["device"]["revision"]);
        assert_eq!(read["devices"][0]["max_sessions"], limit);
        assert_eq!(read["devices"][0]["status"], status);
        d = r["device"].clone();
    }
    for limit in [json!(0), json!(257), json!(1.5)] {
        assert_eq!(patch(&e,&e.owner_cookie,json!({"revision":d["revision"],"status":"暂停接活","name":"Invalid","max_sessions":limit})).await.0,400);
        let persisted = e.store().devices().list().unwrap().remove(0);
        assert_eq!(persisted.revision, d["revision"].as_i64().unwrap());
        assert_eq!(persisted.max_sessions, 32);
        assert_ne!(persisted.name, "Invalid");
        assert_eq!(persisted.status, "正常");
    }
    assert_eq!(
        patch(&e, &e.owner_cookie, json!({"status":"暂停接活"}))
            .await
            .0,
        400
    );
    assert_eq!(
        patch(&e, &e.owner_cookie, json!({"revision":0,"max_sessions":32}))
            .await
            .0,
        409
    );
    let (a, b) = tokio::join!(
        patch(
            &e,
            &e.owner_cookie,
            json!({"revision":d["revision"],"max_sessions":32})
        ),
        patch(
            &e,
            &e.owner_cookie,
            json!({"revision":d["revision"],"max_sessions":256})
        )
    );
    let mut codes = [a.0, b.0];
    codes.sort();
    assert_eq!(codes, [200, 409]);
    let persisted = e.store().devices().list().unwrap().remove(0);
    assert_eq!(persisted.revision, d["revision"].as_i64().unwrap() + 1);
}
