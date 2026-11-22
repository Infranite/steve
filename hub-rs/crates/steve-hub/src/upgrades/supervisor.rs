//! The service manager owns this process, which owns the replaceable Hub child.
use super::*;
use std::sync::Arc;
use std::time::Duration;
#[cfg(test)]
use steve_runtime::testkit::process::{AsyncChild as Child, AsyncCommand as Command};
#[cfg(not(test))]
use tokio::process::{Child, Command};
const STOP_GRACE: Duration = Duration::from_secs(15);

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
async fn drain(cfg: &Config, u: &Updates, op: &mut Operation) -> Result<()> {
    maintenance(cfg, u, reqwest::Method::POST).await?;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(120);
    while tokio::time::Instant::now() < deadline {
        let observed = maintenance(cfg, u, reqwest::Method::GET).await?;
        if op.drain.as_ref() != Some(&observed) {
            op.drain = Some(observed.clone());
            u.progress(op, "waiting", None)?;
        }
        if observed["ready"] == true {
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
/// `bundle`'s Hub in the environment a child of this supervisor gets.
fn hub_command(cfg: &Config, bundle: &Path, manifest: &Manifest) -> Command {
    let mut c = Command::new(bundle.join("bin/steve-hub"));
    c.env_remove("STEVE_UPGRADE_CONTROL_TOKEN")
        .env("STEVE_PORT", cfg.listen.port.to_string())
        .env("STEVE_HOST", cfg.listen.env_value())
        .env("STEVE_HOME", &cfg.home)
        .env("STEVE_VERSION", &manifest.version)
        .env("STEVE_WEB_DIR", bundle.join("web/dist"))
        .kill_on_drop(true);
    c
}
/// The candidate's own startup checks (`--check-config`, check.rs) on the environment it would
/// start with, before anything is drained: a setting the new Hub refuses fails the upgrade while
/// the current version keeps serving. A candidate from before `--check-config` is not asked.
pub(super) async fn check_candidate(
    cfg: &Config,
    bundle: &Path,
    manifest: &Manifest,
) -> Result<()> {
    let bin = bundle.join("bin/steve-hub");
    let run = |mut c: Command| async move {
        tokio::time::timeout(Duration::from_secs(30), c.output())
            .await
            .context("candidate check timed out")?
            .context("candidate check")
    };
    let mut info = Command::new(&bin);
    info.arg("--build-info").kill_on_drop(true);
    // no answer here is left to the gate after the switch, which still rolls back
    let asks = match run(info).await {
        Ok(out) => serde_json::from_slice::<Value>(&out.stdout)
            .is_ok_and(|info| info["check_config"] == true),
        Err(_) => false,
    };
    if !asks {
        return Ok(());
    }
    let mut c = hub_command(cfg, bundle, manifest);
    c.arg("--check-config").env_remove("STEVE_UPGRADE_ID");
    let out = run(c).await?;
    if out.status.success() {
        return Ok(());
    }
    let errors = serde_json::from_slice::<Value>(&out.stdout)
        .ok()
        .and_then(|v| v["errors"].as_array().cloned())
        .map(|e| {
            e.iter()
                .filter_map(Value::as_str)
                .collect::<Vec<_>>()
                .join("; ")
        })
        .unwrap_or_else(|| String::from_utf8_lossy(&out.stderr).trim().to_string());
    bail!(
        "{} 按当前环境（steve.env）启动不了，没有排空也没有切换，当前版本照常运行。改好 steve.env 里的以下设置，重启 Steve 服务（托管模式只在启动时读取 steve.env）后再升级：{errors}",
        manifest.version
    )
}
fn spawn(cfg: &Config, bundle: &Path, op: Option<&Operation>) -> Result<HubChild> {
    let manifest: Manifest = read(&bundle.join("release.json"))?;
    let _ = control_token(cfg)?;
    let mut c = hub_command(cfg, bundle, &manifest);
    c.env("STEVE_SUPERVISOR_PID", std::process::id().to_string())
        .env("STEVE_MANAGED", "1")
        .process_group(0);

    if let Some(op) = op {
        c.env("STEVE_UPGRADE_ID", &op.id);
    } else {
        c.env_remove("STEVE_UPGRADE_ID");
    }
    let process = c.spawn()?;
    let group = process.id().context("spawned child has no PID")?;
    Ok(HubChild { process, group })
}
/// Observe exit without releasing the leader's PID (and therefore its process-group number).
fn exited_unreaped(pid: u32, options: i32) -> std::io::Result<bool> {
    loop {
        let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
        let result = unsafe {
            libc::waitid(
                libc::P_PID,
                pid as libc::id_t,
                &mut info,
                libc::WEXITED | libc::WNOWAIT | options,
            )
        };
        if result == 0 {
            #[cfg(target_os = "macos")]
            let observed = info.si_pid;
            #[cfg(not(target_os = "macos"))]
            let observed = unsafe { info.si_pid() };
            return Ok(observed != 0);
        }
        let error = std::io::Error::last_os_error();
        if error.kind() != std::io::ErrorKind::Interrupted {
            return Err(error);
        }
    }
}
/// Must run to completion: do not put this future in a cancellable select/timeout context.
/// The blocking waitid can return only after the child exits and cannot be aborted with the future.
async fn stop(child: &mut HubChild, grace: Duration) -> Result<()> {
    let Some(pid) = child.id() else {
        // A previous wait already released this identity; never signal its cached group.
        return Ok(());
    };
    let group = child.group as i32;
    if !exited_unreaped(pid, libc::WNOHANG)? {
        unsafe {
            libc::kill(-group, libc::SIGTERM);
        }
        // Wait in the blocking pool without polling timers or reaping the leader.
        let mut observed = tokio::task::spawn_blocking(move || exited_unreaped(pid, 0));
        match tokio::time::timeout(grace, &mut observed).await {
            Ok(result) => {
                result??;
            }
            Err(_) => {
                // An observation error must not turn a cached group number into a signal.
                exited_unreaped(pid, libc::WNOHANG)?;
                unsafe {
                    libc::kill(-group, libc::SIGKILL);
                    // The unreaped leader still pins its PID even if it left the recorded group.
                    libc::kill(pid as i32, libc::SIGKILL);
                }
                observed.await??;
            }
        }
    }
    // The live or zombie leader still pins the group. Clear descendants before reaping it.
    unsafe {
        libc::kill(-group, libc::SIGKILL);
    }
    child.wait().await?;
    Ok(())
}

fn saved_snapshot(op: &Operation) -> Result<&crate::backup::SnapshotId> {
    op.backup_snapshot
        .as_ref()
        .context("verified upgrade snapshot missing")
}
async fn healthy(cfg: &Config, client: &reqwest::Client, version: &str, id: Option<&str>) -> bool {
    let host = cfg.listen.local_host();
    let Ok(r) = client
        .get(format!("http://{host}:{}/healthz", cfg.listen.port))
        .timeout(Duration::from_secs(2))
        .send()
        .await
        .and_then(reqwest::Response::error_for_status)
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
/// How often the running Hub is asked whether it still answers, and how many unanswered probes in a
/// row make it hung: it is stopped, and the service manager starts the supervisor (and it) again, as
/// when the Hub exits. Only once it has answered since it started, and never during an upgrade.
const LIVENESS_EVERY: Duration = Duration::from_secs(30);
const LIVENESS_MISSES: u32 = 4;

/// The Hub answers `/healthz` at all (its version and maintenance do not matter here).
async fn answers(cfg: &Config, client: &reqwest::Client) -> bool {
    let host = cfg.listen.local_host();
    client
        .get(format!("http://{host}:{}/healthz", cfg.listen.port))
        .timeout(Duration::from_secs(5))
        .send()
        .await
        .is_ok_and(|r| r.status().is_success())
}

/// Counts unanswered liveness probes; [`Liveness::hung`] once there were too many in a row.
#[derive(Default)]
struct Liveness {
    answered: bool,
    misses: u32,
}

impl Liveness {
    /// Takes one probe's outcome; true when the Hub is to be taken for hung.
    fn probe(&mut self, ok: bool) -> bool {
        if ok {
            self.answered = true;
            self.misses = 0;
        } else if self.answered {
            self.misses += 1;
        }
        self.misses >= LIVENESS_MISSES
    }
}

async fn wait_health(
    cfg: &Config,
    u: &Updates,
    child: &mut HubChild,
    version: &str,
    id: Option<&str>,
) -> Result<()> {
    let mut budget = steve_install::migration::Budget::new(
        id.unwrap_or(""),
        child.group,
        Duration::from_secs(60),
    );
    loop {
        if let Some(status) = child.try_wait()? {
            bail!("candidate exited: {status}")
        };
        budget.check(id.and_then(|id| steve_install::migration::read(&cfg.home, id)))?;
        if healthy(cfg, &u.client, version, id).await {
            return Ok(());
        };
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
}
async fn apply(cfg: &Config, u: &Updates, child: &mut HubChild, op: &mut Operation) -> Result<()> {
    let previous = fs::canonicalize(cfg.home.join("current"))?;
    let target = PathBuf::from(op.target.as_ref().context("staged target missing")?);
    let manifest: Manifest = read(&target.join("release.json"))?;
    super::client::validate_manifest(&manifest, &op.version)?;
    check_candidate(cfg, &target, &manifest).await?;
    op.previous = Some(previous.to_string_lossy().into());
    u.phase(op, "draining", None)?;
    if let Err(e) = drain(cfg, u, op).await {
        let cancel = release_maintenance(cfg, u).await;
        return Err(anyhow::anyhow!(
            "{e:#}; maintenance cancellation: {cancel:?}"
        ));
    }
    u.phase(op, "stopping", None)?;
    stop(child, STOP_GRACE).await?;
    u.phase(op, "backing_up", None)?;
    let copied = (|| {
        let _instance = super::instance_lock(cfg)?;
        crate::backup::create(
            &cfg.home,
            &cfg.db_path(),
            crate::backup::Purpose::PreUpgrade,
        )
    })();
    if let Err(e) = &copied {
        *child = spawn(cfg, &previous, None)?;
        let old: Manifest = read(&previous.join("release.json"))?;
        wait_health(cfg, u, child, &old.version, None).await?;
        release_maintenance(cfg, u).await?;
        return Err(anyhow::anyhow!("{e:#}"));
    }
    op.backup_snapshot = Some(copied?.snapshot_id);
    u.phase(op, "restarting", None)?;
    switch(&cfg.home, &target)?;
    *child = spawn(cfg, &target, Some(op))?;
    u.phase(op, "verifying", None)?;
    if let Err(e) = wait_health(cfg, u, child, &op.version, Some(&op.id)).await {
        rollback(cfg, u, child, op, format!("{e:#}")).await?;
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
    error: String,
) -> Result<()> {
    u.phase(op, "rolling_back", Some(error.clone()))?;
    stop(child, STOP_GRACE).await?;
    {
        let _instance = super::instance_lock(cfg)?;
        crate::backup::restore_database(&cfg.home, &cfg.db_path(), saved_snapshot(op)?)?;
    }
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
                crate::backup::restore_database(&cfg.home, &cfg.db_path(), saved_snapshot(&op)?)?;
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
    let mut live = Liveness::default();
    let mut probed = tokio::time::Instant::now();
    loop {
        tokio::select! {_ = term.recv()=>{stop(&mut child, STOP_GRACE).await?;return Ok(())},_ = tokio::signal::ctrl_c()=>{stop(&mut child, STOP_GRACE).await?;return Ok(())},_ = tokio::time::sleep(Duration::from_millis(500))=>{}}
        if let Some(mut op) = u.active()?
            && op.phase == "ready"
            && let Err(e) = apply(&cfg, &u, &mut child, &mut op).await
        {
            // If we crossed the switch but not commit, make a best-effort rollback under the closed gate.
            if matches!(op.phase.as_str(), "restarting" | "verifying") {
                if let Err(rb) = rollback(&cfg, &u, &mut child, &mut op, format!("{e:#}")).await {
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
        /* a Hub that runs but no longer answers is as gone as one that exited (2026-11-09) */
        if probed.elapsed() >= LIVENESS_EVERY {
            probed = tokio::time::Instant::now();
            if u.active()?.is_none() && live.probe(answers(&cfg, &u.client).await) {
                tracing::error!(
                    "Hub did not answer /healthz {LIVENESS_MISSES} times in a row; stopping it"
                );
                stop(&mut child, STOP_GRACE).await?;
                bail!("Hub stopped answering; supervisor must be restarted by service manager")
            }
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
    let progress = std::sync::Mutex::new(steve_install::migration::Reporter::new(&cfg.home, &id)?);
    let store = steve_store::Store::open_with_migration_progress(
        cfg.db_path(),
        steve_store::NoPublisher,
        Arc::new(move |snapshot| {
            if let Err(error) = progress.lock().unwrap_or_else(|e| e.into_inner()).report(
                snapshot.migration,
                snapshot.target,
                snapshot.vm_steps,
                snapshot.complete,
            ) {
                tracing::warn!(%error, "could not report upgrade migration progress");
            }
        }),
    )?;
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
import os,json,http.server,threading,time,sys,errno
# Build metadata is a CLI query, not a request to start a server. In particular,
# never bind an inherited STEVE_PORT when these tests run inside a live Node.
if '--build-info' in sys.argv:
 print(json.dumps(dict(check_config=False))); sys.exit(0)
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
# the port was chosen before this binds it: say so when something else took it meanwhile
try: server=Server(('127.0.0.1',int(os.environ['STEVE_PORT'])),Handler)
except OSError as e:
 if e.errno==errno.EADDRINUSE: open(home+'/port-taken','w').close()
 raise
server.serve_forever()
"#.into()
        };
        fs::write(dir.join("bin/steve-hub"), script).unwrap();
        fs::set_permissions(dir.join("bin/steve-hub"), fs::Permissions::from_mode(0o755)).unwrap();
        dir
    }
    #[tokio::test]
    async fn exit_observation_keeps_the_leader_waitable() {
        let mut child = Command::new("/bin/sh")
            .args(["-c", "exit 23"])
            .spawn()
            .unwrap();
        let pid = child.id().unwrap();
        steve_runtime::testkit::eventually_sync("leader exited without reaping", || {
            exited_unreaped(pid, libc::WNOHANG).unwrap()
        })
        .await;
        assert!(
            exited_unreaped(pid, libc::WNOHANG).unwrap(),
            "a second observation still sees it"
        );
        assert_eq!(child.wait().await.unwrap().code(), Some(23));
        assert_eq!(
            exited_unreaped(pid, libc::WNOHANG)
                .unwrap_err()
                .raw_os_error(),
            Some(libc::ECHILD)
        );
    }

    #[tokio::test]
    async fn stop_clears_descendants_before_reaping_and_ignores_a_reused_group() {
        let home = tempfile::tempdir().unwrap();
        let ready = home.path().join("descendant");
        let process = Command::new("/bin/sh")
            .args([
                "-c",
                "/bin/sleep 600 & printf '%s' \"$!\" > \"$1\"; exit 0",
                "fixture",
            ])
            .arg(&ready)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .unwrap();
        let group = process.id().unwrap();
        let mut child = HubChild { process, group };
        steve_runtime::testkit::eventually_sync("leader exited with a live descendant", || {
            ready.exists() && exited_unreaped(group, libc::WNOHANG).unwrap()
        })
        .await;
        let descendant: i32 = fs::read_to_string(ready).unwrap().parse().unwrap();
        assert_eq!(unsafe { libc::kill(descendant, 0) }, 0);
        stop(&mut child, Duration::from_millis(50)).await.unwrap();
        assert!(child.id().is_none());
        steve_runtime::testkit::eventually_sync(
            "the old group and its descendant are gone",
            || unsafe { libc::kill(descendant, 0) == -1 && libc::kill(-(group as i32), 0) == -1 },
        )
        .await;
        let mut canary = Command::new("/bin/sleep").arg("600").spawn().unwrap();
        child.group = canary.id().unwrap();
        stop(&mut child, Duration::from_millis(50)).await.unwrap();
        assert!(canary.try_wait().unwrap().is_none());
        canary.kill().await.unwrap();
    }

    #[tokio::test]
    async fn stop_forces_a_term_ignoring_child_outside_the_recorded_group() {
        let home = tempfile::tempdir().unwrap();
        let ready = home.path().join("ready");
        let process = Command::new("/bin/sh")
            .args([
                "-c",
                "trap '' TERM; printf ready > \"$1\"; exec /bin/sleep 600",
                "fixture",
            ])
            .arg(&ready)
            .spawn()
            .unwrap();
        let leader = process.id().unwrap();
        // Model a leader outside the recorded group using another independently owned fixture.
        // Keep that group's leader unreaped too, so this never signals a reusable/fabricated ID.
        let mut former_group = Command::new("/bin/sleep").arg("600").spawn().unwrap();
        let group = former_group.id().unwrap();
        let mut child = HubChild { process, group };
        steve_runtime::testkit::eventually_sync("child ignores TERM", || ready.exists()).await;
        // Test-only deadline: on failure the guarded children drop and release blocking waitid.
        tokio::time::timeout(
            steve_runtime::testkit::EVENTUALLY,
            stop(&mut child, Duration::from_millis(50)),
        )
        .await
        .expect("forced shutdown must finish before the shared test deadline")
        .unwrap();
        assert!(child.id().is_none());
        former_group.wait().await.unwrap();
        steve_runtime::testkit::eventually_sync("forced shutdown removed both groups", || unsafe {
            libc::kill(-(group as i32), 0) == -1 && libc::kill(-(leader as i32), 0) == -1
        })
        .await;
    }

    #[tokio::test]
    async fn health_rejects_http_failure_even_when_maintenance_identity_matches() {
        let home = tempfile::tempdir().unwrap();
        let candidate = bundle(home.path(), "0.2.0", false);
        switch(home.path(), &candidate).unwrap();
        for status in [
            axum::http::StatusCode::SERVICE_UNAVAILABLE,
            axum::http::StatusCode::OK,
        ] {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let port = listener.local_addr().unwrap().port();
            let cfg = Config::from_vars(|key| match key {
                "STEVE_HOME" => Some(home.path().to_string_lossy().into()),
                "STEVE_PORT" => Some(port.to_string()),
                "STEVE_ADAPTER_CLAUDE" | "STEVE_ADAPTER_CODEX" => Some("unused".into()),
                _ => None,
            })
            .unwrap();
            let app = axum::Router::new().route("/healthz", axum::routing::get(move || async move {
                (status, axum::Json(json!({"ok":true,"maintenance":true,"version":"0.2.0","web_version":"0.2.0","upgrade_id":"health-fixture","build_commit":"a".repeat(40)})))
            }));
            let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
            let healthy = healthy(
                &cfg,
                &reqwest::Client::new(),
                "0.2.0",
                Some("health-fixture"),
            )
            .await;
            server.abort();
            assert_eq!(
                healthy,
                status.is_success(),
                "a matching JSON identity cannot make an HTTP failure healthy"
            );
        }
    }
    /// An upgrade between two fixture Hubs, on a port chosen before they bind it: another test
    /// can take it first, or between one child and the next. A fixture that finds it taken says so
    /// (`port-taken`), and the case runs again on another port, three times at most (#429).
    async fn scenario(broken: bool) {
        for _ in 0..3 {
            if scenario_on_a_free_port(broken).await {
                return;
            }
        }
        panic!("the fixtures' port was taken three times");
    }
    /// The case, or `false` when a fixture's port was taken.
    async fn scenario_on_a_free_port(broken: bool) -> bool {
        let home = tempfile::tempdir().unwrap();
        let taken = || home.path().join("port-taken").exists();
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
        let up = wait_health(&cfg, &u, &mut child, "0.1.0", None).await;
        if taken() {
            let _ = stop(&mut child, STOP_GRACE).await;
            return false;
        }
        up.unwrap();
        let applied = apply(&cfg, &u, &mut child, &mut op).await;
        if taken() {
            let _ = stop(&mut child, STOP_GRACE).await;
            return false;
        }
        applied.unwrap();
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
        stop(&mut child, STOP_GRACE).await.unwrap();
        true
    }
    #[tokio::test]
    async fn fixture_build_info_does_not_start_a_server_or_need_runtime_env() {
        let home = tempfile::tempdir().unwrap();
        let dir = bundle(home.path(), "0.2.0", false);
        let out = tokio::time::timeout(
            Duration::from_secs(5),
            Command::new(dir.join("bin/steve-hub"))
                .arg("--build-info")
                .env_remove("STEVE_HOME")
                .env_remove("STEVE_VERSION")
                .env("STEVE_PORT", "must-not-bind")
                .kill_on_drop(true)
                .output(),
        )
        .await
        .expect("metadata must exit without starting a server")
        .unwrap();
        assert!(out.status.success(), "{:?}", out.stderr);
        assert_eq!(
            serde_json::from_slice::<Value>(&out.stdout).unwrap()["check_config"],
            false
        );
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
    fn liveness_counts_misses_only_after_an_answer() {
        let mut l = Liveness::default();
        /* still starting: never answered, never hung */
        for _ in 0..10 {
            assert!(!l.probe(false));
        }
        assert!(!l.probe(true));
        for _ in 1..LIVENESS_MISSES {
            assert!(!l.probe(false));
        }
        /* one answer in between starts again */
        assert!(!l.probe(true));
        for _ in 1..LIVENESS_MISSES {
            assert!(!l.probe(false));
        }
        assert!(
            l.probe(false),
            "hung after {LIVENESS_MISSES} misses in a row"
        );
    }
}
