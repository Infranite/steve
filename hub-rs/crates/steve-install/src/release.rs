//! Signed-install deployment with a durable journal and an explicit business commit boundary.
//! No Python, compiler, source checkout, or live Hub API is needed to inspect its receipts.
use crate::signing::{
    self, Tools, atomic_json, hash, json_file, owned_file, private_dir, successful,
};
use anyhow::{Context, Result, ensure};
use reqwest::Method;
use reqwest::blocking::Client;
use serde_json::{Value, json};
use std::collections::{BTreeMap, HashSet};
use std::fs::{self, File, OpenOptions};
use std::net::{TcpListener, TcpStream, ToSocketAddrs};
use std::os::fd::AsRawFd;
use std::os::unix::fs::OpenOptionsExt;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use steve_store::db::inspect_schema;
use steve_types::backup::{BackupPurpose, BackupReceipt, SnapshotId};

pub struct DeployOutcome {
    pub exit_code: i32,
    pub receipt: Value,
}

/// POSIX record lock, interoperable with the historical Python lockf and zsh zsystem flock.
/// Keep this descriptor open for the complete operation; do not substitute BSD flock.
pub(crate) fn lock_release(home: &Path) -> Result<File> {
    let directory = home.join("releases");
    private_dir(&directory)?;
    let path = directory.join(".release.lock");
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)?;
    let mut lock: libc::flock = unsafe { std::mem::zeroed() };
    lock.l_type = libc::F_WRLCK as libc::c_short;
    lock.l_whence = libc::SEEK_SET as libc::c_short;
    let result = unsafe { libc::fcntl(file.as_raw_fd(), libc::F_SETLK, &lock) };
    ensure!(
        result == 0,
        "another release operation holds releases/.release.lock: {}",
        std::io::Error::last_os_error()
    );
    Ok(file)
}
fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}
fn stamp() -> String {
    let seconds = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as libc::time_t;
    let mut local: libc::tm = unsafe { std::mem::zeroed() };
    unsafe {
        libc::localtime_r(&seconds, &mut local);
    }
    format!(
        "{:04}{:02}{:02}-{:02}{:02}{:02}",
        local.tm_year + 1900,
        local.tm_mon + 1,
        local.tm_mday,
        local.tm_hour,
        local.tm_min,
        local.tm_sec
    )
}
struct Journal {
    path: Option<PathBuf>,
}
impl Journal {
    fn new(home: &Path, operation: Option<&str>) -> Result<Self> {
        let path = operation
            .map(|id| -> Result<PathBuf> {
                let parsed = uuid::Uuid::parse_str(id)?;
                ensure!(
                    parsed.to_string() == id,
                    "operation ID must be a canonical UUID"
                );
                let path = home.join("updates/operations").join(format!("{id}.json"));
                owned_file(&path)?;
                let record = json_file(&path)?;
                ensure!(
                    record["id"] == id
                        && !matches!(
                            record["phase"].as_str(),
                            Some("succeeded" | "failed" | "rolled_back")
                        ),
                    "upgrade operation is mismatched or already terminal"
                );
                Ok(path)
            })
            .transpose()?;
        Ok(Self { path })
    }
    fn update(
        &self,
        phase: &str,
        error: Option<&str>,
        step: Option<&str>,
        fields: Value,
    ) -> Result<()> {
        let Some(path) = &self.path else {
            return Ok(());
        };
        let mut op = json_file(path)?;
        ensure!(op.is_object(), "upgrade journal is not an object");
        let step = step.unwrap_or(match phase {
            "draining" => "waiting",
            "restarting" => "starting",
            "verifying" | "committed" => "checking",
            "succeeded" => "complete",
            "rolling_back" | "rolled_back_pending_health" | "rolled_back" => "rollback",
            "staging" => "resolving",
            "ready" => "preflight",
            _ => phase,
        });
        let fields = fields
            .as_object()
            .context("journal fields must be an object")?;
        if op["phase"] == phase
            && op["error"] == json!(error)
            && op["progress"]["step"] == step
            && fields.iter().all(|(key, value)| op.get(key) == Some(value))
        {
            return Ok(());
        }
        let at = now();
        let started = if op["progress"]["step"] == step {
            op["progress"]["started_at"].as_u64().unwrap_or(at)
        } else {
            at
        };
        op["progress"] = json!({"step":step,"started_at":started,"updated_at":at});
        op["phase"] = phase.into();
        op["error"] = json!(error);
        op["updated_at"] = at.into();
        for (key, value) in fields {
            op[key] = value.clone();
        }
        atomic_json(path, &op)
    }
}

pub fn check_schema(db: &Path, release: &Path) -> Result<()> {
    let needed = inspect_schema(db)?.min_reader;
    if needed == 0 {
        return Ok(());
    }
    let manifest = json_file(&release.join("release.json"))?;
    let known = manifest["schema"]["target"]
        .as_i64()
        .context("release does not declare its supported schema")?;
    ensure!(
        known >= needed,
        "release knows schema {known}, but database requires at least {needed}; nothing was changed"
    );
    Ok(())
}

fn read_text(path: &Path) -> std::io::Result<String> {
    fs::read_to_string(path)
}

fn environment(home: &Path) -> Result<BTreeMap<String, String>> {
    let mut values = BTreeMap::new();
    for key in ["HOME", "PATH", "USER", "TMPDIR"] {
        if let Ok(value) = std::env::var(key) {
            values.insert(key.into(), value);
        }
    }
    values.insert("STEVE_HOME".into(), home.to_string_lossy().into());
    let path = home.join("steve.env");
    if path.exists() {
        owned_file(&path)?;
        let raw = read_text(&path)?;
        let parsed = crate::env_file::parse_runtime_env(raw.as_bytes(), &values)?;
        values.extend(parsed);
    }
    values.insert("STEVE_HOME".into(), home.to_string_lossy().into());
    values
        .entry("STEVE_KEYHELPER".into())
        .or_insert_with(|| home.join("bin/steve-keyhelper").to_string_lossy().into());
    Ok(values)
}
// Keep the same selection as Hub Config::Listen::local_host without a crate cycle:
// prefer an explicitly listed loopback and map wildcard listeners to loopback.
fn local_host(raw: &str) -> String {
    let hosts: Vec<_> = raw
        .split(',')
        .map(str::trim)
        .filter(|host| !host.is_empty())
        .map(|host| {
            host.strip_prefix('[')
                .and_then(|host| host.strip_suffix(']'))
                .unwrap_or(host)
        })
        .collect();
    let host = hosts
        .iter()
        .copied()
        .find(|host| {
            *host == "localhost"
                || host
                    .parse::<std::net::IpAddr>()
                    .is_ok_and(|ip| ip.is_loopback())
        })
        .or_else(|| hosts.first().copied())
        .unwrap_or("127.0.0.1");
    match host {
        "0.0.0.0" => "127.0.0.1".into(),
        "::" => "[::1]".into(),
        host if host.contains(':') => format!("[{host}]"),
        host => host.into(),
    }
}

