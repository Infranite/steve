//! Signed-install deployment with a durable journal and an explicit business commit boundary.
//! No Python, compiler, source checkout, or live Hub API is needed to inspect its receipts.
use crate::signing::{
    self, Tools, atomic_json, hash, json_file, owned_file, private_dir, successful,
};
use anyhow::{Context, Result, ensure};
use reqwest::Method;
use reqwest::blocking::Client;
use rusqlite::{Connection, OpenFlags, OptionalExtension};
use serde_json::{Value, json};
use std::collections::{BTreeMap, HashSet};
use std::fs::{self, File, OpenOptions};
use std::net::{TcpListener, TcpStream, ToSocketAddrs};
use std::os::fd::AsRawFd;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

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

/// Reads schema compatibility without creating or migrating the database. This is the same rule as
/// steve_store::db::compat: unstamped legacy databases require a reader of their own schema.
pub fn schema_state(db: &Path) -> Result<Value> {
    if !db.exists() {
        return Ok(json!({"schema":0,"min_reader":0}));
    }
    let conn = Connection::open_with_flags(db, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    let has = |name: &str| -> Result<bool> {
        Ok(conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name=?)",
            [name],
            |r| r.get(0),
        )?)
    };
    let schema: i64 = if has("schema_version")? {
        conn.query_row("SELECT v FROM schema_version", [], |r| r.get(0))
            .optional()?
            .unwrap_or(0)
    } else {
        0
    };
    let min_reader: i64 = if has("schema_compat")? {
        conn.query_row("SELECT min_reader FROM schema_compat", [], |r| r.get(0))
            .optional()?
            .unwrap_or(schema)
    } else {
        schema
    };
    Ok(json!({"schema":schema,"min_reader":min_reader}))
}
pub fn check_schema(db: &Path, release: &Path) -> Result<()> {
    let state = schema_state(db)?;
    let needed = state["min_reader"]
        .as_i64()
        .context("invalid schema minimum")?;
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

fn environment(home: &Path, tools: &Tools) -> Result<BTreeMap<String, String>> {
    let mut values = BTreeMap::new();
    for key in ["HOME", "PATH", "USER", "TMPDIR"] {
        if let Ok(value) = std::env::var(key) {
            values.insert(key.into(), value);
        }
    }
    let path = home.join("steve.env");
    if path.exists() {
        owned_file(&path)?;
        let mut command = Command::new(&tools.shell);
        command
            .args(["-fc", "set -a; source \"$1\"; /usr/bin/env -0", "zsh"])
            .arg(path)
            .env_clear()
            .envs(&values)
            .stdin(Stdio::null());
        let output = successful(&mut command, Duration::from_secs(30))?;
        for entry in output.stdout.split(|b| *b == 0) {
            if let Some(index) = entry.iter().position(|b| *b == b'=') {
                let key = String::from_utf8(entry[..index].to_vec())?;
                if !["PWD", "OLDPWD", "SHLVL", "_"].contains(&key.as_str()) {
                    values.insert(key, String::from_utf8(entry[index + 1..].to_vec())?);
                }
            }
        }
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
        let env = environment(home, &tools)?;
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
        let domain = format!("gui/{}", unsafe { libc::getuid() });
        let mut command = Command::new(&self.launchctl);
        if action == "stop" {
            command.args(["bootout", &format!("{domain}/{label}")]);
        } else {
            command.args(["bootstrap", &domain]).arg(
                self.service_home
                    .join("Library/LaunchAgents")
                    .join(format!("{label}.plist")),
            );
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
            .env("STEVE_NODE_MODE", "off")
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
            let deadline = Instant::now() + self.health_timeout;
            loop {
                ensure!(
                    process.0.try_wait()?.is_none(),
                    "candidate exited during its maintenance probe"
                );
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
                            .get(format!("http://127.0.0.1:{port}/api/state"))
                            .timeout(Duration::from_secs(2))
                            .send()?;
                        ensure!(
                            response.status().as_u16() == 503,
                            "business is reachable during the maintenance probe"
                        );
                        return Ok(health);
                    }
                }
                ensure!(
                    Instant::now() < deadline,
                    "candidate did not become healthy in maintenance"
                );
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
fn private_backups(home: &Path) -> Result<()> {
    fn restrict(path: &Path) -> Result<()> {
        let metadata = fs::symlink_metadata(path)?;
        if metadata.file_type().is_symlink() {
            return Ok(());
        }
        if metadata.is_dir() {
            fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
            for entry in fs::read_dir(path)? {
                restrict(&entry?.path())?;
            }
        } else if metadata.is_file() {
            fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
        }
        Ok(())
    }
    let directory = home.join("backups");
    private_dir(&directory)?;
    restrict(&directory)?;
    for entry in fs::read_dir(home)? {
        let path = entry?.path();
        if path
            .file_name()
            .is_some_and(|n| n.to_string_lossy().starts_with("steve.env"))
        {
            restrict(&path)?;
        }
    }
    Ok(())
}
fn backup(db: &Path, destination: &Path) -> Result<()> {
    ensure!(db.is_file(), "database missing; refusing an empty backup");
    let file = OpenOptions::new()
        .create_new(true)
        .write(true)
        .mode(0o600)
        .open(destination)?;
    drop(file);
    let source = Connection::open_with_flags(db, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    let mut target = Connection::open(destination)?;
    rusqlite::backup::Backup::new(&source, &mut target)?.run_to_completion(
        128,
        Duration::from_millis(5),
        None,
    )?;
    let checked: String = target.query_row("PRAGMA quick_check", [], |row| row.get(0))?;
    ensure!(checked == "ok", "upgrade backup integrity check failed");
    target.close().map_err(|(_, e)| e)?;
    File::open(destination)?.sync_all()?;
    File::open(destination.parent().unwrap())?.sync_all()?;
    Ok(())
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
    started: bool,
}
fn restore(
    runtime: &Runtime,
    changed: &Changed,
    old_hub: &Path,
    old_web: &Path,
    old_manifest: &Value,
    saved: Option<&Path>,
    candidate: &Path,
    stamp: &str,
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
                let failed = runtime.home.join("backups").join(format!(
                    "failed-probe-{}-{stamp}",
                    candidate
                        .file_name()
                        .context("candidate name missing")?
                        .to_string_lossy()
                ));
                fs::create_dir(&failed)?;
                fs::set_permissions(&failed, fs::Permissions::from_mode(0o700))?;
                let db = runtime.home.join("data/steve.sqlite3");
                for entry in fs::read_dir(db.parent().unwrap())? {
                    let path = entry?.path();
                    if path
                        .file_name()
                        .is_some_and(|n| n.to_string_lossy().starts_with("steve.sqlite3"))
                    {
                        fs::rename(&path, failed.join(path.file_name().unwrap()))?;
                    }
                }
                fs::copy(saved, &db)?;
                File::open(&db)?.sync_all()?;
                File::open(db.parent().unwrap())?.sync_all()?;
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
    let mut receipt = json!({"version":version,"release":candidate,"previous":old_hub,"previous_web":old_web,"db_before":schema_state(&db)?,"started":stamp});
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
        private_backups(home)?;
        let path = home.join("backups").join(format!(
            "pre-{}-{stamp}.sqlite3",
            candidate.file_name().unwrap().to_string_lossy()
        ));
        backup(&db, &path)?;
        saved = Some(path.clone());
        receipt["backup"] = json!(path);
        atomic_json(&result_path, &receipt)?;
        journal.update("backing_up", None, None, json!({"backup":path}))?;
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
            receipt["db_after"] = schema_state(&db)?;
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
                saved
                    .as_deref()
                    .map_or_else(|| "none".into(), |p| p.display().to_string())
            );
            receipt["ok"] = false.into();
            receipt["degraded"] = true.into();
            receipt["phase"] = phase.into();
            receipt["error"] = message.clone().into();
            receipt["db_after"] = schema_state(&db).unwrap_or(Value::Null);
            journal.update("failed", Some(&message), None, json!({}))?;
            3
        }
        Err(error) => {
            let (recovered, steps) = restore(
                runtime,
                &changed,
                &old_hub,
                &old_web,
                &old_manifest,
                saved.as_deref(),
                candidate,
                &stamp,
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
    prune_unlocked(home, &protected, dry_run)
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
    if let Ok(history) = fs::read_to_string(home.join("releases/history.jsonl")) {
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
    let backup_pattern = regex::Regex::new(
        r"^(pre-(\d+\.\d+\.\d+)-(?:local-[0-9a-f]{7}|[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12})-(\d{8}-\d{6})\.sqlite3)(?:-shm|-wal|-journal)?$",
    )?;
    let backups = home.join("backups");
    if backups.is_dir() {
        let mut groups: BTreeMap<String, (String, Vec<PathBuf>)> = BTreeMap::new();
        for entry in fs::read_dir(&backups)? {
            let path = entry?.path();
            let name = path.file_name().unwrap().to_string_lossy();
            if fs::symlink_metadata(&path)?.file_type().is_file() {
                if let Some(caps) = backup_pattern.captures(&name) {
                    groups
                        .entry(caps[1].into())
                        .or_insert_with(|| (caps[3].into(), Vec::new()))
                        .1
                        .push(path);
                }
            }
        }
        let mut whole: Vec<_> = groups
            .iter()
            .filter(|(name, (_, paths))| paths.contains(&backups.join(name)))
            .map(|(name, (stamp, _))| (stamp.clone(), name.clone()))
            .collect();
        whole.sort_by(|a, b| b.cmp(a));
        let preserve: HashSet<_> = whole.into_iter().take(8).map(|(_, name)| name).collect();
        for (name, (_, paths)) in groups {
            if !preserve.contains(&name) {
                remove.extend(paths);
            }
        }
    }
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

    fn response(mut stream: TcpStream, status: u16, body: Value) {
        let text = body.to_string();
        let message = format!(
            "HTTP/1.1 {status} Test\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{text}",
            text.len()
        );
        let _ = stream.write_all(message.as_bytes());
    }
    fn request(stream: &mut TcpStream) -> String {
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
        let db = Connection::open(home.join("data/steve.sqlite3")).unwrap();
        db.execute_batch("UPDATE schema_version SET v=54; UPDATE schema_compat SET min_reader=54; CREATE TABLE probe_write(v INTEGER); INSERT INTO probe_write VALUES(7);").unwrap();
        drop(db);
        assert_ne!(
            std::env::var("STEVE_NATIVE_TEST_PROBE_FAIL").as_deref(),
            Ok("1"),
            "fixture fails after a migration"
        );
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
        stop: Arc<AtomicBool>,
        thread: Option<std::thread::JoinHandle<()>>,
    }
    impl Drop for ServiceFixture {
        fn drop(&mut self) {
            self.stop.store(true, Ordering::Relaxed);
            if let Some(thread) = self.thread.take() {
                thread.join().unwrap();
            }
        }
    }
    fn service_fixture(home: &Path) -> (ServiceFixture, u16) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let port = listener.local_addr().unwrap().port();
        let stop = Arc::new(AtomicBool::new(false));
        let stopped = stop.clone();
        let home = home.to_owned();
        let thread = std::thread::spawn(move || {
            let mut maintenance = false;
            while !stopped.load(Ordering::Relaxed) {
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        let line = request(&mut stream);
                        if fs::read_to_string(home.join("service-state")).is_ok_and(|v| v == "stop")
                        {
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
                            response(
                                stream,
                                200,
                                json!({"version":manifest["version"],"web_version":manifest["version"],"build_commit":manifest["commit"],"drain":maintenance,"recovery":{"state":"ready"}}),
                            );
                        }
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(Duration::from_millis(5))
                    }
                    Err(error) => panic!("{error}"),
                }
            }
        });
        (
            ServiceFixture {
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
        Connection::open(home.join("data/steve.sqlite3")).unwrap().execute_batch("CREATE TABLE schema_version(v INTEGER); INSERT INTO schema_version VALUES(53); CREATE TABLE schema_compat(min_reader INTEGER); INSERT INTO schema_compat VALUES(53); CREATE TABLE original(v INTEGER); INSERT INTO original VALUES(9);").unwrap();
        let helper = std::env::current_exe().unwrap();
        let quoted = shlex::try_quote(helper.to_str().unwrap()).unwrap();
        executable(
            &candidate.join("bin/steve-hub"),
            &format!(
                "#!/bin/sh\ncase \"${{1:-}}\" in --build-info) printf '%s\\n' '{{\"version\":\"1.2.3\",\"commit\":\"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\",\"target\":\"aarch64-apple-darwin\",\"schema\":54,\"check_config\":true}}'; exit 0;; --check-config) printf '%s\\n' '{{\"ok\":true}}'; exit 0;; esac\nexport STEVE_NATIVE_TEST_FIXTURE=1\nexec {quoted} --exact release::tests::fixture_process --nocapture\n"
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
            schema_state(&dir.path().join("data/steve.sqlite3")).unwrap()["schema"],
            54
        );
        let backup = PathBuf::from(outcome.receipt["backup"].as_str().unwrap());
        assert_eq!(schema_state(&backup).unwrap()["schema"], 53);
        assert_eq!(
            fs::metadata(backup).unwrap().permissions().mode() & 0o777,
            0o600
        );
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
            schema_state(&dir.path().join("data/steve.sqlite3")).unwrap()["schema"],
            53
        );
        assert_eq!(
            json_file(&dir.path().join("current/release.json")).unwrap()["version"],
            "1.2.2"
        );
        let files: Vec<_> = fs::read_dir(dir.path().join("backups"))
            .unwrap()
            .flatten()
            .collect();
        assert!(
            files
                .iter()
                .any(|p| p.file_name().to_string_lossy().starts_with("failed-probe-"))
        );
        assert_eq!(
            fs::read_to_string(dir.path().join("service-state")).unwrap(),
            "start"
        );
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
            schema_state(&dir.path().join("data/steve.sqlite3")).unwrap()["schema"],
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
    }
    #[test]
    fn schema_gate_preserves_legacy_and_future_databases() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("data.sqlite3");
        assert_eq!(
            schema_state(&db).unwrap(),
            json!({"schema":0,"min_reader":0})
        );
        assert!(!db.exists());
        Connection::open(&db)
            .unwrap()
            .execute_batch(
                "CREATE TABLE schema_version(v INTEGER); INSERT INTO schema_version VALUES(55);",
            )
            .unwrap();
        assert_eq!(
            schema_state(&db).unwrap(),
            json!({"schema":55,"min_reader":55})
        );
        fs::write(
            dir.path().join("release.json"),
            r#"{"schema":{"target":54}}"#,
        )
        .unwrap();
        assert!(check_schema(&db, dir.path()).is_err());
        Connection::open(&db).unwrap().execute_batch("CREATE TABLE schema_compat(min_reader INTEGER); INSERT INTO schema_compat VALUES(54);").unwrap();
        check_schema(&db, dir.path()).unwrap();
        assert_eq!(schema_state(&db).unwrap()["schema"], 55);
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
