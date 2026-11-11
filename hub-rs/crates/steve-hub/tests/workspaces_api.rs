//! `/api/system/workspaces` on the wired Hub (ST-45): the disk report with the configured thresholds and
//! folders no item points at, and a reclaim pass on demand. Temp STEVE_HOME, adapters `/usr/bin/false`.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

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
        base,
        http,
        cookie,
        worktrees: cfg.worktrees.clone(),
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn workspaces_report_and_reclaim() {
    let e = start().await;
    std::fs::create_dir_all(e.worktrees.join("OLD-1")).expect("orphan");
    std::fs::write(e.worktrees.join("OLD-1/f"), vec![0u8; 8192]).expect("orphan file");
    let (s, v) = e.call("GET", "/api/system/workspaces", None).await;
    assert_eq!(s, 200, "{v}");
    assert_eq!(v["policy"]["floor"], 7u64 << 30, "{v}");
    assert_eq!(v["policy"]["low"], 9u64 << 30, "{v}");
    assert!(v["free"].as_u64().is_some_and(|f| f > 0), "{v}");
    assert_eq!(v["entries"], Value::Array(vec![]));
    let orphans = v["orphans"].as_array().expect("orphans");
    assert_eq!(orphans.len(), 1, "{v}");
    assert!(orphans[0]["path"].as_str().unwrap().ends_with("OLD-1"));
    assert!(orphans[0]["bytes"].as_u64().unwrap() >= 8192);

    /* the boot pass may still run: a second caller is told it was skipped, never blocked */
    let end = tokio::time::Instant::now() + Duration::from_secs(20);
    loop {
        let (s, v) = e.call("POST", "/api/system/workspaces/reclaim", None).await;
        assert_eq!(s, 200, "{v}");
        if v["skipped"] == false {
            assert_eq!(v["removed"], Value::Array(vec![]));
            assert!(v["free_after"].as_u64().is_some(), "{v}");
            break;
        }
        assert!(tokio::time::Instant::now() < end, "never ran: {v}");
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(
        e.worktrees.join("OLD-1/f").exists(),
        "a folder no item points at is never deleted"
    );
}

/// The data page asks where the database and its copies are: the Hub's own paths, the daily copy as
/// it is started (this Hub runs with NO_RESUME, as one on a copy of a real database does, so it makes
/// none), and no copy yet on a Hub that has just started.
#[tokio::test(flavor = "multi_thread")]
async fn the_owner_reads_where_the_database_and_its_copies_are() {
    let e = start().await;
    let (s, v) = e.call("GET", "/api/system/backup", None).await;
    assert_eq!(s, 200, "{v}");
    let (database, dir) = (v["database"].as_str().unwrap(), v["dir"].as_str().unwrap());
    assert!(database.ends_with("/data/steve.sqlite3"), "{v}");
    assert!(std::path::Path::new(database).exists(), "{v}");
    assert_eq!(
        std::path::Path::new(dir),
        std::path::Path::new(database)
            .parent()
            .and_then(|d| d.parent())
            .unwrap()
            .join("backups")
    );
    assert_eq!(v["daily"], false, "{v}");
    assert_eq!(v["keep_days"], 14, "{v}");
    assert_eq!(v["last"], Value::Null, "{v}");
}
