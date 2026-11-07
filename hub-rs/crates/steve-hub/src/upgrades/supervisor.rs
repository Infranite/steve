//! The service manager owns this process, which owns the replaceable Hub child.
use super::*;
use std::time::Duration;
use tokio::process::{Child, Command};
struct HubChild {
    process: Child,
    group: u32,
}
impl std::ops::Deref for HubChild {
    type Target = Child;
    fn deref(&self) -> &Child {
        &self.process
    }
}
impl std::ops::DerefMut for HubChild {
    fn deref_mut(&mut self) -> &mut Child {
        &mut self.process
    }
}

fn switch(home: &Path, target: &Path) -> Result<()> {
    let tmp = home.join(format!(".current-{}", uuid::Uuid::new_v4()));
    std::os::unix::fs::symlink(target, &tmp)?;
    fs::rename(&tmp, home.join("current"))?;
    File::open(home)?.sync_all()?;
    Ok(())
}
fn control_token(cfg: &Config) -> Result<String> {
    let path = cfg.home.join("updates/control-token.json");
    if path.exists() {
        return read(&path);
    }
    let token = format!("{}{}", uuid::Uuid::new_v4(), uuid::Uuid::new_v4());
    atomic(&path, &token)?;
    Ok(token)
}
async fn maintenance(cfg: &Config, u: &Updates, method: reqwest::Method) -> Result<Value> {
    Ok(u.client
        .request(
            method,
            format!(
                "http://{}:{}/internal/upgrade/drain",
                cfg.listen.local_host(),
                cfg.listen.port
            ),
        )
        .bearer_auth(control_token(cfg)?)
        .timeout(Duration::from_secs(10))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?)
}
async fn drain(cfg: &Config, u: &Updates) -> Result<()> {
    maintenance(cfg, u, reqwest::Method::POST).await?;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(120);
    while tokio::time::Instant::now() < deadline {
        if maintenance(cfg, u, reqwest::Method::GET).await?["ready"] == true {
            return Ok(());
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    bail!("维护排空超时；当前版本保留，尚未停止或切换")
}
async fn release_maintenance(cfg: &Config, u: &Updates) -> Result<()> {
    maintenance(cfg, u, reqwest::Method::DELETE).await?;
    Ok(())
}
fn spawn(cfg: &Config, bundle: &Path, op: Option<&Operation>) -> Result<HubChild> {
    let manifest: Manifest = read(&bundle.join("release.json"))?;
    let _ = control_token(cfg)?;
    let mut c = Command::new(bundle.join("bin/steve-hub"));
    c.env("STEVE_SUPERVISOR_PID", std::process::id().to_string())
        .env_remove("STEVE_UPGRADE_CONTROL_TOKEN")
        .env("STEVE_PORT", cfg.listen.port.to_string())
        .env("STEVE_HOST", cfg.listen.env_value())
        .env("STEVE_HOME", &cfg.home)
        .env("STEVE_MANAGED", "1")
        .env("STEVE_VERSION", &manifest.version)
        .env("STEVE_WEB_DIR", bundle.join("web/dist"))
        .process_group(0)
        .kill_on_drop(true);

    if let Some(op) = op {
        c.env("STEVE_UPGRADE_ID", &op.id);
    } else {
        c.env_remove("STEVE_UPGRADE_ID");
    }
    let process = c.spawn()?;
    let group = process.id().context("spawned child has no PID")?;
    Ok(HubChild { process, group })
}
async fn stop(child: &mut HubChild) -> Result<()> {
    let group = child.group as i32;
    if child.try_wait()?.is_none() {
        unsafe {
            libc::kill(-group, libc::SIGTERM);
        }
        if tokio::time::timeout(Duration::from_secs(15), child.wait())
            .await
            .is_err()
        {
            unsafe {
                libc::kill(-group, libc::SIGKILL);
            }
            child.kill().await?;
            child.wait().await?;
        }
    }
    // A group may outlive its leader; remove descendants even when wait() already reaped the Hub.
    unsafe {
        libc::kill(-group, libc::SIGKILL);
    }
    Ok(())
}

fn backup(db: &Path, dest: &Path) -> Result<()> {
    if !db.exists() {
        bail!("database missing; cannot make verified upgrade backup")
    }
    let src = rusqlite::Connection::open(db)?;
    let mut dst = rusqlite::Connection::open(dest)?;
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(dest, fs::Permissions::from_mode(0o600))?;
    }
    {
        let backup = rusqlite::backup::Backup::new(&src, &mut dst)?;
        backup.run_to_completion(128, Duration::from_millis(5), None)?;
    }
    let result: String = dst.query_row("PRAGMA quick_check", [], |r| r.get(0))?;
    if result != "ok" {
        bail!("backup integrity check failed: {result}")
    }
    dst.close().map_err(|(_, e)| e)?;
    File::open(dest)?.sync_all()?;
    File::open(dest.parent().context("backup needs parent directory")?)?.sync_all()?;
    Ok(())
}
fn restore(db: &Path, saved: &Path, failed: &Path) -> Result<()> {
    // Child is stopped, so no open database/wal writers remain.
    let failed = failed.with_extension(format!("{}.sqlite3", uuid::Uuid::new_v4()));
    if db.exists() {
        fs::rename(db, &failed)?;
    }
    for suffix in ["-wal", "-shm"] {
        let p = PathBuf::from(format!("{}{suffix}", db.display()));
        if p.exists() {
            fs::rename(&p, PathBuf::from(format!("{}{suffix}", failed.display())))?;
        }
    }
    fs::copy(saved, db)?;
    File::open(db)?.sync_all()?;
    Ok(())
}
async fn healthy(cfg: &Config, client: &reqwest::Client, version: &str, id: Option<&str>) -> bool {
    let host = cfg.listen.local_host();
    let Ok(r) = client
        .get(format!("http://{host}:{}/healthz", cfg.listen.port))
        .timeout(Duration::from_secs(2))
        .send()
        .await
    else {
        return false;
    };
    let Ok(v) = r.json::<Value>().await else {
        return false;
    };
    let expected = read::<Manifest>(&cfg.home.join("current/release.json")).ok();
    let identity_ok = match v["build_commit"].as_str() {
        Some(commit) => expected.as_ref().is_some_and(|m| m.commit == commit),
        None => cfg!(debug_assertions),
    };
    identity_ok
        && v["version"] == version
        && v["web_version"] == version
        && match id {
            Some(id) => v["upgrade_id"] == id && v["maintenance"] == true,
            None => v["maintenance"] != true,
        }
}
async fn wait_health(
    cfg: &Config,
    u: &Updates,
    child: &mut HubChild,
    version: &str,
    id: Option<&str>,
) -> Result<()> {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(60);
    while tokio::time::Instant::now() < deadline {
        if let Some(status) = child.try_wait()? {
            bail!("candidate exited: {status}")
        };
        if tokio::time::timeout_at(deadline, healthy(cfg, &u.client, version, id))
            .await
            .unwrap_or(false)
        {
            return Ok(());
        };
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    bail!("Hub health verification timed out")
}
async fn apply(cfg: &Config, u: &Updates, child: &mut HubChild, op: &mut Operation) -> Result<()> {
    let previous = fs::canonicalize(cfg.home.join("current"))?;
    let target = PathBuf::from(op.target.as_ref().context("staged target missing")?);
    let manifest: Manifest = read(&target.join("release.json"))?;
    super::client::validate_manifest(&manifest, &op.version)?;
    op.previous = Some(previous.to_string_lossy().into());
    u.phase(op, "draining", None)?;
    if let Err(e) = drain(cfg, u).await {
        let cancel = release_maintenance(cfg, u).await;
        return Err(anyhow::anyhow!(
            "{e:#}; maintenance cancellation: {cancel:?}"
        ));
    }
    u.phase(op, "stopping", None)?;
    stop(child).await?;
    u.phase(op, "backing_up", None)?;
    let backup_path = u.dir().join(format!("{}.sqlite3", op.id));
    if let Err(e) = backup(&cfg.db_path(), &backup_path) {
        *child = spawn(cfg, &previous, None)?;
        let old: Manifest = read(&previous.join("release.json"))?;
        wait_health(cfg, u, child, &old.version, None).await?;
        release_maintenance(cfg, u).await?;
        return Err(e);
    }
    u.phase(op, "restarting", None)?;
    switch(&cfg.home, &target)?;
    *child = spawn(cfg, &target, Some(op))?;
    u.phase(op, "verifying", None)?;
    if let Err(e) = wait_health(cfg, u, child, &op.version, Some(&op.id)).await {
        rollback(cfg, u, child, op, &backup_path, format!("{e:#}")).await?;
        return Ok(());
    }
    // This is the irreversible business boundary: after commit never restore an old database.
    u.phase(op, "committed", None)?;
    match wait_health(cfg, u, child, &op.version, None).await {
        Ok(()) => {
            release_maintenance(cfg, u).await?;
            u.phase(op, "succeeded", None)?;
        }
        Err(e) => u.phase(
            op,
            "failed",
            Some(format!(
                "升级已提交，业务启动失败；保留新数据库，禁止自动回滚：{e:#}"
            )),
        )?,
    };
    Ok(())
}
async fn rollback(
    cfg: &Config,
    u: &Updates,
    child: &mut HubChild,
    op: &mut Operation,
    backup_path: &Path,
    error: String,
) -> Result<()> {
    u.phase(op, "rolling_back", Some(error.clone()))?;
    stop(child).await?;
    restore(
        &cfg.db_path(),
        backup_path,
        &u.dir().join(format!("{}.failed.sqlite3", op.id)),
    )?;
    let previous = PathBuf::from(
        op.previous
            .as_ref()
            .context("rollback missing previous bundle")?,
    );
    switch(&cfg.home, &previous)?;
    u.phase(op, "rolled_back_pending_health", Some(error.clone()))?;
    *child = spawn(cfg, &previous, None)?;
    let manifest: Manifest = read(&previous.join("release.json"))?;
    wait_health(cfg, u, child, &manifest.version, None).await?;
    release_maintenance(cfg, u).await?;
    u.phase(op, "rolled_back", Some(error))?;
    Ok(())
}
pub async fn run(cfg: Config) -> Result<()> {
    let mut u = Updates::new(&cfg)?;
    u.managed = true;
    if let Some(reason) = u.execution_reason() {
        bail!("{reason}")
    }
    fs::create_dir_all(u.dir())?;
    let lock = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(u.dir().join("supervisor.lock"))?;
    lock.try_lock()
        .context("another supervisor already owns this installation")?;
    let instance = super::instance_lock(&cfg)?;
    // Service-manager restart must stop the complete service cgroup/job before invoking a new supervisor.
    // On a standalone launch, refuse an occupied port instead of killing an unrelated process.
    let probe = cfg
        .listen
        .bind()
        .await
        .context("Hub port occupied; stop the previous managed instance first")?;
    drop(probe);
    if let Some(mut op) = u.active()? {
        match op.phase.as_str() {
            "restarting" | "verifying" | "rolling_back" => {
                let previous =
                    PathBuf::from(op.previous.as_ref().context("missing rollback bundle")?);
                restore(
                    &cfg.db_path(),
                    &u.dir().join(format!("{}.sqlite3", op.id)),
                    &u.dir().join(format!("{}.failed.sqlite3", op.id)),
                )?;
                switch(&cfg.home, &previous)?;
                u.phase(
                    &mut op,
                    "rolled_back_pending_health",
                    Some("supervisor restarted before health commit".into()),
                )?;
            }
            "draining" | "stopping" | "backing_up" => u.phase(
                &mut op,
                "failed",
                Some(
                    "supervisor restarted before version switch; retained original database".into(),
                ),
            )?,
            "staging" => u.phase(
                &mut op,
                "failed",
                Some("download interrupted; retry with a new idempotency key".into()),
            )?,
            _ => {}
        }
    }
    let current = fs::canonicalize(cfg.home.join("current"))?;
    drop(instance);
    let mut child = spawn(&cfg, &current, None)?;
    let initial_manifest: Manifest = read(&current.join("release.json"))?;
    wait_health(&cfg, &u, &mut child, &initial_manifest.version, None).await?;
    release_maintenance(&cfg, &u).await?;
    if let Some(mut op) = u.active()?
        && matches!(
            op.phase.as_str(),
            "committed" | "rolled_back_pending_health" | "failed"
        )
    {
        let current_manifest: Manifest = read(&current.join("release.json"))?;
        match wait_health(&cfg, &u, &mut child, &current_manifest.version, None).await {
            Ok(()) => {
                release_maintenance(&cfg, &u).await?;
                if op.phase == "committed" {
                    u.phase(&mut op, "succeeded", None)?
                } else if op.phase == "rolled_back_pending_health" {
                    let error = op.error.clone();
                    u.phase(&mut op, "rolled_back", error)?
                }
            }
            Err(e) => {
                u.phase(
                    &mut op,
                    "failed",
                    Some(format!("version startup failed: {e:#}")),
                )?;
                return Err(e);
            }
        }
    }
    let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
    loop {
        tokio::select! {_ = term.recv()=>{stop(&mut child).await?;return Ok(())},_ = tokio::signal::ctrl_c()=>{stop(&mut child).await?;return Ok(())},_ = tokio::time::sleep(Duration::from_millis(500))=>{}}
        if let Some(mut op) = u.active()?
            && op.phase == "ready"
            && let Err(e) = apply(&cfg, &u, &mut child, &mut op).await
        {
            // If we crossed the switch but not commit, make a best-effort rollback under the closed gate.
            if matches!(op.phase.as_str(), "restarting" | "verifying") {
                let backup = u.dir().join(format!("{}.sqlite3", op.id));
                if let Err(rb) =
                    rollback(&cfg, &u, &mut child, &mut op, &backup, format!("{e:#}")).await
                {
                    u.phase(
                        &mut op,
                        "failed",
                        Some(format!("upgrade: {e:#}; rollback: {rb:#}")),
                    )?;
                    return Err(rb);
                }
            } else {
                u.phase(&mut op, "failed", Some(format!("{e:#}")))?;
            }
        }
        if let Some(status) = child.try_wait()? {
            bail!("Hub child exited: {status}; supervisor must be restarted by service manager")
        }
    }
}
/// Called before wire(), boot(), adapters and all background tasks. No business code runs here.
pub async fn startup_gate(cfg: &Config) -> Result<()> {
    let supervisor_pid = unsafe { libc::getppid() };
    let Ok(id) = std::env::var("STEVE_UPGRADE_ID") else {
        return Ok(());
    };
    if supervisor_pid <= 1 {
        bail!("upgrade supervisor is no longer alive")
    }
    let u = Updates::new(cfg)?;
    let op = u.operation(&id)?;
    if op.phase == "committed" || op.phase == "succeeded" {
        return Ok(());
    }
    if op.version != cfg.version || !matches!(op.phase.as_str(), "restarting" | "verifying") {
        bail!("invalid upgrade gate state")
    }
    let manifest: Manifest =
        read(&PathBuf::from(op.target.context("gate target missing")?).join("release.json"))?;
    super::client::validate_manifest(&manifest, &cfg.version)?;
    match (
        option_env!("STEVE_BUILD_VERSION"),
        option_env!("STEVE_BUILD_COMMIT"),
    ) {
        (Some(version), Some(commit))
            if version == manifest.version && commit == manifest.commit => {}
        (None, None) if cfg!(debug_assertions) => {}
        _ => bail!("candidate compiled identity does not match release manifest"),
    }

    let keys = steve_modules::vault::keystore::from_env(&cfg.home, |k| std::env::var(k).ok())
        .map_err(|e| anyhow::anyhow!("vault unavailable: {e}"))?;
    let store = steve_store::Store::open(cfg.db_path(), steve_store::NoPublisher)?;
    drop(store);
    let db = rusqlite::Connection::open(cfg.db_path())?;
    let schema: u32 = db.query_row("SELECT v FROM schema_version", [], |r| r.get(0))?;
    if schema != manifest.schema.target
        || schema < manifest.schema.min_read
        || schema > manifest.schema.max_read
    {
        bail!("candidate database schema incompatible with manifest")
    }
    let rows:Vec<(String,i64,String,String)>=db.prepare("SELECT workspace_id,version,keychain_account,fingerprint FROM vault_keys WHERE status != 'retired'")?.query_map([],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?)))?.collect::<std::result::Result<_,_>>()?;
    let db_path = cfg.db_path();
    tokio::time::timeout(Duration::from_secs(15),tokio::task::spawn_blocking(move||->Result<()>{
        let db=rusqlite::Connection::open_with_flags(db_path,rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)?;
        for (ws,version,account,expected) in rows {
            let key=keys.get(&account).map_err(|e|anyhow::anyhow!("vault probe: {e}"))?.context("vault probe: existing KEK missing")?;
            if steve_modules::vault::crypto::fingerprint(&key)!=expected{bail!("vault probe: KEK fingerprint mismatch")}
            let mut stmt=db.prepare("SELECT id,dek_wrapped FROM vault_items WHERE workspace_id=? AND kek_version=? AND dek_wrapped IS NOT NULL LIMIT 1")?;
            let mut rows=stmt.query(rusqlite::params![ws,version])?;
            if let Some(row)=rows.next()?{let id:String=row.get(0)?;let wrapped:Vec<u8>=row.get(1)?;let _=steve_modules::vault::crypto::unwrap_dek(&key,&ws,&id,version,&wrapped).map_err(|_|anyhow::anyhow!("vault probe: existing data key cannot unwrap"))?;}
        }Ok(())
    })).await.context("vault probe timeout")???;
    drop(db);
    if crate::web::web_version(cfg) != cfg.version || !cfg.web_dir.join("index.html").is_file() {
        bail!("candidate Web version missing or incompatible")
    }
    let version = cfg.version.clone();
    let gate_id = id.clone();
    let app=axum::Router::new().route("/healthz",axum::routing::get(move||{let (version,id)=(version.clone(),gate_id.clone());async move{axum::Json(json!({"ok":true,"version":version,"web_version":version,"upgrade_id":id,"maintenance":true,"build_commit":option_env!("STEVE_BUILD_COMMIT")}))}})).fallback(||async{(axum::http::StatusCode::SERVICE_UNAVAILABLE,"升级验证中，请稍后重试")});
    let (done, rx) = tokio::sync::watch::channel(false);
    let mut servers = tokio::task::JoinSet::new();
    for listener in cfg.listen.bind().await? {
        let (app, mut rx) = (app.clone(), rx.clone());
        servers.spawn(async move {
            axum::serve(listener, app)
                .with_graceful_shutdown(async move {
                    let _ = rx.wait_for(|d| *d).await;
                })
                .await
        });
    }
    drop(rx);
    loop {
        tokio::time::sleep(Duration::from_millis(100)).await;
        if unsafe { libc::getppid() } != supervisor_pid {
            bail!("upgrade supervisor disappeared before commit")
        }
        let op = u.operation(&id)?;
        if op.phase == "committed" {
            break;
        }
        if !matches!(op.phase.as_str(), "restarting" | "verifying") {
            bail!("upgrade gate aborted: {}", op.phase)
        }
    }
    let _ = done.send(true);
    while let Some(r) = servers.join_next().await {
        r??;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;
    fn fixture(version: &str) -> Manifest {
        Manifest {
            format_version: 1,
            version: version.into(),
            commit: "a".repeat(40),
            hub_version: version.into(),
            web_version: version.into(),
            api_protocol: 1,
            schema: Schema {
                min_read: 1,
                max_read: 1,
                target: 1,
            },
            assets: vec![],
        }
    }
    fn bundle(home: &Path, version: &str, broken: bool) -> PathBuf {
        let dir = home.join("releases").join(version);
        fs::create_dir_all(dir.join("bin")).unwrap();
        fs::create_dir_all(dir.join("web/dist")).unwrap();
        atomic(&dir.join("release.json"), &fixture(version)).unwrap();
        let script = if broken {
            "#!/bin/sh\nexit 1\n".to_string()
        } else {
            r#"#!/usr/bin/env python3
import os,json,http.server,threading,time
home=os.environ['STEVE_HOME']; version=os.environ['STEVE_VERSION']; op=os.environ.get('STEVE_UPGRADE_ID')
class Server(http.server.HTTPServer): allow_reuse_address=True
class Handler(http.server.BaseHTTPRequestHandler):
 def log_message(self,*args): pass
 def do_POST(self): self.do_GET()
 def do_DELETE(self): self.do_GET()
 def do_GET(self):
  maintenance=False
  if op:
   with open(home+'/updates/operations/'+op+'.json') as f: maintenance=json.load(f)['phase'] not in ['committed','succeeded']
  body=json.dumps(dict(ok=True,ready=True,version=version,web_version=version,upgrade_id=op,maintenance=maintenance)).encode()
  self.send_response(200);self.send_header('Content-Length',str(len(body)));self.end_headers();self.wfile.write(body)
Server(('127.0.0.1',int(os.environ['STEVE_PORT'])),Handler).serve_forever()
"#.into()
        };
        fs::write(dir.join("bin/steve-hub"), script).unwrap();
        fs::set_permissions(dir.join("bin/steve-hub"), fs::Permissions::from_mode(0o755)).unwrap();
        dir
    }
    async fn scenario(broken: bool) {
        let home = tempfile::tempdir().unwrap();
        let port = std::net::TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port();
        let cfg = Config::from_vars(|k| match k {
            "STEVE_HOME" => Some(home.path().to_string_lossy().into()),
            "STEVE_PORT" => Some(port.to_string()),
            "STEVE_ADAPTER_CLAUDE" | "STEVE_ADAPTER_CODEX" => Some("unused".into()),
            _ => None,
        })
        .unwrap();
        fs::create_dir_all(&cfg.data_dir).unwrap();
        let db = rusqlite::Connection::open(cfg.db_path()).unwrap();
        db.execute_batch(
            "CREATE TABLE evidence(value); INSERT INTO evidence VALUES('before-upgrade');",
        )
        .unwrap();
        drop(db);
        let old = bundle(home.path(), "0.1.0", false);
        let new = bundle(home.path(), "0.2.0", broken);
        switch(home.path(), &old).unwrap();
        let u = Updates {
            home: home.path().into(),
            repository: "Infranite/steve".into(),
            current_hub: "0.1.0".into(),
            current_web: "0.1.0".into(),
            managed: true,
            blocked_reason: None,
            client: reqwest::Client::new(),
            api_base: String::new(),
            web_base: String::new(),
            repo: PathBuf::new(),
            kind: None,
            launch: crate::upgrades::runner::Launch::Launchd,
        };
        let mut op = u.request("0.2.0", "integration", "owner", true).unwrap();
        op.target = Some(new.to_string_lossy().into());
        u.phase(&mut op, "ready", None).unwrap();
        let mut child = spawn(&cfg, &old, None).unwrap();
        wait_health(&cfg, &u, &mut child, "0.1.0", None)
            .await
            .unwrap();
        apply(&cfg, &u, &mut child, &mut op).await.unwrap();
        assert_eq!(op.phase, if broken { "rolled_back" } else { "succeeded" });
        assert_eq!(
            fs::canonicalize(home.path().join("current")).unwrap(),
            fs::canonicalize(if broken { old } else { new }).unwrap()
        );
        let db = rusqlite::Connection::open(cfg.db_path()).unwrap();
        assert_eq!(
            db.query_row("SELECT value FROM evidence", [], |r| r.get::<_, String>(0))
                .unwrap(),
            "before-upgrade"
        );
        stop(&mut child).await.unwrap();
    }
    #[tokio::test]
    async fn real_process_upgrade_commits_pair() {
        scenario(false).await
    }
    #[tokio::test]
    async fn real_process_bad_candidate_restores_database_and_old_pair() {
        scenario(true).await
    }
    #[test]
    fn repeated_restore_preserves_failed_databases() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("db");
        let saved = dir.path().join("saved");
        fs::write(&db, b"bad-new").unwrap();
        fs::write(&saved, b"good-old").unwrap();
        restore(&db, &saved, &dir.path().join("failed")).unwrap();
        restore(&db, &saved, &dir.path().join("failed")).unwrap();
        assert_eq!(fs::read(db).unwrap(), b"good-old");
        assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 4);
    }
}