struct Runtime {
    home: PathBuf,
    env: BTreeMap<String, String>,
    client: Client,
    address: String,
    tools: Tools,
    launchctl: PathBuf,
    service_home: PathBuf,
    health_timeout: Duration,
    #[cfg(test)]
    simulated_service: bool,
}
impl Runtime {
    fn new(home: &Path) -> Result<Self> {
        let tools = Tools::default();
        let env = environment(home)?;
        let port: u16 = std::env::var("STEVE_PORT")
            .ok()
            .or_else(|| env.get("STEVE_PORT").cloned())
            .unwrap_or_else(|| "8440".into())
            .parse()?;
        ensure!(port != 0, "Hub port must be nonzero");
        let host = std::env::var("STEVE_HOST")
            .ok()
            .or_else(|| env.get("STEVE_HOST").cloned())
            .unwrap_or_default();
        let address = format!("{}:{port}", local_host(&host));
        let account = unsafe { libc::getpwuid(libc::getuid()) };
        ensure!(
            !account.is_null(),
            "cannot establish the installation OS account"
        );
        let service_home =
            PathBuf::from(unsafe { std::ffi::CStr::from_ptr((*account).pw_dir) }.to_str()?);
        Ok(Self {
            home: home.to_owned(),
            env,
            client: Client::builder()
                .no_proxy()
                .redirect(reqwest::redirect::Policy::none())
                .timeout(Duration::from_secs(10))
                .build()?,
            address,
            tools,
            launchctl: "/bin/launchctl".into(),
            service_home,
            health_timeout: Duration::from_secs(60),
            #[cfg(test)]
            simulated_service: false,
        })
    }
    fn control(&self, method: Method) -> Result<Value> {
        let path = self.home.join("updates/control-token.json");
        if fs::symlink_metadata(&path).is_err() {
            private_dir(path.parent().unwrap())?;
            let token = json!(format!(
                "{}{}",
                uuid::Uuid::new_v4().simple(),
                uuid::Uuid::new_v4().simple()
            ));
            if let Err(error) = signing::exclusive_json(&path, &token) {
                if !path.is_file() {
                    return Err(error);
                }
            }
        }
        owned_file(&path)?;
        let token = json_file(&path)?;
        Ok(self
            .client
            .request(
                method,
                format!("http://{}/internal/upgrade/drain", self.address),
            )
            .bearer_auth(token.as_str().context("control token is not a string")?)
            .send()?
            .error_for_status()?
            .json()?)
    }
    fn health(&self) -> Option<Value> {
        self.client
            .get(format!("http://{}/healthz", self.address))
            .timeout(Duration::from_secs(2))
            .send()
            .ok()?
            .error_for_status()
            .ok()?
            .json()
            .ok()
    }
    fn wait_health(&self, check: impl Fn(&Value) -> bool) -> Result<Value> {
        let deadline = Instant::now() + self.health_timeout;
        loop {
            if let Some(health) = self.health() {
                if check(&health) {
                    return Ok(health);
                }
            }
            ensure!(
                Instant::now() < deadline,
                "Hub did not answer with the expected health identity"
            );
            std::thread::sleep(Duration::from_millis(200));
        }
    }
    fn lift(&self) -> Result<()> {
        let deadline = Instant::now() + self.health_timeout * 2;
        loop {
            if self.control(Method::DELETE).is_ok()
                && self
                    .control(Method::GET)
                    .is_ok_and(|v| v["maintenance"] == false)
            {
                return Ok(());
            }
            ensure!(
                Instant::now() < deadline,
                "maintenance drain could not be lifted"
            );
            std::thread::sleep(Duration::from_millis(200));
        }
    }
    fn service(&self, action: &str, ident: &Value) -> Result<()> {
        let label = ident["label"].as_str().context("service label missing")?;
        let plist = self
            .service_home
            .join("Library/LaunchAgents")
            .join(format!("{label}.plist"));
        self.service_at(action, label, &plist)
    }
    fn service_at(&self, action: &str, label: &str, plist: &Path) -> Result<()> {
        let domain = format!("gui/{}", unsafe { libc::getuid() });
        let mut command = Command::new(&self.launchctl);
        if action == "stop" {
            command.args(["bootout", &format!("{domain}/{label}")]);
        } else {
            command.args(["bootstrap", &domain]).arg(plist);
        }
        successful(command.stdin(Stdio::null()), Duration::from_secs(30))?;
        if action == "stop" {
            #[cfg(test)]
            if self.simulated_service {
                return Ok(());
            }
            let deadline = Instant::now() + self.health_timeout.min(Duration::from_secs(15));
            let addresses: Vec<_> = self.address.to_socket_addrs()?.collect();
            ensure!(!addresses.is_empty(), "local Hub address did not resolve");
            while addresses.iter().any(|address| {
                TcpStream::connect_timeout(address, Duration::from_millis(100)).is_ok()
            }) {
                ensure!(
                    Instant::now() < deadline,
                    "old Hub still listens after bootout"
                );
                std::thread::sleep(Duration::from_millis(100));
            }
        }
        Ok(())
    }
    fn hub_command(&self, candidate: &Path) -> Command {
        let mut command = Command::new(candidate.join("bin/steve-hub"));
        command
            .env_clear()
            .envs(&self.env)
            .env("STEVE_WEB_DIR", candidate.join("web/dist"));
        command
    }
    fn check_config(&self, candidate: &Path, info: &Value) -> Result<()> {
        if info["check_config"] != true {
            return Ok(());
        }
        let mut command = self.hub_command(candidate);
        command.arg("--check-config").stdin(Stdio::null());
        let output = signing::output(&mut command, Duration::from_secs(120))?;
        let report: Value = serde_json::from_slice(&output.stdout)
            .context("candidate returned no configuration report")?;
        ensure!(
            output.status.success() && report["ok"] == true,
            "candidate configuration check failed; current Hub has not been drained or stopped"
        );
        Ok(())
    }
    fn probe(&self, candidate: &Path, version: &str) -> Result<Value> {
        let id = uuid::Uuid::new_v4();
        let operations = self.home.join("updates/operations");
        private_dir(&operations)?;
        let operation = operations.join(format!("{id}.json"));
        signing::exclusive_json(
            &operation,
            &json!({"id":id.to_string(),"idempotency_key":format!("local-release-probe-{version}"),"version":version,"phase":"verifying","created_at":0,"updated_at":0,"requested_by":"local-release","error":null,"previous":null,"target":candidate}),
        )?;
        let port = TcpListener::bind("127.0.0.1:0")?.local_addr()?.port();
        let log_dir = self.home.join("logs/release");
        private_dir(&log_dir)?;
        let log = OpenOptions::new()
            .create_new(true)
            .write(true)
            .mode(0o600)
            .open(log_dir.join(format!("probe-{version}-{id}.log")))?;
        let mut command = self.hub_command(candidate);
        command
            .env("STEVE_PORT", port.to_string())
            .env("STEVE_HOST", "127.0.0.1")
            .env("STEVE_UPGRADE_ID", id.to_string())
            .env("STEVE_NO_RESUME", "1")
            .env("STEVE_REPO", self.home.join("probe-unused-repo"))
            .env("STEVE_ADAPTER_CLAUDE", "/usr/bin/false")
            .env("STEVE_ADAPTER_CODEX", "/usr/bin/false")
            .env("CLAUDE_CODE_EXECUTABLE", "/usr/bin/false")
            .stdin(Stdio::null())
            .stdout(log.try_clone()?)
            .stderr(log)
            .process_group(0);
        let result = (|| -> Result<Value> {
            let mut process = ProbeProcess(command.spawn()?);
            let mut budget =
                crate::migration::Budget::new(&id.to_string(), process.0.id(), self.health_timeout);
            loop {
                ensure!(
                    process.0.try_wait()?.is_none(),
                    "candidate exited during its maintenance probe"
                );
                budget.check(crate::migration::read(&self.home, &id.to_string()))?;
                let health = self
                    .client
                    .get(format!("http://127.0.0.1:{port}/healthz"))
                    .timeout(Duration::from_secs(2))
                    .send()
                    .and_then(|r| r.error_for_status())
                    .and_then(|r| r.json::<Value>());
                if let Ok(health) = health {
                    if health["maintenance"] == true {
                        ensure!(
                            health["version"] == version && health["web_version"] == version,
                            "maintenance probe identity mismatch"
                        );
                        let response = self
                            .client
                            .get(format!("http://127.0.0.1:{port}/api/summary"))
                            .timeout(Duration::from_secs(2))
                            .send()?;
                        ensure!(
                            response.status().as_u16() == 503,
                            "business is reachable during the maintenance probe"
                        );
                        return Ok(health);
                    }
                }
                std::thread::sleep(Duration::from_millis(200));
            }
        })();
        let _ = fs::remove_file(operation);
        result
    }
}
struct ProbeProcess(Child);
impl Drop for ProbeProcess {
    fn drop(&mut self) {
        let group = self.0.id() as i32;
        unsafe {
            libc::kill(-group, libc::SIGTERM);
        }
        let until = Instant::now() + Duration::from_secs(10);
        while self.0.try_wait().ok().flatten().is_none() && Instant::now() < until {
            std::thread::sleep(Duration::from_millis(20));
        }
        unsafe {
            libc::kill(-group, libc::SIGKILL);
        }
        let _ = self.0.wait();
    }
}
/// Take the pre-upgrade snapshot with the verified candidate, after stopping the running Hub.
fn backup(runtime: &Runtime, candidate: &Path) -> Result<SnapshotId> {
    let mut command = runtime.hub_command(candidate);
    command
        .args(["--admin", "backup", "--reason", "pre-upgrade", "--offline"])
        .stdin(Stdio::null());
    let output = signing::output(&mut command, Duration::from_secs(120))?;
    ensure!(
        output.status.success(),
        "candidate backup failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let receipt: BackupReceipt = serde_json::from_slice(&output.stdout)
        .context("candidate returned no verified backup receipt")?;
    ensure!(
        receipt.purpose == BackupPurpose::PreUpgrade
            && receipt.database.bytes > 0
            && receipt.database.sha256.len() == 64
            && receipt
                .database
                .sha256
                .bytes()
                .all(|b| b.is_ascii_hexdigit()),
        "candidate backup is incomplete or unverified"
    );
    Ok(receipt.snapshot_id)
}
fn link(path: &Path, target: &Path) -> Result<()> {
    private_dir(path.parent().context("link parent missing")?)?;
    let temporary = path.with_file_name(format!(".deploy-{}", uuid::Uuid::new_v4()));
    std::os::unix::fs::symlink(target, &temporary)?;
    if let Err(error) = fs::rename(&temporary, path) {
        let _ = fs::remove_file(temporary);
        return Err(error.into());
    }
    File::open(path.parent().unwrap())?.sync_all()?;
    Ok(())
}
#[derive(Default)]
struct Changed {
    drain: bool,
    stopped: bool,
    probed: bool,
    hub_link: bool,
    web_link: bool,
    service_definition: Option<crate::service::ServiceDefinitionBackup>,
    started: bool,
}
fn restore(
    runtime: &Runtime,
    changed: &Changed,
    old_hub: &Path,
    old_web: &Path,
    old_manifest: &Value,
    saved: Option<&SnapshotId>,
    candidate: &Path,
    ident: &Value,
) -> (bool, Vec<String>) {
    let mut steps = Vec::new();
    let mut ok = true;
    let mut step = |name: &str, result: Result<()>| match result {
        Ok(()) => steps.push(name.to_owned()),
        Err(error) => {
            ok = false;
            steps.push(format!("{name} failed: {error:#}"));
        }
    };
    if let Some(backup) = &changed.service_definition {
        step(
            "service definition restored",
            crate::service::restore_service_definition(backup),
        );
    }
    if changed.web_link {
        step(
            "web/current restored",
            link(&runtime.home.join("web/current"), old_web),
        );
    }
    if changed.hub_link {
        step(
            "current restored",
            link(&runtime.home.join("current"), old_hub),
        );
    }
    if changed.probed {
        step(
            "database backup restored; probed files retained separately",
            (|| -> Result<()> {
                let saved = saved.context("verified backup missing")?;
                let mut command = runtime.hub_command(candidate);
                command
                    .args(["--admin", "restore", saved.as_str(), "--database-only"])
                    .stdin(Stdio::null());
                let output = signing::output(&mut command, Duration::from_secs(3600))?;
                ensure!(
                    output.status.success(),
                    "candidate restore failed: {}",
                    String::from_utf8_lossy(&output.stderr)
                );
                ensure!(
                    String::from_utf8_lossy(&output.stdout).trim() == saved.as_str(),
                    "candidate returned the wrong restored snapshot"
                );
                Ok(())
            })(),
        );
    }
    if !ok && changed.probed {
        steps.push("service not started: database recovery needs operator inspection".into());
        return (false, steps);
    }
    if changed.stopped && runtime.health().is_none() {
        match runtime.service("start", ident) {
            Ok(()) => steps.push("old service started".into()),
            Err(error) => {
                ok = false;
                steps.push(format!("old service start failed: {error:#}"));
            }
        }
    }
    if runtime
        .wait_health(|h| {
            h["version"] == old_manifest["version"]
                && (h["build_commit"].is_null() || h["build_commit"] == old_manifest["commit"])
        })
        .is_err()
    {
        steps.push("old Hub did not answer as its original identity".into());
        return (false, steps);
    }
    if changed.drain {
        match runtime.lift() {
            Ok(()) => steps.push("drain lifted".into()),
            Err(error) => {
                ok = false;
                steps.push(format!("drain lift failed: {error:#}"));
            }
        }
    }
    (ok, steps)
}

pub fn deploy(
    home: &Path,
    candidate: &Path,
    operation: Option<&str>,
    drain_timeout: Duration,
) -> Result<DeployOutcome> {
    ensure!(
        cfg!(target_os = "macos"),
        "signed local deployment requires macOS"
    );
    let (home, candidate, operation) = (
        home.to_owned(),
        candidate.to_owned(),
        operation.map(str::to_owned),
    );
    // reqwest blocking owns a runtime; isolate it from a calling Hub's Tokio runtime.
    std::thread::spawn(move || {
        let _lock = lock_release(&home)?;
        let journal = Journal::new(&home, operation.as_deref())?;
        let result = (|| { let runtime = Runtime::new(&home)?; deploy_with(&runtime, &candidate, &journal, drain_timeout) })();
        if let Err(error) = &result {
            let terminal = journal.path.as_ref().and_then(|p| json_file(p).ok()).is_some_and(|v| matches!(v["phase"].as_str(), Some("succeeded" | "failed" | "rolled_back")));
            if !terminal { let _ = journal.update("failed", Some(&format!("Native deployment interrupted: {error:#}; inspect current and the recorded backup before retrying")), None, json!({})); }
        }
        result
    }).join().map_err(|_| anyhow::anyhow!("native deployment worker stopped"))?
}
fn deploy_with(
    runtime: &Runtime,
    candidate: &Path,
    journal: &Journal,
    drain_timeout: Duration,
) -> Result<DeployOutcome> {
    let home = &runtime.home;
    let ident = signing::identity(home, &runtime.tools)?;
    let manifest = json_file(&candidate.join("release.json"))?;
    let version = manifest["version"]
        .as_str()
        .context("candidate version missing")?;
    let commit = manifest["commit"]
        .as_str()
        .context("candidate commit missing")?;
    let info = signing::validate_bundle(home, candidate, version, commit)?;
    signing::verify_signature(
        &candidate.join("bin/steve-hub"),
        ident["hub_identifier"].as_str().unwrap(),
        &runtime.tools,
    )?;
    let old_hub = fs::canonicalize(home.join("current"))?;
    let old_web = fs::canonicalize(home.join("web/current"))
        .or_else(|_| fs::canonicalize(old_hub.join("web/dist")))?;
    ensure!(old_hub != candidate, "candidate is already current");
    let db = home.join("data/steve.sqlite3");
    check_schema(&db, candidate)?;
    runtime.check_config(candidate, &info)?;
    let stamp = stamp();
    let mut receipt = json!({"version":version,"release":candidate,"previous":old_hub,"previous_web":old_web,"db_before":inspect_schema(&db)?,"started":stamp});
    let logs = home.join("logs/release");
    private_dir(&logs)?;
    let result_path = logs.join(format!("local-{version}-{stamp}.json"));
    let helper_hash = hash(&home.join("bin/steve-keyhelper"))?;
    let old_manifest = json_file(&old_hub.join("release.json"))?;
    let mut changed = Changed::default();
    let mut phase = "drain";
    let mut saved = None;
    let transaction = (|| -> Result<Value> {
        journal.update("draining", None, None, json!({"target":candidate}))?;
        changed.drain = true;
        runtime.control(Method::POST)?;
        let until = Instant::now() + drain_timeout;
        loop {
            let state = runtime.control(Method::GET)?;
            journal.update("draining", None, None, json!({"drain":state}))?;
            if state["ready"] == true {
                break;
            }
            ensure!(
                Instant::now() < until,
                "Hub did not drain before its deadline"
            );
            std::thread::sleep(Duration::from_millis(200));
        }
        phase = "backup";
        journal.update("stopping", None, None, json!({}))?;
        changed.stopped = true;
        runtime.service("stop", &ident)?;
        journal.update("backing_up", None, None, json!({}))?;
        let snapshot = backup(runtime, candidate)?;
        receipt["backup_snapshot"] = json!(snapshot);
        atomic_json(&result_path, &receipt)?;
        journal.update(
            "backing_up",
            None,
            None,
            json!({"backup_snapshot":snapshot}),
        )?;
        saved = Some(snapshot);
        phase = "probe";
        changed.probed = true;
        journal.update("backing_up", None, Some("probing"), json!({}))?;
        runtime.probe(candidate, version)?;
        phase = "switch";
        journal.update("restarting", None, None, json!({"previous":old_hub}))?;
        // Rename can succeed before its parent fsync fails. Treat an attempted switch as
        // possibly applied so pre-business recovery always restores both original links.
        changed.hub_link = true;
        link(&home.join("current"), candidate)?;
        changed.web_link = true;
        link(&home.join("web/current"), &candidate.join("web/dist"))?;
        let label = ident["label"].as_str().context("service label missing")?;
        let plist = runtime
            .service_home
            .join("Library/LaunchAgents")
            .join(format!("{label}.plist"));
        changed.service_definition =
            crate::service::sync_hub_launchd_plist(&plist, home, candidate)?;
        // From here the new process may run business, even if launchctl returns an error.
        changed.started = true;
        runtime.service("start", &ident)?;
        journal.update("verifying", None, None, json!({}))?;
        runtime.wait_health(|h| h["version"] == version && h["build_commit"] == commit)?;
        journal.update("committed", None, Some("checking"), json!({}))?;
        runtime.lift()?;
        runtime.wait_health(|h| {
            h["version"] == version
                && h["web_version"] == version
                && h["drain"] != true
                && h["recovery"]["state"] != "failed"
        })
    })();
    let exit_code = match transaction {
        Ok(health) => {
            receipt["ok"] = true.into();
            receipt["health"] = health;
            receipt["db_after"] = json!(inspect_schema(&db)?);
            receipt["finished"] = self::stamp().into();
            if hash(&home.join("bin/steve-keyhelper"))? != helper_hash {
                receipt["warning"] = "installed keyhelper changed during deployment".into();
            }
            atomic_json(&result_path, &receipt)?;
            journal.update("succeeded", None, None, json!({}))?;
            match prune_unlocked(home, std::slice::from_ref(&old_hub), false) {
                Ok(paths) => receipt["pruned"] = json!(paths),
                Err(error) => receipt["pruned_error"] = format!("{error:#}").into(),
            }
            0
        }
        Err(error) if changed.started => {
            let message = format!(
                "Upgrade committed; new database retained and automatic rollback refused: {error:#}. Verified backup: {}",
                saved.as_ref().map_or("none", SnapshotId::as_str)
            );
            receipt["ok"] = false.into();
            receipt["degraded"] = true.into();
            receipt["phase"] = phase.into();
            receipt["error"] = message.clone().into();
            receipt["db_after"] = inspect_schema(&db).map_or(Value::Null, |state| json!(state));
            receipt["error_code"] = "committed_startup_failed".into();
            journal.update(
                "failed",
                Some(&message),
                None,
                json!({"error_code": "committed_startup_failed"}),
            )?;
            3
        }
        Err(error) => {
            let (recovered, steps) = restore(
                runtime,
                &changed,
                &old_hub,
                &old_web,
                &old_manifest,
                saved.as_ref(),
                candidate,
                &ident,
            );
            let message = format!("{phase} failed: {error:#}; recovery: {}", steps.join("; "));
            receipt["ok"] = false.into();
            receipt["phase"] = phase.into();
            receipt["error"] = message.clone().into();
            receipt["recovered"] = recovered.into();
            receipt["recovery"] = json!(steps);
            journal.update(
                if recovered && changed.stopped {
                    "rolled_back"
                } else {
                    "failed"
                },
                Some(&message),
                None,
                json!({}),
            )?;
            if recovered { 1 } else { 2 }
        }
    };
    if let Err(error) = atomic_json(&result_path, &receipt) {
        receipt["receipt_write_error"] = format!("{error:#}").into();
    }
    Ok(DeployOutcome { exit_code, receipt })
}

pub fn prune(home: &Path, keep: &[PathBuf], dry_run: bool) -> Result<Vec<PathBuf>> {
    let _lock = lock_release(home)?;
    let mut protected = keep.to_vec();
    if let Some(previous) = last_previous(home) {
        protected.push(previous);
    }
    if let Some(previous) = last_macos_rollback_previous(home) {
        protected.push(previous);
    }
    prune_unlocked(home, &protected, dry_run)
}

fn last_macos_rollback_previous(home: &Path) -> Option<PathBuf> {
    let path = home.join("releases/.macos-rollback.json");
    let value = json_file(&path).ok()?;
    value["previous_hub"].as_str().map(PathBuf::from)
}

fn validate_script_free_bundle(candidate: &Path) -> Result<()> {
    for path in crate::common::regular_tree(candidate)? {
        let rel = path.strip_prefix(candidate)?;
        let in_ops = rel
            .components()
            .next()
            .is_some_and(|c| c.as_os_str() == "ops");
        let is_script = rel
            .extension()
            .and_then(|e| e.to_str())
            .is_some_and(|ext| matches!(ext, "py" | "sh"));
        ensure!(
            !in_ops && !is_script,
            "release bundle contains forbidden runtime script: {}",
            rel.display()
        );
    }
    Ok(())
}

fn restore_snapshot(runtime: &Runtime, helper_release: &Path, saved: &SnapshotId) -> Result<()> {
    let mut command = runtime.hub_command(helper_release);
    command
        .args(["--admin", "restore", saved.as_str(), "--database-only"])
        .stdin(Stdio::null());
    let output = signing::output(&mut command, Duration::from_secs(3600))?;
    ensure!(
        output.status.success(),
        "candidate restore failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    ensure!(
        String::from_utf8_lossy(&output.stdout).trim() == saved.as_str(),
        "candidate returned the wrong restored snapshot"
    );
    Ok(())
}

/// One-shot macOS migration/upgrade driven purely by the Rust binary inside the new release
/// package: validates the script-free bundle, stages it into `<home>/releases/`, drains/stops the
/// prior Hub, backs up SQLite, switches `current` + `web/current`, rewrites the legacy LaunchAgent
/// plist to invoke `steve-hub` directly, starts launchd, and probes `/healthz`. Any failure during
/// switch, plist rewrite, service start, or health probe automatically rolls back to the prior
/// (e.g. v0.12.0) release and plist.
pub fn upgrade_macos(
    home: &Path,
    candidate: &Path,
    label_override: Option<&str>,
    drain_timeout: Duration,
    signing_password: Option<&str>,
) -> Result<DeployOutcome> {
    ensure!(
        cfg!(target_os = "macos"),
        "macOS one-shot service upgrade requires macOS"
    );
    let (home, candidate, label_override, signing_password) = (
        home.to_owned(),
        candidate.to_owned(),
        label_override.map(str::to_owned),
        signing_password.map(str::to_owned),
    );
    std::thread::spawn(move || {
        let _lock = lock_release(&home)?;
        let runtime = Runtime::new(&home)?;
        upgrade_macos_with(
            &runtime,
            &candidate,
            label_override.as_deref(),
            drain_timeout,
            signing_password.as_deref(),
        )
    })
    .join()
    .map_err(|_| anyhow::anyhow!("macOS upgrade worker stopped"))?
}

fn upgrade_macos_with(
    runtime: &Runtime,
    candidate: &Path,
    label_override: Option<&str>,
    drain_timeout: Duration,
    signing_password: Option<&str>,
) -> Result<DeployOutcome> {
    let home = &runtime.home;
    validate_script_free_bundle(candidate)?;
    let manifest = json_file(&candidate.join("release.json"))?;
    ensure!(
        manifest["format_version"] == 1,
        "unsupported release manifest format_version"
    );
    let version = manifest["version"]
        .as_str()
        .context("candidate version missing")?;
    let commit = manifest["commit"]
        .as_str()
        .context("candidate commit missing")?;
    let old_hub = fs::canonicalize(home.join("current"))
        .context("current release symlink is missing; cannot establish rollback target")?;
    let old_manifest = json_file(&old_hub.join("release.json"))?;
    let old_web = fs::canonicalize(home.join("web/current"))
        .or_else(|_| fs::canonicalize(old_hub.join("web/dist")))?;
    let (label, plist_path) =
        crate::service::hub_launchd_plist_path(&runtime.service_home, home, label_override)?;
    ensure!(
        crate::common::exists(&plist_path) && plist_path.is_file(),
        "LaunchAgent plist not found at {}",
        plist_path.display()
    );
    let db = home.join("data/steve.sqlite3");
    check_schema(&db, candidate)?;

    let staged = crate::service::stage_release_bundle(home, candidate, version, commit)?;
    ensure!(old_hub != staged, "candidate is already current");
    let info = signing::validate_bundle(home, &staged, version, commit)?;
    runtime.check_config(&staged, &info)?;

    if home.join("signing/identity.json").is_file() {
        let ident = signing::identity(home, &runtime.tools)?;
        let hub_id = ident["hub_identifier"]
            .as_str()
            .context("signing hub_identifier missing")?;
        if signing::verify_signature(&staged.join("bin/steve-hub"), hub_id, &runtime.tools).is_err()
        {
            let password = signing_password.context(
                "signed macOS installation requires the signing keychain password (pass --keychain-pass-stdin or unlock steve-keyhelper)",
            )?;
            signing::prepare_bundle(home, &staged, version, commit, password)?;
            signing::verify_signature(&staged.join("bin/steve-hub"), hub_id, &runtime.tools)?;
        }
    }

    let stamp = stamp();
    let logs = home.join("logs/release");
    private_dir(&logs)?;
    let result_path = logs.join(format!("macos-upgrade-{version}-{stamp}.json"));
    let mut receipt = json!({
        "version": version,
        "release": staged,
        "previous": old_hub,
        "previous_web": old_web,
        "label": label,
        "plist": plist_path,
        "db_before": inspect_schema(&db)?,
        "started": stamp,
    });

    let mut drained = false;
    let mut stopped = false;
    let mut hub_link = false;
    let mut web_link = false;
    let mut plist_backup: Option<crate::service::ServiceDefinitionBackup> = None;
    let mut started_new = false;
    let mut saved: Option<SnapshotId> = None;
    let mut phase = "drain";

    let transaction = (|| -> Result<Value> {
        let was_running = runtime.health().is_some();
        if was_running {
            if home.join("updates/control-token.json").is_file()
                && runtime.control(Method::POST).is_ok()
            {
                drained = true;
                let until = Instant::now() + drain_timeout;
                loop {
                    let state = runtime.control(Method::GET)?;
                    if state["ready"] == true {
                        break;
                    }
                    ensure!(
                        Instant::now() < until,
                        "Hub did not drain before its deadline"
                    );
                    std::thread::sleep(Duration::from_millis(200));
                }
            }
            phase = "stop";
            stopped = true;
            runtime.service_at("stop", &label, &plist_path)?;
        }
        if db.is_file() {
            phase = "backup";
            let snapshot = backup(runtime, &staged)?;
            receipt["backup_snapshot"] = json!(snapshot);
            atomic_json(&result_path, &receipt)?;
            saved = Some(snapshot);
        }
        phase = "switch";
        hub_link = true;
        link(&home.join("current"), &staged)?;
        web_link = true;
        link(&home.join("web/current"), &staged.join("web/dist"))?;
        phase = "plist";
        plist_backup = crate::service::sync_hub_launchd_plist(&plist_path, home, &staged)?;
        let rollback_meta = json!({
            "version": version,
            "candidate": staged,
            "previous_hub": old_hub,
            "previous_web": old_web,
            "previous_manifest": old_manifest,
            "label": label,
            "plist_path": plist_path,
            "plist_backup_path": crate::service::previous_service_definition_path(&plist_path),
            "backup_snapshot": saved,
            "started": stamp,
        });
        atomic_json(&home.join("releases/.macos-rollback.json"), &rollback_meta)?;
        phase = "start";
        started_new = true;
        runtime.service_at("start", &label, &plist_path)?;
        phase = "health";
        let health = runtime.wait_health(|h| {
            h["version"] == version
                && (h["build_commit"].is_null() || h["build_commit"] == commit)
                && (h["web_version"].is_null() || h["web_version"] == version)
                && h["recovery"]["state"] != "failed"
        })?;
        if health["drain"] == true {
            runtime.lift()?;
            runtime.wait_health(|h| {
                h["version"] == version
                    && (h["web_version"].is_null() || h["web_version"] == version)
                    && h["drain"] != true
                    && h["recovery"]["state"] != "failed"
            })
        } else {
            Ok(health)
        }
    })();

    let exit_code = match transaction {
        Ok(health) => {
            receipt["ok"] = true.into();
            receipt["plist_rewritten"] = plist_backup.is_some().into();
            receipt["health"] = health;
            receipt["db_after"] = json!(inspect_schema(&db)?);
            receipt["finished"] = self::stamp().into();
            match prune_unlocked(home, std::slice::from_ref(&old_hub), false) {
                Ok(paths) => receipt["pruned"] = json!(paths),
                Err(error) => receipt["pruned_error"] = format!("{error:#}").into(),
            }
            0
        }
        Err(error) => {
            let mut steps = Vec::new();
            let mut ok = true;
            fn record_step(ok: &mut bool, steps: &mut Vec<String>, name: &str, result: Result<()>) {
                match result {
                    Ok(()) => steps.push(name.to_owned()),
                    Err(err) => {
                        *ok = false;
                        steps.push(format!("{name} failed: {err:#}"));
                    }
                }
            }
            if started_new {
                let _ = runtime.service_at("stop", &label, &plist_path);
                steps.push("failed candidate stopped".into());
            }
            if let Some(backup_def) = &plist_backup {
                record_step(
                    &mut ok,
                    &mut steps,
                    "LaunchAgent plist restored",
                    crate::service::restore_service_definition(backup_def),
                );
            }
            if web_link {
                record_step(
                    &mut ok,
                    &mut steps,
                    "web/current restored",
                    link(&home.join("web/current"), &old_web),
                );
            }
            if hub_link {
                record_step(
                    &mut ok,
                    &mut steps,
                    "current restored",
                    link(&home.join("current"), &old_hub),
                );
            }
            if let Some(snapshot) = saved.as_ref()
                && started_new
            {
                record_step(
                    &mut ok,
                    &mut steps,
                    "database backup restored",
                    restore_snapshot(runtime, &staged, snapshot),
                );
            }
            if stopped {
                if ok {
                    record_step(
                        &mut ok,
                        &mut steps,
                        "previous service started",
                        runtime.service_at("start", &label, &plist_path),
                    );
                    if ok {
                        match runtime.wait_health(|h| {
                            h["version"] == old_manifest["version"]
                                && (h["build_commit"].is_null()
                                    || h["build_commit"] == old_manifest["commit"])
                        }) {
                            Ok(old_health) => {
                                steps.push("previous Hub healthy".into());
                                if drained || old_health["drain"] == true {
                                    record_step(
                                        &mut ok,
                                        &mut steps,
                                        "drain lifted",
                                        runtime.lift(),
                                    );
                                }
                            }
                            Err(err) => {
                                ok = false;
                                steps.push(format!("previous Hub health check failed: {err:#}"));
                            }
                        }
                    }
                } else {
                    steps.push("previous service not started: recovery needs inspection".into());
                }
            }
            let message = format!("{phase} failed: {error:#}; recovery: {}", steps.join("; "));
            receipt["ok"] = false.into();
            receipt["phase"] = phase.into();
            receipt["error"] = message.into();
            receipt["rolled_back"] = ok.into();
            receipt["recovery"] = json!(steps);
            receipt["finished"] = self::stamp().into();
            if ok { 1 } else { 2 }
        }
    };
    if let Err(error) = atomic_json(&result_path, &receipt) {
        receipt["receipt_write_error"] = format!("{error:#}").into();
    }
    Ok(DeployOutcome { exit_code, receipt })
}

/// Manual rollback for a macOS one-shot upgrade: restores `current`, `web/current`, the original
/// LaunchAgent plist (`.plist.previous`), and (when DB schema requires it) the pre-upgrade database
/// backup, then restarts the previous (e.g. v0.12.0) Hub and verifies `/healthz`.
pub fn rollback_macos(
    home: &Path,
    label_override: Option<&str>,
    drain_timeout: Duration,
) -> Result<DeployOutcome> {
    ensure!(
        cfg!(target_os = "macos"),
        "macOS service rollback requires macOS"
    );
    let (home, label_override) = (home.to_owned(), label_override.map(str::to_owned));
    std::thread::spawn(move || {
        let _lock = lock_release(&home)?;
        let runtime = Runtime::new(&home)?;
        rollback_macos_with(&runtime, label_override.as_deref(), drain_timeout)
    })
    .join()
    .map_err(|_| anyhow::anyhow!("macOS rollback worker stopped"))?
}

fn rollback_macos_with(
    runtime: &Runtime,
    label_override: Option<&str>,
    drain_timeout: Duration,
) -> Result<DeployOutcome> {
    let home = &runtime.home;
    let meta = json_file(&home.join("releases/.macos-rollback.json"))
        .context("no recorded macOS upgrade rollback metadata (releases/.macos-rollback.json)")?;
    let previous_hub = PathBuf::from(
        meta["previous_hub"]
            .as_str()
            .context("rollback metadata missing previous_hub")?,
    );
    let previous_web = meta["previous_web"]
        .as_str()
        .map(PathBuf::from)
        .unwrap_or_else(|| previous_hub.join("web/dist"));
    let previous_manifest = json_file(&previous_hub.join("release.json"))
        .context("previous release manifest missing; cannot roll back")?;
    ensure!(
        previous_hub.join("bin/steve-hub").is_file(),
        "previous release has no bin/steve-hub"
    );
    let (resolved_label, resolved_plist) =
        crate::service::hub_launchd_plist_path(&runtime.service_home, home, label_override)?;
    let label = label_override
        .map(str::to_owned)
        .or_else(|| meta["label"].as_str().map(str::to_owned))
        .unwrap_or(resolved_label);
    let plist_path = if label_override.is_some() {
        resolved_plist
    } else {
        meta["plist_path"]
            .as_str()
            .map(PathBuf::from)
            .unwrap_or(resolved_plist)
    };
    let helper_release = meta["candidate"]
        .as_str()
        .map(PathBuf::from)
        .filter(|p| p.join("bin/steve-hub").is_file())
        .or_else(|| fs::canonicalize(home.join("current")).ok())
        .unwrap_or_else(|| previous_hub.clone());
    let saved_snapshot = meta["backup_snapshot"]
        .as_str()
        .and_then(|s| SnapshotId::try_from(s.to_owned()).ok());

    let stamp = stamp();
    let logs = home.join("logs/release");
    private_dir(&logs)?;
    let result_path = logs.join(format!("macos-rollback-{stamp}.json"));

    let mut drained = false;
    if runtime.health().is_some() {
        if home.join("updates/control-token.json").is_file()
            && runtime.control(Method::POST).is_ok()
        {
            drained = true;
            let until = Instant::now() + drain_timeout;
            loop {
                let state = runtime.control(Method::GET)?;
                if state["ready"] == true {
                    break;
                }
                ensure!(
                    Instant::now() < until,
                    "Hub did not drain before rollback deadline"
                );
                std::thread::sleep(Duration::from_millis(200));
            }
        }
        runtime.service_at("stop", &label, &plist_path)?;
    }

    let restored_from_backup = crate::service::restore_service_definition_from_backup(&plist_path)?;
    if !restored_from_backup {
        crate::service::sync_hub_launchd_plist(&plist_path, home, &previous_hub)?;
    }
    link(&home.join("web/current"), &previous_web)?;
    link(&home.join("current"), &previous_hub)?;

    let db = home.join("data/steve.sqlite3");
    let mut restored_db = false;
    if db.is_file() && check_schema(&db, &previous_hub).is_err() {
        let snapshot = saved_snapshot.as_ref().context(
            "database schema is newer than previous release and no backup snapshot is recorded",
        )?;
        restore_snapshot(runtime, &helper_release, snapshot)?;
        restored_db = true;
    }

    runtime.service_at("start", &label, &plist_path)?;
    let health = runtime.wait_health(|h| {
        h["version"] == previous_manifest["version"]
            && (h["build_commit"].is_null() || h["build_commit"] == previous_manifest["commit"])
    })?;
    let health = if drained || health["drain"] == true {
        runtime.lift()?;
        runtime
            .wait_health(|h| h["version"] == previous_manifest["version"] && h["drain"] != true)?
    } else {
        health
    };

    let receipt = json!({
        "ok": true,
        "rolled_back_to": previous_hub,
        "version": previous_manifest["version"],
        "plist": plist_path,
        "plist_restored_from_backup": restored_from_backup,
        "database_restored": restored_db,
        "health": health,
        "finished": self::stamp(),
    });
    atomic_json(&result_path, &receipt)?;
    Ok(DeployOutcome {
        exit_code: 0,
        receipt,
    })
}
fn last_previous(home: &Path) -> Option<PathBuf> {
    let mut records = Vec::new();
    if let Ok(entries) = fs::read_dir(home.join("logs/release")) {
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            if name.starts_with("local-") && name.ends_with(".json") {
                if let Ok(value) = json_file(&entry.path()) {
                    if value["ok"] == true {
                        if let Some(previous) = value["previous"].as_str() {
                            records.push((
                                value["finished"]
                                    .as_str()
                                    .or(value["started"].as_str())
                                    .unwrap_or("")
                                    .to_owned(),
                                name,
                                PathBuf::from(previous),
                            ));
                        }
                    }
                }
            }
        }
    }
    if let Some((_, _, path)) = records.into_iter().max() {
        return Some(path);
    }
    let mut previous = None;
    if let Ok(history) = read_text(&home.join("releases/history.jsonl")) {
        for line in history.lines() {
            if let Ok(value) = serde_json::from_str::<Value>(line) {
                if value["kind"] == "hub" && value["ok"] == true {
                    if let Some(name) = value["prev"]
                        .as_str()
                        .and_then(|p| Path::new(p).file_name())
                    {
                        previous = Some(home.join("releases").join(name));
                    }
                }
            }
        }
    }
    previous
}
fn prune_unlocked(home: &Path, keep: &[PathBuf], dry_run: bool) -> Result<Vec<PathBuf>> {
    let mut remove = Vec::new();
    let releases = home.join("releases");
    let release_root = fs::canonicalize(&releases)?;
    let mut protected = HashSet::new();
    for path in keep.iter().cloned().chain([
        home.join("current"),
        home.join("previous"),
        home.join("web/current"),
    ]) {
        if let Ok(real) = fs::canonicalize(path) {
            for ancestor in real.ancestors() {
                if ancestor.parent() == Some(release_root.as_path()) {
                    protected.insert(ancestor.to_owned());
                }
            }
        }
    }
    let local = regex::Regex::new(r"^(\d+)\.(\d+)\.(\d+)-local-[0-9a-f]{7}$")?;
    let bundle = regex::Regex::new(
        r"^(\d+)\.(\d+)\.(\d+)-[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$",
    )?;
    let old = regex::Regex::new(r"^(\d{8}-\d{6})-[0-9a-f]{7,40}$")?;
    let mut ranked = Vec::new();
    for entry in fs::read_dir(&releases)? {
        let path = entry?.path();
        let name = path.file_name().unwrap().to_string_lossy().into_owned();
        if !fs::symlink_metadata(&path)?.file_type().is_dir() {
            continue;
        }
        if let Some(caps) = local.captures(&name).or_else(|| bundle.captures(&name)) {
            if !local.is_match(&name) {
                let manifest_path = path.join("release.json");
                let Ok(value) = json_file(&manifest_path) else {
                    continue;
                };
                if fs::symlink_metadata(manifest_path)?
                    .file_type()
                    .is_symlink()
                    || value["format_version"] != 1
                    || value["version"] != format!("{}.{}.{}", &caps[1], &caps[2], &caps[3])
                    || !value["commit"].as_str().is_some_and(|v| {
                        v.len() == 40
                            && v.bytes()
                                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
                    })
                {
                    continue;
                }
            }
            let version = (
                caps[1].parse::<u64>()?,
                caps[2].parse::<u64>()?,
                caps[3].parse::<u64>()?,
            );
            ranked.push(((1, version, name), path));
        } else if let Some(caps) = old.captures(&name) {
            ranked.push(((0, (0, 0, 0), caps[1].into()), path));
        }
    }
    ranked.sort_by(|a, b| b.0.cmp(&a.0));
    for (_, path) in ranked.into_iter().skip(3) {
        if !protected.contains(&fs::canonicalize(&path)?) {
            remove.push(path);
        }
    }
    remove.sort();
    if !dry_run {
        for path in &remove {
            if fs::symlink_metadata(path)?.file_type().is_dir() {
                fs::remove_dir_all(path)?;
            } else {
                fs::remove_file(path)?;
            }
        }
    }
    Ok(remove)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn local_control_follows_ipv4_ipv6_and_explicit_loopback_listeners() {
        for (configured, expected) in [
            ("", "127.0.0.1"),
            ("0.0.0.0", "127.0.0.1"),
            ("::", "[::1]"),
            ("[::1]", "[::1]"),
            ("192.0.2.10", "192.0.2.10"),
            ("2001:db8::10", "[2001:db8::10]"),
            ("192.0.2.10, ::1", "[::1]"),
            ("::,127.0.0.1", "127.0.0.1"),
            ("192.0.2.10, localhost", "localhost"),
        ] {
            assert_eq!(local_host(configured), expected, "{configured}");
        }
    }
    use crate::signing::tests::{executable, fixture};
    use std::io::{Read, Write};
    use std::sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    };
    use steve_store::db::{Compat, testkit::set_schema};

    fn response(mut stream: TcpStream, status: u16, body: Value) {
        let text = body.to_string();
        let message = format!(
            "HTTP/1.1 {status} Test\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{text}",
            text.len()
        );
        let _ = stream.write_all(message.as_bytes());
    }
    fn request(stream: &mut TcpStream) -> String {
        stream.set_nonblocking(false).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        let mut bytes = Vec::new();
        let mut buffer = [0; 1024];
        while bytes.len() < 16384 {
            let Ok(n) = stream.read(&mut buffer) else {
                break;
            };
            if n == 0 {
                break;
            }
            bytes.extend_from_slice(&buffer[..n]);
            if bytes.windows(4).any(|p| p == b"\r\n\r\n") {
                break;
            }
        }
        String::from_utf8_lossy(&bytes)
            .lines()
            .next()
            .unwrap_or("")
            .to_owned()
    }
    // Re-executed only by the fixture shell's normal Hub path. It is an ordinary no-op when the
    // test suite runs it. The probe uses a real native process, real SQLite, and real loopback HTTP.
    #[test]
    fn fixture_process() {
        if std::env::var("STEVE_NATIVE_TEST_FIXTURE").as_deref() != Ok("1") {
            return;
        }
        let home = PathBuf::from(std::env::var("STEVE_HOME").unwrap());
        let fixture = home.join("fixture-backup.sqlite3");
        if std::env::var("STEVE_NATIVE_TEST_BACKUP").as_deref() == Ok("1") {
            assert_ne!(
                std::env::var("STEVE_NATIVE_TEST_BACKUP_FAIL").as_deref(),
                Ok("1")
            );
            fs::copy(home.join("data/steve.sqlite3"), &fixture).unwrap();
            atomic_json(&home.join("fixture-manifest.json"), &json!({
                "purpose":"pre-upgrade", "snapshot_id":"a".repeat(64), "created_ms":0, "written_bytes":1,
                "database":{"bytes":1,"sha256":"b".repeat(64)}
            })).unwrap();
            return;
        }
        if std::env::var("STEVE_NATIVE_TEST_RESTORE").as_deref() == Ok("1") {
            let db = home.join("data/steve.sqlite3");
            fs::rename(&db, home.join("data/failed-probe.sqlite3")).unwrap();
            fs::copy(fixture, db).unwrap();
            return;
        }
        set_schema(&home.join("data/steve.sqlite3"), 54, Some(54)).unwrap();
        assert_ne!(
            std::env::var("STEVE_NATIVE_TEST_PROBE_FAIL").as_deref(),
            Ok("1"),
            "fixture fails after a migration"
        );
        if let Ok(mode) = std::env::var("STEVE_NATIVE_TEST_PROGRESS") {
            let id = std::env::var("STEVE_UPGRADE_ID").unwrap();
            let mut progress = crate::migration::Reporter::new(&home, &id).unwrap();
            progress
                .report(54, 54, 100_000, mode == "finished")
                .unwrap();
            if mode == "advancing" {
                for n in 1..=17 {
                    // This native child must advance beyond the parent's real health window.
                    // Keep this pacing: a child-local fake clock cannot drive the parent's
                    // std::time budget or preserve the test's elapsed-time assertion.
                    std::thread::sleep(Duration::from_millis(400));
                    progress.report(54, 54, n * 100_000, false).unwrap();
                }
                progress.report(54, 54, 1_800_000, true).unwrap();
            } else {
                // The parent must stop this candidate and roll back, whether migration stalled or
                // finished but startup never became healthy. Only the test's own child is stopped.
                let (_sender, receiver) = std::sync::mpsc::channel::<()>();
                receiver.recv().unwrap();
            }
        }
        let port: u16 = std::env::var("STEVE_PORT").unwrap().parse().unwrap();
        let listener = TcpListener::bind(("127.0.0.1", port)).unwrap();
        for stream in listener.incoming() {
            let mut stream = stream.unwrap();
            let line = request(&mut stream);
            if line.contains("/healthz") {
                response(
                    stream,
                    200,
                    json!({"version":"1.2.3","web_version":"1.2.3","maintenance":true}),
                );
            } else {
                response(stream, 503, json!({"maintenance":true}));
            }
        }
    }
    struct ServiceFixture {
        address: std::net::SocketAddr,
        stop: Arc<AtomicBool>,
        thread: Option<std::thread::JoinHandle<()>>,
    }
    impl Drop for ServiceFixture {
        fn drop(&mut self) {
            self.stop.store(true, Ordering::Relaxed);
            // Wake blocking accept so an idle fixture also exits before joining.
            let _ = TcpStream::connect(self.address);
            if let Some(thread) = self.thread.take() {
                thread.join().unwrap();
            }
        }
    }
    fn service_fixture(home: &Path) -> (ServiceFixture, u16) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let address = listener.local_addr().unwrap();
        let port = address.port();
        let stop = Arc::new(AtomicBool::new(false));
        let stopped = stop.clone();
        let home = home.to_owned();
        let thread = std::thread::spawn(move || {
            let mut maintenance = false;
            loop {
                if stopped.load(Ordering::Relaxed) {
                    break;
                }
                let mut stream = match listener.accept() {
                    Ok((stream, _)) => stream,
                    Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(Duration::from_millis(10));
                        continue;
                    }
                    Err(_) => break,
                };
                if stopped.load(Ordering::Relaxed) {
                    break;
                }
                let line = request(&mut stream);
                if fs::read_to_string(home.join("service-state")).is_ok_and(|v| v == "stop") {
                    continue;
                }
                if line.contains("/internal/upgrade/drain") {
                    if line.starts_with("POST ") {
                        maintenance = true;
                    }
                    if line.starts_with("DELETE ") {
                        maintenance = false;
                    }
                    response(stream, 200, json!({"ready":true,"maintenance":maintenance}));
                } else {
                    let manifest = json_file(&home.join("current/release.json")).unwrap();
                    if fs::read_to_string(home.join("fail-health-version"))
                        .is_ok_and(|v| v.trim() == manifest["version"].as_str().unwrap_or(""))
                    {
                        response(
                            stream,
                            503,
                            json!({"ok":false,"error":"simulated health failure"}),
                        );
                    } else {
                        response(
                            stream,
                            200,
                            json!({"version":manifest["version"],"web_version":manifest["version"],"build_commit":manifest["commit"],"drain":maintenance,"recovery":{"state":"ready"}}),
                        );
                    }
                }
            }
        });
        (
            ServiceFixture {
                address,
                stop,
                thread: Some(thread),
            },
            port,
        )
    }
    fn runtime_fixture(
        fail_probe: bool,
        fail_start: bool,
    ) -> (tempfile::TempDir, PathBuf, Runtime, ServiceFixture, Journal) {
        let (dir, candidate, tools) = fixture();
        let home = dir.path();
        let old = home.join("releases/1.2.2-local-bbbbbbb");
        fs::create_dir_all(old.join("bin")).unwrap();
        fs::create_dir_all(old.join("web/dist")).unwrap();
        let old_manifest = json!({"format_version":1,"version":"1.2.2","commit":"bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb","schema":{"target":53}});
        fs::write(old.join("release.json"), old_manifest.to_string()).unwrap();
        executable(&old.join("bin/steve-hub"), "#!/bin/sh\nexit 0\n");
        fs::create_dir_all(home.join("web")).unwrap();
        std::os::unix::fs::symlink(&old, home.join("current")).unwrap();
        std::os::unix::fs::symlink(old.join("web/dist"), home.join("web/current")).unwrap();
        fs::create_dir_all(home.join("bin")).unwrap();
        executable(&home.join("bin/steve-keyhelper"), "#!/bin/sh\nexit 0\n");
        private_dir(&home.join("signing")).unwrap();
        signing::exclusive_json(&home.join("signing/identity.json"), &json!({"label":"fixture.steve","hub_identifier":"io.github.infranite.steve.hub","keyhelper_identifier":"io.github.infranite.steve.keyhelper","vault_service":"fixture.vault"})).unwrap();
        fs::create_dir(home.join("data")).unwrap();
        set_schema(&home.join("data/steve.sqlite3"), 53, Some(53)).unwrap();
        let helper = std::env::current_exe().unwrap();
        let quoted = shlex::try_quote(helper.to_str().unwrap()).unwrap();
        executable(
            &candidate.join("bin/steve-hub"),
            &format!(
                r#"#!/bin/sh
case "${{1:-}}" in
    --build-info) printf '%s\n' '{{"version":"1.2.3","commit":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","target":"aarch64-apple-darwin","schema":54,"check_config":true}}'; exit 0;;
    --check-config) printf '%s\n' '{{"ok":true}}'; exit 0;;
    --admin)
        case "$2" in
            backup) [ "$3 $4 $5" = "--reason pre-upgrade --offline" ] || exit 9
                STEVE_NATIVE_TEST_BACKUP=1 STEVE_NATIVE_TEST_FIXTURE=1 {quoted} --exact release::tests::fixture_process --nocapture >/dev/null || exit 8
                cat "$STEVE_HOME/fixture-manifest.json";;
            restore) [ "$3 $4" = "{snapshot} --database-only" ] || exit 9
                STEVE_NATIVE_TEST_RESTORE=1 STEVE_NATIVE_TEST_FIXTURE=1 {quoted} --exact release::tests::fixture_process --nocapture >/dev/null || exit 8
                printf '%s\n' "$3";;
            *) exit 9;;
        esac
        exit 0;;
