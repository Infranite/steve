//! Offline user-service management. A process ID never proves a Node is idle or
//! identifies its running build. Replacing or unregistering a service definition
//! requires a known stopped state; a boot Node's release receipt is separate.
mod boot;
mod uninstall;

#[doc(hidden)]
pub use boot::run_boot_node;
pub mod file_access;

use anyhow::{Context, Result, bail, ensure};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::ffi::OsStr;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt, symlink};
use std::path::{Component, Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use steve_types::devices::{
    ServiceCommandDiagnostic, ServiceEnvDiagnosticReason, ServicePathSource,
    extract_login_shell_path, login_shell_path_script,
};
use uuid::Uuid;
use wait_timeout::ChildExt;

#[derive(Clone, Copy, Debug, PartialEq)]
enum System {
    Darwin,
    Linux,
}

#[derive(Debug)]
struct Output {
    code: i32,
    stdout: String,
    stderr: String,
}
type Runner = Arc<dyn Fn(&[String]) -> Result<Output> + Send + Sync>;
type BootRegistrar =
    Arc<dyn Fn(&[String], &mut dyn FnMut() -> Result<()>) -> Result<()> + Send + Sync>;

const MANAGED_RELEASE_COMMIT_PREFIX_LEN: usize = 8;
const NODE_EXIT_WAIT_TIMEOUT: Duration = Duration::from_secs(15);
const LINUX_UPDATE_STOP_WAIT_MIN_SECS: u64 = 15;
const DARWIN_DEFAULT_SERVICE_PATH: &str =
    "/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin";
const LINUX_DEFAULT_SERVICE_PATH: &str = "/usr/local/bin:/usr/bin:/bin";

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ServicePath {
    pub path: String,
    pub source: ServicePathSource,
    pub shell: Option<String>,
    pub captured_at: String,
}

impl ServicePath {
    pub(super) fn to_json(&self) -> Value {
        json!({
            "path": self.path,
            "path_source": self.source,
            "path_shell": self.shell,
            "path_captured_at": self.captured_at,
        })
    }
}

fn utc_now_rfc3339() -> String {
    let seconds = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as libc::time_t;
    let mut utc: libc::tm = unsafe { std::mem::zeroed() };
    unsafe {
        libc::gmtime_r(&seconds, &mut utc);
    }
    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}Z",
        utc.tm_year + 1900,
        utc.tm_mon + 1,
        utc.tm_mday,
        utc.tm_hour,
        utc.tm_min,
        utc.tm_sec
    )
}

fn is_executable_file(path: &Path) -> bool {
    fs::metadata(path).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
}

fn which_in_path(command: &str, path_env: &str) -> Option<PathBuf> {
    if command.is_empty() {
        return None;
    }
    let candidate = Path::new(command);
    if candidate.is_absolute() || candidate.components().count() > 1 {
        return is_executable_file(candidate).then(|| candidate.to_path_buf());
    }
    std::env::split_paths(OsStr::new(path_env))
        .map(|dir| dir.join(command))
        .find(|p| is_executable_file(p))
}

/// Manages only one OS user's registered Hub or Node service.
pub struct Service {
    component: String,
    home: PathBuf,
    user_home: PathBuf,
    system: System,
    record_path: PathBuf,
    record: Value,
    signed: bool,
    label: String,
    config: PathBuf,
    config_uid: u32,
    runner: Runner,
    boot_registrar: BootRegistrar,
    node_signer: Arc<dyn boot::NodeSigner>,
    node_exit_wait: Duration,
}

/// An existing Hub installation that may have its configuration edited without
/// reinstalling its service. The caller snapshots these paths for its transaction.
pub(crate) struct ReconfigurationContext {
    pub release: PathBuf,
    pub config: PathBuf,
    pub record_path: PathBuf,
    pub label: String,
}

fn maintenance_file(path: &Path) -> Result<Vec<u8>> {
    metadata_file(path, unsafe { libc::getuid() })
}

fn metadata_file(path: &Path, owner: u32) -> Result<Vec<u8>> {
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)?;
    let metadata = file.metadata()?;
    ensure!(
        metadata.is_file()
            && metadata.uid() == owner
            && metadata.mode() & 0o022 == 0
            && metadata.len() <= 1024 * 1024,
        "configuration maintenance requires an owned regular metadata file not writable by others: {}",
        path.display()
    );
    let mut bytes = Vec::new();
    file.take(1024 * 1024 + 1).read_to_end(&mut bytes)?;
    ensure!(bytes.len() <= 1024 * 1024, "metadata file is too large");
    Ok(bytes)
}

fn configured_release(record: &Value) -> Option<&str> {
    record["boot_release"]
        .as_str()
        .or_else(|| record["release"].as_str())
}

fn registration(home: &Path, role: &str) -> Result<Option<Value>> {
    ensure!(
        matches!(role, "hub" | "node"),
        "component must be hub or node"
    );
    let path = home.join("services").join(format!("{role}.json"));
    if !exists(&path) {
        return Ok(None);
    }
    let record: Value = serde_json::from_slice(&maintenance_file(&path)?)?;
    ensure!(
        record["component"] == role,
        "service record has the wrong component"
    );
    if record["uninstalled"] == true || record.get("uninstall").is_some() {
        return Ok(None);
    }
    ensure!(
        record["label"].as_str().is_some_and(|s| !s.is_empty())
            && configured_release(&record).is_some_and(|s| Path::new(s).is_absolute())
            && ["version", "commit"]
                .iter()
                .all(|k| record["build"][k].as_str().is_some_and(|s| !s.is_empty())),
        "service record is incomplete"
    );
    Ok(Some(record))
}

/// Presence alone is not an installation: retired registrations must not take precedence over
/// another home, and unreadable or malformed records must not silently select another service.
pub fn has_active_registration(home: &Path, role: &str) -> Result<bool> {
    Ok(registration(home, role)?.is_some())
}

/// Resolve maintenance commands without contacting the service manager. A separate Node pins
/// its release in the service record (and need not have `current`); a Hub's updater switches
/// `current` without rewriting its original service or setup receipt.
pub fn adapter_target(home: &Path, requested: Option<&str>) -> Result<(String, PathBuf)> {
    let mut registrations = Vec::new();
    for role in ["hub", "node"] {
        if let Some(record) = registration(home, role)? {
            registrations.push((role, record));
        }
    }
    let (role, record) = if let Some(role) = requested {
        registrations
            .iter()
            .find(|(r, _)| *r == role)
            .with_context(|| {
                format!(
                    "no active {role} installation in {}; inspect services/{role}.json",
                    home.display()
                )
            })?
    } else {
        ensure!(
            registrations.len() == 1,
            "expected one active Hub or Node installation in {}; inspect services/hub.json and services/node.json, or specify hub/node when both are installed",
            home.display()
        );
        &registrations[0]
    };
    let setup = home.join("setup.json");
    if exists(&setup) {
        let receipt: Value = serde_json::from_slice(&maintenance_file(&setup)?)?;
        ensure!(
            receipt["format"] == 1
                && receipt["home"].as_str() == home.to_str()
                && receipt["component"] == *role,
            "setup receipt does not identify this installation"
        );
    }
    let registered = Path::new(configured_release(record).unwrap());
    let release = if *role == "hub" {
        let release = home
            .join("current")
            .canonicalize()
            .context("Hub current release is missing")?;
        ensure!(
            registered.canonicalize().ok().as_ref() == Some(&release)
                || home.join("releases").canonicalize().ok().as_deref() == release.parent(),
            "Hub current does not belong to this installation"
        );
        release
    } else {
        registered
            .canonicalize()
            .context("registered Node release is missing")?
    };
    let manifest: Value =
        serde_json::from_slice(&maintenance_file(&release.join("release.json"))?)?;
    ensure!(
        manifest["format_version"] == 1
            && ["version", "commit"]
                .iter()
                .all(|k| manifest[k].as_str().is_some_and(|s| !s.is_empty()))
            && (*role == "hub"
                || ["version", "commit"]
                    .iter()
                    .all(|k| manifest[k] == record["build"][k])),
        "installed release manifest is missing, unsupported or differs from the Node registration"
    );
    let binary = release.join(format!("bin/steve-{role}"));
    crate::signing::owned_file(&binary)
        .context("installed release has no trusted adapter command")?;
    ensure!(
        fs::symlink_metadata(&binary)?.mode() & 0o111 != 0,
        "installed adapter command is not executable"
    );
    Ok(((*role).into(), release))
}

fn text(path: &Path) -> Result<String> {
    path.to_str()
        .map(str::to_owned)
        .context("service path is not UTF-8")
}

fn resolve(path: &Path) -> Result<PathBuf> {
    let path = if path.is_absolute() {
        path.to_owned()
    } else {
        std::env::current_dir()?.join(path)
    };
    if path.exists() {
        return Ok(fs::canonicalize(path)?);
    }
    let mut normalized = PathBuf::new();
    for c in path.components() {
        match c {
            Component::CurDir => {}
            Component::ParentDir => {
                normalized.pop();
            }
            _ => normalized.push(c),
        }
    }
    if let (Some(parent), Some(name)) = (normalized.parent(), normalized.file_name()) {
        return Ok(resolve(parent)?.join(name));
    }
    Ok(normalized)
}

fn exists(path: &Path) -> bool {
    fs::symlink_metadata(path).is_ok()
}

fn read_json(path: &Path) -> Result<Value> {
    if !path.exists() {
        return Ok(json!({}));
    }
    serde_json::from_slice(&fs::read(path)?)
        .with_context(|| format!("invalid JSON: {}", path.display()))
}

fn object(path: &Path) -> Result<Value> {
    let value = read_json(path)?;
    ensure!(
        value.is_object(),
        "expected a JSON object: {}",
        path.display()
    );
    Ok(value)
}

fn json_bytes(value: &Value) -> Result<Vec<u8>> {
    let mut data = serde_json::to_vec_pretty(value)?;
    data.push(b'\n');
    Ok(data)
}

fn atomic(path: &Path, data: &[u8]) -> Result<()> {
    atomic_mode(path, data, 0o600)
}

fn atomic_mode(path: &Path, data: &[u8], mode: u32) -> Result<()> {
    let parent = path.parent().context("file has no parent")?;
    fs::create_dir_all(parent)?;
    let temporary = parent.join(format!(
        ".{}.{}",
        path.file_name().unwrap().to_string_lossy(),
        Uuid::new_v4().simple()
    ));
    let result = (|| -> Result<()> {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&temporary)?;
        file.write_all(data)?;
        file.set_permissions(fs::Permissions::from_mode(mode))?;
        file.sync_all()?;
        fs::rename(&temporary, path)?;
        File::open(parent)?.sync_all()?;
        Ok(())
    })();
    let _ = fs::remove_file(&temporary);
    result
}

fn digest(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

fn regular(path: &Path) -> Result<Vec<u8>> {
    let metadata = fs::symlink_metadata(path)?;
    ensure!(
        metadata.is_file() && metadata.uid() == unsafe { libc::getuid() },
        "uninstall requires an owned regular file: {}",
        path.display()
    );
    Ok(fs::read(path)?)
}

fn directory(path: &Path) -> Result<()> {
    if !exists(path) {
        fs::create_dir(path)?;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    }
    let metadata = fs::symlink_metadata(path)?;
    ensure!(
        metadata.is_dir() && metadata.uid() == unsafe { libc::getuid() },
        "uninstall requires an owned directory: {}",
        path.display()
    );
    Ok(())
}

fn execute(args: &[String]) -> Result<Output> {
    let mut command = Command::new(&args[0]);
    command.args(&args[1..]);
    if args[0] == "/usr/bin/sudo" {
        // Explicit CLI actions only. Inherit the terminal for the admin prompt.
        let output = command.stdin(Stdio::inherit()).output()?;
        return Ok(Output {
            code: output.status.code().unwrap_or(-1),
            stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        });
    }
    let mut child = command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .with_context(|| format!("service manager unavailable: {}", args[0]))?;
    // Drain both pipes while waiting, so a verbose manager cannot deadlock the timeout.
    let mut stdout = child.stdout.take().context("stdout unavailable")?;
    let mut stderr = child.stderr.take().context("stderr unavailable")?;
    let out = std::thread::spawn(move || {
        let mut bytes = Vec::new();
        stdout.read_to_end(&mut bytes).map(|_| bytes)
    });
    let err = std::thread::spawn(move || {
        let mut bytes = Vec::new();
        stderr.read_to_end(&mut bytes).map(|_| bytes)
    });
    let status = child.wait_timeout(Duration::from_secs(20))?;
    if status.is_none() {
        let _ = child.kill();
        let _ = child.wait();
        bail!("service manager unavailable: command timed out after 20 seconds");
    }
    let stdout = out
        .join()
        .map_err(|_| anyhow::anyhow!("stdout reader failed"))??;
    let stderr = err
        .join()
        .map_err(|_| anyhow::anyhow!("stderr reader failed"))??;
    Ok(Output {
        code: status.unwrap().code().unwrap_or(-1),
        stdout: String::from_utf8_lossy(&stdout).into_owned(),
        stderr: String::from_utf8_lossy(&stderr).into_owned(),
    })
}

fn unit_quote(value: &str, command: bool) -> Result<String> {
    ensure!(
        !value.contains(['\n', '\r', '\0']),
        "service paths and environment cannot contain newlines or NUL"
    );
    let mut value = value
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('%', "%%");
    if command {
        value = value.replace('$', "$$");
    }
    Ok(format!("\"{value}\""))
}

fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

impl Service {
    pub fn new(component: &str, home: &Path, label: Option<&str>) -> Result<Self> {
        let system = if cfg!(target_os = "macos") {
            System::Darwin
        } else if cfg!(target_os = "linux") {
            System::Linux
        } else {
            bail!("supported service managers: macOS launchd and Linux systemd --user");
        };
        ensure!(
            !(system == System::Darwin && component == "node" && unsafe { libc::getuid() } == 0),
            "run steve as the original Node account, not through sudo"
        );
        let user_home = PathBuf::from(std::env::var_os("HOME").context("HOME is not set")?);
        Self::with_runner(
            component,
            home,
            label,
            system,
            &user_home,
            Arc::new(execute),
        )
    }

    /// Inspect only: do not ask the service manager for state or recreate its
    /// registration. An upgrade changes `current` but may retain the original
    /// installation receipt's release and build, so those versions are not compared.
    pub(crate) fn reconfiguration_context(&self) -> Result<ReconfigurationContext> {
        self.reconfiguration_context_with_tools(&crate::signing::Tools::default())
    }

    fn reconfiguration_context_with_tools(
        &self,
        signing_tools: &crate::signing::Tools,
    ) -> Result<ReconfigurationContext> {
        ensure!(
            self.component == "hub",
            "only Hub configuration can be edited here"
        );
        ensure!(
            self.system != System::Darwin || self.signed,
            "original macOS Hub signing identity is missing or empty; configuration was not changed"
        );
        let record_bytes = maintenance_file(&self.record_path)
            .context("original Hub service registration is missing or untrusted")?;
        let record: Value = serde_json::from_slice(&record_bytes)?;
        ensure!(
            record == self.record
                && record["component"] == "hub"
                && record["label"].as_str() == Some(&self.label)
                && record["release"].as_str().is_some_and(|s| !s.is_empty())
                && record["build"].is_object()
                && record.get("uninstall").is_none()
                && record["uninstalled"] != true,
            "original Hub service registration changed, is incomplete, or was retired"
        );
        let definition = maintenance_file(&self.config)
            .context("original Hub service definition is missing or untrusted")?;
        if self.system == System::Darwin {
            let value: Value = plist::from_bytes(&definition)?;
            ensure!(
                value["Label"] == self.label,
                "service definition label differs from its registration"
            );
        }
        self.check_owner()?;

        let current = self.home.join("current");
        let release = resolve(&current)?;
        for relative in ["release.json", "bin/steve-hub", "web/dist/release.json"] {
            crate::signing::owned_file(&release.join(relative))?;
        }
        if self.signed {
            ensure!(
                self.system == System::Darwin,
                "signed Hub maintenance requires macOS"
            );
            crate::signing::owned_file(&self.home.join("bin/steve-keyhelper"))?;
            let identity = crate::signing::identity(&self.home, signing_tools)?;
            ensure!(
                identity["label"] == self.label,
                "original signing identity changed"
            );
        }
        // Verify a signed binary before executing even its read-only build-info command.
        let (release, build) = self.bundle(&release)?;
        ensure!(
            build["check_config"] == true,
            "installed Hub does not support --check-config; upgrade it before reconfiguring"
        );
        ensure!(
            maintenance_file(&self.record_path)? == record_bytes
                && maintenance_file(&self.config)? == definition
                && resolve(&current)? == release,
            "Hub installation changed while inspecting its configuration"
        );
        Ok(ReconfigurationContext {
            release,
            config: self.config.clone(),
            record_path: self.record_path.clone(),
            label: self.label.clone(),
        })
    }

    fn with_runner(
        component: &str,
        home: &Path,
        label: Option<&str>,
        system: System,
        user_home: &Path,
        runner: Runner,
    ) -> Result<Self> {
        ensure!(
            matches!(component, "hub" | "node"),
            "component must be hub or node"
        );
        let home = if home == Path::new("~") {
            user_home.to_owned()
        } else if let Ok(rest) = home.strip_prefix("~/") {
            user_home.join(rest)
        } else {
            home.to_owned()
        };
        let home = resolve(&home)?;
        let user_home = resolve(user_home)?;
        let record_path = home.join("services").join(format!("{component}.json"));
        let record = object(&record_path)?;
        let identity = if component == "hub" {
            object(&home.join("signing/identity.json"))?
        } else {
            json!({})
        };
        let signed = identity.as_object().is_some_and(|o| !o.is_empty());
        let label = label
            .or_else(|| record["label"].as_str())
            .or_else(|| identity["label"].as_str())
            .map(str::to_owned)
            .unwrap_or_else(|| format!("io.github.infranite.steve.{component}"));
        ensure!(
            !signed || identity["label"].as_str() == Some(&label),
            "signed Hub service label must match signing/identity.json; changing it would break upgrades"
        );
        ensure!(
            !label.is_empty()
                && label.len() <= 201
                && label.as_bytes()[0].is_ascii_alphanumeric()
                && label
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b)),
            "invalid service label"
        );
        ensure!(
            record["label"].is_null() || record["label"].as_str() == Some(&label),
            "service registration {} uses label {:?}, not {label}; use this home's original --label",
            record_path.display(),
            record["label"]
        );
        let boot = system == System::Darwin && component == "node";
        let config = if boot {
            Path::new("/Library/LaunchDaemons").join(format!("{label}.plist"))
        } else if system == System::Darwin {
            user_home
                .join("Library/LaunchAgents")
                .join(format!("{label}.plist"))
        } else {
            user_home
                .join(".config/systemd/user")
                .join(format!("{label}.service"))
        };
        Ok(Self {
            component: component.into(),
            home,
            user_home,
            system,
            record_path,
            record,
            signed,
            label,
            config,
            config_uid: if boot { 0 } else { unsafe { libc::getuid() } },
            runner,
            boot_registrar: Arc::new(boot::execute_register),
            node_signer: Arc::new(boot::LocalNodeSigner(crate::signing::Tools::default())),
            node_exit_wait: NODE_EXIT_WAIT_TIMEOUT,
        })
    }

    fn command(&self, args: Vec<String>, check: bool) -> Result<Output> {
        let result = (self.runner)(&args)?;
        ensure!(
            !check || result.code == 0,
            "service manager command failed ({}): {}",
            args[0],
            result.stderr.trim()
        );
        Ok(result)
    }

    fn systemctl(&self, args: &[&str], check: bool) -> Result<Output> {
        self.command(
            [
                vec!["systemctl".into(), "--user".into()],
                args.iter().map(|s| s.to_string()).collect(),
            ]
            .concat(),
            check,
        )
    }

    fn unit(&self) -> String {
        format!("{}.service", self.label)
    }

    fn definition_bytes(&self) -> Result<Vec<u8>> {
        if !self.boot_node() {
            return regular(&self.config);
        }
        metadata_file(&self.config, self.config_uid)
            .context("boot definition must be an owned regular file")
    }

    fn check_launch_definition(&self, bytes: &[u8]) -> Result<Value> {
        let definition: Value = plist::from_bytes(bytes)
            .context("existing service definition cannot establish its home")?;
        let owner = definition["EnvironmentVariables"]["STEVE_HOME"].as_str();
        ensure!(
            definition["Label"] == self.label && self.matches_home(owner)?,
            "service label belongs to another or unknown home; use that home's original --label"
        );
        Ok(definition)
    }

    fn matches_home(&self, owner: Option<&str>) -> Result<bool> {
        Ok(owner
            .filter(|s| !s.is_empty())
            .map(|s| resolve(Path::new(s)))
            .transpose()?
            .as_ref()
            == Some(&self.home))
    }

    fn check_owner(&self) -> Result<()> {
        if !exists(&self.config) {
            return Ok(());
        }
        if self.system == System::Darwin {
            let bytes = if self.boot_node() {
                self.definition_bytes()?
            } else {
                fs::read(&self.config)?
            };
            let definition = self.check_launch_definition(&bytes)?;
            if self.boot_node() {
                ensure!(
                    definition["UserName"].as_str() == Some(self.account()?.as_str()),
                    "boot service does not run as the original Node account"
                );
                ensure!(
                    definition["ProgramArguments"] == self.boot_arguments()?,
                    "boot service does not start the registered Node supervisor directly"
                );
            }
            return Ok(());
        }
        let mut owner = None;
        for line in fs::read_to_string(&self.config)?.lines() {
            if let Some(assignments) = line.strip_prefix("Environment=") {
                for assignment in shlex::split(assignments)
                    .context("existing unit has malformed environment quoting")?
                {
                    if let Some(value) = assignment.strip_prefix("STEVE_HOME=") {
                        owner = Some(value.replace("%%", "%"));
                    }
                }
            }
        }
        ensure!(
            self.matches_home(owner.as_deref())?,
            "service label belongs to another or unknown home; use that home's original --label"
        );
        Ok(())
    }
}

