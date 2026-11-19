//! Isolated in-process HTTP acceptance for the shared backup plan.
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
    worktrees: std::path::PathBuf,
    store: steve_store::Store,
    home: std::path::PathBuf,
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
    let store = w.modules.as_ref().unwrap().store.clone();
    let st = AppState::new(cfg.clone(), w.backend, w.domain, bus).with_modules(w.modules);
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
        worktrees: cfg.worktrees.clone(),
        store,
        home: cfg.home.clone(),
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn owner_http_backup_publishes_manifest_and_updates_purpose_usage() {
    let e = start().await;
    let unauthorized = e
        .http
        .post(format!("{}/api/system/backup", e.base))
        .send()
        .await
        .unwrap();
    assert_eq!(unauthorized.status(), 401);
    let (status, m) = e.call("POST", "/api/system/backup", None).await;
    assert_eq!(status, 200, "{m}");
    assert_eq!(m["reason"]["kind"], "diagnostic");
    assert_eq!(m["completed"], true);
    let manifest: steve_store_sqlite::backup::Manifest = serde_json::from_value(m).unwrap();
    let artifact = steve_store_sqlite::backup::plan::artifact(&manifest.database_path()).unwrap();
    assert_eq!(artifact.sha256, manifest.database.sha256);
    assert!(manifest.directory.join("manifest.json").is_file());
    let (status, usage) = e.call("GET", "/api/system/backup", None).await;
    assert_eq!(status, 200);
    assert_eq!(usage["purposes"]["diagnostic"]["count"], 1);
    assert!(usage["purposes"]["diagnostic"]["bytes"].as_u64().unwrap() > 0);
    assert!(e.worktrees.is_dir());
}

#[tokio::test(flavor = "multi_thread")]
async fn assembled_cleanup_route_receives_the_hub_backup_capability() {
    let e = start().await;
    let seed_sql = "INSERT INTO parties(id,workspace_id,type,name,source,created_by,created,updated)
             VALUES('backup-owner','default','contact','Backup owner','manual','usr_dev',1,1);
             INSERT INTO accounts(id,workspace_id,system_id,family,party_id,kind,handle,handle_display,source,created,updated)
             VALUES('backup-account','default','test','test','backup-owner','personal','backup','Backup','connect',1,1);
             INSERT INTO spaces(id,workspace_id,system_id,kind,external_id,name,status,created,updated,account_id)
             VALUES('backup-space','default','test','group','backup-space','Backup','left',1,1,'backup-account');
             INSERT INTO chan_messages(id,workspace_id,space_id,direction,status,text,created)
             VALUES('backup-message','default','backup-space','in','received','before cleanup',1);";
    e.store
        .transaction(|tx| -> Result<(), steve_store::StoreError> {
            tx.execute_batch(seed_sql)?;
            steve_store_sqlite::digest::sync_message(
                tx,
                steve_store_sqlite::digest::Source::ChanMessage,
                "backup-message",
                None,
            )?;
            Ok(())
        })
        .unwrap();
    let (status, summary) = e
        .call(
            "POST",
            "/api/channels/cleanup",
            Some(
                serde_json::json!({"account_id":"backup-account", "level":"text", "confirm":true}),
            ),
        )
        .await;
    assert_eq!(status, 200, "{summary}");
    assert_eq!(summary["messages"], 1);
    let relative = summary["backup"].as_str().unwrap();
    let root = steve_hub::backup::root(&e.home);
    assert!(root.join(relative).is_file());
    let manifests = steve_store_sqlite::backup::list(&root).unwrap();
    assert_eq!(manifests.len(), 1);
    assert_eq!(
        manifests[0].reason,
        steve_store_sqlite::backup::Reason::PreDestructive {
            what: "purge".into()
        }
    );
    let snapshot = rusqlite::Connection::open(root.join(relative)).unwrap();
    let before: String = snapshot
        .query_row(
            "SELECT text FROM chan_messages WHERE id='backup-message'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(before, "before cleanup");
    e.store
        .reader(|c| -> Result<(), steve_store::StoreError> {
            let text: Option<String> = c.query_row(
                "SELECT text FROM chan_messages WHERE id='backup-message'",
                [],
                |r| r.get(0),
            )?;
            assert_eq!(text, None);
            assert!(steve_store_sqlite::digest::verify_pending(c, "default")?.is_empty());
            Ok(())
        })
        .unwrap();
}