esac
export STEVE_NATIVE_TEST_FIXTURE=1
exec {quoted} --exact release::tests::fixture_process --nocapture
"#,
                snapshot = "a".repeat(64),
            ),
        );
        let manager = home.join("launchctl");
        let quoted_home = shlex::try_quote(home.to_str().unwrap()).unwrap();
        executable(
            &manager,
            &format!(
                "#!/bin/sh\ncase \"$1\" in bootout) printf stop > {quoted_home}/service-state;; bootstrap) printf start > {quoted_home}/service-state; {};; *) exit 4;; esac\n",
                if fail_start { "exit 7" } else { "exit 0" }
            ),
        );
        fs::write(home.join("service-state"), "start").unwrap();
        let (service, port) = service_fixture(home);
        let mut env = BTreeMap::new();
        env.insert("STEVE_HOME".into(), home.to_string_lossy().into());
        env.insert("PATH".into(), "/usr/bin:/bin".into());
        if fail_probe {
            env.insert("STEVE_NATIVE_TEST_PROBE_FAIL".into(), "1".into());
        }
        let runtime = Runtime {
            home: home.to_owned(),
            env,
            client: Client::builder()
                .no_proxy()
                .timeout(Duration::from_secs(2))
                .build()
                .unwrap(),
            address: format!("127.0.0.1:{port}"),
            tools,
            launchctl: manager,
            service_home: home.to_owned(),
            health_timeout: Duration::from_secs(3),
            simulated_service: true,
        };
        let id = uuid::Uuid::new_v4().to_string();
        private_dir(&home.join("updates/operations")).unwrap();
        signing::exclusive_json(
            &home.join(format!("updates/operations/{id}.json")),
            &json!({"id":id,"phase":"ready","version":"1.2.3","runner":{"kind":"release_bundle"}}),
        )
        .unwrap();
        let journal = Journal::new(home, Some(&id)).unwrap();
        (dir, candidate, runtime, service, journal)
    }
    #[test]
    fn native_deploy_runs_probe_backup_switch_and_journal_without_python() {
        let (dir, candidate, runtime, _service, journal) = runtime_fixture(false, false);
        let outcome = deploy_with(&runtime, &candidate, &journal, Duration::from_secs(2)).unwrap();
        assert_eq!(outcome.exit_code, 0, "{}", outcome.receipt);
        assert_eq!(
            fs::canonicalize(dir.path().join("current")).unwrap(),
            candidate
        );
        assert_eq!(
            json_file(journal.path.as_ref().unwrap()).unwrap()["phase"],
            "succeeded"
        );
        assert_eq!(
            inspect_schema(&dir.path().join("data/steve.sqlite3"))
                .unwrap()
                .schema,
            54
        );
        assert_eq!(outcome.receipt["backup_snapshot"], "a".repeat(64));
        assert_eq!(
            inspect_schema(&dir.path().join("fixture-backup.sqlite3"))
                .unwrap()
                .schema,
            53
        );
    }
    #[test]
    fn failed_candidate_backup_aborts_before_probe_or_switch() {
        let (dir, candidate, mut runtime, _service, journal) = runtime_fixture(false, false);
        runtime
            .env
            .insert("STEVE_NATIVE_TEST_BACKUP_FAIL".into(), "1".into());
        let outcome = deploy_with(&runtime, &candidate, &journal, Duration::from_secs(2)).unwrap();
        assert_eq!(outcome.exit_code, 1, "{}", outcome.receipt);
        assert_eq!(outcome.receipt["phase"], "backup");
        assert_eq!(
            inspect_schema(&dir.path().join("data/steve.sqlite3"))
                .unwrap()
                .schema,
            53
        );
        assert_ne!(
            fs::canonicalize(dir.path().join("current")).unwrap(),
            candidate
        );
        assert!(!dir.path().join("fixture-manifest.json").exists());
    }
    #[test]
    fn failed_probe_restores_database_and_old_service_before_business_start() {
        let (dir, candidate, runtime, _service, journal) = runtime_fixture(true, false);
        let outcome = deploy_with(&runtime, &candidate, &journal, Duration::from_secs(2)).unwrap();
        assert_eq!(outcome.exit_code, 1, "{}", outcome.receipt);
        assert_eq!(
            json_file(journal.path.as_ref().unwrap()).unwrap()["phase"],
            "rolled_back"
        );
        assert_eq!(
            inspect_schema(&dir.path().join("data/steve.sqlite3"))
                .unwrap()
                .schema,
            53
        );
        assert_eq!(
            json_file(&dir.path().join("current/release.json")).unwrap()["version"],
            "1.2.2"
        );
        assert!(dir.path().join("data/failed-probe.sqlite3").is_file());
        assert_eq!(
            fs::read_to_string(dir.path().join("service-state")).unwrap(),
            "start"
        );
    }
    #[test]
    fn advancing_migration_outlives_the_normal_health_window() {
        let (dir, candidate, mut runtime, _service, journal) = runtime_fixture(false, false);
        runtime.health_timeout = Duration::from_secs(3);
        runtime
            .env
            .insert("STEVE_NATIVE_TEST_PROGRESS".into(), "advancing".into());
        let start = Instant::now();
        let outcome = deploy_with(&runtime, &candidate, &journal, Duration::from_secs(2)).unwrap();
        assert_eq!(outcome.exit_code, 0, "{}", outcome.receipt);
        assert!(start.elapsed() > runtime.health_timeout * 2);
        assert_eq!(
            inspect_schema(&dir.path().join("data/steve.sqlite3"))
                .unwrap()
                .schema,
            54
        );
    }
    #[test]
    fn stalled_and_finished_but_unhealthy_candidates_restore_the_backup() {
        for mode in ["stalled", "finished"] {
            let (dir, candidate, mut runtime, _service, journal) = runtime_fixture(false, false);
            runtime.health_timeout = Duration::from_millis(1600);
            runtime
                .env
                .insert("STEVE_NATIVE_TEST_PROGRESS".into(), mode.into());
            let start = Instant::now();
            let outcome =
                deploy_with(&runtime, &candidate, &journal, Duration::from_secs(2)).unwrap();
            assert_eq!(outcome.exit_code, 1, "{}", outcome.receipt);
            assert!(start.elapsed() < Duration::from_secs(8));
            assert_eq!(
                json_file(journal.path.as_ref().unwrap()).unwrap()["phase"],
                "rolled_back"
            );
            assert_eq!(
                inspect_schema(&dir.path().join("data/steve.sqlite3"))
                    .unwrap()
                    .schema,
                53
            );
            assert_eq!(
                json_file(&dir.path().join("current/release.json")).unwrap()["version"],
                "1.2.2"
            );
            assert_eq!(
                fs::read_to_string(dir.path().join("service-state")).unwrap(),
                "start"
            );
        }
    }
    #[test]
    fn failed_start_keeps_new_database_and_reports_committed_failure() {
        let (dir, candidate, runtime, _service, journal) = runtime_fixture(false, true);
        let outcome = deploy_with(&runtime, &candidate, &journal, Duration::from_secs(2)).unwrap();
        assert_eq!(outcome.exit_code, 3, "{}", outcome.receipt);
        assert_eq!(
            json_file(journal.path.as_ref().unwrap()).unwrap()["phase"],
            "failed"
        );
        assert_eq!(
            inspect_schema(&dir.path().join("data/steve.sqlite3"))
                .unwrap()
                .schema,
            54
        );
        assert_eq!(
            fs::canonicalize(dir.path().join("current")).unwrap(),
            candidate
        );
        assert!(
            outcome.receipt["error"]
                .as_str()
                .unwrap()
                .contains("automatic rollback refused")
        );
        assert_eq!(
            json_file(journal.path.as_ref().unwrap()).unwrap()["error_code"],
            "committed_startup_failed"
        );
    }
    #[test]
    fn upgrade_from_012_plist_rewrites_to_direct_hub_and_rollback_preserves_012_plist() {
        let (dir, candidate, runtime, _service, journal) = runtime_fixture(false, false);
        let home = dir.path();
        // Shape the current installation like v0.12.0: ops/run.sh + LaunchAgent plist calling /bin/zsh .../ops/run.sh.
        let old = fs::canonicalize(home.join("current")).unwrap();
        fs::create_dir_all(old.join("ops")).unwrap();
        executable(&old.join("ops/run.sh"), "#!/bin/zsh\nexit 0\n");
        let agents = home.join("Library/LaunchAgents");
        fs::create_dir_all(&agents).unwrap();
        let plist_path = agents.join("fixture.steve.plist");
        let legacy_plist = json!({
            "Label": "fixture.steve",
            "ProgramArguments": ["/bin/zsh", format!("{}/current/ops/run.sh", home.display())],
            "EnvironmentVariables": {"STEVE_HOME": home.display().to_string()},
            "RunAtLoad": true,
        });
        let mut plist_bytes = Vec::new();
        plist::to_writer_xml(&mut plist_bytes, &legacy_plist).unwrap();
        fs::write(&plist_path, &plist_bytes).unwrap();
        assert!(
            !candidate.join("ops").exists(),
            "candidate has 0 ops scripts"
        );

        let outcome = deploy_with(&runtime, &candidate, &journal, Duration::from_secs(2)).unwrap();
        assert_eq!(outcome.exit_code, 0, "{}", outcome.receipt);
        let rewritten: Value = plist::from_bytes(&fs::read(&plist_path).unwrap()).unwrap();
        assert_eq!(
            rewritten["ProgramArguments"],
            json!([format!("{}/current/bin/steve-hub", home.display())])
        );

        // Pre-commit rollback from a 0.12.0 fixture keeps 0.12.0 plist and restarts cleanly.
        let (dir_rb, cand_rb, runtime_rb, _svc_rb, journal_rb) = runtime_fixture(true, false);
        let home_rb = dir_rb.path();
        let old_rb = fs::canonicalize(home_rb.join("current")).unwrap();
        fs::create_dir_all(old_rb.join("ops")).unwrap();
        executable(&old_rb.join("ops/run.sh"), "#!/bin/zsh\nexit 0\n");
        let agents_rb = home_rb.join("Library/LaunchAgents");
        fs::create_dir_all(&agents_rb).unwrap();
        let plist_rb = agents_rb.join("fixture.steve.plist");
        let legacy_rb = json!({
            "Label": "fixture.steve",
            "ProgramArguments": ["/bin/zsh", format!("{}/current/ops/run.sh", home_rb.display())],
            "EnvironmentVariables": {"STEVE_HOME": home_rb.display().to_string()},
            "RunAtLoad": true,
        });
        let mut rb_bytes = Vec::new();
        plist::to_writer_xml(&mut rb_bytes, &legacy_rb).unwrap();
        fs::write(&plist_rb, &rb_bytes).unwrap();

        let rb_outcome =
            deploy_with(&runtime_rb, &cand_rb, &journal_rb, Duration::from_secs(2)).unwrap();
        assert_eq!(rb_outcome.exit_code, 1, "{}", rb_outcome.receipt);
        assert_eq!(
            json_file(journal_rb.path.as_ref().unwrap()).unwrap()["phase"],
            "rolled_back"
        );
        let kept: Value = plist::from_bytes(&fs::read(&plist_rb).unwrap()).unwrap();
        assert_eq!(
            kept["ProgramArguments"],
            json!([
                "/bin/zsh",
                format!("{}/current/ops/run.sh", home_rb.display())
            ])
        );
        assert_eq!(
            fs::read_to_string(home_rb.join("service-state")).unwrap(),
            "start"
        );
    }
    /// Build a realistic v0.12.0 macOS installation (release directory with `ops/run.sh`,
    /// LaunchAgent plist invoking `/bin/zsh <home>/current/ops/run.sh`, and an external
    /// script-free candidate directory outside `<home>/releases/`).
    fn macos_012_fixture(
        managed: bool,
    ) -> (
        tempfile::TempDir,
        tempfile::TempDir,
        PathBuf,
        PathBuf,
        Vec<u8>,
        Runtime,
        ServiceFixture,
    ) {
        let (dir, staged_like_candidate, tools) = fixture();
        let home = dir.path();
        let old = home.join("releases/0.12.0");
        fs::create_dir_all(old.join("bin")).unwrap();
        fs::create_dir_all(old.join("web/dist")).unwrap();
        fs::create_dir_all(old.join("ops")).unwrap();
        let old_manifest = json!({
            "format_version": 1,
            "version": "0.12.0",
            "commit": "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
            "hub_version": "0.12.0",
            "web_version": "0.12.0",
            "api_protocol": 1,
            "schema": {"min_read": 53, "max_read": 53, "target": 53},
            "assets": []
        });
        fs::write(old.join("release.json"), old_manifest.to_string()).unwrap();
        executable(&old.join("bin/steve-hub"), "#!/bin/sh\nexit 0\n");
        fs::write(old.join("web/dist/index.html"), "<html>0.12.0</html>").unwrap();
        executable(&old.join("ops/run.sh"), "#!/bin/zsh\nexit 0\n");
        executable(&old.join("ops/run-managed.sh"), "#!/bin/sh\nexit 0\n");
        fs::create_dir_all(home.join("web")).unwrap();
        std::os::unix::fs::symlink(&old, home.join("current")).unwrap();
        std::os::unix::fs::symlink(old.join("web/dist"), home.join("web/current")).unwrap();
        fs::create_dir_all(home.join("data")).unwrap();
        set_schema(&home.join("data/steve.sqlite3"), 53, Some(53)).unwrap();

        let label = "io.github.infranite.steve.hub";
        if !managed {
            private_dir(&home.join("signing")).unwrap();
            signing::exclusive_json(
                &home.join("signing/identity.json"),
                &json!({
                    "label": label,
                    "hub_identifier": "io.github.infranite.steve.hub",
                    "keyhelper_identifier": "io.github.infranite.steve.keyhelper",
                    "vault_service": "fixture.vault"
                }),
            )
            .unwrap();
        }
        let agents = home.join("Library/LaunchAgents");
        fs::create_dir_all(&agents).unwrap();
        let plist_path = agents.join(format!("{label}.plist"));
        let legacy_args = if managed {
            json!([
                "/bin/sh",
                format!("{}/current/ops/run-managed.sh", home.display())
            ])
        } else {
            json!(["/bin/zsh", format!("{}/current/ops/run.sh", home.display())])
        };
        let legacy_plist = json!({
            "Label": label,
            "ProgramArguments": legacy_args,
            "EnvironmentVariables": {"STEVE_HOME": home.display().to_string()},
            "RunAtLoad": true,
        });
        let mut original_plist = Vec::new();
        plist::to_writer_xml(&mut original_plist, &legacy_plist).unwrap();
        fs::write(&plist_path, &original_plist).unwrap();

        // External unpacked new release bundle (outside `<home>/releases/`, 0 scripts).
        let ext_dir = tempfile::tempdir().unwrap();
        let ext_candidate = ext_dir.path().join("steve-1.2.3-bundle");
        fs::create_dir_all(ext_candidate.join("bin")).unwrap();
        fs::create_dir_all(ext_candidate.join("web/dist")).unwrap();
        fs::copy(
            staged_like_candidate.join("release.json"),
            ext_candidate.join("release.json"),
        )
        .unwrap();
        fs::copy(
            staged_like_candidate.join("web/dist/release.json"),
            ext_candidate.join("web/dist/release.json"),
        )
        .unwrap();
        fs::copy(
            staged_like_candidate.join("web/dist/index.html"),
            ext_candidate.join("web/dist/index.html"),
        )
        .unwrap();
        let helper = std::env::current_exe().unwrap();
        let quoted = shlex::try_quote(helper.to_str().unwrap()).unwrap();
        executable(
            &ext_candidate.join("bin/steve-hub"),
            &format!(
                r#"#!/bin/sh
case "${{1:-}}" in
    --build-info) printf '%s\n' '{{"version":"1.2.3","commit":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","target":"aarch64-apple-darwin","schema":54,"check_config":true}}'; exit 0;;
    --check-config) printf '%s\n' '{{"ok":true}}'; exit 0;;
    --admin)
        case "$2" in
            backup) [ "$3 $4 $5" = "--reason pre-upgrade --offline" ] || exit 9
                STEVE_NATIVE_TEST_BACKUP=1 STEVE_NATIVE_TEST_FIXTURE=1 {quoted} --exact release::tests::fixture_process --nocapture >/dev/null || exit 8
                cat "$STEVE_HOME/fixture-manifest.json";;
            restore) [ "$3 $4" = "{snapshot} --database-only" ] || exit 9
                STEVE_NATIVE_TEST_RESTORE=1 STEVE_NATIVE_TEST_FIXTURE=1 {quoted} --exact release::tests::fixture_process --nocapture >/dev/null || exit 8
                printf '%s\n' "$3";;
            *) exit 9;;
        esac
        exit 0;;