/// Deterministic launchd error codes from `launchctl print <domain>/<label>`:
/// - `3` (`ESRCH`): no such process / job not present
/// - `113`: service or domain not found in launchd
/// - `125`: domain does not support specified action (returned for `gui/<uid>` when no desktop
///   session is logged in, e.g. `/dev/console` owned by `root`)
const LAUNCHCTL_ERR_NO_SUCH_PROCESS: i32 = 3;
const LAUNCHCTL_ERR_NOT_FOUND: i32 = 113;
const LAUNCHCTL_ERR_DOMAIN_UNSUPPORTED: i32 = 125;

#[derive(Debug, Clone, PartialEq, Eq)]
enum LaunchQuery {
    Loaded,
    Absent,
    Unknown(String),
}

/// Extracts launchctl's structured `<prefix>: <code>: <message>` numeric code (for example
/// `Could not print domain: 125: Domain does not support specified action`), or uses the process
/// exit code when it directly carries one of the known launchd status codes.
fn launchctl_error_code(result: &Output) -> Option<i32> {
    for stream in [&result.stderr, &result.stdout] {
        for line in stream.lines() {
            let mut parts = line.splitn(3, ':');
            if parts.next().is_none() {
                continue;
            }
            let Some(code_part) = parts.next() else {
                continue;
            };
            if parts.next().is_some()
                && let Ok(code) = code_part.trim().parse::<i32>()
            {
                return Some(code);
            }
        }
    }
    matches!(
        result.code,
        LAUNCHCTL_ERR_NO_SUCH_PROCESS | LAUNCHCTL_ERR_NOT_FOUND | LAUNCHCTL_ERR_DOMAIN_UNSUPPORTED
    )
    .then_some(result.code)
}

fn launchctl_raw_output(result: &Output) -> String {
    let mut parts = Vec::new();
    let stderr = result.stderr.trim();
    let stdout = result.stdout.trim();
    if !stderr.is_empty() {
        parts.push(stderr);
    }
    if !stdout.is_empty() {
        parts.push(stdout);
    }
    if parts.is_empty() {
        format!("launchctl exited with status {}", result.code)
    } else {
        format!(
            "launchctl exited with status {}: {}",
            result.code,
            parts.join(" | ")
        )
    }
}

fn classify_launchctl_print(domain: &str, result: &Output) -> LaunchQuery {
    if result.code == 0 {
        return LaunchQuery::Loaded;
    }
    match launchctl_error_code(result) {
        Some(LAUNCHCTL_ERR_NOT_FOUND | LAUNCHCTL_ERR_NO_SUCH_PROCESS) => LaunchQuery::Absent,
        Some(LAUNCHCTL_ERR_DOMAIN_UNSUPPORTED) if domain.starts_with("gui/") => LaunchQuery::Absent,
        _ => LaunchQuery::Unknown(launchctl_raw_output(result)),
    }
}

impl Service {
    fn launch_state(&self, domain: &str, config: &Path, exact: bool) -> Result<Value> {
        let result = self.command(
            vec![
                "launchctl".into(),
                "print".into(),
                format!("{domain}/{}", self.label),
            ],
            false,
        )?;
        // The top-level job fields are indented one tab. Nested dictionaries can
        // also have keys named path, state, or pid, and must not override them.
        let fields: BTreeMap<_, _> = result
            .stdout
            .lines()
            .filter_map(|line| {
                let field = line.trim_start_matches('\t');
                (line.len() - field.len() <= 1)
                    .then(|| field.trim().split_once(" = "))
                    .flatten()
            })
            .collect();
        if exact && result.code == 0 {
            ensure!(
                fields.get("path").map(Path::new) == Some(config),
                "loaded launchd job does not use the owned service definition"
            );
        }
        let pid = fields.get("pid").and_then(|s| s.parse::<u64>().ok());
        let query = classify_launchctl_print(domain, &result);
        let (running, default_state, diagnostic) = match query {
            LaunchQuery::Loaded => (Some(pid.is_some()), "unknown", None),
            LaunchQuery::Absent => (Some(false), "stopped", None),
            LaunchQuery::Unknown(raw) => (None, "unknown", Some(raw)),
        };
        let mut out = json!({
            "installed": config.is_file(),
            "loaded": result.code == 0,
            "running": running,
            "pid": pid,
            "program": fields.get("program"),
            "state": fields.get("state").copied().unwrap_or(default_state),
        });
        if let Some(diagnostic) = diagnostic {
            out["diagnostic"] = json!(diagnostic);
        }
        Ok(out)
    }

    fn state(&self) -> Result<Value> {
        self.check_owner()?;
        if self.system == System::Darwin {
            let domain = if self.boot_node() {
                "system".into()
            } else {
                format!("gui/{}", unsafe { libc::getuid() })
            };
            return self.launch_state(&domain, &self.config, self.boot_node());
        }
        let result = self.systemctl(
            &[
                "show",
                &self.unit(),
                "--no-pager",
                "--property=LoadState,ActiveState,MainPID,ExecMainStatus",
            ],
            false,
        )?;
        let fields: BTreeMap<_, _> = result
            .stdout
            .lines()
            .filter_map(|s| s.split_once('='))
            .collect();
        let pid = fields.get("MainPID").and_then(|s| s.parse::<u64>().ok());
        let active = fields.get("ActiveState").copied().unwrap_or("unknown");
        let running =
            if result.code != 0 || fields.is_empty() || active == "unknown" || pid.is_none() {
                None
            } else {
                Some(
                    pid.unwrap() != 0
                        || matches!(
                            active,
                            "active" | "activating" | "deactivating" | "reloading"
                        ),
                )
            };
        Ok(
            json!({"installed":self.config.is_file(),"loaded":fields.get("LoadState") == Some(&"loaded"),"running":running,
            "pid":pid.filter(|p| *p != 0),"state":active}),
        )
    }

    fn bundle(&self, release: &Path) -> Result<(PathBuf, Value)> {
        let release = resolve(release)?;
        let manifest = object(&release.join("release.json"))?;
        ensure!(
            manifest["format_version"] == 1,
            "release.json is missing or unsupported; use a verified release bundle"
        );
        let binary = release
            .join("bin")
            .join(format!("steve-{}", self.component));
        let meta = fs::symlink_metadata(&binary)
            .with_context(|| format!("missing executable {}", binary.display()))?;
        ensure!(
            meta.is_file() && meta.permissions().mode() & 0o111 != 0,
            "missing executable {}",
            binary.display()
        );
        let output = self.command(vec![text(&binary)?, "--build-info".into()], true)?;
        let info: Value =
            serde_json::from_str(&output.stdout).context("cannot read binary build identity")?;
        ensure!(
            ["version", "commit"]
                .iter()
                .all(|k| manifest[k].is_string() && info[k] == manifest[k]),
            "binary version/commit differs from release manifest"
        );
        if self.component == "hub" {
            let web = object(&release.join("web/dist/release.json"))?;
            ensure!(
                ["version", "commit"].iter().all(|k| web[k] == manifest[k]),
                "Web version/commit differs from release manifest"
            );
        }
        Ok((release, info))
    }

