//! `/api/yolo` as wired in the binary (YOLO Y0): behind the login, workspace-scoped, forced off on a
//! production-copy Hub. In process on a temp STEVE_HOME with an empty database; no agent can start
//! (adapters are `/usr/bin/false`) and nothing outside the temp dir is touched.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use serde_json::{Value, json};
use steve_hub::api::AppState;
use steve_hub::bus::Bus;
use steve_hub::config::Config;

struct Hub {
    home: tempfile::TempDir,
    base: String,
    http: reqwest::Client,
    cookie: String,
    db: std::path::PathBuf,
}

impl Hub {
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

    fn query(&self, sql: &str) -> Vec<String> {
        let c = rusqlite::Connection::open(&self.db).expect("db");
        c.busy_timeout(Duration::from_secs(5)).expect("timeout");
        let mut st = c.prepare(sql).expect("prepare");
        st.query_map([], |r| r.get::<_, String>(0))
            .expect("query")
            .collect::<rusqlite::Result<_>>()
            .expect("rows")
    }

    fn sql(&self, sql: &str) {
        let c = rusqlite::Connection::open(&self.db).expect("db");
        c.busy_timeout(Duration::from_secs(5)).expect("timeout");
        c.execute_batch(sql).expect("sql");
    }
}

async fn start(no_resume: &str, yolo: Option<&str>) -> Hub {
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
        // an empty temp database: nothing to resume; the agents cannot start anyway
        ("STEVE_NO_RESUME", no_resume.into()),
        ("CLAUDE_CODE_EXECUTABLE", "/usr/bin/false".into()),
        ("STEVE_ADAPTER_CLAUDE", "/usr/bin/false".into()),
        ("STEVE_ADAPTER_CODEX", "/usr/bin/false".into()),
        ("STEVE_WEB_DIR", h.join("no-web").display().to_string()),
    ] {
        vars.insert(k.into(), v);
    }
    if let Some(y) = yolo {
        vars.insert("STEVE_YOLO".into(), y.into());
    }
    let vars = Arc::new(vars);
    let get = move |k: &str| vars.get(k).cloned();
    let cfg = Arc::new(Config::from_vars(&get).expect("config"));
    for d in [&cfg.data_dir, &cfg.worktrees, &cfg.scratch] {
        std::fs::create_dir_all(d).expect("dirs");
    }
    let bus = Bus::new();
    let w = steve_hub::wire::wire_with(&cfg, &bus, Arc::new(get.clone())).expect("wire");
    let st = AppState::new(cfg.clone(), w.backend, w.domain, bus).with_modules(w.modules);
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
    let cookie = r
        .headers()
        .get_all("set-cookie")
        .iter()
        .filter_map(|v| v.to_str().ok())
        .find(|c| c.starts_with("steve_session="))
        .map(|c| c.split(';').next().unwrap_or_default().to_string())
        .expect("session cookie");
    Hub {
        db: cfg.db_path(),
        home,
        base,
        http,
        cookie,
    }
}

