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
    start_with_retention(true).await
}

async fn start_with_retention(run_retention: bool) -> Env {
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
        if job.name == "storage.retention" && !run_retention {
            continue;
        }
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
    assert_eq!(m["reason"]["kind"], "manual");
    assert_eq!(m["completed"], true);
    let manifest: steve_store_sqlite::backup::Manifest = serde_json::from_value(m).unwrap();
    let artifact = steve_store_sqlite::backup::plan::artifact(&manifest.database_path()).unwrap();
    assert_eq!(artifact.sha256, manifest.database.sha256);
    assert!(manifest.directory.join("manifest.json").is_file());
    let (status, usage) = e.call("GET", "/api/system/backup", None).await;
    assert_eq!(status, 200);
    assert_eq!(usage["purposes"]["manual"]["count"], 1);
    assert!(usage["purposes"]["manual"]["bytes"].as_u64().unwrap() > 0);
    assert!(e.worktrees.is_dir());
    let (status, second) = e.call("POST", "/api/system/backup", None).await;
    assert_eq!(status, 200, "{second}");
    let second: steve_store_sqlite::backup::Manifest = serde_json::from_value(second).unwrap();
    assert_eq!(second.reason, steve_store_sqlite::backup::Reason::Manual);
    // Sparse logical file creates total pressure without allocating/copying a large payload.
    std::fs::OpenOptions::new()
        .write(true)
        .open(manifest.database_path())
        .unwrap()
        .set_len(steve_store_sqlite::backup::TOTAL_BYTES + 1)
        .unwrap();
    let report = steve_store_sqlite::backup::retain_at(
        &steve_hub::backup::root(&e.home),
        u64::MAX,
        steve_store_sqlite::backup::DEFAULT_DAILY_KEEP_DAYS,
    )
    .unwrap();
    assert_eq!(report.removed, 0);
    assert!(manifest.database_path().exists() && second.database_path().exists());
    let (_, status) = e.call("GET", "/api/system/backup", None).await;
    assert_eq!(status["retention"]["over_limit"], false);
    assert_eq!(status["retention"]["bytes"], 0);
    assert!(
        status["retention"]["manual_bytes"].as_u64().unwrap()
            > steve_store_sqlite::backup::TOTAL_BYTES
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn backup_status_exposes_sweep_failures_and_clears_them_after_retry() {
    use std::os::unix::fs::PermissionsExt;
    let e = start_with_retention(false).await;
    let old = steve_hub::daily_backup::make(&e.home, e.store.path(), "old").unwrap();
    let latest = steve_hub::daily_backup::make(&e.home, e.store.path(), "latest").unwrap();
    let path = old.database.parent().unwrap().join("manifest.json");
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o000)).unwrap();
    let denied = std::fs::read(&path).is_err();
    let report = steve_hub::backup::retain(&e.store, &e.home, u64::MAX).unwrap();
    let (_, status) = e.call("GET", "/api/system/backup", None).await;
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
    assert!(denied);
    assert_eq!(report.failures, 1);
    assert_eq!(status["retention"]["failures"], report.failures);
    assert_eq!(status["retention"]["inventory_failures"], 1);
    assert_eq!(
        steve_hub::backup::retain(&e.store, &e.home, u64::MAX)
            .unwrap()
            .removed,
        1
    );
    let (_, status) = e.call("GET", "/api/system/backup", None).await;
    assert_eq!(status["retention"]["failures"], 0);
    assert_eq!(status["retention"]["inventory_failures"], 0);
    assert!(!old.database.exists() && latest.database.exists());
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

#[tokio::test(flavor = "multi_thread")]
async fn owner_backup_settings_persist_and_control_generation_and_retention() {
    use steve_hub::backup;
    use steve_store_sqlite::backup::{self as storage, Manifest};
    // This test advances sweeps explicitly at a controlled `now`. A SystemClock background
    // sweep would race those calls and could retire the old copy before our counted sweep.
    let e = start_with_retention(false).await;
    let (status, initial) = e.call("GET", "/api/system/backup", None).await;
    assert_eq!(status, 200, "{initial}");
    assert_eq!(
        initial["settings"],
        serde_json::json!({"enabled":true,"keep_days":7})
    );
    // Unauthorized callers cannot alter installation settings.
    let unauthorized = e
        .http
        .patch(format!("{}/api/system/backup", e.base))
        .json(&serde_json::json!({"enabled":false,"keep_days":30}))
        .send()
        .await
        .unwrap();
    assert_eq!(unauthorized.status(), 401);
    let (status, saved) = e
        .call(
            "PATCH",
            "/api/system/backup",
            Some(serde_json::json!({"enabled":false,"keep_days":30})),
        )
        .await;
    assert_eq!(status, 200, "{saved}");
    assert_eq!(
        backup::daily_settings(&e.store).unwrap(),
        backup::DailySettings {
            enabled: false,
            keep_days: 30
        }
    );
    let (_, current) = e.call("GET", "/api/system/backup", None).await;
    assert_eq!(current["settings"], saved);
    assert_eq!(current["keep_days"], 30);
    assert_eq!(current["daily"], false);
    let root = backup::root(&e.home);
    assert!(
        steve_hub::daily_backup::make_if_enabled(&e.store, &e.home, e.store.path(), "today")
            .unwrap()
            .is_none()
    );
    assert!(storage::list(&root).unwrap().is_empty());
    // Create one 10-day-old daily backup and a newer daily backup; the wall clock is explicit.
    let first = steve_hub::daily_backup::make(&e.home, e.store.path(), "older").unwrap();
    let manifest_path = first.database.parent().unwrap().join("manifest.json");
    let mut manifest: Manifest =
        serde_json::from_slice(&std::fs::read(&manifest_path).unwrap()).unwrap();
    let now = manifest.created_ms;
    manifest.created_ms = now - 10 * 86_400_000;
    std::fs::write(&manifest_path, serde_json::to_vec(&manifest).unwrap()).unwrap();
    let latest = steve_hub::daily_backup::make(&e.home, e.store.path(), "latest").unwrap();
    assert_eq!(backup::retain(&e.store, &e.home, now).unwrap().removed, 0);
    // Changing the saved period applies next sweep, even while generation is off.
    let (status, saved) = e
        .call(
            "PATCH",
            "/api/system/backup",
            Some(serde_json::json!({"enabled":false,"keep_days":7})),
        )
        .await;
    assert_eq!(status, 200, "{saved}");
    assert!(first.database.exists());
    assert_eq!(backup::daily_settings(&e.store).unwrap().keep_days, 7);
    let inventory = storage::list(&root).unwrap();
    assert_eq!(inventory.len(), 2, "inventory: {inventory:?}");
    let old = inventory
        .iter()
        .find(|m| m.database_path() == first.database)
        .unwrap();
    let newest = inventory
        .iter()
        .find(|m| m.database_path() == latest.database)
        .unwrap();
    assert_eq!(old.created_ms, now - 10 * 86_400_000);
    assert!(
        old.published_order < newest.published_order,
        "{inventory:?}"
    );
    let children: Vec<_> = std::fs::read_dir(&old.directory)
        .unwrap()
        .map(|e| e.unwrap().file_name())
        .collect();
    let report = backup::retain(&e.store, &e.home, now).unwrap();
    assert_eq!(report.failures, 0, "{report:?}");
    assert_eq!(
        report.removed, 1,
        "{report:?}; inventory={inventory:?}; children={children:?}"
    );
    assert!(!first.database.exists() && latest.database.exists());
    assert!(
        steve_hub::daily_backup::make_if_enabled(&e.store, &e.home, e.store.path(), "today")
            .unwrap()
            .is_none()
    );
    let (status, saved) = e
        .call(
            "PATCH",
            "/api/system/backup",
            Some(serde_json::json!({"enabled":true,"keep_days":7})),
        )
        .await;
    assert_eq!(status, 200, "{saved}");
    assert!(
        steve_hub::daily_backup::make_if_enabled(&e.store, &e.home, e.store.path(), "today")
            .unwrap()
            .is_some()
    );
    for bad in [
        serde_json::json!({"enabled":true,"keep_days":0}),
        serde_json::json!({"enabled":true,"keep_days":366}),
        serde_json::json!({"enabled":"yes","keep_days":7}),
        serde_json::json!({"enabled":true,"keep_days":7,"max_bytes":1}),
    ] {
        assert_eq!(
            e.call("PATCH", "/api/system/backup", Some(bad)).await.0,
            400
        );
    }
    assert_eq!(
        backup::daily_settings(&e.store).unwrap(),
        backup::DailySettings::default()
    );
}