    fn managed_release_dir_name(info: &Value) -> Result<String> {
        let version = info["version"]
            .as_str()
            .filter(|s| !s.is_empty())
            .context("release build metadata missing version")?;
        let commit = info["commit"]
            .as_str()
            .filter(|s| !s.is_empty())
            .context("release build metadata missing commit")?;
        let valid_token = |s: &str| {
            !s.is_empty()
                && s != "."
                && s != ".."
                && s.bytes()
                    .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'_' | b'+'))
        };
        let short_commit: String = commit
            .chars()
            .take(MANAGED_RELEASE_COMMIT_PREFIX_LEN)
            .collect();
        ensure!(
            valid_token(version) && valid_token(&short_commit),
            "release version/commit contains unsafe path characters"
        );
        Ok(format!("{version}-{short_commit}"))
    }

    fn managed_release_matches_source(
        &self,
        dest: &Path,
        expected_info: &Value,
        source_files: &[(PathBuf, String)],
    ) -> bool {
        let Ok((dest_canon, dest_info)) = self.bundle(dest) else {
            return false;
        };
        if dest_info != *expected_info {
            return false;
        }
        let Ok(dest_paths) = crate::common::regular_tree(&dest_canon) else {
            return false;
        };
        if dest_paths.len() != source_files.len() {
            return false;
        }
        let signed_ok = self.boot_node()
            && self.node_signing_identity().ok().is_some_and(|identity| {
                self.verify_release_signatures(&dest_canon, &identity)
                    .is_ok()
            });
        for (rel, expected_hash) in source_files {
            let candidate = dest_canon.join(rel);
            let Ok(actual_hash) = crate::common::digest(&candidate) else {
                return false;
            };
            if actual_hash != *expected_hash {
                let signed_binary = signed_ok
                    && (rel == Path::new("bin/steve-node") || rel == Path::new("bin/steve"));
                if !signed_binary {
                    return false;
                }
            }
        }
        true
    }

    /// Ensure an update release lives under `<home>/releases/<version>-<commit8>`, copying and
    /// verifying external bundles before switching so deleting the extracted installer directory
    /// cannot break the running service.
    pub(super) fn ensure_managed_release(
        &self,
        source: &Path,
        info: &Value,
    ) -> Result<(PathBuf, Value)> {
        let releases = self.home.join("releases");
        fs::create_dir_all(&self.home)?;
        directory(&releases)?;
        let releases_canon = fs::canonicalize(&releases)?;
        if source.parent() == Some(releases_canon.as_path()) {
            return Ok((source.to_path_buf(), info.clone()));
        }
        let source_files = crate::common::regular_tree(source)?
            .into_iter()
            .map(|path| -> Result<(PathBuf, String)> {
                let rel = path.strip_prefix(source)?.to_path_buf();
                let hash = crate::common::digest(&path)?;
                Ok((rel, hash))
            })
            .collect::<Result<Vec<_>>>()?;

        let dir_name = Self::managed_release_dir_name(info)?;
        let dest = releases_canon.join(&dir_name);
        if exists(&dest) {
            ensure!(
                !fs::symlink_metadata(&dest)?.file_type().is_symlink(),
                "managed release path cannot be a symlink: {}",
                dest.display()
            );
            if self.managed_release_matches_source(&dest, info, &source_files) {
                return self.bundle(&dest);
            }
        }

        let staging = releases_canon.join(format!(".update-{}", Uuid::new_v4().simple()));
        directory(&staging)?;
        let copy_result = (|| -> Result<PathBuf> {
            for (rel, expected_hash) in &source_files {
                let src_file = source.join(rel);
                let dst_file = staging.join(rel);
                if let Some(parent) = dst_file.parent() {
                    crate::common::private_dir(parent)?;
                }
                let mut input = OpenOptions::new()
                    .read(true)
                    .custom_flags(libc::O_NOFOLLOW)
                    .open(&src_file)?;
                let meta = input.metadata()?;
                ensure!(
                    meta.is_file(),
                    "release source changed during copy: {}",
                    src_file.display()
                );
                let mode = if meta.permissions().mode() & 0o111 != 0 {
                    0o755
                } else {
                    0o644
                };
                let mut output = OpenOptions::new()
                    .create_new(true)
                    .write(true)
                    .mode(mode)
                    .custom_flags(libc::O_NOFOLLOW)
                    .open(&dst_file)?;
                std::io::copy(&mut input, &mut output)?;
                output.set_permissions(fs::Permissions::from_mode(mode))?;
                output.sync_all()?;
                ensure!(
                    crate::common::digest(&dst_file)? == *expected_hash,
                    "copied release file digest mismatch: {}",
                    rel.display()
                );
            }
            let (_, verified_info) = self
                .bundle(&staging)
                .context("copied release failed verification before switch")?;
            ensure!(
                verified_info == *info,
                "copied release build identity differs from source"
            );

            let active_release = configured_release(&self.record).map(PathBuf::from);
            let previous_release = self.record["previous"].as_str().map(PathBuf::from);
            let dest_in_use = active_release.as_deref() == Some(dest.as_path())
                || previous_release.as_deref() == Some(dest.as_path());
            let final_dest = if !exists(&dest) {
                fs::rename(&staging, &dest)?;
                File::open(&releases_canon)?.sync_all()?;
                dest
            } else if dest_in_use {
                let unique = releases_canon.join(format!(
                    "{dir_name}-{}",
                    &Uuid::new_v4().simple().to_string()[..MANAGED_RELEASE_COMMIT_PREFIX_LEN]
                ));
                fs::rename(&staging, &unique)?;
                File::open(&releases_canon)?.sync_all()?;
                unique
            } else {
                let retired = releases_canon.join(format!(".replace-{}", Uuid::new_v4().simple()));
                fs::rename(&dest, &retired)?;
                if let Err(err) = fs::rename(&staging, &dest) {
                    let _ = fs::rename(&retired, &dest);
                    return Err(err.into());
                }
                File::open(&releases_canon)?.sync_all()?;
                let _ = fs::remove_dir_all(&retired);
                dest
            };
            Ok(final_dest)
        })();
        if exists(&staging) {
            let _ = fs::remove_dir_all(&staging);
        }
        let managed = copy_result?;
        self.bundle(&managed)
    }

    pub(super) fn resolve_user_shell(&self) -> String {
        if let Some(shell) = std::env::var_os("SHELL")
            .map(PathBuf::from)
            .filter(|p| p.is_absolute() && is_executable_file(p))
            && let Some(text) = shell.to_str()
        {
            return text.to_owned();
        }
        // SAFETY: `getpwuid` returns the account record for our current UID; copy immediately.
        let pw = unsafe { libc::getpwuid(libc::getuid()) };
        if !pw.is_null() && !unsafe { (*pw).pw_shell }.is_null() {
            let cstr = unsafe { std::ffi::CStr::from_ptr((*pw).pw_shell) };
            if let Ok(text) = cstr.to_str() {
                let p = Path::new(text);
                if p.is_absolute() && is_executable_file(p) {
                    return text.to_owned();
                }
            }
        }
        if self.system == System::Darwin && is_executable_file(Path::new("/bin/zsh")) {
            "/bin/zsh".to_owned()
        } else {
            "/bin/sh".to_owned()
        }
    }

    pub(super) fn probe_login_shell_path(&self) -> Option<(String, String)> {
        let shell = self.resolve_user_shell();
        let script = login_shell_path_script(Path::new(&shell));
        let out = self
            .command(vec![shell.clone(), "-l".into(), "-c".into(), script], false)
            .ok()?;
        if out.code != 0 {
            return None;
        }
        let path = extract_login_shell_path(&out.stdout)?;
        Some((shell, path))
    }

    pub(super) fn resolve_service_path(&self) -> ServicePath {
        let captured_at = utc_now_rfc3339();
        if let Some((shell, path)) = self.probe_login_shell_path() {
            return ServicePath {
                path,
                source: ServicePathSource::LoginShell,
                shell: Some(shell),
                captured_at,
            };
        }
        if let Ok(path) = std::env::var("PATH")
            && !path.trim().is_empty()
        {
            return ServicePath {
                path,
                source: ServicePathSource::ProcessEnv,
                shell: None,
                captured_at,
            };
        }
        let default = if self.system == System::Darwin {
            DARWIN_DEFAULT_SERVICE_PATH
        } else {
            LINUX_DEFAULT_SERVICE_PATH
        };
        ServicePath {
            path: default.to_owned(),
            source: ServicePathSource::DefaultFallback,
            shell: None,
            captured_at,
        }
    }

    pub(super) fn configured_service_path(&self) -> Option<String> {
        if exists(&self.config) {
            if self.system == System::Darwin {
                if let Ok(bytes) = self.definition_bytes()
                    && let Ok(def) = plist::from_bytes::<Value>(&bytes)
                    && let Some(path) = def["EnvironmentVariables"]["PATH"]
                        .as_str()
                        .filter(|s| !s.is_empty())
                {
                    return Some(path.to_owned());
                }
            } else if let Ok(bytes) = regular(&self.config)
                && let Ok(content) = std::str::from_utf8(&bytes)
            {
                for line in content.lines() {
                    if let Some(assignments) = line.strip_prefix("Environment=")
                        && let Some(items) = shlex::split(assignments)
                    {
                        for item in items {
                            if let Some(value) = item.strip_prefix("PATH=")
                                && !value.is_empty()
                            {
                                return Some(value.replace("%%", "%"));
                            }
                        }
                    }
                }
            }
        }
        self.record["service_env"]["path"]
            .as_str()
            .filter(|s| !s.is_empty())
            .map(str::to_owned)
    }

    fn refresh_env_command(&self) -> String {
        let default_home = self.user_home.join(if self.component == "hub" {
            ".steve"
        } else {
            ".steve-node"
        });
        let default_label = format!("io.github.infranite.steve.{}", self.component);
        let mut cmd = format!("steve {} refresh-env", self.component);
        if self.home != default_home
            && let Ok(home) = text(&self.home)
        {
            cmd.push_str(&format!(" --home {}", shell_quote(&home)));
        }
        if self.label != default_label {
            cmd.push_str(&format!(" --label {}", shell_quote(&self.label)));
        }
        cmd
    }

    fn doctor_command_diagnostics(
        &self,
        service_path: &str,
        login_shell_path: &str,
    ) -> Vec<ServiceCommandDiagnostic> {
        let mut diagnostics = Vec::new();
        let mut checked = std::collections::BTreeSet::new();
        let refresh_command = self.refresh_env_command();

        for builtin in steve_harness_spec::builtins() {
            let mut candidates = vec![builtin.cli_bin];
            candidates.extend(builtin.detect_bins.iter().copied());
            let in_service = candidates
                .iter()
                .any(|bin| which_in_path(bin, service_path).is_some());
            if in_service {
                continue;
            }
            if let Some((bin, found)) = candidates
                .iter()
                .find_map(|bin| which_in_path(bin, login_shell_path).map(|p| (*bin, p)))
                && checked.insert((builtin.id.to_owned(), bin.to_owned()))
            {
                diagnostics.push(ServiceCommandDiagnostic {
                    reason: ServiceEnvDiagnosticReason::CommandNotInServicePath,
                    harness: builtin.id.to_owned(),
                    command: bin.to_owned(),
                    service_path: service_path.to_owned(),
                    login_shell_path: found.to_string_lossy().into_owned(),
                    refresh_command: refresh_command.clone(),
                });
            }
        }

        if let Ok(harness_cfg) = read_json(&self.home.join("harness.json"))
            && let Some(adapters) = harness_cfg["adapters"].as_object()
        {
            for (harness, entry) in adapters {
                let raw_cmd = entry
                    .as_str()
                    .or_else(|| entry.get("command").and_then(Value::as_str));
                let Some(cmd) = raw_cmd.filter(|c| !c.is_empty()) else {
                    continue;
                };
                let path = Path::new(cmd);
                if path.is_absolute() || path.components().count() > 1 {
                    continue;
                }
                if which_in_path(cmd, service_path).is_some() {
                    continue;
                }
                if let Some(found) = which_in_path(cmd, login_shell_path)
                    && checked.insert((harness.clone(), cmd.to_owned()))
                {
                    diagnostics.push(ServiceCommandDiagnostic {
                        reason: ServiceEnvDiagnosticReason::CommandNotInServicePath,
                        harness: harness.clone(),
                        command: cmd.to_owned(),
                        service_path: service_path.to_owned(),
                        login_shell_path: found.to_string_lossy().into_owned(),
                        refresh_command: refresh_command.clone(),
                    });
                }
            }
        }

        diagnostics
    }

    fn definition(&self, release: &Path, path_env: &str) -> Result<Vec<u8>> {
        let mut env = BTreeMap::from([
            ("STEVE_HOME", text(&self.home)?),
            ("PATH", path_env.to_owned()),
        ]);
        let command = if self.component == "hub" {
            let script = if self.signed {
                "run.sh"
            } else {
                "run-managed.sh"
            };
            ensure!(
                release.join("ops").join(script).is_file(),
                "release has no runtime ops/{script}"
            );
            vec![
                if self.signed {
                    "/bin/zsh".into()
                } else {
                    "/bin/sh".into()
                },
                text(&self.home.join("current/ops").join(script))?,
            ]
        } else {
            if read_json(&self.home.join("node/node.json"))?["loopback"] == true {
                env.insert("STEVE_NODE_LOOPBACK_PLAINTEXT", "1".into());
            }
            vec![text(&release.join("bin/steve-node"))?, "run".into()]
        };
        if self.system == System::Darwin {
            let definition = json!({"Label":self.label,"ProgramArguments":command,"EnvironmentVariables":env,
                "RunAtLoad":true,"KeepAlive":true,"ExitTimeOut":600,
                "StandardOutPath":text(&self.home.join("logs").join(format!("{}.log",self.component)))?,
                "StandardErrorPath":text(&self.home.join("logs").join(format!("{}.log",self.component)))?});
            let mut output = Vec::new();
            plist::to_writer_xml(&mut output, &definition)?;
            return Ok(output);
        }
        let mut lines = vec![
            "[Unit]".into(),
            format!("Description=Steve {}", self.component),
            "[Service]".into(),
            "Type=simple".into(),
            format!(
                "ExecStart={}",
                command
                    .iter()
                    .map(|s| unit_quote(s, true))
                    .collect::<Result<Vec<_>>>()?
                    .join(" ")
            ),
        ];
        for (k, v) in env {
            lines.push(format!(
                "Environment={}",
                unit_quote(&format!("{k}={v}"), false)?
            ));
        }
        lines.extend(
            [
                "Restart=on-failure",
                "RestartSec=3",
                "KillMode=control-group",
                "TimeoutStopSec=600",
                "[Install]",
                "WantedBy=default.target",
                "",
            ]
            .map(str::to_owned),
        );
        Ok(lines.join("\n").into_bytes())
    }

    fn install(&mut self, release: &Path, update: bool, wait: u64) -> Result<Value> {
        let (source, info) = self.bundle(release)?;
        ensure!(
            !(update && self.component == "hub"),
            "Hub updates use Settings > About or steve upgrade for task drain, backup and schema checks"
        );
        let current = self.home.join("current");
        ensure!(
            self.component != "hub" || !current.exists() || resolve(&current)? == source,
            "current Hub already exists; use Settings > About or steve upgrade to update it"
        );
        ensure!(
            self.component != "hub" || self.system != System::Darwin || self.signed,
            "new macOS Hub needs explicit Keychain setup: run steve setup first"
        );
        let mut state = self.state()?;
        let was_running = update && self.component == "node" && state["running"] == true;
        if update && self.component == "node" {
            ensure!(
                state["running"].is_boolean(),
                "service is running or its state is unknown{}; no files changed. Finish Node sessions, verify Devices, then stop the service before update; --wait only waits for a stopped service",
                state["diagnostic"]
                    .as_str()
                    .map(|d| format!(" ({d})"))
                    .unwrap_or_default()
            );
        }
        let (release, info) = if update && self.component == "node" {
            self.ensure_managed_release(&source, &info)?
        } else {
            (source, info)
        };
        let service_path = self.resolve_service_path();
        let data = self.definition(&release, &service_path.path)?;
        let same = self.config.is_file() && fs::read(&self.config)? == data;
        if !same {
            if was_running {
                self.systemctl(&["stop", &self.unit()], true)?;
                state = self.state()?;
            }
            let stop_wait = if was_running {
                wait.max(LINUX_UPDATE_STOP_WAIT_MIN_SECS)
            } else {
                wait
            };
            let deadline = Instant::now()
                .checked_add(Duration::from_secs(stop_wait))
                .context("wait is too large")?;
            while (state["running"] == true
                || (self.system == System::Darwin && state["loaded"] == true))
                && Instant::now() < deadline
            {
                std::thread::sleep(
                    Duration::from_secs(1).min(deadline.saturating_duration_since(Instant::now())),
                );
                state = self.state()?;
            }
            ensure!(
                state["running"] == false
                    && (self.system != System::Darwin || state["loaded"] == false),
                "service is running or its state is unknown{}; no files changed. Finish Node sessions, verify Devices, then stop the service before update; --wait only waits for a stopped service",
                state["diagnostic"]
                    .as_str()
                    .map(|d| format!(" ({d})"))
                    .unwrap_or_default()
            );
        }
        fs::create_dir_all(&self.home)?;
        let logs = self.home.join("logs");
        if !logs.exists() {
            fs::create_dir(&logs)?;
            fs::set_permissions(&logs, fs::Permissions::from_mode(0o700))?;
        }
        if self.component == "hub" && !current.exists() {
            ensure!(
                !exists(&current),
                "current is a broken symlink; inspect it before installation"
            );
            symlink(&release, &current)?;
        }
        if self.component == "hub" {
            let web = self.home.join("web/current");
            fs::create_dir_all(web.parent().unwrap())?;
            if !exists(&web) {
                symlink(release.join("web/dist"), &web)?;
            }
        }
        if !same {
            if self.config.exists() {
                atomic(
                    &self.config.with_extension(format!(
                        "{}.previous",
                        self.config.extension().unwrap().to_string_lossy()
                    )),
                    &fs::read(&self.config)?,
                )?;
            }
            atomic(&self.config, &data)?;
        }
        let previous = if update && !same {
            self.record["release"].clone()
        } else {
            self.record["previous"].clone()
        };
        let service_env = service_path.to_json();
        self.record = json!({"component":self.component,"label":self.label,"release":text(&release)?,"build":info,"previous":previous,"service_env":service_env});
        atomic(&self.record_path, &json_bytes(&self.record)?)?;
        if self.system == System::Linux {
            self.systemctl(&["daemon-reload"], true)?;
            self.systemctl(&["enable", &self.unit()], true)?;
            if !same && was_running {
                self.systemctl(&["start", &self.unit()], true)?;
            }
        }
        Ok(
            json!({"installed":true,"changed":!same,"started":was_running,"release":text(&release)?,"build":info,"service_env":service_env}),
        )
    }

    fn refresh_env(&mut self) -> Result<Value> {
        self.require_registration("refresh-env")?;
        ensure!(
            self.config.is_file(),
            "service is not installed; run steve setup first"
        );
        self.check_owner()?;
        let state = self.state()?;
        ensure!(
            state["running"].is_boolean(),
            "service state is unknown{}; no files changed",
            state["diagnostic"]
                .as_str()
                .map(|d| format!(" ({d})"))
                .unwrap_or_default()
        );
        let release_raw = if self.component == "hub" {
            self.home.join("current")
        } else {
            PathBuf::from(
                configured_release(&self.record).context("service has no configured release")?,
            )
        };
        let (release, info) = self.bundle(&release_raw)?;
        let service_path = self.resolve_service_path();
        let data = self.definition(&release, &service_path.path)?;
        let current_bytes = regular(&self.config)?;
        let same = current_bytes == data;
        let was_running = state["running"] == true;
        if !same {
            atomic(
                &self.config.with_extension(format!(
                    "{}.previous",
                    self.config.extension().unwrap().to_string_lossy()
                )),
                &current_bytes,
            )?;
            atomic(&self.config, &data)?;
        }
        let service_env = service_path.to_json();
        self.record["service_env"] = service_env.clone();
        atomic(&self.record_path, &json_bytes(&self.record)?)?;
        let mut restarted = false;
        if !same {
            if self.system == System::Linux {
                self.systemctl(&["daemon-reload"], true)?;
                self.systemctl(&["enable", &self.unit()], true)?;
                if was_running {
                    self.systemctl(&["restart", &self.unit()], true)?;
                    restarted = true;
                }
            } else if was_running {
                self.action("restart")?;
                restarted = true;
            }
        }
        Ok(json!({
            "installed": true,
            "changed": !same,
            "restarted": restarted,
            "started": was_running,
            "release": text(&release)?,
            "build": info,
            "service_env": service_env,
        }))
    }

    fn require_registration(&self, action: &str) -> Result<()> {
        // Re-read before any manager query: a cached receipt or a default-label unit
        // elsewhere in the user's account does not authorize acting on this home.
        let context = || {
            format!(
                "cannot {action} {}: matching service registration required at {}; run steve setup {} --home {} with the original service label",
                self.component,
                self.record_path.display(),
                self.component,
                self.home.display()
            )
        };
        let bytes = maintenance_file(&self.record_path).with_context(context)?;
        let record: Value = serde_json::from_slice(&bytes).with_context(context)?;
        ensure!(
            record["component"] == self.component && record["label"].as_str() == Some(&self.label),
            "{}; expected component {} and label {}, found component {:?} and label {:?}",
            context(),
            self.component,
            self.label,
            record["component"],
            record["label"]
        );
        Ok(())
    }

    fn action(&self, action: &str) -> Result<Value> {
        self.require_registration(action)?;
        let state = self.state()?;
        if action == "stop" && state["running"] == false && state["loaded"] == false {
            if self.component == "hub" {
                self.stop_local_node()?;
            }
            return Ok(state);
        }
        ensure!(
            self.config.is_file(),
            "service is not installed; run steve setup first"
        );
        if self.system == System::Darwin {
            let domain = format!("gui/{}", unsafe { libc::getuid() });
            let target = format!("{domain}/{}", self.label);
            if matches!(action, "stop" | "restart") && state["loaded"] == true {
                self.command(
                    vec!["launchctl".into(), "bootout".into(), target.clone()],
                    true,
                )?;
                /* bootout returns before launchd has let the job go; a bootstrap then fails and a
                 * restart leaves the service stopped */
                if action == "restart" {
                    let until = Instant::now() + Duration::from_secs(15);
                    while self.state()?["loaded"] == true && Instant::now() < until {
                        std::thread::sleep(Duration::from_millis(200));
                    }
                }
            }
            if matches!(action, "start" | "restart") {
                if action == "start" && state["loaded"] == true {
                    if state["running"] != true {
                        self.command(vec!["launchctl".into(), "kickstart".into(), target], true)?;
                    }
                } else {
                    self.command(
                        vec![
                            "launchctl".into(),
                            "bootstrap".into(),
                            domain,
                            text(&self.config)?,
                        ],
                        true,
                    )?;
                }
            }
        } else {
            self.systemctl(&[action, &self.unit()], true)?;
        }
        if action == "stop" && self.component == "hub" {
            self.stop_local_node()?;
        }
        self.status(None, false)
    }

    /// The Hub's own Node in a process of its own (`STEVE_NODE_MODE=service`) outlives a Hub restart
    /// or upgrade so its turns keep running; a stop stops it too. Only the process this home's
    /// `steve-hub --local-node` wrote into its pid file.
    fn stop_local_node(&self) -> Result<()> {
        let data = fs::read_to_string(self.home.join("steve.env"))
            .ok()
            .and_then(|env| {
                env.lines().find_map(|l| {
                    l.trim()
                        .strip_prefix("STEVE_DATA_DIR=")
                        .map(|v| PathBuf::from(v.trim().trim_matches(['\'', '"'])))
                })
            })
            .unwrap_or_else(|| self.home.join("data"));
        let Some(pid) = fs::read_to_string(data.join("node/service.pid"))
            .ok()
            .and_then(|p| p.trim().parse::<u32>().ok())
            .filter(|p| *p > 1)
        else {
            return Ok(());
        };
        let pid = pid.to_string();
        let ours = |out: &Output| {
            let cmd = out.stdout.trim();
            out.code == 0
                && cmd.ends_with("steve-hub --local-node")
                && fs::canonicalize(&self.home).is_ok_and(|h| {
                    cmd.starts_with(&*h.to_string_lossy())
                        || cmd.starts_with(&*self.home.to_string_lossy())
                })
        };
        let ps = || {
            self.command(
                vec![
                    "ps".into(),
                    "-p".into(),
                    pid.clone(),
                    "-o".into(),
                    "command=".into(),
                ],
                false,
            )
        };
        if !ours(&ps()?) {
            return Ok(());
        }
        self.command(vec!["kill".into(), pid.clone()], false)?;
        let until = Instant::now() + Duration::from_secs(15);
        while Instant::now() < until {
            if !ours(&ps()?) {
                return Ok(());
            }
            std::thread::sleep(Duration::from_millis(250));
        }
        bail!("the Hub's local Node (pid {pid}) did not stop; stop it before uninstalling")
    }

    fn stopped(&self) -> Result<()> {
        let state = self.state()?;
        ensure!(
            state["running"] == false
                && (self.system != System::Darwin || state["loaded"] == false),
            "service is running, loaded, or its state is unknown{}; no service was stopped. Finish sessions, verify Hub Devices, then explicitly stop before uninstall",
            state["diagnostic"]
                .as_str()
                .map(|d| format!(" ({d})"))
                .unwrap_or_default()
        );
        Ok(())
    }

    fn fresh_unit(&self) -> Result<()> {
        let output = self.systemctl(
            &[
                "show",
                &self.unit(),
                "--property=NeedDaemonReload",
                "--value",
            ],
            true,
        )?;
        ensure!(
            output.stdout.trim() == "no",
            "unit definitions need reload or freshness is unknown; inspect them and run systemctl --user daemon-reload before retrying uninstall"
        );
        Ok(())
    }

    fn isolated_unit(&self, definition: &[u8]) -> Result<String> {
        self.fresh_unit()?;
        let output = self.systemctl(&["cat", "--no-pager", &self.unit()], true)?;
        ensure!(
            output.stderr.trim().is_empty()
                && output.stdout.contains(std::str::from_utf8(definition)?),
            "cannot verify the complete owned unit and its drop-ins; no unit was disabled"
        );
        let logical = regex::Regex::new(r"\\\n[ \t]*")?.replace_all(&output.stdout, "");
        ensure!(
            !regex::Regex::new(r"(?m)^[ \t]*(?:Also[ \t]*=|\.include(?:[ \t]|$))")?
                .is_match(&logical),
            "unit or drop-in has Also= or .include; uninstall will not disable other services"
        );
        self.fresh_unit()?;
        Ok(output.stdout)
    }

    fn remove_uninstall_definition(
        &self,
        definition: Option<Vec<u8>>,
        effective: Option<String>,
    ) -> Result<()> {
        if let Some(definition) = definition {
            self.stopped()?;
            ensure!(
                self.definition_bytes()? == definition,
                "service definition changed; original backup is retained"
            );
            if self.system == System::Linux {
                ensure!(
                    Some(self.isolated_unit(&definition)?) == effective,
                    "unit or drop-ins changed during uninstall; original backup is retained"
                );
                self.systemctl(&["disable", &self.unit()], true)?;
                self.stopped()?;
                ensure!(
                    regular(&self.config)? == definition,
                    "service definition changed after disable; original backup is retained"
                );
            }
            if self.boot_node() {
                self.remove_boot_definition()?;
            } else {
                fs::remove_file(&self.config)?;
            }
        }
        if self.system == System::Linux {
            self.systemctl(&["daemon-reload"], true)?;
        }
        self.stopped()?;
        ensure!(
            !exists(&self.config),
            "service definition reappeared; inspect it before retrying uninstall"
        );
        Ok(())
    }

    fn resume_uninstall_archive(
        &self,
        retired: &Path,
        pending: &Value,
        definition: Option<&[u8]>,
    ) -> Result<PathBuf> {
        let name = pending["archive"].as_str().unwrap_or_default();
        let suffix = name.strip_prefix(&format!("{}-", self.component));
        ensure!(
            suffix.is_some_and(|s| s.len() == 32
                && s.bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)))
                && pending["config"] == text(&self.config)?,
            "uninstall receipt does not identify this service archive"
        );
        let archive = retired.join(name);
        directory(&archive)?;
        let saved = regular(&archive.join("definition"))?;
        ensure!(
            pending["definition_sha256"] == digest(&saved),
            "uninstall backup changed; no definition will be removed"
        );
        if !pending["record_sha256"].is_null() {
            ensure!(
                pending["record_sha256"] == digest(&regular(&archive.join("record.json"))?),
                "uninstall record backup changed; inspect the retained archive"
            );
        }
        ensure!(
            definition.is_none_or(|bytes| bytes == saved.as_slice()),
            "service definition changed during uninstall; original backup is retained"
        );
        Ok(archive)
    }

    fn status(&self, candidate: Option<&Path>, doctor: bool) -> Result<Value> {
        let state = self.state()?;
        let release = if self.component == "hub" {
            self.home.join("current")
        } else {
            configured_release(&self.record)
                .map(PathBuf::from)
                .unwrap_or_else(|| self.home.join("current"))
        };
        let mut report = json!({"component":self.component,"home":text(&self.home)?,"manager":if self.system == System::Darwin {"launchd"}else{"systemd-user"},
            "label":self.label,"service":state,"configured_release":text(&resolve(&release)?)?,"configured_build":self.record["build"],"connection":"unknown", "running_build":null,
            "runtime_identity":"unknown; verify Hub /healthz or Node handshake in Devices"});
        match self.bundle(&release) {
            Ok((_, info)) => report["configured_build"] = info,
            Err(e) => report["build_error"] = json!(e.to_string()),
        }
        if self.component == "node" {
            let pairing = read_json(&self.home.join("node/node.json"))?;
            report["pairing"] = json!({"configured":pairing.as_object().is_some_and(|o|!o.is_empty()),"identity_present":self.home.join("node/identity.key").is_file(),
                "node_id":pairing["node_id"],"hub_url":pairing["hub_url"]});
            report["sessions"] = json!("unknown; inspect Hub Devices before stopping");
        }
        let configured_path = self.configured_service_path();
        let mut service_env = self
            .record
            .get("service_env")
            .and_then(Value::as_object)
            .cloned()
            .unwrap_or_default();
        if let Some(path) = &configured_path {
            service_env.insert("path".into(), json!(path));
        }
        if !service_env.is_empty() {
            report["service_env"] = Value::Object(service_env);
        }
        if let Some(candidate) = candidate {
            let (path, info) = self.bundle(candidate)?;
            report["candidate"] = json!({"release":text(&path)?,"build":info});
            report["update_command"] = json!(if self.component == "node" {
                format!(
                    "steve node update --home {} --label {} --release {}",
                    shell_quote(&text(&self.home)?),
                    shell_quote(&self.label),
                    shell_quote(&text(&path)?)
                )
            } else {
                "Settings > About".into()
            });
        }
        if self.component == "hub" && self.system == System::Darwin && self.signed {
            let supported = self.home == resolve(&self.user_home.join(".steve"))?;
            report["automatic_upgrade"] = json!({"supported":supported,"entry":if supported {"Settings > About or steve upgrade"}else{"explicit terminal preparation/deployment"},
                "reason":if supported {None}else{Some("signed runner grants Keychain access only to the default ~/.steve home; custom homes require explicit terminal preparation/deployment with their original signing identity")}});
            if supported {
                report["upgrade_check_command"] = json!("steve upgrade check");
                if candidate.is_some() {
                    report["update_command"] = json!("Settings > About or steve upgrade");
                }
            }
        }
        if doctor {
            let mut issues = Vec::new();
            if state["installed"] != true {
                issues.push("service is not installed".to_owned());
            }
            if state["running"] != true {
                issues.push("service is stopped or state is unknown".to_owned());
            }
            if let Some(error) = report["build_error"].as_str() {
                issues.push(error.into());
            }
            if self.component == "node" {
                for name in ["roots.json", "policy.json", "harness.json"] {
                    if read_json(&self.home.join(name)).is_err() {
                        issues.push(format!("{name} is not valid JSON"));
                    }
                }
                if report["pairing"]["configured"] != true {
                    issues.push(
                        "Node is not paired; install never creates or approves a pairing".into(),
                    );
                }
                if report["pairing"]["identity_present"] != true {
                    issues.push("Node identity is missing".into());
                }
                issues
                    .push("connection and active sessions require Hub Devices verification".into());
            }
            let login_env = self.probe_login_shell_path();
            let mut command_diagnostics = Vec::new();
            if let Some((login_shell, login_path)) = &login_env {
                let mut env_obj = report
                    .get("service_env")
                    .and_then(Value::as_object)
                    .cloned()
                    .unwrap_or_default();
                env_obj.insert("login_shell".into(), json!(login_shell));
                env_obj.insert("login_shell_path".into(), json!(login_path));
                if let Some(svc_path) = &configured_path {
                    env_obj.insert("path_outdated".into(), json!(svc_path != login_path));
                    if self.component == "node" {
                        command_diagnostics = self.doctor_command_diagnostics(svc_path, login_path);
                    }
                }
                report["service_env"] = Value::Object(env_obj);
            }
            if !command_diagnostics.is_empty() {
                report["command_diagnostics"] = json!(command_diagnostics);
            }
            if report["automatic_upgrade"]["supported"] == false {
                issues.push(
                    report["automatic_upgrade"]["reason"]
                        .as_str()
                        .unwrap_or_default()
                        .into(),
                );
            }
            report["diagnostics"] = json!(issues);
        }
        Ok(report)
    }

    pub fn dispatch(&mut self, action: &str, release: Option<&Path>, wait: u64) -> Result<Value> {
        if self.boot_node() {
            return self.boot_dispatch(action, release, wait);
        }
        match action {
            "install" | "update" => {
                let release = release
                    .map(Path::to_owned)
                    .or_else(|| self.record["release"].as_str().map(PathBuf::from))
                    .unwrap_or_else(|| self.home.join("current"));
                self.install(&release, action == "update", wait)
            }
            "status" | "doctor" => self.status(release, action == "doctor"),
            "refresh-env" => self.refresh_env(),
            "uninstall" => self.uninstall(),
            "start" | "stop" | "restart" => self.action(action),
            _ => bail!("unsupported service action: {action}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    #[derive(Default)]
    struct Manager {
        running: bool,
        loaded: bool,
        unknown: bool,
        show_failure: bool,
        failure: Option<String>,
        fail_after_removal: bool,
        calls: Vec<Vec<String>>,
        config: PathBuf,
        dropin: String,
        pending_dropin: String,
        stale: Option<String>,
        gui_domain_unsupported: bool,
        print_error: Option<(i32, String)>,
        node_signatures: BTreeMap<PathBuf, String>,
        cert_sha1: Option<String>,
        signing_failure: bool,
        lsof_pids: Option<Vec<u32>>,
        ps_commands: BTreeMap<u32, String>,
        login_shell_path: Option<String>,
    }

    struct FakeNodeSigner(Arc<Mutex<Manager>>);

    impl boot::NodeSigner for FakeNodeSigner {
        fn ensure_identity(
            &self,
            home: &Path,
            label: &str,
        ) -> Result<crate::signing::NodeSigningIdentity> {
            let identity_path = crate::signing::node_signing_identity_path(home);
            let keychain_path = crate::signing::node_signing_keychain(home);
            let pass_path = crate::signing::node_signing_password_path(home);
            let present = [
                exists(&identity_path),
                exists(&keychain_path),
                exists(&pass_path),
            ];
            if present.iter().all(|v| *v) {
                return self.load_identity(home, label);
            }
            ensure!(
                present.iter().all(|v| !*v),
                "incomplete Node signing identity"
            );
            let sha1 = self
                .0
                .lock()
                .unwrap()
                .cert_sha1
                .clone()
                .unwrap_or_else(|| "11".repeat(20));
            crate::signing::private_dir(&home.join("signing"))?;
            crate::signing::private_dir(&home.join("secrets"))?;
            crate::signing::private_dir(&home.join(crate::signing::NODE_KEYSTORE_DIR))?;
            atomic(&keychain_path, b"fake-keychain")?;
            atomic(
                &pass_path,
                data_encoding::BASE64.encode(&[0x42; 32]).as_bytes(),
            )?;
            let identity = crate::signing::NodeSigningIdentity {
                label: label.to_owned(),
                cli_identifier: crate::signing::NODE_CLI_IDENTIFIER.to_owned(),
                node_identifier: crate::signing::NODE_RUNTIME_IDENTIFIER.to_owned(),
                certificate_name: crate::signing::NODE_CERTIFICATE_NAME.to_owned(),
                certificate_sha1: sha1,
            };
            crate::signing::exclusive_json(&identity_path, &serde_json::to_value(&identity)?)?;
            Ok(identity)
        }

        fn load_identity(
            &self,
            home: &Path,
            label: &str,
        ) -> Result<crate::signing::NodeSigningIdentity> {
            crate::signing::load_node_identity(home, label)
        }

        fn sign_binary(
            &self,
            home: &Path,
            binary: &Path,
            identity: &crate::signing::NodeSigningIdentity,
            identifier: &str,
        ) -> Result<()> {
            let req =
                crate::signing::self_signed_requirement(identifier, &identity.certificate_sha1);
            if self.verify_binary(binary, &req).is_ok() {
                return Ok(());
            }
            ensure!(
                !self.0.lock().unwrap().signing_failure,
                "fake local signing failure"
            );
            crate::signing::owned_file(binary)?;
            crate::signing::load_node_identity(home, &identity.label)?;
            self.0
                .lock()
                .unwrap()
                .node_signatures
                .insert(binary.to_owned(), req);
            Ok(())
        }

        fn verify_binary(&self, binary: &Path, requirement: &str) -> Result<()> {
            ensure!(
                self.0
                    .lock()
                    .unwrap()
                    .node_signatures
                    .get(binary)
                    .map(String::as_str)
                    == Some(requirement),
                "candidate executable must satisfy the installation's designated requirement"
            );
            Ok(())
        }

        fn stage_runner(
            &self,
            home: &Path,
            source: &Path,
            destination_dir: &Path,
            identity: &crate::signing::NodeSigningIdentity,
        ) -> Result<PathBuf> {
            directory(destination_dir)?;
            let staged = destination_dir.join(format!(
                ".node-boot-runner.sign-{}",
                Uuid::new_v4().simple()
            ));
            atomic_mode(&staged, &fs::read(source)?, 0o700)?;
            if let Err(err) = self.sign_binary(home, &staged, identity, &identity.cli_identifier) {
                let _ = fs::remove_file(&staged);
                return Err(err);
            }
            Ok(staged)
        }

        fn commit_runner(&self, staged: &Path, destination: &Path) -> Result<()> {
            let parent = destination.parent().context("runner has no parent")?;
            fs::rename(staged, destination)?;
            File::open(parent)?.sync_all()?;
            let mut m = self.0.lock().unwrap();
            if let Some(sig) = m.node_signatures.remove(staged) {
                m.node_signatures.insert(destination.to_owned(), sig);
            }
            Ok(())
        }
    }

    impl Manager {
        fn run(&mut self, args: &[String]) -> Result<Output> {
            self.calls.push(args.to_vec());
            let has = |arg: &str| args.iter().any(|a| a == arg);
            let mut result = Output {
                code: 0,
                stdout: String::new(),
                stderr: String::new(),
            };
            if has("--build-info") {
                result.stdout = fs::read_to_string(
                    Path::new(&args[0])
                        .parent()
                        .unwrap()
                        .parent()
                        .unwrap()
                        .join("release.json"),
                )?;
            } else if self.failure.as_ref().is_some_and(|a| has(a))
                && (!self.fail_after_removal || !self.config.exists())
            {
                result.code = 1;
                result.stderr = "fake manager failure".into();
            } else if args.len() == 4
                && args[1] == "-l"
                && args[2] == "-c"
                && args[3].contains(steve_types::devices::LOGIN_SHELL_PATH_BEGIN)
            {
                if let Some(path) = &self.login_shell_path {
                    result.stdout = format!(
                        "welcome banner\n{}\n{path}\n{}\n",
                        steve_types::devices::LOGIN_SHELL_PATH_BEGIN,
                        steve_types::devices::LOGIN_SHELL_PATH_END
                    );
                } else {
                    result.code = 1;
                }
            } else if args[0] == "/usr/bin/id" {
                assert_eq!(args, &["/usr/bin/id", "-un"]);
                result.stdout = "fixture-node\n".into();
            } else if args[0] == "/usr/sbin/lsof" || args[0] == "lsof" {
                if let Some(pids) = &self.lsof_pids {
                    result.stdout = pids
                        .iter()
                        .map(u32::to_string)
                        .collect::<Vec<_>>()
                        .join("\n")
                        + "\n";
                } else {
                    result.code = 1;
                }
            } else if args[0] == "/bin/ps" || args[0] == "ps" {
                let pid = args.get(2).and_then(|s| s.parse::<u32>().ok());
                if let Some(cmd) = pid.and_then(|p| self.ps_commands.get(&p)) {
                    result.stdout = format!("{cmd}\n");
                } else {
                    result.code = 1;
                }
            } else if args.starts_with(&["/usr/bin/sudo".into(), "/bin/rm".into()]) {
                assert_eq!(Path::new(&args[3]), self.config);
                fs::remove_file(&self.config)?;
            } else if has("print") {
                if let Some((code, stderr)) = &self.print_error {
                    result.code = *code;
                    result.stderr = stderr.clone();
                } else if self.gui_domain_unsupported
                    && args.get(2).is_some_and(|a| a.starts_with("gui/"))
                {
                    result.code = LAUNCHCTL_ERR_DOMAIN_UNSUPPORTED;
                    result.stderr =
                        "Could not print domain: 125: Domain does not support specified action"
                            .into();
                } else if self.unknown {
                    result.code = 1;
                    result.stderr = "access denied".into();
                } else if !self.loaded {
                    result.code = 113;
                    result.stderr = "Could not find service in domain".into();
                } else {
                    result.stdout = format!(
                        "\tpath = {}\n{}",
                        self.config.display(),
                        if self.running {
                            "\tstate = running\n\tpid = 123\n"
                        } else {
                            "\tstate = waiting\n"
                        }
                    );
                }
            } else if has("--property=NeedDaemonReload") {
                result.stdout = self.stale.clone().unwrap_or_else(|| "no\n".into());
            } else if has("cat") {
                result.stdout = format!(
                    "# {}\n{}{}",
                    self.config.display(),
                    fs::read_to_string(&self.config)?,
                    self.dropin
                );
            } else if has("show") {
                if !self.unknown {
                    result.stdout = format!(
                        "LoadState={}\nActiveState={}\nMainPID={}\nExecMainStatus=0\n",
                        if self.config.exists() {
                            "loaded"
                        } else {
                            "not-found"
                        },
                        if self.running { "active" } else { "inactive" },
                        if self.running { 123 } else { 0 }
                    );
                }
                if self.show_failure {
                    result.code = 1;
                }
            } else if has("daemon-reload") {
                if !self.pending_dropin.is_empty() {
                    self.dropin = std::mem::take(&mut self.pending_dropin);
                }
            } else if args
                .iter()
                .any(|a| matches!(a.as_str(), "bootstrap" | "kickstart" | "start" | "restart"))
            {
                self.loaded = true;
                self.running = true;
            } else if args
                .iter()
                .any(|a| matches!(a.as_str(), "bootout" | "stop"))
            {
                self.loaded = false;
                self.running = false;
            }
            Ok(result)
        }
    }

    struct Fixture {
        _tmp: tempfile::TempDir,
        root: PathBuf,
        home: PathBuf,
        release: PathBuf,
        next: PathBuf,
    }
    impl Fixture {
        fn new() -> Self {
            let tmp = tempfile::tempdir().unwrap();
            let root = fs::canonicalize(tmp.path())
                .unwrap()
                .join("space & quote \" % $");
            fs::create_dir(&root).unwrap();
            let home = root.join("node-home");
            fs::create_dir_all(home.join("node")).unwrap();
            for (name, data) in [
                ("node/node.json", r#"{"node_id":"test","loopback":true}"#),
                ("node/identity.key", "private identity"),
                ("roots.json", "[]"),
                ("policy.json", "{}"),
                ("harness.json", "{}"),
                ("retained.data", "retained data"),
            ] {
                fs::write(home.join(name), data).unwrap();
            }
            let release = Self::bundle(&root, "0.2.0", "a");
            let next = Self::bundle(&root, "0.2.1", "b");
            Self {
                _tmp: tmp,
                root,
                home,
                release,
                next,
            }
        }
        fn bundle(root: &Path, version: &str, commit: &str) -> PathBuf {
            let path = root.join(version);
            fs::create_dir_all(path.join("bin")).unwrap();
            fs::create_dir_all(path.join("web/dist")).unwrap();
            fs::create_dir(path.join("ops")).unwrap();
            let manifest = json!({"format_version":1,"version":version,"commit":commit.repeat(40)});
            fs::write(path.join("release.json"), json_bytes(&manifest).unwrap()).unwrap();
            fs::write(
                path.join("web/dist/release.json"),
                json_bytes(&manifest).unwrap(),
            )
            .unwrap();
            for name in ["steve", "steve-hub", "steve-node"] {
                let binary = path.join("bin").join(name);
                fs::write(&binary, b"fake native binary").unwrap();
                fs::set_permissions(binary, fs::Permissions::from_mode(0o755)).unwrap();
            }
            for name in ["run.sh", "run-managed.sh"] {
                fs::write(path.join("ops").join(name), "runtime").unwrap();
            }
            path
        }
        fn service(&self, system: System, component: &str) -> (Service, Arc<Mutex<Manager>>) {
            self.service_at(system, component, &self.home, None)
        }
        fn service_at(
            &self,
            system: System,
            component: &str,
            home: &Path,
            label: Option<&str>,
        ) -> (Service, Arc<Mutex<Manager>>) {
            let manager = Arc::new(Mutex::new(Manager::default()));
            let other = manager.clone();
            let mut service = Service::with_runner(
                component,
                home,
                label,
                system,
                &self.root,
                Arc::new(move |args| other.lock().unwrap().run(args)),
            )
            .unwrap();
            if service.boot_node() {
                service.config = self
                    .root
                    .join("Library/LaunchDaemons")
                    .join(service.config.file_name().unwrap());
                service.config_uid = unsafe { libc::getuid() };
            }
            let boot_manager = manager.clone();
            service.boot_registrar = Arc::new(move |args, finish| {
                let is_refresh = args.get(4).map(String::as_str) == Some("steve-node-refresh-env");
                let refresh_reload =
                    is_refresh && args.get(8).map(String::as_str) == Some("reload");
                {
                    let mut m = boot_manager.lock().unwrap();
                    m.calls.push(args.to_vec());
                    assert_eq!(args[..3], ["/usr/bin/sudo", "/bin/sh", "-c"]);
                    assert_eq!(m.config, Path::new(&args[6]));
                    ensure!(
                        m.failure.as_deref() != Some("-v"),
                        "fake authorization denied"
                    );
                    if is_refresh && refresh_reload {
                        m.loaded = false;
                        m.running = false;
                    }
                }
                finish()?;
                let mut m = boot_manager.lock().unwrap();
                ensure!(
                    m.failure.as_deref() != Some("/usr/bin/install"),
                    "fake install failure"
                );
                fs::create_dir_all(m.config.parent().unwrap())?;
                fs::write(&m.config, &args[5])?;
                if !is_refresh || refresh_reload {
                    m.loaded = true;
                    m.running = true;
                }
                Ok(())
            });
            service.node_signer = Arc::new(FakeNodeSigner(manager.clone()));
            manager.lock().unwrap().config = service.config.clone();
            (service, manager)
        }
        fn preserved(&self) {
            assert_eq!(
                fs::read_to_string(self.home.join("node/identity.key")).unwrap(),
                "private identity"
            );
            assert_eq!(
                fs::read_to_string(self.home.join("retained.data")).unwrap(),
                "retained data"
            );
        }

        fn maintenance_tools(&self, system: System) -> crate::signing::Tools {
            let mut tools = crate::signing::Tools::default();
            if system == System::Darwin {
                fs::create_dir_all(self.home.join("signing")).unwrap();
                fs::create_dir_all(self.home.join("bin")).unwrap();
                fs::write(
                    self.home.join("signing/identity.json"),
                    json_bytes(&json!({
                        "label":"io.github.infranite.steve.hub",
                        "hub_identifier":"io.github.infranite.steve.hub",
                        "keyhelper_identifier":"io.github.infranite.steve.keyhelper",
                        "vault_service":"io.github.infranite.steve.vault"
                    }))
                    .unwrap(),
                )
                .unwrap();
                crate::signing::tests::executable(
                    &self.home.join("bin/steve-keyhelper"),
                    "#!/bin/sh\nexit 0\n",
                );
                tools.codesign = self.root.join("readonly-codesign");
                // This fixture accepts only signature inspection. No signing or
                // security command can reach an operator's Keychain.
                crate::signing::tests::executable(
                    &tools.codesign,
                    r#"#!/bin/sh
set -eu
case "$1" in
  --verify) test "$#" -eq 3 && test "$2" = --strict ;;
  -dv)
    test "$#" -eq 2
    case "$2" in
      */steve-keyhelper) printf 'Identifier=io.github.infranite.steve.keyhelper\n' >&2 ;;
      */steve-hub) printf 'Identifier=io.github.infranite.steve.hub\n' >&2 ;;
      *) exit 1 ;;
    esac ;;
  *) exit 1 ;;
esac
"#,
                );
            }
            tools
        }
    }

    #[test]
    fn unregistered_or_mismatched_services_never_reach_the_manager() {
        for system in [System::Darwin, System::Linux] {
            for component in ["hub", "node"] {
                for condition in ["missing", "removed", "label", "component", "symlink"] {
                    let f = Fixture::new();
                    let (mut s, manager) = f.service(system, component);
                    fs::create_dir_all(s.config.parent().unwrap()).unwrap();
                    let definition = s
                        .definition(&f.release, LINUX_DEFAULT_SERVICE_PATH)
                        .unwrap();
                    fs::write(&s.config, &definition).unwrap();
                    if condition != "missing" {
                        atomic(
                            &s.record_path,
                            &json_bytes(&json!({"component":component,"label":s.label})).unwrap(),
                        )
                        .unwrap();
                        // Keep a cached matching receipt, then change it before dispatch.
                        s.record = object(&s.record_path).unwrap();
                        match condition {
                            "removed" => fs::remove_file(&s.record_path).unwrap(),
                            "label" | "component" => {
                                let mut record = s.record.clone();
                                record[condition] = "another".into();
                                atomic(&s.record_path, &json_bytes(&record).unwrap()).unwrap();
                            }
                            "symlink" => {
                                let target = f.root.join("foreign-record.json");
                                fs::rename(&s.record_path, &target).unwrap();
                                symlink(target, &s.record_path).unwrap();
                            }
                            _ => unreachable!(),
                        }
                    }
                    for action in ["start", "stop", "restart", "uninstall"] {
                        let error = s.dispatch(action, None, 0).unwrap_err();
                        assert!(
                            format!("{error:#}").contains(s.record_path.to_str().unwrap()),
                            "{system:?} {component} {condition} {action}: {error:#}"
                        );
                        assert!(manager.lock().unwrap().calls.is_empty());
                        assert_eq!(fs::read(&s.config).unwrap(), definition);
                    }
                }
            }
        }
    }

    #[test]
    fn registered_hub_and_node_actions_keep_using_their_own_label() {
        for system in [System::Darwin, System::Linux] {
            for component in ["hub", "node"] {
                let f = Fixture::new();
                if component == "hub" {
                    f.maintenance_tools(system);
                }
                let (mut s, manager) = f.service(system, component);
                s.dispatch("install", Some(&f.release), 0).unwrap();
                if s.boot_node() {
                    s.dispatch("enable-boot", None, 0).unwrap();
                }
                manager.lock().unwrap().calls.clear();
                for action in ["start", "restart", "stop"] {
                    s.dispatch(action, None, 0).unwrap();
                    assert_eq!(manager.lock().unwrap().running, action != "stop");
                }
                s.dispatch("uninstall", None, 0).unwrap();
                assert!(s.record["uninstalled"].as_bool().unwrap());
                let calls = &manager.lock().unwrap().calls;
                if s.boot_node() {
                    let target = format!("system/{}", s.label);
                    assert!(calls.iter().any(
                        |call| call == &["/usr/bin/sudo", "/bin/launchctl", "bootout", &target]
                    ));
                    assert!(calls.iter().any(|call| call
                        == &[
                            "/usr/bin/sudo",
                            "/bin/launchctl",
                            "bootstrap",
                            "system",
                            s.config.to_str().unwrap()
                        ]));
                } else if system == System::Darwin {
                    let target = format!("gui/{}/{}", unsafe { libc::getuid() }, s.label);
                    assert!(
                        calls
                            .iter()
                            .any(|call| call == &["launchctl", "bootout", &target])
                    );
                    assert!(calls.iter().any(|call| call
                        == &[
                            "launchctl",
                            "bootstrap",
                            &format!("gui/{}", unsafe { libc::getuid() }),
                            s.config.to_str().unwrap()
                        ]));
                } else {
                    for action in ["start", "restart", "stop", "disable"] {
                        assert!(
                            calls
                                .iter()
                                .any(|call| call == &["systemctl", "--user", action, &s.unit()])
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn reconfiguration_uses_current_release_without_touching_the_service() {
        for system in [System::Darwin, System::Linux] {
            let f = Fixture::new();
            let tools = f.maintenance_tools(system);
            let (mut s, manager) = f.service(system, "hub");
            s.dispatch("install", Some(&f.release), 0).unwrap();
            let mut manifest = object(&f.next.join("release.json")).unwrap();
            manifest["check_config"] = true.into();
            fs::write(f.next.join("release.json"), json_bytes(&manifest).unwrap()).unwrap();
            fs::remove_file(f.home.join("current")).unwrap();
            symlink(&f.next, f.home.join("current")).unwrap();
            let record = fs::read(&s.record_path).unwrap();
            let definition = fs::read(&s.config).unwrap();
            {
                let mut manager = manager.lock().unwrap();
                manager.calls.clear();
                manager.unknown = true;
            }

            let context = s.reconfiguration_context_with_tools(&tools).unwrap();
            assert_eq!(context.release, f.next);
            assert_eq!(context.record_path, s.record_path);
            assert_eq!(context.config, s.config);
            assert_eq!(context.label, s.label);
            assert_eq!(fs::read(&s.record_path).unwrap(), record);
            assert_eq!(fs::read(&s.config).unwrap(), definition);
            assert_eq!(fs::read_link(f.home.join("current")).unwrap(), f.next);
            assert_eq!(
                manager.lock().unwrap().calls,
                vec![vec![
                    text(&f.next.join("bin/steve-hub")).unwrap(),
                    "--build-info".into()
                ]]
            );
            f.preserved();
        }
    }

    #[test]
    fn reconfiguration_refuses_missing_changed_or_retired_registration_and_unsupported_builds() {
        for system in [System::Darwin, System::Linux] {
            for failure in [
                "missing-record",
                "record-link",
                "changed-record",
                "retired",
                "missing-definition",
                "wrong-home",
                "unsupported",
                "web-mismatch",
            ] {
                let f = Fixture::new();
                let tools = f.maintenance_tools(system);
                if failure != "unsupported" {
                    let mut manifest = object(&f.release.join("release.json")).unwrap();
                    manifest["check_config"] = true.into();
                    fs::write(
                        f.release.join("release.json"),
                        json_bytes(&manifest).unwrap(),
                    )
                    .unwrap();
                }
                let (mut s, manager) = f.service(system, "hub");
                s.dispatch("install", Some(&f.release), 0).unwrap();
                match failure {
                    "missing-record" => fs::remove_file(&s.record_path).unwrap(),
                    "record-link" => {
                        let saved = f.root.join("record.json");
                        fs::rename(&s.record_path, &saved).unwrap();
                        symlink(saved, &s.record_path).unwrap();
                    }
                    "changed-record" => {
                        let mut changed = s.record.clone();
                        changed["previous"] = "changed during inspection".into();
                        atomic(&s.record_path, &json_bytes(&changed).unwrap()).unwrap();
                    }
                    "retired" => {
                        s.record["uninstalled"] = true.into();
                        atomic(&s.record_path, &json_bytes(&s.record).unwrap()).unwrap();
                    }
                    "missing-definition" => fs::remove_file(&s.config).unwrap(),
                    "wrong-home" => {
                        let definition = if system == System::Darwin {
                            let mut bytes = Vec::new();
                            plist::to_writer_xml(&mut bytes, &json!({"Label":s.label,"EnvironmentVariables":{"STEVE_HOME":f.root}})).unwrap();
                            bytes
                        } else {
                            format!(
                                "[Service]\nEnvironment={}\n",
                                unit_quote(&format!("STEVE_HOME={}", f.root.display()), false)
                                    .unwrap()
                            )
                            .into_bytes()
                        };
                        fs::write(&s.config, definition).unwrap();
                    }
                    "unsupported" => {}
                    "web-mismatch" => {
                        fs::write(f.release.join("web/dist/release.json"), b"{}").unwrap();
                    }
                    _ => unreachable!(),
                }
                manager.lock().unwrap().calls.clear();
                assert!(
                    s.reconfiguration_context_with_tools(&tools).is_err(),
                    "{system:?}/{failure}"
                );
                assert!(
                    manager
                        .lock()
                        .unwrap()
                        .calls
                        .iter()
                        .all(|args| args.get(1).is_some_and(|arg| arg == "--build-info"))
                );
            }
        }
    }

    #[test]
    fn reconfiguration_requires_the_original_signing_identity_before_running_the_hub() {
        for failure in [
            "missing",
            "empty",
            "invalid",
            "missing-helper",
            "signature-mismatch",
        ] {
            let f = Fixture::new();
            let tools = f.maintenance_tools(System::Darwin);
            let (mut original, _) = f.service(System::Darwin, "hub");
            original.dispatch("install", Some(&f.release), 0).unwrap();
            let identity = f.home.join("signing/identity.json");
            match failure {
                "missing" => fs::remove_file(&identity).unwrap(),
                "empty" => fs::write(&identity, b"{}").unwrap(),
                "invalid" => fs::write(
                    &identity,
                    json_bytes(&json!({"label":original.label,"hub_identifier":"invalid!"}))
                        .unwrap(),
                )
                .unwrap(),
                "missing-helper" => fs::remove_file(f.home.join("bin/steve-keyhelper")).unwrap(),
                "signature-mismatch" => crate::signing::tests::executable(
                    &tools.codesign,
                    "#!/bin/sh\ncase \"$1\" in --verify) exit 0;; -dv) printf 'Identifier=wrong\\n' >&2;; *) exit 1;; esac\n",
                ),
                _ => unreachable!(),
            }
            // Reconstruct just as a fresh CLI invocation does. Missing identity
            // must not turn an existing signed installation into an unsigned one.
            let (service, manager) = f.service(System::Darwin, "hub");
            assert!(
                service.reconfiguration_context_with_tools(&tools).is_err(),
                "{failure}"
            );
            assert!(manager.lock().unwrap().calls.is_empty(), "{failure}");
        }
    }

    #[test]
    fn install_is_idempotent_never_starts_and_preserves_arguments_and_pairing() {
        for system in [System::Darwin, System::Linux] {
            let f = Fixture::new();
            let (mut s, m) = f.service(system, "node");
            assert_eq!(
                s.dispatch("install", Some(&f.release), 0).unwrap()["changed"],
                true
            );
            assert!(!m.lock().unwrap().running);
            if system == System::Darwin {
                assert!(!s.config.exists(), "install only prepares a macOS Node");
                assert!(
                    s.dispatch("start", None, 0)
                        .unwrap_err()
                        .to_string()
                        .contains("enable-boot")
                );
                s.dispatch("enable-boot", None, 0).unwrap();
                let plist: Value = plist::from_bytes(&fs::read(&s.config).unwrap()).unwrap();
                assert_eq!(plist["UserName"], "fixture-node");
                assert_eq!(plist["KeepAlive"], true);
                assert_eq!(plist["RunAtLoad"], true);
                assert_eq!(
                    plist["EnvironmentVariables"]["HOME"],
                    text(&f.root).unwrap()
                );
                assert_eq!(
                    plist["EnvironmentVariables"]["STEVE_HOME"],
                    text(&f.home).unwrap()
                );
                assert_eq!(plist["ProgramArguments"], s.boot_arguments().unwrap());
                assert!(plist.get("StandardOutPath").is_none());
            } else {
                let unit = fs::read_to_string(&s.config).unwrap();
                assert!(unit.contains("%% $$"));
                assert!(unit.contains("\\\""));
                assert!(unit.contains("KillMode=control-group"));
                s.dispatch("start", None, 0).unwrap();
            }
            assert!(m.lock().unwrap().running);
            m.lock().unwrap().calls.clear();
            assert_eq!(
                s.dispatch("install", Some(&f.release), 0).unwrap()["changed"],
                false
            );
            assert!(
                !m.lock()
                    .unwrap()
                    .calls
                    .iter()
                    .flatten()
                    .any(|a| a == "bootout" || a == "stop")
            );
            f.preserved();
        }
    }

    #[test]
    fn unknown_running_or_loaded_launchd_never_replaces_or_uninstalls() {
        for system in [System::Darwin, System::Linux] {
            for condition in ["unknown", "running", "loaded"] {
                if system == System::Linux && condition == "loaded" {
                    continue;
                }
                let f = Fixture::new();
                let (mut s, m) = f.service(system, "node");
                s.dispatch("install", Some(&f.release), 0).unwrap();
                if s.boot_node() {
                    s.dispatch("enable-boot", None, 0).unwrap();
                    s.dispatch("stop", None, 0).unwrap();
                }
                let before = (
                    fs::read(&s.config).unwrap(),
                    fs::read(&s.record_path).unwrap(),
                );
                {
                    let mut m = m.lock().unwrap();
                    m.unknown = condition == "unknown";
                    m.loaded = condition != "unknown";
                    m.running = condition == "running";
                    m.calls.clear();
                }
                if system == System::Linux {
                    if condition == "unknown" {
                        assert!(
                            s.dispatch("update", Some(&f.next), 0)
                                .unwrap_err()
                                .to_string()
                                .contains("no files changed")
                        );
                        assert!(!s.home.join("releases").exists());
                    } else {
                        assert!(
                            s.dispatch("install", Some(&f.next), 0)
                                .unwrap_err()
                                .to_string()
                                .contains("no files changed")
                        );
                    }
                }
                assert!(
                    s.dispatch("uninstall", None, 0)
                        .unwrap_err()
                        .to_string()
                        .contains("explicitly stop")
                );
                assert_eq!(
                    before,
                    (
                        fs::read(&s.config).unwrap(),
                        fs::read(&s.record_path).unwrap()
                    )
                );
                assert!(!s.home.join("services/retired").exists());
                assert!(
                    !m.lock()
                        .unwrap()
                        .calls
                        .iter()
                        .flatten()
                        .any(|a| matches!(a.as_str(), "bootout" | "stop" | "disable"))
                );
                f.preserved();
            }
        }
    }

    #[test]
    fn mac_node_registers_once_then_updates_and_uninstalls_one_definition() {
        let f = Fixture::new();
        let (mut s, manager) =
            f.service_at(System::Darwin, "node", &f.home, Some("example.boot-node"));
        let system = format!("system/{}", s.label);
        s.dispatch("install", Some(&f.release), 0).unwrap();
        assert_eq!(
            s.dispatch("status", None, 0).unwrap()["boot"],
            "enable_required"
        );
        manager.lock().unwrap().calls.clear();
        s.dispatch("enable-boot", None, 0).unwrap();
        assert_eq!(s.record["boot_release"], text(&f.release).unwrap());
        assert_eq!(adapter_target(&f.home, Some("node")).unwrap().1, f.release);
        let original = fs::read(&s.config).unwrap();
        let mutations = |manager: &Arc<Mutex<Manager>>| {
            manager
                .lock()
                .unwrap()
                .calls
                .iter()
                .filter_map(|call| match call.as_slice() {
                    [sudo, cmd, rest @ ..] if sudo == "/usr/bin/sudo" => Some(match cmd.as_str() {
                        "/bin/sh" => "register".into(),
                        "/bin/launchctl" => rest.join(" "),
                        "/bin/rm" => "remove definition".into(),
                        _ => panic!("unexpected admin command {call:?}"),
                    }),
                    _ => None,
                })
                .collect::<Vec<_>>()
        };
        assert_eq!(mutations(&manager), ["register"]);
        manager.lock().unwrap().calls.clear();
        s.dispatch("enable-boot", None, 0).unwrap();
        assert!(mutations(&manager).is_empty());

        // Running update copies external bundles into <home>/releases/<ver>-<commit8> and
        // stages the receipt without sudo, bootout or plist replacement.
        let updated = s.dispatch("update", Some(&f.next), 0).unwrap();
        let managed_next = f.home.join("releases/0.2.1-bbbbbbbb");
        assert_eq!(updated["started"], true);
        assert_eq!(s.record["previous"], text(&f.release).unwrap());
        assert_eq!(s.record["boot_release"], text(&managed_next).unwrap());
        assert_eq!(
            fs::read(&s.config).unwrap(),
            original,
            "upgrade does not rewrite the daemon"
        );
        assert!(mutations(&manager).is_empty());
        s.dispatch("stop", None, 0).unwrap();
        let retired = s.dispatch("uninstall", None, 0).unwrap();
        let archive = Path::new(retired["archive"].as_str().unwrap());
        assert_eq!(fs::read(archive.join("definition")).unwrap(), original);
        assert_eq!(
            mutations(&manager),
            [
                format!("disable {system}"),
                format!("bootout {system}"),
                "remove definition".into(),
            ]
        );
        assert!(!s.config.exists());
        f.preserved();
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn mac_boot_entrypoint_reads_the_selected_release_and_private_pairing_without_launchd() {
        let f = Fixture::new();
        let (mut s, _) = f.service(System::Darwin, "node");
        s.dispatch("install", Some(&f.release), 0).unwrap();
        s.dispatch("enable-boot", None, 0).unwrap();
        let lint = Command::new("/usr/bin/plutil")
            .arg("-lint")
            .arg("--")
            .arg(&s.config)
            .output()
            .unwrap();
        assert!(lint.status.success(), "{lint:?}");
        // With a fake unprivileged runner, run the actual plist launcher with no login env.
        fs::write(
            f.home.join(boot::RUNNER),
            br#"#!/bin/sh
set -eu
[ "$1" = __node-boot-run ]
[ "$(cat "$STEVE_HOME/node/identity.key")" = "private identity" ]
printf 'headless fixture ran\n'
"#,
        )
        .unwrap();
        let plist: Value = plist::from_bytes(&fs::read(&s.config).unwrap()).unwrap();
        let argv = plist["ProgramArguments"].as_array().unwrap();
        assert_eq!(
            argv[0].as_str().unwrap(),
            text(&f.home.join(boot::RUNNER)).unwrap()
        );
        let run = Command::new(argv[0].as_str().unwrap())
            .args(argv[1..].iter().map(|v| v.as_str().unwrap()))
            .env_clear()
            .env("HOME", &f.root)
            .env("STEVE_HOME", &f.home)
            .env("PATH", "/usr/bin:/bin")
            .output()
            .unwrap();
        assert!(run.status.success(), "{run:?}");
        assert_eq!(run.stdout, b"headless fixture ran\n");
    }

    #[test]
    fn boot_supervisor_reaps_the_old_child_and_the_next_launch_reads_the_new_receipt() {
        let f = Fixture::new();
        let (mut s, manager) = f.service(System::Darwin, "node");
        let sha1 = "22".repeat(20);
        manager.lock().unwrap().cert_sha1 = Some(sha1.clone());
        let requirement =
            crate::signing::self_signed_requirement(crate::signing::NODE_RUNTIME_IDENTIFIER, &sha1);
        let cli_requirement =
            crate::signing::self_signed_requirement(crate::signing::NODE_CLI_IDENTIFIER, &sha1);
        s.dispatch("install", Some(&f.release), 0).unwrap();
        s.dispatch("enable-boot", None, 0).unwrap();
        assert_eq!(s.record["code_requirement"], requirement);
        assert_eq!(
            manager
                .lock()
                .unwrap()
                .node_signatures
                .get(&f.home.join(boot::RUNNER)),
            Some(&cli_requirement)
        );
        // Remote upgrades stage the candidate under <home>/releases before signing and switching.
        let releases = f.home.join("releases");
        fs::create_dir_all(&releases).unwrap();
        let staged_next = Fixture::bundle(&releases, "0.2.1", "b");
        // An unsigned candidate or one signed by another certificate fails verification.
        assert!(s.verify_boot_upgrade(&staged_next).is_err());
        manager.lock().unwrap().node_signatures.insert(
            staged_next.join("bin/steve-node"),
            crate::signing::self_signed_requirement(
                crate::signing::NODE_RUNTIME_IDENTIFIER,
                &"99".repeat(20),
            ),
        );
        assert!(s.verify_boot_upgrade(&staged_next).is_err());
        let supervisor = fs::read(f.home.join(boot::RUNNER)).unwrap();
        let definition = fs::read(&s.config).unwrap();
        // Bundle verification uses the fake manager's own metadata, not these shell fixtures.
        fs::write(
            f.release.join("bin/steve-node"),
            br#"#!/bin/sh
cd "${0%/*}"
trap 'echo stopped > stopped; exit 0' TERM
echo ready > ready
while :; do sleep 0.1; done
"#,
        )
        .unwrap();
        fs::write(
            staged_next.join("bin/steve-node"),
            br#"#!/bin/sh
cd "${0%/*}"
[ "$STEVE_NODE_LOOPBACK_PLAINTEXT" = 1 ] && echo next > ran
"#,
        )
        .unwrap();
        {
            let mut manager = manager.lock().unwrap();
            manager.calls.clear();
            manager.unknown = true;
        }
        let home = f.home.clone();
        let (done_tx, done_rx) = std::sync::mpsc::channel();
        let cancel = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let stop = cancel.clone();
        let thread = std::thread::spawn(move || {
            done_tx
                .send(boot::supervise(&home, || {
                    stop.load(std::sync::atomic::Ordering::Relaxed)
                }))
                .unwrap();
        });
        let result = (|| -> Result<()> {
            let until = Instant::now() + Duration::from_secs(5);
            while !f.release.join("bin/ready").exists() {
                ensure!(Instant::now() < until, "old runtime did not start");
                std::thread::sleep(Duration::from_millis(10));
            }
            // A failed local sign leaves the receipt and running release unchanged.
            manager.lock().unwrap().signing_failure = true;
            assert!(s.dispatch("update", Some(&staged_next), 0).is_err());
            assert_eq!(s.record["boot_release"], text(&f.release)?);
            assert_eq!(s.record["code_requirement"], requirement);
            manager.lock().unwrap().signing_failure = false;

            // Signing the candidate with the same local self-signed identity satisfies both requirements.
            s.prepare_boot_upgrade(&staged_next)?;
            s.verify_boot_upgrade(&staged_next)?;
            s.dispatch("update", Some(&staged_next), 0)?;
            done_rx.recv_timeout(Duration::from_secs(5))??;
            Ok(())
        })();
        cancel.store(true, std::sync::atomic::Ordering::Relaxed);
        thread.join().unwrap();
        result.unwrap();
        assert_eq!(fs::read(f.home.join(boot::RUNNER)).unwrap(), supervisor);
        assert_eq!(fs::read(&s.config).unwrap(), definition);
        assert_eq!(s.record["code_requirement"], requirement);
        assert_eq!(
            manager
                .lock()
                .unwrap()
                .node_signatures
                .get(&staged_next.join("bin/steve-node")),
            Some(&requirement)
        );
        assert_eq!(
            manager
                .lock()
                .unwrap()
                .node_signatures
                .get(&staged_next.join("bin/steve")),
            Some(&cli_requirement)
        );
        assert!(f.release.join("bin/stopped").exists());
        assert!(!manager.lock().unwrap().calls.iter().any(|call| {
            call[0] == "/usr/bin/sudo"
                || (call[0] == "launchctl" && call.get(1).map(String::as_str) != Some("print"))
        }));
        boot::supervise(&f.home, || false).unwrap();
        assert_eq!(
            fs::read_to_string(staged_next.join("bin/ran")).unwrap(),
            "next\n"
        );
        f.preserved();
    }

    #[test]
    fn first_upgrade_from_adhoc_boot_node_creates_local_identity_and_signs_supervisor() {
        let f = Fixture::new();
        let third = Fixture::bundle(&f.root, "0.2.2", "c");
        let (mut s, manager) = f.service(System::Darwin, "node");
        let (_, info) = s.bundle(&f.release).unwrap();
        // Simulate a pre-0.10.1 ad-hoc boot Node: daemon installed, unsigned runner, null code_requirement.
        fs::create_dir_all(s.config.parent().unwrap()).unwrap();
        fs::write(
            &s.config,
            s.boot_definition("fixture-node", DARWIN_DEFAULT_SERVICE_PATH)
                .unwrap(),
        )
        .unwrap();
        directory(&f.home.join("services")).unwrap();
        atomic_mode(&f.home.join(boot::RUNNER), b"adhoc-supervisor", 0o700).unwrap();
        atomic(
            &s.record_path,
            &json_bytes(&json!({
                "component": "node", "label": s.label, "boot_release": f.release,
                "build": info, "previous": null, "code_requirement": null
            }))
            .unwrap(),
        )
        .unwrap();
        s.record = object(&s.record_path).unwrap();

        // First update to this version creates the local identity, signs release + supervisor, pins requirement.
        s.dispatch("update", Some(&f.next), 0).unwrap();
        let identity = crate::signing::load_node_identity(&f.home, &s.label).unwrap();
        assert_eq!(s.record["code_requirement"], identity.node_requirement());
        assert_eq!(
            manager
                .lock()
                .unwrap()
                .node_signatures
                .get(&f.home.join(boot::RUNNER)),
            Some(&identity.cli_requirement())
        );
        let signed_supervisor = fs::read(f.home.join(boot::RUNNER)).unwrap();
        assert_eq!(
            signed_supervisor,
            fs::read(f.next.join("bin/steve")).unwrap()
        );

        // Subsequent update reuses the same local identity and leaves the already-signed supervisor in place.
        s.dispatch("update", Some(&third), 0).unwrap();
        assert_eq!(s.record["code_requirement"], identity.node_requirement());
        assert_eq!(
            crate::signing::load_node_identity(&f.home, &s.label).unwrap(),
            identity
        );
        assert_eq!(
            fs::read(f.home.join(boot::RUNNER)).unwrap(),
            signed_supervisor
        );
        f.preserved();
    }

    #[test]
    fn mac_boot_service_end_to_end_with_local_node_signer_tools() {
        let dir = tempfile::tempdir().unwrap();
        let root = fs::canonicalize(dir.path()).unwrap();
        let home = root.join("node-home");
        fs::create_dir_all(home.join("node")).unwrap();
        fs::write(home.join("node/node.json"), r#"{"node_id":"test"}"#).unwrap();
        fs::write(home.join("node/identity.key"), "private identity").unwrap();
        let v1 = Fixture::bundle(&root, "0.10.1", "a");
        let releases = home.join("releases");
        fs::create_dir_all(&releases).unwrap();
        let v2 = Fixture::bundle(&releases, "0.10.2", "b");
        let state = root.join("state");
        fs::create_dir(&state).unwrap();
        let sha1 = "abcdef0123456789abcdef0123456789abcdef01";
        let sha1_colon = "AB:CD:EF:01:23:45:67:89:AB:CD:EF:01:23:45:67:89:AB:CD:EF:01";
        let openssl = root.join("openssl");
        crate::signing::tests::executable(
            &openssl,
            &format!(
                r#"#!/bin/sh
set -eu
case "$1" in
  req)
    keyout=''; out=''; shift
    while [ "$#" -gt 0 ]; do
      case "$1" in -keyout) keyout="$2"; shift 2;; -out) out="$2"; shift 2;; *) shift;; esac
    done
    printf 'KEY' > "$keyout"; printf 'CERT' > "$out" ;;
  x509) printf 'SHA1 Fingerprint={sha1_colon}\n' ;;
  pkcs12)
    [ -n "${{P12:-}}" ] || exit 11
    out=''; shift
    while [ "$#" -gt 0 ]; do
      case "$1" in -out) out="$2"; shift 2;; *) shift;; esac
    done
    printf 'P12' > "$out" ;;
  *) exit 99 ;;
esac
"#
            ),
        );
        let security = root.join("security");
        crate::signing::tests::executable(
            &security,
            &format!(
                r#"#!/bin/sh
set -eu
if [ "$1" = "-i" ]; then
  line=$(cat)
  cmd=$(printf '%s' "$line" | awk '{{print $1}}')
  case "$cmd" in
    create-keychain)
      kc=$(printf '%s' "$line" | sed -n 's/^create-keychain -p [^ ]* "\(.*\)"$/\1/p')
      umask 077; printf 'kc' > "$kc" ;;
    unlock-keychain) printf '1' > '{state}/unlocked' ;;
    import|set-key-partition-list) [ -f '{state}/unlocked' ] || exit 22 ;;
    *) exit 23 ;;
  esac
  exit 0