#[tokio::test]
async fn switch_scopes_and_resume() {
    let h = start("0", None).await;
    // behind the login
    let r = h
        .http
        .get(format!("{}/api/yolo", h.base))
        .send()
        .await
        .expect("get");
    assert_eq!(r.status().as_u16(), 401);

    let (s, v) = h.call("GET", "/api/yolo", None).await;
    assert_eq!(s, 200, "{v}");
    assert_eq!(
        (v["on"].clone(), v["forced_off"].clone()),
        (json!(false), Value::Null)
    );

    let (s, v) = h
        .call(
            "PUT",
            "/api/yolo",
            Some(json!({"on": true, "until": 1_800_000_000_000_i64})),
        )
        .await;
    assert_eq!((s, v), (400, json!({"error": "参数不对：until"})));
    let (s, v) = h
        .call(
            "PUT",
            "/api/yolo",
            Some(json!({"on": true, "preview": true})),
        )
        .await;
    assert_eq!(s, 200, "{v}");
    assert_eq!(v["preview"], true);
    let (s, v) = h.call("PUT", "/api/yolo", Some(json!({"on": true}))).await;
    assert_eq!(s, 200, "{v}");
    assert_eq!(
        (v["on"].clone(), v["active"].clone()),
        (json!(true), json!(true))
    );
    assert!(
        v["window"]["id"]
            .as_str()
            .is_some_and(|w| w.starts_with("yw_"))
    );

    // a project and an item of this workspace, and a project of another one
    let dir = h.home.path().join("proj");
    std::fs::create_dir_all(&dir).expect("dir");
    let (s, p) = h
        .call(
            "POST",
            "/api/projects",
            Some(json!({"name": "p", "path": dir})),
        )
        .await;
    assert_eq!(s, 200, "{p}");
    let pid = p["id"].as_str().expect("project id").to_string();
    // a plain directory: off by default
    let (_, v) = h.call("GET", "/api/yolo", None).await;
    assert!(
        v["scopes"]
            .as_array()
            .expect("scopes")
            .iter()
            .any(|x| x["object_id"] == pid.as_str() && x["default"] == true)
    );
    let (s, v) = h
        .call(
            "PUT",
            &format!("/api/yolo/scopes/project/{pid}"),
            Some(json!({"mode": "on"})),
        )
        .await;
    assert_eq!(s, 200, "{v}");
    assert!(
        v["scopes"]
            .as_array()
            .expect("scopes")
            .iter()
            .any(|x| x["object_id"] == pid.as_str() && x["mode"] == "on")
    );
    let (s, v) = h
        .call(
            "PUT",
            &format!("/api/yolo/scopes/project/{pid}"),
            Some(json!({"mode": "sometimes"})),
        )
        .await;
    assert_eq!((s, v), (400, json!({"error": "参数不对：mode"})));
    h.sql(&format!("INSERT INTO workspaces (id, name, created) VALUES ('ws_b', 'b', 1); UPDATE projects SET workspace_id = 'ws_b' WHERE id = '{pid}'"));
    let (s, v) = h
        .call(
            "PUT",
            &format!("/api/yolo/scopes/project/{pid}"),
            Some(json!({"mode": "off"})),
        )
        .await;
    assert_eq!((s, v), (404, json!({"error": "没有这个项目"})));
    let (s, v) = h
        .call(
            "PUT",
            "/api/yolo/scopes/item/ST-999",
            Some(json!({"mode": "off"})),
        )
        .await;
    assert_eq!((s, v), (404, json!({"error": "没有这个事项"})));

    // resume without a trip
    let (s, v) = h.call("POST", "/api/yolo/resume", None).await;
    assert_eq!((s, v), (409, json!({"error": "YOLO 没有暂停"})));

    let (s, v) = h.call("PUT", "/api/yolo", Some(json!({"on": false}))).await;
    assert_eq!(s, 200, "{v}");
    assert_eq!(v["on"], false);
    assert_eq!(
        h.query("SELECT action FROM audit WHERE action LIKE 'yolo.%' ORDER BY id"),
        vec!["yolo.on", "yolo.scope", "yolo.off"]
    );
    assert_eq!(h.query("SELECT ended_why FROM yolo_windows"), vec!["off"]);
    assert_eq!(
        h.query("SELECT DISTINCT object_type FROM revisions WHERE object_type = 'yolo_scope'"),
        vec!["yolo_scope"]
    );
}

#[tokio::test]
async fn forced_off_on_a_production_copy_and_by_the_master_switch() {
    for (no_resume, yolo, code) in [("1", None, "no_resume"), ("0", Some("off"), "env")] {
        let h = start(no_resume, yolo).await;
        let (s, v) = h.call("GET", "/api/yolo", None).await;
        assert_eq!(s, 200, "{v}");
        assert_eq!(v["forced_off"], code);
        let (s, v) = h.call("PUT", "/api/yolo", Some(json!({"on": true}))).await;
        assert_eq!(s, 409, "{v}");
        assert!(
            v["error"].as_str().is_some_and(|e| e.contains("YOLO")),
            "{v}"
        );
        let (s, _) = h.call("PUT", "/api/yolo", Some(json!({"on": false}))).await;
        assert_eq!(s, 200);
        assert!(
            h.query("SELECT action FROM audit WHERE action = 'yolo.on'")
                .is_empty()
        );
    }
}

#[tokio::test]
async fn the_ledger_routes() {
    let h = start("0", None).await;
    let (s, v) = h.call("PUT", "/api/yolo", Some(json!({"on": true}))).await;
    assert_eq!(s, 200, "{v}");
    let (s, v) = h.call("GET", "/api/yolo/actions", None).await;
    assert_eq!(s, 200, "{v}");
    let windows = v["windows"].as_array().expect("windows");
    assert_eq!(windows.len(), 1, "{v}");
    assert_eq!(windows[0]["items"], json!([]));
    let (s, v) = h
        .call(
            "GET",
            "/api/yolo/actions?window=current&outcome=decided",
            None,
        )
        .await;
    assert_eq!(s, 200, "{v}");
    let (s, v) = h
        .call("GET", "/api/yolo/actions?window=yw_nope", None)
        .await;
    assert_eq!(s, 404, "{v}");
    for verb in ["undo", "now", "cancel"] {
        let (s, v) = h
            .call("POST", &format!("/api/yolo/actions/ya_nope/{verb}"), None)
            .await;
        assert_eq!(s, 404, "{verb}: {v}");
    }
    // a declined row has nothing to undo
    let c = rusqlite::Connection::open(&h.db).expect("db");
    c.execute(
        "INSERT INTO yolo_actions (id, workspace_id, kind, subject_type, rule, outcome, reason, created)
         VALUES ('ya_x', 'default', 'approve', 'tool_call', 'limit.5', 'declined', '硬限制：x', 1)",
        [],
    )
    .expect("row");
    let (s, v) = h.call("POST", "/api/yolo/actions/ya_x/undo", None).await;
    assert_eq!((s, v), (400, json!({"error": "这个决定不能撤销"})));
    let (s, v) = h.call("POST", "/api/yolo/actions/ya_x/now", None).await;
    assert_eq!(s, 409, "{v}");
    let _ = &h.home;
}
