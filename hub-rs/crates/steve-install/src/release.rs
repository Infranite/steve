//! Signed-install deployment with a durable journal and an explicit business commit boundary.
//! No Python, compiler, source checkout, or live Hub API is needed to inspect its receipts.
use crate::signing::{
    self, Tools, atomic_json, hash, json_file, owned_file, private_dir, successful,
};
use anyhow::{Context, Result, ensure};
use reqwest::Method;
use reqwest::blocking::Client;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::{BTreeMap, HashSet};
use std::fs::{self, File, OpenOptions};
use std::net::{TcpListener, TcpStream, ToSocketAddrs};
use std::os::fd::AsRawFd;
use std::os::unix::fs::OpenOptionsExt;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use steve_store::db::inspect_schema;
use steve_types::backup::{BackupPurpose, BackupReceipt, SnapshotId};

pub struct DeployOutcome {
    pub exit_code: i32,
    pub receipt: Value,
}

/// Exclusive POSIX record lock guard for `releases/.release.lock`.
/// Keep a single descriptor open across the entire release operation; never open and close a
/// second descriptor to the same inode in this process, as POSIX releases all fcntl locks on close.
pub(crate) struct ReleaseLock(#[allow(dead_code)] File);

/// POSIX record lock, interoperable with the historical Python lockf and zsh zsystem flock.
/// Keep this descriptor open for the complete operation; do not substitute BSD flock.
pub(crate) fn lock_release(home: &Path) -> Result<ReleaseLock> {
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
    Ok(ReleaseLock(file))
}

static SIGNAL_INTERRUPTED: AtomicBool = AtomicBool::new(false);

extern "C" fn handle_interrupt(_sig: libc::c_int) {
    SIGNAL_INTERRUPTED.store(true, Ordering::SeqCst);
}

struct SignalGuard {
    prev_int: libc::sigaction,
    prev_term: libc::sigaction,
}

impl SignalGuard {
    fn install() -> Self {
        SIGNAL_INTERRUPTED.store(false, Ordering::SeqCst);
        let mut action: libc::sigaction = unsafe { std::mem::zeroed() };
        action.sa_sigaction = handle_interrupt as *const () as usize;
        action.sa_flags = 0;
        let mut prev_int: libc::sigaction = unsafe { std::mem::zeroed() };
        let mut prev_term: libc::sigaction = unsafe { std::mem::zeroed() };
        unsafe {
            libc::sigemptyset(&mut action.sa_mask);
            libc::sigaction(libc::SIGINT, &action, &mut prev_int);
            libc::sigaction(libc::SIGTERM, &action, &mut prev_term);
        }
        Self {
            prev_int,
            prev_term,
        }
    }
}

impl Drop for SignalGuard {
    fn drop(&mut self) {
        unsafe {
            libc::sigaction(libc::SIGINT, &self.prev_int, std::ptr::null_mut());
            libc::sigaction(libc::SIGTERM, &self.prev_term, std::ptr::null_mut());
        }
        SIGNAL_INTERRUPTED.store(false, Ordering::SeqCst);
    }
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

fn remove_file_best_effort(path: &Path) {
    let _ = fs::remove_file(path);
}

fn canonical(path: &Path) -> std::io::Result<PathBuf> {
    fs::canonicalize(path)
}

/// Read `<home>/steve.env` merged with `existing`: caller-supplied variables win except `PATH`,
/// where an explicit `PATH` in `steve.env` overrides launchd's minimal inherited `PATH`.
pub fn read_home_env(
    home: &Path,
    existing: &BTreeMap<String, String>,
) -> Result<BTreeMap<String, String>> {
    let mut values = existing.clone();
    values.insert("STEVE_HOME".into(), home.to_string_lossy().into());
    let path = home.join("steve.env");
    if path.exists() {
        owned_file(&path)?;
        let raw = read_text(&path)?;
        let parsed = crate::env_file::parse_runtime_env(raw.as_bytes(), &values)?;
        for (key, value) in parsed {
            if key == "PATH" || !existing.contains_key(&key) {
                values.insert(key, value);
            }
        }
    }
    values.insert("STEVE_HOME".into(), home.to_string_lossy().into());
    Ok(values)
}

fn environment(home: &Path) -> Result<BTreeMap<String, String>> {
    let mut base = BTreeMap::new();
    for key in ["HOME", "PATH", "USER", "TMPDIR"] {
        if let Ok(value) = std::env::var(key) {
            base.insert(key.into(), value);
        }
    }
    let mut values = read_home_env(home, &base)?;
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
    interrupt: Arc<AtomicBool>,
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
            interrupt: Arc::new(AtomicBool::new(false)),
            #[cfg(test)]
            simulated_service: false,
        })
    }
    fn check_interrupted(&self) -> Result<()> {
        ensure!(
            !SIGNAL_INTERRUPTED.load(Ordering::SeqCst) && !self.interrupt.load(Ordering::SeqCst),
            "upgrade interrupted by signal (SIGINT/SIGTERM)"
        );
        Ok(())
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
    fn wait_port_stopped(&self) -> Result<()> {
        #[cfg(test)]
        if self.simulated_service {
            return Ok(());
        }
        let deadline = Instant::now() + self.health_timeout.min(Duration::from_secs(15));
        let addresses: Vec<_> = self.address.to_socket_addrs()?.collect();
        ensure!(!addresses.is_empty(), "local Hub address did not resolve");
        while addresses
            .iter()
            .any(|address| TcpStream::connect_timeout(address, Duration::from_millis(100)).is_ok())
        {
            ensure!(
                Instant::now() < deadline,
                "old Hub still listens after bootout"
            );
            std::thread::sleep(Duration::from_millis(100));
        }
        Ok(())
    }
    fn run_launchctl(
        &self,
        args: &[&str],
        plist: Option<&Path>,
        allow_unloaded: bool,
    ) -> Result<()> {
        let mut command = Command::new(&self.launchctl);
        command.args(args);
        if let Some(p) = plist {
            command.arg(p);
        }
        command.stdin(Stdio::null());
        if allow_unloaded {
            let out = signing::output(&mut command, Duration::from_secs(30))?;
            if !out.status.success() {
                let stderr = String::from_utf8_lossy(&out.stderr);
                let unloaded = stderr.contains("Could not find service")
                    || stderr.contains("No such process")
                    || stderr.contains("Input/output error")
                    || out.status.code() == Some(3);
                ensure!(unloaded, "launchctl {:?} failed: {}", args, stderr.trim());
            }
            Ok(())
        } else {
            successful(&mut command, Duration::from_secs(30))?;
            Ok(())
        }
    }
    fn service_at(&self, action: &str, label: &str, plist: &Path) -> Result<()> {
        let domain = format!("gui/{}", unsafe { libc::getuid() });
        if action == "stop" {
            let target = format!("{domain}/{label}");
            self.run_launchctl(&["bootout", &target], None, false)?;
            self.wait_port_stopped()
        } else {
            self.run_launchctl(&["bootstrap", &domain], Some(plist), false)
        }
    }
    /// Unconditionally unload `label` from launchd (used by rollback even when `/healthz` is dead
    /// or the Hub is crash-looping), tolerating an already-unloaded service while still ensuring
    /// the local listening port is closed.
    fn bootout_unconditional(&self, label: &str) -> Result<()> {
        let domain = format!("gui/{}", unsafe { libc::getuid() });
        let target = format!("{domain}/{label}");
        self.run_launchctl(&["bootout", &target], None, true)?;
        self.wait_port_stopped()
    }
    fn hub_command(&self, candidate: &Path) -> Command {
        self.hub_command_with_env(candidate, &self.env, None)
    }
    fn hub_command_with_env(
        &self,
        candidate: &Path,
        env: &BTreeMap<String, String>,
        working_dir: Option<&Path>,
    ) -> Command {
        let mut command = Command::new(candidate.join("bin/steve-hub"));
        command
            .env_clear()
            .envs(env)
            .env("STEVE_WEB_DIR", candidate.join("web/dist"));
        if let Some(dir) = working_dir
            && dir.is_dir()
        {
            command.current_dir(dir);
        }
        command
    }
    fn check_config(&self, candidate: &Path, info: &Value) -> Result<()> {
        self.check_config_with_env(candidate, info, &self.env, None, false)
    }
    fn check_config_with_env(
        &self,
        candidate: &Path,
        info: &Value,
        env: &BTreeMap<String, String>,
        working_dir: Option<&Path>,
        enforce_harnesses: bool,
    ) -> Result<()> {
        if info["check_config"] != true {
            return Ok(());
        }
        let mut command = self.hub_command_with_env(candidate, env, working_dir);
        command.arg("--check-config").stdin(Stdio::null());
        let output = signing::output(&mut command, Duration::from_secs(120))?;
        let report: Value = serde_json::from_slice(&output.stdout)
            .context("candidate returned no configuration report")?;
        ensure!(
            output.status.success() && report["ok"] == true,
            "candidate configuration check failed; current Hub has not been drained or stopped"
        );
        if enforce_harnesses
            && let Some(harnesses) = report.get("harnesses").and_then(Value::as_object)
        {
            let missing_node = harnesses
                .get("missing_node")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            let missing_adapters: Vec<&str> = harnesses
                .get("missing_adapters")
                .and_then(Value::as_array)
                .map(|items| items.iter().filter_map(Value::as_str).collect())
                .unwrap_or_default();
            ensure!(
                !missing_node && missing_adapters.is_empty(),
                "candidate service environment cannot resolve configured harnesses/Node (missing_node={missing_node}, missing_adapters={missing_adapters:?}, PATH={})",
                env.get("PATH").map(String::as_str).unwrap_or("")
            );
        }
        Ok(())
    }
    fn probe(&self, candidate: &Path, version: &str) -> Result<Value> {
        self.probe_with_env(candidate, version, &self.env)
    }
    fn probe_with_env(
        &self,
        candidate: &Path,
        version: &str,
        env: &BTreeMap<String, String>,
    ) -> Result<Value> {
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
        let mut command =
            self.hub_command_with_env(candidate, env, Some(&self.home.join("current")));
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
                self.check_interrupted()?;
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
        remove_file_best_effort(&operation);
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
    backup_with_env(runtime, candidate, &runtime.env)
}

fn backup_with_env(
    runtime: &Runtime,
    candidate: &Path,
    env: &BTreeMap<String, String>,
) -> Result<SnapshotId> {
    let mut command = runtime.hub_command_with_env(candidate, env, None);
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
        remove_file_best_effort(&temporary);
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
    let label = ident["label"].as_str().unwrap_or("");
    let plist = runtime
        .service_home
        .join("Library/LaunchAgents")
        .join(format!("{label}.plist"));
    restore_at(
        runtime,
        changed,
        old_hub,
        old_web,
        old_manifest,
        saved,
        candidate,
        label,
        &plist,
    )
}

#[allow(clippy::too_many_arguments)]
fn restore_at(
    runtime: &Runtime,
    changed: &Changed,
    old_hub: &Path,
    old_web: &Path,
    old_manifest: &Value,
    saved: Option<&SnapshotId>,
    candidate: &Path,
    label: &str,
    plist: &Path,
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
                restore_snapshot(runtime, candidate, saved)
            })(),
        );
    }
    if !ok && changed.probed {
        steps.push("service not started: database recovery needs operator inspection".into());
        return (false, steps);
    }
    if changed.stopped && runtime.health().is_none() {
        match runtime.service_at("start", label, plist) {
            Ok(()) => steps.push("old service started".into()),
            Err(error) => {
                ok = false;
                steps.push(format!("old service start failed: {error:#}"));
            }
        }
    }
    if changed.stopped || changed.drain {
        if runtime
            .wait_health(|h| {
                h["version"] == old_manifest["version"]
                    && (h["build_commit"].is_null() || h["build_commit"] == old_manifest["commit"])
            })
            .is_err()
        {
            steps.push("old Hub did not answer as its original identity".into());
            if changed.drain && !changed.stopped {
                let _ = runtime.lift();
            }
            return (false, steps);
        }
        // Always lift maintenance whenever drain was initiated, even when the Hub was never stopped
        // (e.g. drain timed out, control GET failed, or the operator sent SIGINT during drain).
        if changed.drain {
            match runtime.lift() {
                Ok(()) => steps.push("drain lifted".into()),
                Err(error) => {
                    ok = false;
                    steps.push(format!("drain lift failed: {error:#}"));
                }
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum MacosUpgradeStage {
    Planned,
    Draining,
    Stopped,
    BackedUp,
    Probed,
    Switched,
    Started,
    Succeeded,
    RolledBack,
    CommittedFailed,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct MacosUpgradeJournal {
    version: String,
    candidate: PathBuf,
    previous_hub: PathBuf,
    previous_web: PathBuf,
    previous_manifest: Value,
    label: String,
    plist_path: PathBuf,
    plist_backup_path: PathBuf,
    backup_snapshot: Option<SnapshotId>,
    was_running: bool,
    stage: MacosUpgradeStage,
    started: String,
    updated: String,
}

fn macos_journal_path(home: &Path) -> PathBuf {
    home.join("releases/.macos-rollback.json")
}

fn load_macos_journal(home: &Path) -> Result<MacosUpgradeJournal> {
    let value = json_file(&macos_journal_path(home))
        .context("no recorded macOS upgrade rollback metadata (releases/.macos-rollback.json)")?;
    serde_json::from_value(value)
        .context("corrupt macOS upgrade rollback metadata (releases/.macos-rollback.json)")
}

fn save_macos_journal(home: &Path, journal: &mut MacosUpgradeJournal) -> Result<()> {
    journal.updated = stamp();
    atomic_json(&macos_journal_path(home), &serde_json::to_value(&*journal)?)
}

fn effective_service_env(
    runtime: &Runtime,
    plist_path: &Path,
) -> Result<(BTreeMap<String, String>, Option<PathBuf>)> {
    let (plist_env, plist_wd) =
        crate::service::read_hub_launchd_plist_env(plist_path).unwrap_or_default();
    let mut base = runtime.env.clone();
    for (k, v) in &plist_env {
        base.insert(k.clone(), v.clone());
    }
    let resolved_path = crate::service::resolve_hub_launchd_path(
        &runtime.home,
        plist_env
            .get("PATH")
            .or_else(|| runtime.env.get("PATH"))
            .map(String::as_str),
    );
    base.insert("PATH".into(), resolved_path);
    base.entry("NO_COLOR".into()).or_insert_with(|| "1".into());
    let env = read_home_env(&runtime.home, &base)?;
    Ok((env, plist_wd))
}

/// One-shot macOS migration/upgrade driven purely by the Rust binary inside the new release
/// package: records a durable rollback journal and `.plist.previous` before touching the running
/// service, validates and signs the script-free bundle while holding one continuous POSIX release
/// lock, drains and stops the prior Hub, backs up SQLite, runs the bounded maintenance probe
/// (vault read + migration progress budget), switches `current` + `web/current`, rewrites the
/// legacy LaunchAgent plist with `PATH`/`NO_COLOR`/`WorkingDirectory`, verifies configured
/// harnesses/Node, starts launchd, and checks `/healthz`. Pre-commit failures automatically roll
/// back and lift maintenance; post-commit failures follow `committed_startup_failed` (exit 3).
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
        let _signals = SignalGuard::install();
        let lock = lock_release(&home)?;
        let runtime = Runtime::new(&home)?;
        upgrade_macos_with(
            &runtime,
            &candidate,
            label_override.as_deref(),
            drain_timeout,
            signing_password.as_deref(),
            &lock,
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
    lock: &ReleaseLock,
) -> Result<DeployOutcome> {
    let home = &runtime.home;
    runtime.check_interrupted()?;
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
    let (label, plist_path) =
        crate::service::hub_launchd_plist_path(&runtime.service_home, home, label_override)?;
    ensure!(
        crate::common::exists(&plist_path) && plist_path.is_file(),
        "LaunchAgent plist not found at {}",
        plist_path.display()
    );

    let actual_current = canonical(&home.join("current"))
        .context("current release symlink is missing; cannot establish rollback target")?;
    let existing_journal = load_macos_journal(home).ok();
    let preserve_previous = existing_journal.as_ref().filter(|j| {
        let incomplete = !matches!(
            j.stage,
            MacosUpgradeStage::Succeeded | MacosUpgradeStage::RolledBack
        );
        let current_is_not_previous = canonical(&j.previous_hub)
            .ok()
            .is_some_and(|prev| prev != actual_current);
        let current_is_candidate_retry = j.stage == MacosUpgradeStage::RolledBack
            && current_is_not_previous
            && j.version == version;
        incomplete || current_is_candidate_retry
    });

    let (old_hub, old_web, old_manifest, was_running, resumed_backup, preserve_plist_backup) =
        if let Some(prev) = preserve_previous {
            (
                prev.previous_hub.clone(),
                prev.previous_web.clone(),
                prev.previous_manifest.clone(),
                prev.was_running || runtime.health().is_some(),
                prev.backup_snapshot.clone(),
                true,
            )
        } else {
            let old_manifest = json_file(&actual_current.join("release.json"))?;
            let old_web = canonical(&home.join("web/current"))
                .or_else(|_| canonical(&actual_current.join("web/dist")))?;
            (
                actual_current.clone(),
                old_web,
                old_manifest,
                runtime.health().is_some(),
                None,
                false,
            )
        };

    let staged = if let Some(prev) = preserve_previous
        && prev.version == version
        && prev.candidate.join("release.json").is_file()
        && canonical(&prev.candidate)
            .ok()
            .is_some_and(|p| p != actual_current)
        && json_file(&prev.candidate.join("release.json"))
            .ok()
            .is_some_and(|m| m["version"] == version && m["commit"] == commit)
    {
        prev.candidate.clone()
    } else {
        crate::service::stage_release_bundle(home, candidate, version, commit)?
    };
    ensure!(old_hub != staged, "candidate is already current");

    let db = home.join("data/steve.sqlite3");
    check_schema(&db, &staged)?;
    let info = signing::validate_bundle(home, &staged, version, commit)?;

    if home.join("signing/identity.json").is_file() {
        let ident = signing::identity(home, &runtime.tools)?;
        let hub_id = ident["hub_identifier"]
            .as_str()
            .context("signing hub_identifier missing")?;
        if signing::verify_signature(&staged.join("bin/steve-hub"), hub_id, &runtime.tools).is_err()
        {
            let password = signing_password.context(
                "signed macOS installation requires the signing keychain password (unlock the login Keychain or pass --keychain-pass-stdin)",
            )?;
            signing::prepare_bundle_locked(
                home,
                &staged,
                version,
                commit,
                password,
                lock,
                &runtime.tools,
            )?;
            signing::verify_signature(&staged.join("bin/steve-hub"), hub_id, &runtime.tools)?;
        }
    }

    let (effective_env, pre_wd) = effective_service_env(runtime, &plist_path)?;
    let check_wd = pre_wd.or_else(|| Some(home.join("current")));
    runtime.check_config_with_env(&staged, &info, &effective_env, check_wd.as_deref(), true)?;

    // Step 0: persist durable rollback journal and original plist backup BEFORE any mutation.
    let stamp = stamp();
    let logs = home.join("logs/release");
    private_dir(&logs)?;
    let result_path = logs.join(format!("macos-upgrade-{version}-{stamp}.json"));
    let plist_backup_snapshot =
        crate::service::backup_service_definition(&plist_path, preserve_plist_backup)?;
    let mut journal = MacosUpgradeJournal {
        version: version.to_owned(),
        candidate: staged.clone(),
        previous_hub: old_hub.clone(),
        previous_web: old_web.clone(),
        previous_manifest: old_manifest.clone(),
        label: label.clone(),
        plist_path: plist_path.clone(),
        plist_backup_path: crate::service::previous_service_definition_path(&plist_path),
        backup_snapshot: resumed_backup.clone(),
        was_running,
        stage: MacosUpgradeStage::Planned,
        started: preserve_previous
            .map(|j| j.started.clone())
            .unwrap_or_else(|| stamp.clone()),
        updated: stamp.clone(),
    };
    save_macos_journal(home, &mut journal)?;

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

    let mut changed = Changed::default();
    if preserve_previous.is_some() {
        if actual_current != old_hub {
            changed.hub_link = true;
            changed.web_link = true;
        }
        changed.service_definition = plist_backup_snapshot.clone();
    }
    let mut plist_rewritten = false;
    let mut saved = resumed_backup;
    let mut phase = "drain";

    let transaction = (|| -> Result<Value> {
        runtime.check_interrupted()?;
        if runtime.health().is_some() {
            journal.stage = MacosUpgradeStage::Draining;
            save_macos_journal(home, &mut journal)?;
            // Mark drain active BEFORE POST so any partial drain or failure lifts maintenance.
            changed.drain = true;
            runtime.control(Method::POST)?;
            let until = Instant::now() + drain_timeout;
            loop {
                runtime.check_interrupted()?;
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
            phase = "stop";
            changed.stopped = true;
            runtime.service_at("stop", &label, &plist_path)?;
            journal.stage = MacosUpgradeStage::Stopped;
            save_macos_journal(home, &mut journal)?;
        } else if was_running {
            changed.stopped = true;
        }

        runtime.check_interrupted()?;
        if db.is_file() {
            phase = "backup";
            // Reuse a recorded pre-upgrade backup only if a prior crashed attempt already migrated
            // the DB past the old release's schema, so the clean 0.12.0 backup is never overwritten.
            let reuse_saved = saved.is_some() && check_schema(&db, &old_hub).is_err();
            if !reuse_saved {
                let snapshot = backup_with_env(runtime, &staged, &effective_env)?;
                receipt["backup_snapshot"] = json!(snapshot);
                atomic_json(&result_path, &receipt)?;
                saved = Some(snapshot.clone());
                journal.backup_snapshot = Some(snapshot);
                journal.stage = MacosUpgradeStage::BackedUp;
                save_macos_journal(home, &mut journal)?;
            } else if let Some(snapshot) = &saved {
                receipt["backup_snapshot"] = json!(snapshot);
                atomic_json(&result_path, &receipt)?;
            }
        }

        runtime.check_interrupted()?;
        phase = "probe";
        changed.probed = db.is_file();
        runtime.probe_with_env(&staged, version, &effective_env)?;
        journal.stage = MacosUpgradeStage::Probed;
        save_macos_journal(home, &mut journal)?;

        runtime.check_interrupted()?;
        phase = "switch";
        changed.hub_link = true;
        link(&home.join("current"), &staged)?;
        changed.web_link = true;
        link(&home.join("web/current"), &staged.join("web/dist"))?;
        phase = "plist";
        let rewritten_backup = crate::service::sync_hub_launchd_plist_preserving_backup(
            &plist_path,
            home,
            &staged,
            true,
        )?;
        plist_rewritten = rewritten_backup.is_some();
        changed.service_definition = rewritten_backup.or(plist_backup_snapshot.clone());
        journal.stage = MacosUpgradeStage::Switched;
        save_macos_journal(home, &mut journal)?;

        // Re-verify candidate config and harnesses against the rewritten plist before committing.
        let (post_env, post_wd) = effective_service_env(runtime, &plist_path)?;
        runtime.check_config_with_env(&staged, &info, &post_env, post_wd.as_deref(), true)?;

        runtime.check_interrupted()?;
        // Commit boundary: once launchd starts the new Hub, business traffic may run.
        phase = "start";
        changed.started = true;
        journal.stage = MacosUpgradeStage::Started;
        save_macos_journal(home, &mut journal)?;
        runtime.service_at("start", &label, &plist_path)?;
        phase = "health";
        runtime.wait_health(|h| {
            h["version"] == version && (h["build_commit"].is_null() || h["build_commit"] == commit)
        })?;
        if changed.drain || runtime.health().is_some_and(|h| h["drain"] == true) {
            runtime.lift()?;
        }
        runtime.wait_health(|h| {
            h["version"] == version
                && (h["web_version"].is_null() || h["web_version"] == version)
                && h["drain"] != true
                && h["recovery"]["state"] != "failed"
        })
    })();

    let exit_code = match transaction {
        Ok(health) => {
            journal.stage = MacosUpgradeStage::Succeeded;
            save_macos_journal(home, &mut journal)?;
            receipt["ok"] = true.into();
            receipt["plist_rewritten"] = plist_rewritten.into();
            receipt["health"] = health;
            receipt["db_after"] = json!(inspect_schema(&db)?);
            receipt["finished"] = self::stamp().into();
            match prune_unlocked(home, std::slice::from_ref(&old_hub), false) {
                Ok(paths) => receipt["pruned"] = json!(paths),
                Err(error) => receipt["pruned_error"] = format!("{error:#}").into(),
            }
            0
        }
        Err(error) if changed.started => {
            journal.stage = MacosUpgradeStage::CommittedFailed;
            let _ = save_macos_journal(home, &mut journal);
            let message = format!(
                "Upgrade committed; new database retained and automatic rollback refused: {error:#}. Verified backup: {}",
                saved.as_ref().map_or("none", SnapshotId::as_str)
            );
            receipt["ok"] = false.into();
            receipt["degraded"] = true.into();
            receipt["phase"] = phase.into();
            receipt["error"] = message.into();
            receipt["db_after"] = inspect_schema(&db).map_or(Value::Null, |state| json!(state));
            receipt["error_code"] = "committed_startup_failed".into();
            receipt["finished"] = self::stamp().into();
            3
        }
        Err(error) => {
            let (recovered, steps) = restore_at(
                runtime,
                &changed,
                &old_hub,
                &old_web,
                &old_manifest,
                saved.as_ref(),
                &staged,
                &label,
                &plist_path,
            );
            if recovered {
                journal.stage = MacosUpgradeStage::RolledBack;
                let _ = save_macos_journal(home, &mut journal);
            }
            let message = format!("{phase} failed: {error:#}; recovery: {}", steps.join("; "));
            receipt["ok"] = false.into();
            receipt["phase"] = phase.into();
            receipt["error"] = message.into();
            receipt["rolled_back"] = recovered.into();
            receipt["recovered"] = recovered.into();
            receipt["recovery"] = json!(steps);
            receipt["finished"] = self::stamp().into();
            if recovered { 1 } else { 2 }
        }
    };
    if let Err(error) = atomic_json(&result_path, &receipt) {
        receipt["receipt_write_error"] = format!("{error:#}").into();
    }
    Ok(DeployOutcome { exit_code, receipt })
}

/// Manual rollback for a macOS one-shot upgrade: preflights metadata, `current` alignment, plist
/// availability, and database schema/backup before touching the running service; drains if alive
/// (lifting maintenance on failure without calling `bootout`); unconditionally unloads launchd even
/// when `/healthz` is dead or crash-looping; restores `current`, `web/current`, the exact original
/// `.plist.previous`, and the database snapshot when needed; verifies the previous Hub's `/healthz`;
/// cleans up rollback metadata; and always writes a receipt.
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
        let _signals = SignalGuard::install();
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
    let stamp = stamp();
    let logs = home.join("logs/release");
    private_dir(&logs)?;
    let result_path = logs.join(format!("macos-rollback-{stamp}.json"));
    let write_failure_receipt = |phase: &str, error: &anyhow::Error| {
        let receipt = json!({
            "ok": false,
            "phase": phase,
            "error": format!("{error:#}"),
            "started": stamp,
            "finished": self::stamp(),
        });
        let _ = atomic_json(&result_path, &receipt);
    };

    // Preflight: validate EVERYTHING before draining or stopping the service.
    let preflight = (|| -> Result<_> {
        runtime.check_interrupted()?;
        let journal = load_macos_journal(home)?;
        ensure!(
            journal.previous_hub.join("bin/steve-hub").is_file(),
            "previous release has no bin/steve-hub"
        );
        let previous_manifest = json_file(&journal.previous_hub.join("release.json"))
            .context("previous release manifest missing; cannot roll back")?;
        ensure!(
            journal.previous_web.is_dir(),
            "previous web directory missing: {}",
            journal.previous_web.display()
        );
        let actual_current = canonical(&home.join("current"))
            .context("current release symlink missing; cannot verify rollback target")?;
        let expected_candidate =
            canonical(&journal.candidate).unwrap_or_else(|_| journal.candidate.clone());
        let expected_previous =
            canonical(&journal.previous_hub).unwrap_or_else(|_| journal.previous_hub.clone());
        match journal.stage {
            MacosUpgradeStage::RolledBack if actual_current == expected_previous => {
                anyhow::bail!(
                    "installation is already rolled back to previous release {}",
                    expected_previous.display()
                );
            }
            MacosUpgradeStage::Succeeded
            | MacosUpgradeStage::Started
            | MacosUpgradeStage::CommittedFailed => {
                ensure!(
                    actual_current == expected_candidate,
                    "current release ({}) does not match recorded upgrade candidate ({}); refusing rollback",
                    actual_current.display(),
                    expected_candidate.display()
                );
            }
            _ => {
                ensure!(
                    actual_current == expected_candidate || actual_current == expected_previous,
                    "current release ({}) matches neither candidate ({}) nor previous ({}); refusing rollback",
                    actual_current.display(),
                    expected_candidate.display(),
                    expected_previous.display()
                );
            }
        }
        let (resolved_label, resolved_plist) =
            crate::service::hub_launchd_plist_path(&runtime.service_home, home, label_override)?;
        let label = label_override.map(str::to_owned).unwrap_or_else(|| {
            if journal.label.is_empty() {
                resolved_label
            } else {
                journal.label.clone()
            }
        });
        let plist_path = if label_override.is_some() {
            resolved_plist
        } else {
            journal.plist_path.clone()
        };
        let plist_backup_path = crate::service::previous_service_definition_path(&plist_path);
        ensure!(
            (crate::common::exists(&plist_backup_path) && plist_backup_path.is_file())
                || (crate::common::exists(&plist_path) && plist_path.is_file()),
            "neither LaunchAgent plist nor its .previous backup exists at {}",
            plist_path.display()
        );

        let db = home.join("data/steve.sqlite3");
        let needs_db_restore = db.is_file()
            && (check_schema(&db, &journal.previous_hub).is_err()
                || journal.stage == MacosUpgradeStage::Probed);
        let helper_release = if journal.candidate.join("bin/steve-hub").is_file() {
            journal.candidate.clone()
        } else if actual_current.join("bin/steve-hub").is_file() {
            actual_current.clone()
        } else {
            journal.previous_hub.clone()
        };
        if needs_db_restore {
            ensure!(
                journal.backup_snapshot.is_some(),
                "database schema is newer than previous release and no backup snapshot is recorded"
            );
            ensure!(
                helper_release.join("bin/steve-hub").is_file(),
                "cannot restore database backup: helper release bin/steve-hub is missing"
            );
        }
        Ok((
            journal,
            previous_manifest,
            label,
            plist_path,
            needs_db_restore,
            helper_release,
        ))
    })();
    let (journal, previous_manifest, label, plist_path, needs_db_restore, helper_release) =
        match preflight {
            Ok(values) => values,
            Err(error) => {
                write_failure_receipt("preflight", &error);
                return Err(error);
            }
        };

    // Mandatory drain when /healthz answers; on drain failure, lift maintenance and abort WITHOUT bootout.
    let mut drained = false;
    if runtime.health().is_some() {
        drained = true;
        let drain_result = (|| -> Result<()> {
            runtime.control(Method::POST)?;
            let until = Instant::now() + drain_timeout;
            loop {
                runtime.check_interrupted()?;
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
            Ok(())
        })();
        if let Err(error) = drain_result {
            let lifted = runtime.lift().is_ok();
            let receipt = json!({
                "ok": false,
                "phase": "drain",
                "error": format!("{error:#}"),
                "drain_lifted": lifted,
                "started": stamp,
                "finished": self::stamp(),
            });
            let _ = atomic_json(&result_path, &receipt);
            return Ok(DeployOutcome {
                exit_code: if lifted { 1 } else { 2 },
                receipt,
            });
        }
    }

    // Unconditionally bootout even when /healthz is dead or crash-looping, then restore everything.
    let apply_result = (|| -> Result<(bool, bool, Value)> {
        runtime.bootout_unconditional(&label)?;

        let restored_from_backup =
            crate::service::restore_service_definition_from_backup(&plist_path)?;
        if !restored_from_backup {
            crate::service::sync_hub_launchd_plist(&plist_path, home, &journal.previous_hub)?;
        }
        link(&home.join("web/current"), &journal.previous_web)?;
        link(&home.join("current"), &journal.previous_hub)?;

        let mut restored_db = false;
        if needs_db_restore {
            let snapshot = journal
                .backup_snapshot
                .as_ref()
                .context("verified pre-upgrade backup snapshot missing")?;
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
            runtime.wait_health(|h| {
                h["version"] == previous_manifest["version"] && h["drain"] != true
            })?
        } else {
            health
        };
        Ok((restored_from_backup, restored_db, health))
    })();

    let (restored_from_backup, restored_db, health) = match apply_result {
        Ok(values) => values,
        Err(error) => {
            write_failure_receipt("rollback", &error);
            return Err(error);
        }
    };

    // Clean up rollback metadata and `.plist.previous` once rollback succeeds so repeats refuse safely.
    remove_file_best_effort(&macos_journal_path(home));
    let _ = crate::service::remove_service_definition_backup(&plist_path);

    let receipt = json!({
        "ok": true,
        "rolled_back_to": journal.previous_hub,
        "version": previous_manifest["version"],
        "plist": plist_path,
        "plist_restored_from_backup": restored_from_backup,
        "database_restored": restored_db,
        "health": health,
        "started": stamp,
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
                    if let Ok(mut f) = OpenOptions::new()
                        .create(true)
                        .append(true)
                        .open(home.join("drain-log"))
                    {
                        let _ = writeln!(f, "{line}");
                    }
                    if line.starts_with("POST ") {
                        if home.join("drain-fail-post").exists() {
                            response(
                                stream,
                                500,
                                json!({"error": "simulated drain post failure"}),
                            );
                            continue;
                        }
                        maintenance = true;
                    }
                    if line.starts_with("DELETE ") {
                        maintenance = false;
                    }
                    let ready = !home.join("drain-never-ready").exists();
                    response(
                        stream,
                        200,
                        json!({"ready": ready, "maintenance": maintenance}),
                    );
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
            interrupt: Arc::new(AtomicBool::new(false)),
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
    --check-config) printf '%s\n' '{{"ok":true,"harnesses":{{"missing_adapters":[],"missing_node":false,"resolved_adapters":[]}}}}'; exit 0;;
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
                "#!/bin/sh\nprintf '%s %s\\n' \"$1\" \"$2\" >> {quoted_home}/launchctl-log\ncase \"$1\" in bootout) printf stop > {quoted_home}/service-state;; bootstrap) printf start > {quoted_home}/service-state;; *) exit 4;; esac\n"
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
            interrupt: Arc::new(AtomicBool::new(false)),
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

    fn run_upgrade_macos(
        runtime: &Runtime,
        candidate: &Path,
        label_override: Option<&str>,
        drain_timeout: Duration,
        signing_password: Option<&str>,
    ) -> Result<DeployOutcome> {
        let lock = lock_release(&runtime.home)?;
        upgrade_macos_with(
            runtime,
            candidate,
            label_override,
            drain_timeout,
            signing_password,
            &lock,
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
                run_upgrade_macos(&runtime, &ext_candidate, None, Duration::from_secs(2), None)
                    .is_err()
            );
            fs::remove_dir_all(ext_candidate.join("ops")).unwrap();

            let outcome =
                run_upgrade_macos(&runtime, &ext_candidate, None, Duration::from_secs(2), None)
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
                rewritten["WorkingDirectory"],
                json!(format!("{}/current", home.display()))
            );
            assert_eq!(rewritten["EnvironmentVariables"]["NO_COLOR"], json!("1"));
            assert!(
                rewritten["EnvironmentVariables"]["PATH"]
                    .as_str()
                    .is_some_and(|p| p.contains("/opt/homebrew/bin"))
            );
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
        let (dir, _ext_dir, ext_candidate, plist_path, original_plist, mut runtime, _service) =
            macos_012_fixture(false);
        let home = dir.path();
        // Simulate pre-commit maintenance probe failure after migrating DB -> auto rollback to 0.12.0.
        runtime
            .env
            .insert("STEVE_NATIVE_TEST_PROBE_FAIL".into(), "1".into());

        let outcome =
            run_upgrade_macos(&runtime, &ext_candidate, None, Duration::from_secs(2), None)
                .unwrap();
        assert_eq!(outcome.exit_code, 1, "{}", outcome.receipt);
        assert_eq!(outcome.receipt["rolled_back"], true);
        assert_eq!(outcome.receipt["phase"], "probe");
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
            run_upgrade_macos(&runtime, &ext_candidate, None, Duration::from_secs(2), None)
                .unwrap();
        assert_eq!(upgraded.exit_code, 0, "{}", upgraded.receipt);
        assert_eq!(
            json_file(&home.join("current/release.json")).unwrap()["version"],
            "1.2.3"
        );
        assert_ne!(fs::read(&plist_path).unwrap(), original_plist);

        // Maintenance probe already migrated DB to schema 54 -> manual rollback must restore schema 53.
        assert_eq!(
            inspect_schema(&home.join("data/steve.sqlite3"))
                .unwrap()
                .schema,
            54
        );

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
    fn review593_1_drain_failure_lifts_maintenance_on_upgrade_and_rollback() {
        let (dir, _ext_dir, ext_candidate, _plist_path, _original_plist, runtime, _service) =
            macos_012_fixture(false);
        let home = dir.path();
        private_dir(&home.join("updates")).unwrap();
        signing::exclusive_json(&home.join("updates/control-token.json"), &json!("tok")).unwrap();
        // Part A: upgrade drain timeout must lift maintenance instead of leaving 0.12.0 stuck in 503.
        fs::write(home.join("drain-never-ready"), "1").unwrap();
        let outcome = run_upgrade_macos(
            &runtime,
            &ext_candidate,
            None,
            Duration::from_millis(250),
            None,
        )
        .unwrap();
        assert_eq!(outcome.exit_code, 1, "{}", outcome.receipt);
        let health = runtime.health().expect("0.12.0 must still answer /healthz");
        assert_eq!(
            health["drain"], false,
            "drain timeout must lift maintenance instead of leaving 0.12.0 stuck in 503: {health}"
        );

        // Part B: rollback drain timeout must ALSO lift maintenance without calling bootout.
        fs::remove_file(home.join("drain-never-ready")).unwrap();
        let upgraded =
            run_upgrade_macos(&runtime, &ext_candidate, None, Duration::from_secs(2), None)
                .unwrap();
        assert_eq!(upgraded.exit_code, 0, "{}", upgraded.receipt);
        let _ = fs::remove_file(home.join("launchctl-log"));
        fs::write(home.join("drain-never-ready"), "1").unwrap();
        let rb = rollback_macos_with(&runtime, None, Duration::from_millis(250)).unwrap();
        assert_eq!(rb.exit_code, 1, "{}", rb.receipt);
        assert_eq!(rb.receipt["drain_lifted"], true);
        let rb_health = runtime.health().expect("1.2.3 must still answer /healthz");
        assert_eq!(
            rb_health["drain"], false,
            "rollback drain timeout must lift maintenance: {rb_health}"
        );
        assert!(
            !fs::read_to_string(home.join("launchctl-log"))
                .unwrap_or_default()
                .contains("bootout"),
            "rollback drain timeout must NOT bootout running Hub"
        );
    }

    #[test]
    fn review593_2_missing_token_creates_token_and_drain_error_aborts_without_bootout() {
        let (dir, _ext_dir, ext_candidate, _plist_path, _original_plist, runtime, _service) =
            macos_012_fixture(false);
        let home = dir.path();
        // Missing control-token.json must NOT skip drain; drain POST failure must abort before bootout.
        assert!(!home.join("updates/control-token.json").exists());
        fs::write(home.join("drain-fail-post"), "1").unwrap();
        let outcome =
            run_upgrade_macos(&runtime, &ext_candidate, None, Duration::from_secs(2), None)
                .unwrap();
        assert_ne!(outcome.exit_code, 0, "drain failure must abort upgrade");
        assert!(
            home.join("updates/control-token.json").is_file(),
            "upgrade_macos must create missing control-token.json via Runtime::control"
        );
        let launchctl_log = fs::read_to_string(home.join("launchctl-log")).unwrap_or_default();
        assert!(
            !launchctl_log.contains("bootout"),
            "drain failure must NOT bootout running Hub: {launchctl_log}"
        );
    }

    #[test]
    fn review593_4_upgrade_rejects_broken_harness_path() {
        let (dir, _ext_dir, ext_candidate, _plist_path, _original_plist, runtime, _service) =
            macos_012_fixture(false);
        let home = dir.path();
        // Candidate --check-config reports a missing configured adapter/node on the effective PATH.
        let stub = ext_candidate.join("bin/steve-hub");
        let original_stub = fs::read_to_string(&stub).unwrap();
        let broken_stub = original_stub.replace(
            r#"--check-config) printf '%s\n' '{"ok":true,"harnesses":{"missing_adapters":[],"missing_node":false,"resolved_adapters":[]}}'; exit 0;;"#,
            r#"--check-config) printf '%s\n' '{"ok":true,"warnings":["missing adapter"],"harnesses":{"missing_adapters":["claude"],"missing_node":true,"resolved_adapters":[]}}'; exit 0;;"#,
        );
        assert_ne!(original_stub, broken_stub);
        executable(&stub, &broken_stub);
        let res = run_upgrade_macos(&runtime, &ext_candidate, None, Duration::from_secs(2), None);
        let failed = match &res {
            Err(_) => true,
            Ok(o) => o.exit_code != 0,
        };
        assert!(
            failed,
            "upgrade must refuse/fail when configured harness or node is unreachable in effective PATH: {:?}",
            res.map(|o| (o.exit_code, o.receipt))
        );
        assert_eq!(
            json_file(&home.join("current/release.json")).unwrap()["version"],
            "0.12.0"
        );
    }

    #[test]
    fn review593_5_signing_preserves_release_lock_for_entire_upgrade() {
        let (dir, _ext_dir, ext_candidate, _plist_path, _original_plist, mut runtime, _service) =
            macos_012_fixture(false);
        let home = dir.path();
        // Make codesign fail ONLY for the newly staged unsigned 1.2.3 steve-hub until
        // codesign --force --sign runs, while installed 0.12.0 current/bin/steve-hub and
        // bin/steve-keyhelper stay verified as on a real signed macOS installation.
        let flag = home.join("signed-once");
        let codesign = home.join("codesign");
        executable(
            &codesign,
            &format!(
                r#"#!/bin/sh
case "$1" in
    --force) touch '{flag}'; exit 0;;
    --verify)
        case "$3" in
            *1.2.3*/bin/steve-hub) [ -f '{flag}' ] || exit 1; exit 0;;
            *) exit 0;;
        esac;;
    -dv)
        case "$2" in
            *steve-keyhelper) printf 'Identifier=io.github.infranite.steve.keyhelper\n' >&2; exit 0;;
            *1.2.3*/bin/steve-hub) [ -f '{flag}' ] || exit 1; printf 'Identifier=io.github.infranite.steve.hub\n' >&2; exit 0;;
            *) printf 'Identifier=io.github.infranite.steve.hub\n' >&2; exit 0;;
        esac;;
esac
exit 0
"#,
                flag = flag.display()
            ),
        );
        runtime.tools.codesign = codesign;
        fs::create_dir_all(home.join("bin")).unwrap();
        executable(&home.join("bin/steve-keyhelper"), "#!/bin/sh\nexit 0\n");
        fs::write(home.join("signing/steve-signing.keychain-db"), "kc").unwrap();

        // Hold release lock in parent (as upgrade_macos does) and verify lock survives signing!
        let lock = lock_release(home).unwrap();
        let pass = "0123456789abcdef".repeat(4);
        let outcome = upgrade_macos_with(
            &runtime,
            &ext_candidate,
            None,
            Duration::from_secs(2),
            Some(&pass),
            &lock,
        )
        .unwrap();
        assert_eq!(outcome.exit_code, 0, "{}", outcome.receipt);

        // Spawn a separate process attempting non-blocking F_SETLK on releases/.release.lock:
        // if prepare_bundle opened & closed a second fd, POSIX dropped our lock and python succeeds!
        let lock_path = home.join("releases/.release.lock");
        let probe = Command::new("python3")
            .args([
                "-c",
                "import fcntl, sys; f = open(sys.argv[1], 'r+'); fcntl.lockf(f, fcntl.LOCK_EX | fcntl.LOCK_NB)",
            ])
            .arg(&lock_path)
            .status()
            .unwrap();
        assert!(
            !probe.success(),
            "parent's POSIX release lock must still be held after signing inside upgrade_macos"
        );
    }

    #[test]
    fn review593_6_interrupt_mid_upgrade_recovers_and_rerun_preserves_012_target() {
        let (dir, _ext_dir, ext_candidate, plist_path, original_plist, mut runtime, _service) =
            macos_012_fixture(false);
        let home = dir.path();
        // Part A: SIGINT/SIGTERM during upgrade must abort cleanly without leaving Hub stuck in drain.
        runtime.interrupt.store(true, Ordering::SeqCst);
        let sig_err =
            run_upgrade_macos(&runtime, &ext_candidate, None, Duration::from_secs(2), None);
        assert!(
            sig_err.is_err(),
            "pending SIGINT/SIGTERM must abort before mutating installation"
        );
        runtime.interrupt.store(false, Ordering::SeqCst);

        // Part B: crash right after stop/backup (before switch): backup fails -> journal + .plist.previous persist.
        runtime
            .env
            .insert("STEVE_NATIVE_TEST_BACKUP_FAIL".into(), "1".into());
        let failed =
            run_upgrade_macos(&runtime, &ext_candidate, None, Duration::from_secs(2), None)
                .unwrap();
        assert_ne!(failed.exit_code, 0);
        assert!(
            home.join("releases/.macos-rollback.json").is_file(),
            "durable upgrade journal must be written before backup/switch"
        );
        assert_eq!(
            fs::read(crate::service::previous_service_definition_path(
                &plist_path
            ))
            .unwrap(),
            original_plist,
            ".plist.previous must be captured at Step 0 before any mutation"
        );
        // Simulate a mid-switch crash where current points to 1.2.3 before finish:
        // re-running upgrade_macos must preserve 0.12.0 as previous_hub, never overwrite it with 1.2.3!
        runtime.env.remove("STEVE_NATIVE_TEST_BACKUP_FAIL");
        let staged = fs::read_dir(home.join("releases"))
            .unwrap()
            .flatten()
            .map(|e| e.path())
            .find(|p| {
                p.file_name()
                    .is_some_and(|n| n.to_string_lossy().starts_with("1.2.3-"))
            })
            .expect("staged 1.2.3 dir");
        link(&home.join("current"), &staged).unwrap();
        let resumed =
            run_upgrade_macos(&runtime, &ext_candidate, None, Duration::from_secs(2), None)
                .unwrap();
        assert_eq!(resumed.exit_code, 0, "{}", resumed.receipt);
        let meta = json_file(&home.join("releases/.macos-rollback.json")).unwrap();
        assert_eq!(
            meta["previous_hub"],
            json!(fs::canonicalize(home.join("releases/0.12.0")).unwrap()),
            "re-running after mid-switch crash must keep 0.12.0 as rollback target"
        );
        // And manual rollback from that resumed upgrade must restore 0.12.0 & original plist.
        let rb = rollback_macos_with(&runtime, None, Duration::from_secs(2)).unwrap();
        assert_eq!(rb.exit_code, 0, "{}", rb.receipt);
        assert_eq!(fs::read(&plist_path).unwrap(), original_plist);
    }

    #[test]
    fn review593_7_rollback_preflights_unconditionally_bootouts_and_cleans_metadata() {
        let (dir, _ext_dir, ext_candidate, plist_path, original_plist, runtime, _service) =
            macos_012_fixture(false);
        let home = dir.path();
        let upgraded =
            run_upgrade_macos(&runtime, &ext_candidate, None, Duration::from_secs(2), None)
                .unwrap();
        assert_eq!(upgraded.exit_code, 0, "{}", upgraded.receipt);

        let count_rollback_receipts = || -> usize {
            fs::read_dir(home.join("logs/release"))
                .unwrap()
                .flatten()
                .filter(|e| {
                    e.file_name()
                        .to_string_lossy()
                        .starts_with("macos-rollback-")
                })
                .count()
        };

        // 1) Preflight check: incompatible schema with missing backup snapshot must fail BEFORE bootout
        //    and leave the running Hub untouched + write a failure receipt.
        let meta_path = home.join("releases/.macos-rollback.json");
        let mut meta = json_file(&meta_path).unwrap();
        let saved_snap = meta["backup_snapshot"].clone();
        meta["backup_snapshot"] = Value::Null;
        atomic_json(&meta_path, &meta).unwrap();
        set_schema(&home.join("data/steve.sqlite3"), 54, Some(54)).unwrap();
        let _ = fs::remove_file(home.join("launchctl-log"));
        let before_receipts = count_rollback_receipts();
        let err = rollback_macos_with(&runtime, None, Duration::from_secs(2));
        assert!(err.is_err());
        assert!(
            count_rollback_receipts() > before_receipts,
            "failed rollback preflight must still write a receipt"
        );
        assert!(
            !fs::read_to_string(home.join("launchctl-log"))
                .unwrap_or_default()
                .contains("bootout"),
            "missing backup snapshot preflight must fail BEFORE bootout"
        );
        meta["backup_snapshot"] = saved_snap;
        atomic_json(&meta_path, &meta).unwrap();

        // 2) Preflight check: current pointing to an unrelated third release must refuse BEFORE bootout.
        let third = home.join("releases/1.2.9-other");
        fs::create_dir_all(&third).unwrap();
        let real_candidate = fs::canonicalize(home.join("current")).unwrap();
        link(&home.join("current"), &third).unwrap();
        let _ = fs::remove_file(home.join("launchctl-log"));
        assert!(rollback_macos_with(&runtime, None, Duration::from_secs(2)).is_err());
        assert!(
            !fs::read_to_string(home.join("launchctl-log"))
                .unwrap_or_default()
                .contains("bootout"),
            "current mismatch preflight must fail BEFORE bootout"
        );
        link(&home.join("current"), &real_candidate).unwrap();

        // 3) Hung / crash-looping new Hub (/healthz unreachable) MUST still be unconditionally bootout'd!
        fs::write(home.join("fail-health-version"), "1.2.3").unwrap();
        let _ = fs::remove_file(home.join("launchctl-log"));
        let rb = rollback_macos_with(&runtime, None, Duration::from_secs(2)).unwrap();
        assert_eq!(rb.exit_code, 0, "{}", rb.receipt);
        let llog = fs::read_to_string(home.join("launchctl-log")).unwrap_or_default();
        assert!(
            llog.contains("bootout"),
            "rollback must unconditionally bootout even when /healthz is dead: {llog}"
        );
        assert_eq!(fs::read(&plist_path).unwrap(), original_plist);
        // 4) Metadata AND .plist.previous cleaned after rollback so a second rollback refuses safely.
        assert!(
            !meta_path.exists(),
            "rollback metadata must be removed once rollback completes"
        );
        assert!(
            !crate::service::previous_service_definition_path(&plist_path).exists(),
            ".plist.previous must be removed once rollback completes"
        );
        assert!(
            rollback_macos_with(&runtime, None, Duration::from_secs(2)).is_err(),
            "repeating rollback_macos after completion must refuse cleanly"
        );
    }

    #[test]
    fn review593_8_upgrade_runs_maintenance_probe_and_refuses_db_rollback_after_commit() {
        // Part A: pre-commit maintenance probe failure must abort and restore DB before switching.
        let (dir_p, _ext_p, cand_p, plist_p, orig_p, mut rt_p, _svc_p) = macos_012_fixture(false);
        rt_p.env
            .insert("STEVE_NATIVE_TEST_PROBE_FAIL".into(), "1".into());
        let res_p = run_upgrade_macos(&rt_p, &cand_p, None, Duration::from_secs(2), None).unwrap();
        assert_eq!(
            res_p.exit_code, 1,
            "maintenance probe failure must fail & auto-rollback before switch: {}",
            res_p.receipt
        );
        assert_eq!(
            json_file(&dir_p.path().join("current/release.json")).unwrap()["version"],
            "0.12.0"
        );
        assert_eq!(fs::read(&plist_p).unwrap(), orig_p);
        assert_eq!(
            inspect_schema(&dir_p.path().join("data/steve.sqlite3"))
                .unwrap()
                .schema,
            53
        );

        // Part B: post-commit startup failure (after launchd bootstrap) must follow deploy's
        // committed_startup_failed policy (exit 3, keep new DB, NO automatic DB rollback).
        let (dir_c, _ext_c, cand_c, _plist_c, _orig_c, rt_c, _svc_c) = macos_012_fixture(false);
        fs::write(dir_c.path().join("fail-health-version"), "1.2.3").unwrap();
        let res_c = run_upgrade_macos(&rt_c, &cand_c, None, Duration::from_secs(2), None).unwrap();
        assert_eq!(
            res_c.exit_code, 3,
            "post-start failure must return exit 3 (committed_startup_failed) without rolling back DB: {}",
            res_c.receipt
        );
        assert_eq!(res_c.receipt["error_code"], "committed_startup_failed");
        assert_eq!(
            inspect_schema(&dir_c.path().join("data/steve.sqlite3"))
                .unwrap()
                .schema,
            54,
            "post-commit failure must retain migrated DB"
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
