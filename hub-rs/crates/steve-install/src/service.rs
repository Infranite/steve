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
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt, symlink};
use std::path::{Component, Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::time::{Duration, Instant};
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
        let failure = format!("{}{}", result.stderr, result.stdout).to_lowercase();
        let absent = result.code != 0
            && (failure.contains("could not find service")
                || failure.contains("could not find domain")
                || failure.contains("no such process"));
        let running = if result.code == 0 {
            Some(pid.is_some())
        } else if absent {
            Some(false)
        } else {
            None
        };
        Ok(
            json!({"installed": config.is_file(), "loaded":result.code == 0,"running":running,"pid":pid,
            "program":fields.get("program"),
            "state":fields.get("state").copied().unwrap_or(if absent {"stopped"} else {"unknown"})}),
        )
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

    fn definition(&self, release: &Path) -> Result<Vec<u8>> {
        let mut env = BTreeMap::from([
            ("STEVE_HOME", text(&self.home)?),
            (
                "PATH",
                std::env::var("PATH").unwrap_or_else(|_| "/usr/local/bin:/usr/bin:/bin".into()),
            ),
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
        let (release, info) = self.bundle(release)?;
        ensure!(
            !(update && self.component == "hub"),
            "Hub updates use Settings > About or steve upgrade for task drain, backup and schema checks"
        );
        let current = self.home.join("current");
        ensure!(
            self.component != "hub" || !current.exists() || resolve(&current)? == release,
            "current Hub already exists; use Settings > About or steve upgrade to update it"
        );
        ensure!(
            self.component != "hub" || self.system != System::Darwin || self.signed,
            "new macOS Hub needs explicit Keychain setup: run steve setup first"
        );
        let data = self.definition(&release)?;
        let same = self.config.is_file() && fs::read(&self.config)? == data;
        let mut state = self.state()?;
        if !same {
            let deadline = Instant::now()
                .checked_add(Duration::from_secs(wait))
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
                "service is running or its state is unknown; no files changed. Finish Node sessions, verify Devices, then stop the service before update; --wait only waits for a stopped service"
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
        self.record = json!({"component":self.component,"label":self.label,"release":text(&release)?,"build":info,"previous":previous});
        atomic(&self.record_path, &json_bytes(&self.record)?)?;
        if self.system == System::Linux {
            self.systemctl(&["daemon-reload"], true)?;
            self.systemctl(&["enable", &self.unit()], true)?;
        }
        Ok(
            json!({"installed":true,"changed":!same,"started":false,"release":text(&release)?,"build":info}),
        )
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
            "service is running, loaded, or its state is unknown; no service was stopped. Finish sessions, verify Hub Devices, then explicitly stop before uninstall"
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
        node_signatures: BTreeMap<PathBuf, String>,
        cert_sha1: Option<String>,
        signing_failure: bool,
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
            } else if args[0] == "/usr/bin/id" {
                assert_eq!(args, &["/usr/bin/id", "-un"]);
                result.stdout = "fixture-node\n".into();
            } else if args.starts_with(&["/usr/bin/sudo".into(), "/bin/rm".into()]) {
                assert_eq!(Path::new(&args[3]), self.config);
                fs::remove_file(&self.config)?;
            } else if has("print") {
                if self.unknown {
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
                {
                    let mut m = boot_manager.lock().unwrap();
                    m.calls.push(args.to_vec());
                    assert_eq!(args[..3], ["/usr/bin/sudo", "/bin/sh", "-c"]);
                    assert_eq!(m.config, Path::new(&args[6]));
                    ensure!(
                        m.failure.as_deref() != Some("-v"),
                        "fake authorization denied"
                    );
                }
                finish()?;
                let mut m = boot_manager.lock().unwrap();
                ensure!(
                    m.failure.as_deref() != Some("/usr/bin/install"),
                    "fake install failure"
                );
                fs::create_dir_all(m.config.parent().unwrap())?;
                fs::write(&m.config, &args[5])?;
                m.loaded = true;
                m.running = true;
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
                    let definition = s.definition(&f.release).unwrap();
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
                    assert!(
                        s.dispatch("update", Some(&f.next), 0)
                            .unwrap_err()
                            .to_string()
                            .contains("no files changed")
                    );
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

        // Running update stages the receipt without sudo, bootout or plist replacement.
        s.dispatch("update", Some(&f.next), 0).unwrap();
        assert_eq!(s.record["previous"], text(&f.release).unwrap());
        assert_eq!(s.record["boot_release"], text(&f.next).unwrap());
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
        // An unsigned candidate or one signed by another certificate fails verification.
        assert!(s.verify_boot_upgrade(&f.next).is_err());
        manager.lock().unwrap().node_signatures.insert(
            f.next.join("bin/steve-node"),
            crate::signing::self_signed_requirement(
                crate::signing::NODE_RUNTIME_IDENTIFIER,
                &"99".repeat(20),
            ),
        );
        assert!(s.verify_boot_upgrade(&f.next).is_err());
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
            f.next.join("bin/steve-node"),
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
            assert!(s.dispatch("update", Some(&f.next), 0).is_err());
            assert_eq!(s.record["boot_release"], text(&f.release)?);
            assert_eq!(s.record["code_requirement"], requirement);
            manager.lock().unwrap().signing_failure = false;

            // Signing the candidate with the same local self-signed identity satisfies both requirements.
            s.prepare_boot_upgrade(&f.next)?;
            s.verify_boot_upgrade(&f.next)?;
            s.dispatch("update", Some(&f.next), 0)?;
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
                .get(&f.next.join("bin/steve-node")),
            Some(&requirement)
        );
        assert_eq!(
            manager
                .lock()
                .unwrap()
                .node_signatures
                .get(&f.next.join("bin/steve")),
            Some(&cli_requirement)
        );
        assert!(f.release.join("bin/stopped").exists());
        assert!(
            !manager
                .lock()
                .unwrap()
                .calls
                .iter()
                .any(|call| matches!(call[0].as_str(), "/usr/bin/sudo" | "launchctl"))
        );
        boot::supervise(&f.home, || false).unwrap();
        assert_eq!(
            fs::read_to_string(f.next.join("bin/ran")).unwrap(),
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
        fs::write(&s.config, s.boot_definition("fixture-node").unwrap()).unwrap();
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
        let v2 = Fixture::bundle(&root, "0.10.2", "b");
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
        s.dispatch("update", Some(&f.next), 0).unwrap();
        assert_eq!(s.record["previous"], text(&f.release).unwrap());
        assert!(
            fs::read_to_string(&s.config)
                .unwrap()
                .contains("0.2.1/bin/steve-node")
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
}