esac
exit 0
"#,
                snapshot = "a".repeat(64),
            ),
        );

        let manager = home.join("launchctl");
        let quoted_home = shlex::try_quote(home.to_str().unwrap()).unwrap();
        executable(
            &manager,
            &format!(
                "#!/bin/sh\ncase \"$1\" in bootout) printf stop > {quoted_home}/service-state;; bootstrap) printf start > {quoted_home}/service-state;; *) exit 4;; esac\n"
            ),
        );
        fs::write(home.join("service-state"), "start").unwrap();
        let (service, port) = service_fixture(home);
        let mut env = BTreeMap::new();
        env.insert("STEVE_HOME".into(), home.to_string_lossy().into());
        env.insert("PATH".into(), "/usr/bin:/bin".into());
        let runtime = Runtime {
            home: home.to_owned(),
            env,
            client: Client::builder()
                .no_proxy()
                .timeout(Duration::from_secs(2))
                .build()
                .unwrap(),
            address: format!("127.0.0.1:{port}"),
            tools,
            launchctl: manager,
            service_home: home.to_owned(),
            health_timeout: Duration::from_secs(2),
            simulated_service: true,
        };
        (
            dir,
            ext_dir,
            ext_candidate,
            plist_path,
            original_plist,
            runtime,
            service,
        )
    }

    #[test]
    fn macos_upgrade_from_012_succeeds_and_rewrites_plist() {
        for managed in [false, true] {
            let (dir, _ext_dir, ext_candidate, plist_path, original_plist, runtime, _service) =
                macos_012_fixture(managed);
            let home = dir.path();
            assert_eq!(
                crate::service::detect_legacy_hub_launchd_plist(&plist_path, home),
                Some(if managed {
                    crate::service::LegacyMacosServiceKind::RunManagedSh
                } else {
                    crate::service::LegacyMacosServiceKind::RunSh
                })
            );
            // Reject a candidate that still ships a legacy runtime script before changing anything.
            let bad_script = ext_candidate.join("ops/run.sh");
            fs::create_dir_all(bad_script.parent().unwrap()).unwrap();
            fs::write(&bad_script, "#!/bin/sh\n").unwrap();
            assert!(
                upgrade_macos_with(&runtime, &ext_candidate, None, Duration::from_secs(2), None)
                    .is_err()
            );
            fs::remove_dir_all(ext_candidate.join("ops")).unwrap();

            let outcome =
                upgrade_macos_with(&runtime, &ext_candidate, None, Duration::from_secs(2), None)
                    .unwrap();
            assert_eq!(outcome.exit_code, 0, "{}", outcome.receipt);
            let current = fs::canonicalize(home.join("current")).unwrap();
            assert!(
                current.starts_with(fs::canonicalize(home.join("releases")).unwrap()),
                "external bundle must be staged under <home>/releases/"
            );
            assert_eq!(
                json_file(&current.join("release.json")).unwrap()["version"],
                "1.2.3"
            );
            let rewritten: Value = plist::from_bytes(&fs::read(&plist_path).unwrap()).unwrap();
            let expected_args = if managed {
                json!([
                    format!("{}/current/bin/steve-hub", home.display()),
                    "--supervise"
                ])
            } else {
                json!([format!("{}/current/bin/steve-hub", home.display())])
            };
            assert_eq!(rewritten["ProgramArguments"], expected_args);
            assert_eq!(
                crate::service::detect_legacy_hub_launchd_plist(&plist_path, home),
                None
            );
            assert_eq!(
                fs::read(crate::service::previous_service_definition_path(
                    &plist_path
                ))
                .unwrap(),
                original_plist
            );
            assert_eq!(outcome.receipt["health"]["version"], "1.2.3");
            assert_eq!(
                fs::read_to_string(home.join("service-state")).unwrap(),
                "start"
            );
        }
    }

    #[test]
    fn macos_upgrade_from_012_auto_rolls_back_on_health_probe_failure() {
        let (dir, _ext_dir, ext_candidate, plist_path, original_plist, runtime, _service) =
            macos_012_fixture(false);
        let home = dir.path();
        // Simulate new version 1.2.3 failing /healthz after launchd start.
        fs::write(home.join("fail-health-version"), "1.2.3").unwrap();

        let outcome =
            upgrade_macos_with(&runtime, &ext_candidate, None, Duration::from_secs(2), None)
                .unwrap();
        assert_eq!(outcome.exit_code, 1, "{}", outcome.receipt);
        assert_eq!(outcome.receipt["rolled_back"], true);
        assert_eq!(outcome.receipt["phase"], "health");
        assert_eq!(
            json_file(&home.join("current/release.json")).unwrap()["version"],
            "0.12.0"
        );
        assert_eq!(
            fs::canonicalize(home.join("web/current")).unwrap(),
            fs::canonicalize(home.join("releases/0.12.0/web/dist")).unwrap()
        );
        assert_eq!(fs::read(&plist_path).unwrap(), original_plist);
        assert!(home.join("data/failed-probe.sqlite3").is_file());
        assert_eq!(
            inspect_schema(&home.join("data/steve.sqlite3"))
                .unwrap()
                .schema,
            53
        );
        assert_eq!(
            fs::read_to_string(home.join("service-state")).unwrap(),
            "start"
        );
    }

    #[test]
    fn macos_manual_rollback_restores_012_release_and_plist() {
        let (dir, _ext_dir, ext_candidate, plist_path, original_plist, runtime, _service) =
            macos_012_fixture(false);
        let home = dir.path();
        let upgraded =
            upgrade_macos_with(&runtime, &ext_candidate, None, Duration::from_secs(2), None)
                .unwrap();
        assert_eq!(upgraded.exit_code, 0, "{}", upgraded.receipt);
        assert_eq!(
            json_file(&home.join("current/release.json")).unwrap()["version"],
            "1.2.3"
        );
        assert_ne!(fs::read(&plist_path).unwrap(), original_plist);

        // Simulate DB schema bumped after upgrade so manual rollback also proves DB restoration.
        set_schema(&home.join("data/steve.sqlite3"), 54, Some(54)).unwrap();

        let rolled_back = rollback_macos_with(&runtime, None, Duration::from_secs(2)).unwrap();
        assert_eq!(rolled_back.exit_code, 0, "{}", rolled_back.receipt);
        assert_eq!(rolled_back.receipt["ok"], true);
        assert_eq!(rolled_back.receipt["version"], "0.12.0");
        assert_eq!(rolled_back.receipt["database_restored"], true);
        assert_eq!(
            json_file(&home.join("current/release.json")).unwrap()["version"],
            "0.12.0"
        );
        assert_eq!(
            fs::canonicalize(home.join("web/current")).unwrap(),
            fs::canonicalize(home.join("releases/0.12.0/web/dist")).unwrap()
        );
        assert_eq!(fs::read(&plist_path).unwrap(), original_plist);
        assert_eq!(
            inspect_schema(&home.join("data/steve.sqlite3"))
                .unwrap()
                .schema,
            53
        );
        assert_eq!(
            fs::read_to_string(home.join("service-state")).unwrap(),
            "start"
        );
    }
    #[test]
    fn schema_gate_refuses_unstamped_and_preserves_future_databases() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("data.sqlite3");
        assert_eq!(
            inspect_schema(&db).unwrap(),
            Compat {
                schema: 0,
                min_reader: 0
            }
        );
        assert!(!db.exists());
        set_schema(&db, 55, None).unwrap();
        assert!(inspect_schema(&db).is_err());
        fs::write(
            dir.path().join("release.json"),
            r#"{"schema":{"target":54}}"#,
        )
        .unwrap();
        assert!(check_schema(&db, dir.path()).is_err());
        set_schema(&db, 55, Some(54)).unwrap();
        check_schema(&db, dir.path()).unwrap();
        assert_eq!(inspect_schema(&db).unwrap().schema, 55);
    }
    #[test]
    fn prune_preserves_protected_unknown_and_unowned_uuid_directories() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path();
        let releases = home.join("releases");
        fs::create_dir(&releases).unwrap();
        for version in 1..=6 {
            fs::create_dir(releases.join(format!("1.0.{version}-local-aaaaaaa"))).unwrap();
        }
        let protect = releases.join("1.0.1-local-aaaaaaa");
        std::os::unix::fs::symlink(&protect, home.join("current")).unwrap();
        let unknown = releases.join("user-content");
        fs::create_dir(&unknown).unwrap();
        let uuid_dir = releases.join(format!("1.0.0-{}", uuid::Uuid::new_v4()));
        fs::create_dir(&uuid_dir).unwrap();
        let preview = prune(home, &[], true).unwrap();
        assert_eq!(preview.len(), 2);
        assert!(preview.iter().all(|p| p != &protect && p != &uuid_dir));
        assert!(preview.iter().all(|p| p.exists()));
        assert_eq!(prune(home, &[], false).unwrap(), preview);
        assert!(protect.exists() && unknown.exists() && uuid_dir.exists());
        assert!(prune(home, &[], false).unwrap().is_empty());
    }
}