fi
case "$1" in
  set-keychain-settings) ;;
  lock-keychain) rm -f '{state}/unlocked' ;;
  delete-keychain) rm -f "$2" ;;
  find-identity) printf '  1) %s "Steve Node"\n' "$(printf '%s' '{sha1}' | tr 'a-f' 'A-F')" ;;
  *) exit 24 ;;
esac
"#,
                state = state.display()
            ),
        );
        let codesign = root.join("codesign");
        crate::signing::tests::executable(
            &codesign,
            &format!(
                r#"#!/bin/sh
set -eu
case "$1" in
  --force)
    [ -f '{state}/unlocked' ] || exit 31
    req=''; target=''; shift
    while [ "$#" -gt 0 ]; do
      case "$1" in -r) req="$2"; shift 2;; --sign|--identifier|--keychain) shift 2;; *) target="$1"; shift;; esac
    done
    printf '\n#FAKE_SIG:%s\n' "${{req#=}}" >> "$target" ;;
  --display)
    line=$(grep -a '^#FAKE_SIG:' "$3" | tail -n 1 || true)
    [ -n "$line" ] || exit 33
    printf '%s\n' "${{line#\#FAKE_SIG:}}" ;;
  --verify)
    want="${{4#=}}"
    line=$(grep -a '^#FAKE_SIG:' "$5" | tail -n 1 || true)
    [ -n "$line" ] || exit 34
    [ "${{line#\#FAKE_SIG:designated => }}" = "$want" ] || exit 35 ;;
  *) exit 39 ;;
esac
"#,
                state = state.display()
            ),
        );

        let manager = Arc::new(Mutex::new(Manager::default()));
        let other = manager.clone();
        let mut s = Service::with_runner(
            "node",
            &home,
            Some("test.local.selfsign"),
            System::Darwin,
            &root,
            Arc::new(move |args| other.lock().unwrap().run(args)),
        )
        .unwrap();
        s.config = root.join("Library/LaunchDaemons/test.local.selfsign.plist");
        s.config_uid = unsafe { libc::getuid() };
        let boot_manager = manager.clone();
        s.boot_registrar = Arc::new(move |args, finish| {
            finish()?;
            let mut m = boot_manager.lock().unwrap();
            fs::create_dir_all(m.config.parent().unwrap())?;
            fs::write(&m.config, &args[5])?;
            m.loaded = true;
            m.running = true;
            Ok(())
        });
        s.node_signer = Arc::new(boot::LocalNodeSigner(crate::signing::Tools {
            codesign,
            security,
            openssl,
            shell: "/bin/sh".into(),
        }));
        manager.lock().unwrap().config = s.config.clone();

        s.dispatch("install", Some(&v1), 0).unwrap();
        s.dispatch("enable-boot", None, 0).unwrap();
        let expected_node_req =
            crate::signing::self_signed_requirement(crate::signing::NODE_RUNTIME_IDENTIFIER, sha1);
        assert_eq!(s.record["code_requirement"], expected_node_req);
        assert!(!state.join("unlocked").exists());
        assert!(s.verify_boot_upgrade(&v2).is_err());

        s.prepare_boot_upgrade(&v2).unwrap();
        s.verify_boot_upgrade(&v2).unwrap();
        s.dispatch("update", Some(&v2), 0).unwrap();
        assert_eq!(s.record["boot_release"], text(&v2).unwrap());
        assert_eq!(s.record["code_requirement"], expected_node_req);
        assert!(!state.join("unlocked").exists());
    }

    #[test]
    fn stopped_node_update_pins_new_release_and_offline_doctor_does_not_invent_running_identity() {
        let f = Fixture::new();
        let (mut s, m) = f.service(System::Linux, "node");
        s.dispatch("install", Some(&f.release), 0).unwrap();
        let report = s.dispatch("doctor", Some(&f.next), 0).unwrap();
        assert_eq!(report["configured_build"]["version"], "0.2.0");
        assert_eq!(report["candidate"]["build"]["version"], "0.2.1");
        assert!(report["running_build"].is_null());
        assert_eq!(report["connection"], "unknown");
        assert!(
            report["update_command"]
                .as_str()
                .unwrap()
                .starts_with("steve node update")
        );
        let updated = s.dispatch("update", Some(&f.next), 0).unwrap();
        let managed_next = f.home.join("releases/0.2.1-bbbbbbbb");
        assert_eq!(updated["started"], false);
        assert_eq!(updated["release"], text(&managed_next).unwrap());
        assert_eq!(s.record["previous"], text(&f.release).unwrap());
        assert!(
            fs::read_to_string(&s.config)
                .unwrap()
                .contains("releases/0.2.1-bbbbbbbb/bin/steve-node")
        );
        for action in ["start", "restart", "stop"] {
            s.dispatch(action, None, 0).unwrap();
            assert_eq!(m.lock().unwrap().running, action != "stop");
        }
        f.preserved();
    }

    #[test]
    fn hub_preserves_signed_identity_and_never_performs_an_unguarded_update() {
        let f = Fixture::new();
        fs::create_dir(f.home.join("signing")).unwrap();
        fs::write(
            f.home.join("signing/identity.json"),
            r#"{"label":"test.original.hub"}"#,
        )
        .unwrap();
        let (mut s, _) = f.service(System::Darwin, "hub");
        s.dispatch("install", Some(&f.release), 0).unwrap();
        assert_eq!(s.label, "test.original.hub");
        assert_eq!(
            fs::canonicalize(f.home.join("web/current")).unwrap(),
            f.release.join("web/dist")
        );
        assert_eq!(
            s.dispatch("doctor", None, 0).unwrap()["automatic_upgrade"]["supported"],
            false
        );
        assert!(
            s.dispatch("update", Some(&f.next), 0)
                .unwrap_err()
                .to_string()
                .contains("Settings")
        );
        assert_eq!(fs::canonicalize(f.home.join("current")).unwrap(), f.release);
    }

    #[test]
    fn unsigned_mac_hub_requires_setup_but_linux_uses_supervisor() {
        let f = Fixture::new();
        let (mut mac, _) = f.service(System::Darwin, "hub");
        assert!(
            mac.dispatch("install", Some(&f.release), 0)
                .unwrap_err()
                .to_string()
                .contains("Keychain setup")
        );
        let (mut linux, _) = f.service(System::Linux, "hub");
        linux.dispatch("install", Some(&f.release), 0).unwrap();
        assert!(
            fs::read_to_string(linux.config)
                .unwrap()
                .contains("run-managed.sh")
        );
    }

    #[test]
    fn foreign_home_and_symlink_cannot_be_uninstalled() {
        for system in [System::Darwin, System::Linux] {
            let f = Fixture::new();
            let (mut s, m) = f.service(system, "node");
            s.dispatch("install", Some(&f.release), 0).unwrap();
            if s.boot_node() {
                s.dispatch("enable-boot", None, 0).unwrap();
                s.dispatch("stop", None, 0).unwrap();
            }
            m.lock().unwrap().calls.clear();
            let (mut foreign, _) = f.service_at(system, "node", &f.root.join("foreign"), None);
            for action in ["stop", "status", "uninstall"] {
                let expected = if action == "status" {
                    "another or unknown home"
                } else {
                    "matching service registration required"
                };
                assert!(
                    foreign
                        .dispatch(action, None, 0)
                        .unwrap_err()
                        .to_string()
                        .contains(expected)
                );
            }
            let original = fs::read(&s.config).unwrap();
            let target = f.root.join("foreign-definition");
            fs::rename(&s.config, &target).unwrap();
            symlink(&target, &s.config).unwrap();
            assert!(
                s.dispatch("uninstall", None, 0)
                    .unwrap_err()
                    .to_string()
                    .contains("owned regular file")
            );
            assert_eq!(fs::read(target).unwrap(), original);
            f.preserved();
        }
    }

    #[test]
    fn uninstall_archives_once_preserves_data_and_allows_reinstall() {
        for system in [System::Darwin, System::Linux] {
            let f = Fixture::new();
            let (mut s, m) = f.service(system, "node");
            s.dispatch("install", Some(&f.release), 0).unwrap();
            if s.boot_node() {
                s.dispatch("enable-boot", None, 0).unwrap();
                s.dispatch("stop", None, 0).unwrap();
            }
            let original = fs::read(&s.config).unwrap();
            let record = fs::read(&s.record_path).unwrap();
            let result = s.dispatch("uninstall", None, 0).unwrap();
            let archive = Path::new(result["archive"].as_str().unwrap());
            assert_eq!(fs::read(archive.join("definition")).unwrap(), original);
            assert_eq!(fs::read(archive.join("record.json")).unwrap(), record);
            assert_eq!(result["home_preserved"], true);
            assert!(!s.config.exists());
            assert_eq!(s.record["uninstalled"], true);
            let after = fs::read(&s.record_path).unwrap();
            m.lock().unwrap().calls.clear();
            assert_eq!(s.dispatch("uninstall", None, 0).unwrap()["changed"], false);
            assert_eq!(fs::read(&s.record_path).unwrap(), after);
            assert!(
                !m.lock()
                    .unwrap()
                    .calls
                    .iter()
                    .flatten()
                    .any(|a| a == "daemon-reload" || a == "disable")
            );
            s.dispatch("install", Some(&f.release), 0).unwrap();
            assert!(s.record.get("uninstall").is_none());
            assert!(!m.lock().unwrap().running);
            f.preserved();
        }
    }

    #[test]
    fn failed_disable_and_reload_resume_without_losing_the_original_archive() {
        for fail in ["disable", "daemon-reload"] {
            let f = Fixture::new();
            let (mut s, m) = f.service(System::Linux, "node");
            s.dispatch("install", Some(&f.release), 0).unwrap();
            let original = fs::read(&s.config).unwrap();
            {
                let mut m = m.lock().unwrap();
                m.failure = Some(fail.into());
                m.fail_after_removal = fail == "daemon-reload";
            }
            assert!(
                s.dispatch("uninstall", None, 0)
                    .unwrap_err()
                    .to_string()
                    .contains("command failed")
            );
            let archive = s
                .home
                .join("services/retired")
                .join(s.record["uninstall"]["archive"].as_str().unwrap());
            assert_eq!(fs::read(archive.join("definition")).unwrap(), original);
            assert_eq!(s.config.exists(), fail == "disable");
            assert_eq!(s.record["uninstall"]["phase"], "pending");
            {
                let mut m = m.lock().unwrap();
                m.failure = None;
                m.calls.clear();
            }
            // Reconstruct the service as a new process would, using the persisted receipt.
            let mut resumed = Service::with_runner(
                "node",
                &f.home,
                None,
                System::Linux,
                &f.root,
                s.runner.clone(),
            )
            .unwrap();
            assert_eq!(
                resumed.dispatch("uninstall", None, 0).unwrap()["changed"],
                true
            );
            assert_eq!(fs::read(archive.join("definition")).unwrap(), original);
            if fail == "daemon-reload" {
                assert!(
                    !m.lock()
                        .unwrap()
                        .calls
                        .iter()
                        .flatten()
                        .any(|a| a == "disable")
                );
            }
            f.preserved();
        }
    }

    #[test]
    fn tampered_uninstall_archive_is_never_used_to_remove_registration() {
        let f = Fixture::new();
        let (mut s, m) = f.service(System::Linux, "node");
        s.dispatch("install", Some(&f.release), 0).unwrap();
        m.lock().unwrap().failure = Some("disable".into());
        s.dispatch("uninstall", None, 0).unwrap_err();
        let archive = s
            .home
            .join("services/retired")
            .join(s.record["uninstall"]["archive"].as_str().unwrap());
        fs::write(archive.join("definition"), "tampered").unwrap();
        m.lock().unwrap().failure = None;
        assert!(
            s.dispatch("uninstall", None, 0)
                .unwrap_err()
                .to_string()
                .contains("backup changed")
        );
        assert!(s.config.exists());
        f.preserved();
    }

    #[test]
    fn also_in_dropins_continuations_or_new_directories_and_unknown_freshness_are_refused() {
        for condition in [
            "main",
            "dropin",
            "continued",
            "new",
            "stale",
            "unknown",
            "include",
        ] {
            let f = Fixture::new();
            let (mut s, m) = f.service(System::Linux, "node");
            s.dispatch("install", Some(&f.release), 0).unwrap();
            {
                let mut m = m.lock().unwrap();
                match condition {
                    "main" => {
                        let mut file = OpenOptions::new().append(true).open(&s.config).unwrap();
                        writeln!(file, "Also=foreign.service").unwrap();
                    }
                    "dropin" => m.dropin = "\n[Install]\nAlso=foreign.service\n".into(),
                    "continued" => m.dropin = "\n[Install]\nAl\\\nso=foreign.service\n".into(),
                    "new" => m.pending_dropin = "\n[Install]\nAlso=foreign.service\n".into(),
                    "stale" => m.stale = Some("yes\n".into()),
                    "unknown" => m.stale = Some(String::new()),
                    "include" => m.dropin = "\n.include /foreign/unit\n".into(),
                    _ => unreachable!(),
                };
                m.calls.clear();
            }
            let original = fs::read(&s.config).unwrap();
            let record = fs::read(&s.record_path).unwrap();
            let error = s.dispatch("uninstall", None, 0).unwrap_err().to_string();
            assert!(
                error.contains("Also=") || error.contains("freshness is unknown"),
                "{condition}: {error}"
            );
            assert_eq!(fs::read(&s.config).unwrap(), original);
            assert_eq!(fs::read(&s.record_path).unwrap(), record);
            assert!(
                !m.lock()
                    .unwrap()
                    .calls
                    .iter()
                    .flatten()
                    .any(|a| a == "disable")
            );
            assert!(!f.home.join("services/retired").exists());
        }
    }

    #[test]
    fn failed_systemd_query_is_unknown_even_if_its_output_says_inactive() {
        let f = Fixture::new();
        let (mut s, m) = f.service(System::Linux, "node");
        assert!(s.dispatch("uninstall", None, 0).is_err());
        assert!(m.lock().unwrap().calls.is_empty());
        s.dispatch("install", Some(&f.release), 0).unwrap();
        m.lock().unwrap().show_failure = true;
        assert!(
            s.dispatch("uninstall", None, 0)
                .unwrap_err()
                .to_string()
                .contains("state is unknown")
        );
        assert!(s.config.exists());
    }

    #[test]
    fn a_missing_pairing_is_reported_without_creating_one() {
        let f = Fixture::new();
        fs::remove_file(f.home.join("node/node.json")).unwrap();
        fs::remove_file(f.home.join("node/identity.key")).unwrap();
        let (mut s, _) = f.service(System::Linux, "node");
        s.dispatch("install", Some(&f.release), 0).unwrap();
        let report = s.dispatch("doctor", None, 0).unwrap();
        assert_eq!(report["pairing"]["configured"], false);
        assert_eq!(report["pairing"]["identity_present"], false);
        assert!(!f.home.join("node/node.json").exists());
    }

    #[test]
    fn launchctl_print_classifier_handles_missing_gui_domain_125_and_preserves_unknown_output() {
        let verbatim = "Could not print domain: 125: Domain does not support specified action\n";
        // Missing desktop session on gui/<uid>: exit code 125 or exit code 1 with structured 125.
        for code in [125, 1] {
            let out = Output {
                code,
                stdout: String::new(),
                stderr: verbatim.into(),
            };
            assert_eq!(
                classify_launchctl_print("gui/501", &out),
                LaunchQuery::Absent,
                "gui/501 with code={code} must be treated as absent"
            );
        }
        // Code 125 on the system domain must NEVER be treated as absent.
        let sys_out = Output {
            code: 125,
            stdout: String::new(),
            stderr: verbatim.into(),
        };
        match classify_launchctl_print("system", &sys_out) {
            LaunchQuery::Unknown(msg) => assert!(msg.contains(verbatim.trim()), "{msg}"),
            other => panic!("expected Unknown for system domain 125, got {other:?}"),
        }
        // Standard launchd absent codes (113 not found, 3 ESRCH).
        for code in [113, 3] {
            assert_eq!(
                classify_launchctl_print(
                    "user/501",
                    &Output {
                        code,
                        stdout: String::new(),
                        stderr: String::new(),
                    }
                ),
                LaunchQuery::Absent
            );
        }
        // Unrecognized launchctl failure output is preserved verbatim.
        let unknown_stderr = "Could not print service: 150: Operation not permitted";
        match classify_launchctl_print(
            "gui/501",
            &Output {
                code: 150,
                stdout: String::new(),
                stderr: format!("{unknown_stderr}\n"),
            },
        ) {
            LaunchQuery::Unknown(msg) => assert!(msg.contains(unknown_stderr), "{msg}"),
            other => panic!("expected Unknown for code 150, got {other:?}"),
        }
    }

    #[test]
    fn fresh_macos_node_completes_enable_boot_without_desktop_gui_session() {
        let f = Fixture::new();
        let (mut s, m) = f.service(System::Darwin, "node");
        m.lock().unwrap().gui_domain_unsupported = true;

        let prepared = s.dispatch("install", Some(&f.release), 0).unwrap();
        assert_eq!(prepared["prepared"], true);
        assert_eq!(prepared["boot"], "enable_required");
        // Verify gui/<uid> returning 125 is classified as absent (not loaded, not running).
        let gui_state = s
            .launch_state("gui/501", &f.root.join("unused.plist"), false)
            .unwrap();
        assert_eq!(gui_state["loaded"], false);
        assert_eq!(gui_state["running"], false);

        let enabled = s.dispatch("enable-boot", None, 0).unwrap();
        assert_eq!(enabled["registered_now"], true);
        assert_eq!(enabled["service"]["installed"], true);
        assert_eq!(enabled["service"]["running"], true);
        assert!(s.config.is_file());
        assert_eq!(
            s.record["boot_release"].as_str(),
            Some(f.release.canonicalize().unwrap().to_str().unwrap())
        );

        // An unrecognized launchctl error during enable-boot must fail and preserve stderr verbatim.
        let f2 = Fixture::new();
        let (mut s2, m2) = f2.service(System::Darwin, "node");
        s2.dispatch("install", Some(&f2.release), 0).unwrap();
        m2.lock().unwrap().print_error = Some((
            150,
            "Could not print service: 150: Operation not permitted".into(),
        ));
        let err = s2.dispatch("enable-boot", None, 0).unwrap_err().to_string();
        assert!(
            err.contains("Could not print service: 150: Operation not permitted"),
            "unrecognized launchctl output must be preserved verbatim: {err}"
        );
    }

    #[test]
    fn node_update_copies_external_release_into_managed_dir_and_survives_source_removal() {
        for system in [System::Darwin, System::Linux] {
            let f = Fixture::new();
            let (mut s, manager) = f.service(system, "node");
            s.dispatch("install", Some(&f.release), 0).unwrap();
            if s.boot_node() {
                s.dispatch("enable-boot", None, 0).unwrap();
            }

            let external_parent = f.root.join("downloaded-installer");
            fs::create_dir(&external_parent).unwrap();
            let external = Fixture::bundle(&external_parent, "0.11.1", "7c25c5c5");
            // Fix commit to a realistic 40-char git SHA starting with 7c25c5c5.
            let commit = "7c25c5c5deadbeef0123456789abcdef01234567";
            let manifest = json!({"format_version":1,"version":"0.11.1","commit":commit});
            fs::write(
                external.join("release.json"),
                json_bytes(&manifest).unwrap(),
            )
            .unwrap();
            fs::write(
                external.join("web/dist/release.json"),
                json_bytes(&manifest).unwrap(),
            )
            .unwrap();

            let report = s.dispatch("update", Some(&external), 0).unwrap();
            let managed = f.home.join("releases/0.11.1-7c25c5c5");
            assert_eq!(report["changed"], true, "{system:?}");
            assert_eq!(report["release"], text(&managed).unwrap(), "{system:?}");
            assert_eq!(
                configured_release(&s.record),
                Some(managed.to_str().unwrap()),
                "{system:?}"
            );

            // Idempotent re-run against the same external directory reuses the verified managed copy.
            let again = s.dispatch("update", Some(&external), 0).unwrap();
            assert_eq!(again["changed"], false, "{system:?}");
            assert_eq!(again["release"], text(&managed).unwrap(), "{system:?}");

            // Deleting the downloaded installer directory must NOT affect the managed release.
            fs::remove_dir_all(&external_parent).unwrap();
            assert!(!external.exists());
            let (verified_path, verified_info) = s.bundle(&managed).unwrap();
            assert_eq!(verified_path, managed);
            assert_eq!(verified_info["version"], "0.11.1");
            assert_eq!(verified_info["commit"], commit);

            if s.boot_node() {
                let sigs = &manager.lock().unwrap().node_signatures;
                assert!(
                    sigs.contains_key(&managed.join("bin/steve-node")),
                    "managed binary must be signed"
                );
                assert!(
                    !sigs.contains_key(&external.join("bin/steve-node")),
                    "external installer binary must never be signed in place"
                );
            } else {
                let unit = fs::read_to_string(&s.config).unwrap();
                assert!(
                    unit.contains("releases/0.11.1-7c25c5c5/bin/steve-node"),
                    "unit must point to managed release: {unit}"
                );
                assert!(
                    !unit.contains("downloaded-installer"),
                    "unit must not reference deleted source dir: {unit}"
                );
            }
            f.preserved();
        }
    }

    #[test]
    fn node_update_auto_restarts_running_service_and_leaves_stopped_service_stopped() {
        for system in [System::Darwin, System::Linux] {
            let f = Fixture::new();
            let third = Fixture::bundle(&f.root, "0.2.2", "c");
            let (mut s, manager) = f.service(system, "node");
            s.dispatch("install", Some(&f.release), 0).unwrap();
            if s.boot_node() {
                s.dispatch("enable-boot", None, 0).unwrap();
            } else {
                s.dispatch("start", None, 0).unwrap();
            }
            assert!(manager.lock().unwrap().running, "{system:?}");

            // Running before update -> update keeps/restarts the service and reports started=true.
            manager.lock().unwrap().calls.clear();
            let report = s.dispatch("update", Some(&f.next), 0).unwrap();
            assert_eq!(report["changed"], true, "{system:?}");
            assert_eq!(report["started"], true, "{system:?}");
            assert!(manager.lock().unwrap().running, "{system:?}");
            if system == System::Linux {
                let ops: Vec<String> = manager
                    .lock()
                    .unwrap()
                    .calls
                    .iter()
                    .filter(|c| c.first().map(String::as_str) == Some("systemctl"))
                    .filter_map(|c| c.get(2).cloned())
                    .collect();
                let stop_at = ops.iter().position(|op| op == "stop").expect("linux stop");
                let reload_at = ops
                    .iter()
                    .position(|op| op == "daemon-reload")
                    .expect("linux reload");
                let start_at = ops
                    .iter()
                    .position(|op| op == "start")
                    .expect("linux start");
                assert!(stop_at < reload_at && reload_at < start_at, "{ops:?}");
            } else {
                assert!(
                    !manager.lock().unwrap().calls.iter().any(|c| {
                        c[0] == "/usr/bin/sudo"
                            || (c[0] == "launchctl"
                                && c.get(1).map(String::as_str) != Some("print"))
                    }),
                    "macOS supervised boot update must not require sudo or mutating launchctl"
                );
            }

            // Stopped before update -> update leaves the service stopped and reports started=false.
            s.dispatch("stop", None, 0).unwrap();
            assert!(!manager.lock().unwrap().running, "{system:?}");
            manager.lock().unwrap().calls.clear();
            let stopped_report = s.dispatch("update", Some(&third), 0).unwrap();
            assert_eq!(stopped_report["changed"], true, "{system:?}");
            assert_eq!(stopped_report["started"], false, "{system:?}");
            assert!(!manager.lock().unwrap().running, "{system:?}");
            assert!(
                !manager
                    .lock()
                    .unwrap()
                    .calls
                    .iter()
                    .flatten()
                    .any(|arg| { matches!(arg.as_str(), "start" | "bootstrap" | "kickstart") }),
                "{system:?} stopped service must not be started by update"
            );
            f.preserved();
        }
    }

    #[test]
    fn enable_boot_runtime_lock_error_reports_holder_pid_and_binary_path() {
        // Case 1: Legacy Node holding node.lock without writing its PID -> discovered via lsof + ps.
        {
            let f = Fixture::new();
            let (mut s, manager) = f.service(System::Darwin, "node");
            s.node_exit_wait = Duration::ZERO;
            s.dispatch("install", Some(&f.release), 0).unwrap();
            let lock_path = f.home.join("node/local/node.lock");
            fs::create_dir_all(lock_path.parent().unwrap()).unwrap();
            let holder = OpenOptions::new()
                .create(true)
                .truncate(false)
                .write(true)
                .open(&lock_path)
                .unwrap();
            holder.try_lock().unwrap();
            let sentinel_pid = 999_999u32;
            let expected_binary =
                "/Users/example/.steve-node/releases/0.9.0-7c25c5c5/bin/steve-node";
            {
                let mut m = manager.lock().unwrap();
                m.lsof_pids = Some(vec![sentinel_pid]);
                m.ps_commands
                    .insert(sentinel_pid, expected_binary.to_owned());
            }
            let err = s.dispatch("enable-boot", None, 0).unwrap_err().to_string();
            assert!(
                err.contains("a Node process still owns the runtime lock"),
                "{err}"
            );
            assert!(err.contains("pid 999999"), "{err}");
            assert!(err.contains(expected_binary), "{err}");
            drop(holder);
        }

        // Case 2: Real child process holding flock and recording its PID in node.lock (lsof unavailable)
        // -> discovered via lock file PID + native OS binary path lookup.
        {
            use std::io::{BufRead, BufReader};
            struct ChildGuard(std::process::Child);
            impl Drop for ChildGuard {
                fn drop(&mut self) {
                    let _ = self.0.kill();
                    let _ = self.0.wait();
                }
            }
            let f = Fixture::new();
            let (mut s, _manager) = f.service(System::Darwin, "node");
            s.node_exit_wait = Duration::ZERO;
            s.dispatch("install", Some(&f.release), 0).unwrap();
            let lock_path = f.home.join("node/local/node.lock");
            fs::create_dir_all(lock_path.parent().unwrap()).unwrap();
            fs::write(&lock_path, b"").unwrap();
            let mut child = Command::new("python3")
                .args([
                    "-c",
                    "import fcntl, os, sys, time; \
                     f = open(sys.argv[1], 'r+b'); \
                     fcntl.flock(f.fileno(), fcntl.LOCK_EX); \
                     f.seek(0); f.truncate(0); f.write(f'{os.getpid()}\\n'.encode()); f.flush(); \
                     print('locked', flush=True); \
                     time.sleep(30)",
                ])
                .arg(&lock_path)
                .stdout(Stdio::piped())
                .stderr(Stdio::null())
                .spawn()
                .expect("spawn lock holder child");
            let child_pid = child.id();
            let mut ready = String::new();
            BufReader::new(child.stdout.take().unwrap())
                .read_line(&mut ready)
                .unwrap();
            assert_eq!(ready.trim(), "locked");
            let _guard = ChildGuard(child);

            let err = s.dispatch("enable-boot", None, 0).unwrap_err().to_string();
            assert!(
                err.contains("a Node process still owns the runtime lock"),
                "{err}"
            );
            assert!(err.contains(&format!("pid {child_pid}")), "{err}");
            assert!(
                err.contains("binary /") && !err.contains("binary unknown"),
                "expected resolved binary path, got: {err}"
            );
        }
    }

    #[test]
    fn setup_and_enable_boot_capture_login_shell_path_with_source_and_timestamp() {
        let login_path = "/Users/example/.local/bin:/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin";
        for system in [System::Darwin, System::Linux] {
            let f = Fixture::new();
            let (mut s, manager) = f.service(system, "node");
            manager.lock().unwrap().login_shell_path = Some(login_path.into());

            s.dispatch("install", Some(&f.release), 0).unwrap();
            if s.boot_node() {
                s.dispatch("enable-boot", None, 0).unwrap();
                let plist: Value = plist::from_bytes(&fs::read(&s.config).unwrap()).unwrap();
                assert_eq!(plist["EnvironmentVariables"]["PATH"], login_path);
            } else {
                let unit = fs::read_to_string(&s.config).unwrap();
                assert!(unit.contains(&format!("PATH={login_path}")), "{unit}");
            }
            assert_eq!(s.configured_service_path().as_deref(), Some(login_path));
            let receipt = object(&s.record_path).unwrap();
            assert_eq!(receipt["service_env"]["path"], login_path, "{system:?}");
            assert_eq!(
                receipt["service_env"]["path_source"], "login_shell",
                "{system:?}"
            );
            assert!(
                receipt["service_env"]["path_shell"]
                    .as_str()
                    .is_some_and(|sh| sh.starts_with('/')),
                "{system:?}: {:?}",
                receipt["service_env"]
            );
            assert!(
                receipt["service_env"]["path_captured_at"]
                    .as_str()
                    .is_some_and(|ts| ts.ends_with('Z') && ts.len() == 20),
                "{system:?}: {:?}",
                receipt["service_env"]
            );
            // Verify the shell was invoked with `-l -c`.
            assert!(
                manager
                    .lock()
                    .unwrap()
                    .calls
                    .iter()
                    .any(|c| c.len() == 4 && c[1] == "-l" && c[2] == "-c"),
                "{system:?}"
            );
        }
    }

    #[test]
    fn refresh_env_and_rerun_enable_boot_update_path_and_restart_when_running() {
        let initial_path = "/usr/bin:/bin:/usr/sbin:/sbin";
        let refreshed_path = "/Users/example/.local/bin:/opt/homebrew/bin:/usr/bin:/bin";
        let rerun_path =
            "/Users/example/.local/bin:/Users/example/.cargo/bin:/opt/homebrew/bin:/usr/bin:/bin";

        for system in [System::Darwin, System::Linux] {
            let f = Fixture::new();
            let (mut s, manager) = f.service(system, "node");
            manager.lock().unwrap().login_shell_path = Some(initial_path.into());
            s.dispatch("install", Some(&f.release), 0).unwrap();
            if s.boot_node() {
                s.dispatch("enable-boot", None, 0).unwrap();
            } else {
                s.dispatch("start", None, 0).unwrap();
            }
            assert_eq!(s.configured_service_path().as_deref(), Some(initial_path));

            // No PATH change -> changed=false, no restart.
            manager.lock().unwrap().calls.clear();
            let unchanged = s.dispatch("refresh-env", None, 0).unwrap();
            assert_eq!(unchanged["changed"], false, "{system:?}");
            assert_eq!(unchanged["restarted"], false, "{system:?}");
            assert_eq!(unchanged["started"], true, "{system:?}");
            assert!(
                !manager
                    .lock()
                    .unwrap()
                    .calls
                    .iter()
                    .any(|c| c[0] == "/usr/bin/sudo" || c.iter().any(|a| a == "restart")),
                "{system:?}"
            );

            // New CLI directory added to login shell -> refresh-env updates definition and restarts running service.
            manager.lock().unwrap().login_shell_path = Some(refreshed_path.into());
            manager.lock().unwrap().calls.clear();
            let updated = s.dispatch("refresh-env", None, 0).unwrap();
            assert_eq!(updated["changed"], true, "{system:?}");
            assert_eq!(updated["restarted"], true, "{system:?}");
            assert_eq!(updated["started"], true, "{system:?}");
            assert_eq!(updated["service_env"]["path"], refreshed_path, "{system:?}");
            assert_eq!(
                s.configured_service_path().as_deref(),
                Some(refreshed_path),
                "{system:?}"
            );
            assert!(manager.lock().unwrap().running, "{system:?}");

            // Re-running enable-boot on macOS also refreshes the plist when login-shell PATH changed.
            if s.boot_node() {
                manager.lock().unwrap().login_shell_path = Some(rerun_path.into());
                s.dispatch("enable-boot", None, 0).unwrap();
                assert_eq!(s.configured_service_path().as_deref(), Some(rerun_path));
                assert!(manager.lock().unwrap().running);
            }

            // Stopped service -> refresh-env updates definition without starting it.
            s.dispatch("stop", None, 0).unwrap();
            assert!(!manager.lock().unwrap().running, "{system:?}");
            let stopped_path = "/Users/example/bin:/usr/bin:/bin";
            manager.lock().unwrap().login_shell_path = Some(stopped_path.into());
            let stopped_refresh = s.dispatch("refresh-env", None, 0).unwrap();
            assert_eq!(stopped_refresh["changed"], true, "{system:?}");
            assert_eq!(stopped_refresh["restarted"], false, "{system:?}");
            assert_eq!(stopped_refresh["started"], false, "{system:?}");
            assert_eq!(
                s.configured_service_path().as_deref(),
                Some(stopped_path),
                "{system:?}"
            );
            assert!(!manager.lock().unwrap().running, "{system:?}");
            f.preserved();
        }
    }

    #[test]
    fn node_doctor_reports_missing_command_with_service_path_login_shell_location_and_refresh_hint()
    {
        for system in [System::Darwin, System::Linux] {
            let f = Fixture::new();
            let minimal_bin = f.root.join("minimal-bin");
            let user_bin = f.root.join("user-local-bin");
            fs::create_dir(&minimal_bin).unwrap();
            fs::create_dir(&user_bin).unwrap();
            let agy = user_bin.join("agy");
            fs::write(&agy, "#!/bin/sh\nexit 0\n").unwrap();
            fs::set_permissions(&agy, fs::Permissions::from_mode(0o755)).unwrap();

            let (mut s, manager) = f.service(system, "node");
            let svc_path = minimal_bin.to_str().unwrap().to_owned();
            manager.lock().unwrap().login_shell_path = Some(svc_path.clone());
            s.dispatch("install", Some(&f.release), 0).unwrap();
            if s.boot_node() {
                s.dispatch("enable-boot", None, 0).unwrap();
            }

            let login_path = format!("{}:{}", user_bin.display(), minimal_bin.display());
            manager.lock().unwrap().login_shell_path = Some(login_path.clone());
            let report = s.dispatch("doctor", None, 0).unwrap();
            assert_eq!(report["service_env"]["path"], svc_path, "{system:?}");
            assert_eq!(
                report["service_env"]["login_shell_path"], login_path,
                "{system:?}"
            );
            assert_eq!(report["service_env"]["path_outdated"], true, "{system:?}");

            let diags = report["command_diagnostics"]
                .as_array()
                .expect("command_diagnostics present");
            let agy_diag = diags
                .iter()
                .find(|d| d["harness"] == "antigravity" && d["command"] == "agy")
                .unwrap_or_else(|| panic!("{system:?}: expected agy diagnostic in {diags:?}"));
            assert_eq!(agy_diag["reason"], "command_not_in_service_path");
            assert_eq!(agy_diag["service_path"], svc_path);
            assert_eq!(agy_diag["login_shell_path"], text(&agy).unwrap());
            assert!(
                agy_diag["refresh_command"]
                    .as_str()
                    .unwrap()
                    .starts_with("steve node refresh-env"),
                "{agy_diag:?}"
            );

            // After refresh-env, doctor reports no missing-command mismatch for agy.
            s.dispatch("refresh-env", None, 0).unwrap();
            let clean = s.dispatch("doctor", None, 0).unwrap();
            assert_eq!(clean["service_env"]["path_outdated"], false, "{system:?}");
            assert!(
                clean["command_diagnostics"]
                    .as_array()
                    .is_none_or(|d| !d.iter().any(|x| x["command"] == "agy")),
                "{clean:?}"
            );
        }
    }

    #[test]
    fn enable_boot_rejects_legacy_launch_agent_before_sudo_with_actionable_migration_hint() {
        let f = Fixture::new();
        let (mut s, manager) = f.service(System::Darwin, "node");
        s.dispatch("install", Some(&f.release), 0).unwrap();
        let legacy_dir = f.root.join("Library/LaunchAgents");
        fs::create_dir_all(&legacy_dir).unwrap();
        let legacy_plist = legacy_dir.join(format!("{}.plist", s.label));
        fs::write(&legacy_plist, b"<plist/>").unwrap();

        manager.lock().unwrap().calls.clear();
        let err = s.dispatch("enable-boot", None, 0).unwrap_err().to_string();
        assert!(
            err.contains("legacy LaunchAgent"),
            "must name legacy LaunchAgent migration: {err}"
        );
        assert!(
            err.contains("launchctl bootout gui/$(id -u)/")
                && err.contains("rm -f")
                && err.contains("steve node enable-boot"),
            "must include actionable bootout, remove, and retry commands: {err}"
        );
        assert!(
            !manager
                .lock()
                .unwrap()
                .calls
                .iter()
                .any(|c| c.first().map(String::as_str) == Some("/usr/bin/sudo")),
            "must fail before requesting sudo when a legacy LaunchAgent exists"
        );
    }

    #[test]
    fn node_upgrade_keeps_stable_tcc_runner_path_and_signing_identity_across_bundles() {
        let f = Fixture::new();
        let (mut s, manager) = f.service(System::Darwin, "node");
        s.dispatch("install", Some(&f.release), 0).unwrap();
        s.dispatch("enable-boot", None, 0).unwrap();

        let expected_tcc_path = file_access::tcc_executable_path("node", &f.home);
        let setup_plist: Value = plist::from_bytes(&fs::read(&s.config).unwrap()).unwrap();
        assert_eq!(
            setup_plist["ProgramArguments"][0].as_str(),
            Some(text(&expected_tcc_path).unwrap().as_str())
        );
        let v1_req = s.record["code_requirement"].as_str().unwrap().to_owned();
        let v1_sig = manager
            .lock()
            .unwrap()
            .node_signatures
            .get(&f.release.join("bin/steve-node"))
            .cloned()
            .unwrap();
        assert_eq!(v1_sig, v1_req);

        // Upgrade from v1 bundle to v2 bundle: TCC path, identifier and requirement stay identical.
        let updated = s.dispatch("update", Some(&f.next), 0).unwrap();
        assert_eq!(updated["changed"], true);
        let managed_v2 = PathBuf::from(s.record["boot_release"].as_str().unwrap());
        let v2_req = s.record["code_requirement"].as_str().unwrap().to_owned();
        assert_eq!(v2_req, v1_req);
        let v2_sig = manager
            .lock()
            .unwrap()
            .node_signatures
            .get(&managed_v2.join("bin/steve-node"))
            .cloned()
            .unwrap();
        assert_eq!(v2_sig, v1_sig);
        let upgrade_plist: Value = plist::from_bytes(&fs::read(&s.config).unwrap()).unwrap();
        assert_eq!(
            upgrade_plist["ProgramArguments"][0].as_str(),
            Some(text(&expected_tcc_path).unwrap().as_str())
        );
    }
}
