//! Offline user-service management. A process ID never proves a Node is idle or
//! identifies its running build. Replacing or unregistering a service requires
//! a known stopped state; launchd must additionally have unloaded its job.
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
    runner: Runner,
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
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)?;
    let metadata = file.metadata()?;
    ensure!(
        metadata.is_file()
            && metadata.uid() == unsafe { libc::getuid() }
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
    let mut child = Command::new(&args[0])
        .args(&args[1..])
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
            "this home already uses another service label"
        );
        let config = if system == System::Darwin {
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
            runner,
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

    fn check_owner(&self) -> Result<()> {
        if !self.config.exists() {
            return Ok(());
        }
        let owner = if self.system == System::Darwin {
            let definition: Value = plist::from_bytes(&fs::read(&self.config)?)
                .context("existing service definition cannot establish its home")?;
            let env = definition.get("EnvironmentVariables");
            let legacy = self.home.join("current/ops/run.sh");
            if self.component == "hub"
                && self.signed
                && env.is_none_or(|v| v.as_object().is_some_and(|o| o.is_empty()))
                && self.home == resolve(&self.user_home.join(".steve"))?
                && definition["Label"] == self.label
                && definition["ProgramArguments"] == json!(["/bin/zsh", text(&legacy)?])
                && ["Program", "RootDirectory", "UserName", "GroupName"]
                    .iter()
                    .all(|k| definition.get(k).is_none())
                && legacy.is_file()
            {
                return Ok(());
            }
            definition["EnvironmentVariables"]["STEVE_HOME"]
                .as_str()
                .map(str::to_owned)
        } else {
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
            owner
        };
        ensure!(
            owner
                .as_deref()
                .filter(|s| !s.is_empty())
                .map(|s| resolve(Path::new(s)))
                .transpose()?
                .as_ref()
                == Some(&self.home),
            "service label belongs to another or unknown home; use that home's original --label"
        );
        Ok(())
    }

    fn state(&self) -> Result<Value> {
        self.check_owner()?;
        if self.system == System::Darwin {
            let result = self.command(
                vec![
                    "launchctl".into(),
                    "print".into(),
                    format!("gui/{}/{}", unsafe { libc::getuid() }, self.label),
                ],
                false,
            )?;
            let fields: BTreeMap<_, _> = result
                .stdout
                .lines()
                .filter_map(|s| s.trim().split_once(" = "))
                .collect();
            let pid = fields.get("pid").and_then(|s| s.parse::<u64>().ok());
            let failure = format!("{}{}", result.stderr, result.stdout).to_lowercase();
            let absent = result.code != 0
                && (failure.contains("could not find service")
                    || failure.contains("no such process"));
            let running = if result.code == 0 {
                Some(pid.is_some())
            } else if absent {
                Some(false)
            } else {
                None
            };
            return Ok(
                json!({"installed":self.config.is_file(),"loaded":result.code == 0,"running":running,"pid":pid,
                "state":fields.get("state").copied().unwrap_or(if absent {"stopped"} else {"unknown"})}),
            );
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

    fn action(&self, action: &str) -> Result<Value> {
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

    fn uninstall(&mut self) -> Result<Value> {
        let present = exists(&self.config);
        let definition = if present {
            Some(regular(&self.config)?)
        } else {
            None
        };
        if let Some(bytes) = &definition
            && self.system == System::Darwin
        {
            let value: Value = plist::from_bytes(bytes)?;
            ensure!(
                value["Label"] == self.label,
                "service definition label differs from the exact owned label"
            );
        }
        let old_record = if exists(&self.record_path) {
            Some(regular(&self.record_path)?)
        } else {
            None
        };
        ensure!(
            self.record.as_object().is_some_and(|o| o.is_empty())
                || (self.record["component"] == self.component
                    && self.record["label"] == self.label),
            "service record does not identify this component and label"
        );
        self.stopped()?;
        if present && self.system == System::Linux {
            self.systemctl(&["daemon-reload"], true)?;
            self.stopped()?;
        }
        let effective = if present && self.system == System::Linux {
            Some(self.isolated_unit(definition.as_ref().unwrap())?)
        } else {
            None
        };
        let mut pending = self.record.get("uninstall").cloned().unwrap_or(json!({}));
        ensure!(
            pending.is_object(),
            "uninstall receipt is invalid; inspect the retained service record"
        );
        if !present && pending["phase"] != "pending" {
            return Ok(
                json!({"installed":false,"changed":false,"started":false,"home_preserved":true,"archive":pending["archive"]}),
            );
        }
        let services = self.home.join("services");
        directory(&services)?;
        let retired = services.join("retired");
        directory(&retired)?;
        let archive;
        if pending["phase"] == "pending" {
            let name = pending["archive"].as_str().unwrap_or_default();
            let suffix = name.strip_prefix(&format!("{}-", self.component));
            ensure!(
                suffix.is_some_and(|s| s.len() == 32
                    && s.bytes()
                        .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)))
                    && pending["config"] == text(&self.config)?,
                "uninstall receipt does not identify this service archive"
            );
            archive = retired.join(name);
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
                definition.as_ref().is_none_or(|bytes| *bytes == saved),
                "service definition changed during uninstall; original backup is retained"
            );
        } else {
            let definition = definition
                .as_ref()
                .context("uninstall definition disappeared")?;
            archive = retired.join(format!("{}-{}", self.component, Uuid::new_v4().simple()));
            directory(&archive)?;
            atomic(&archive.join("definition"), definition)?;
            ensure!(
                regular(&archive.join("definition"))? == *definition,
                "service definition backup did not verify"
            );
            if let Some(bytes) = &old_record {
                atomic(&archive.join("record.json"), bytes)?;
                ensure!(
                    regular(&archive.join("record.json"))? == *bytes,
                    "service record backup did not verify"
                );
            }
            pending = json!({"phase":"pending","archive":archive.file_name().unwrap().to_str().unwrap(),"config":text(&self.config)?,
                "definition_sha256":digest(definition),"record_sha256":old_record.as_ref().map(|bytes|digest(bytes))});
            self.record["component"] = json!(self.component);
            self.record["label"] = json!(self.label);
            self.record["uninstall"] = pending.clone();
            self.record["uninstalled"] = json!(false);
            atomic(&self.record_path, &json_bytes(&self.record)?)?;
        }
        if let Some(definition) = definition {
            self.stopped()?;
            ensure!(
                regular(&self.config)? == definition,
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
            fs::remove_file(&self.config)?;
        }
        if self.system == System::Linux {
            self.systemctl(&["daemon-reload"], true)?;
        }
        self.stopped()?;
        ensure!(
            !exists(&self.config),
            "service definition reappeared; inspect it before retrying uninstall"
        );
        pending["phase"] = json!("complete");
        self.record["uninstall"] = pending;
        self.record["uninstalled"] = json!(true);
        let bytes = json_bytes(&self.record)?;
        atomic(&self.record_path, &bytes)?;
        ensure!(
            regular(&self.record_path)? == bytes,
            "uninstall receipt did not verify"
        );
        Ok(
            json!({"installed":false,"changed":true,"started":false,"home_preserved":true,"archive":text(&archive)?}),
        )
    }

    fn status(&self, candidate: Option<&Path>, doctor: bool) -> Result<Value> {
        let state = self.state()?;
        let release = if self.component == "hub" {
            self.home.join("current")
        } else {
            self.record["release"]
                .as_str()
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
            } else if has("print") {
                if self.unknown {
                    result.code = 1;
                    result.stderr = "access denied".into();
                } else if !self.loaded {
                    result.code = 113;
                    result.stderr = "Could not find service in domain".into();
                } else {
                    result.stdout = if self.running {
                        "state = running\npid = 123\n"
                    } else {
                        "state = waiting\n"
                    }
                    .into();
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
            for name in ["steve-hub", "steve-node"] {
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
            let service = Service::with_runner(
                component,
                home,
                label,
                system,
                &self.root,
                Arc::new(move |args| other.lock().unwrap().run(args)),
            )
            .unwrap();
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
                let plist: Value = plist::from_bytes(&fs::read(&s.config).unwrap()).unwrap();
                assert_eq!(
                    plist["ProgramArguments"],
                    json!([text(&f.release.join("bin/steve-node")).unwrap(), "run"])
                );
                assert_eq!(
                    plist["EnvironmentVariables"]["STEVE_NODE_LOOPBACK_PLAINTEXT"],
                    "1"
                );
            } else {
                let unit = fs::read_to_string(&s.config).unwrap();
                assert!(unit.contains("%% $$"));
                assert!(unit.contains("\\\""));
                assert!(unit.contains("KillMode=control-group"));
            }
            s.dispatch("start", None, 0).unwrap();
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
                assert!(
                    s.dispatch("update", Some(&f.next), 0)
                        .unwrap_err()
                        .to_string()
                        .contains("no files changed")
                );
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
            m.lock().unwrap().calls.clear();
            let (mut foreign, _) = f.service_at(system, "node", &f.root.join("foreign"), None);
            for action in ["stop", "status", "uninstall"] {
                assert!(
                    foreign
                        .dispatch(action, None, 0)
                        .unwrap_err()
                        .to_string()
                        .contains("another or unknown home")
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
        assert_eq!(s.dispatch("uninstall", None, 0).unwrap()["changed"], false);
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
    fn legacy_signed_default_home_is_accepted_only_with_its_exact_command() {
        let f = Fixture::new();
        let home = f.root.join(".steve");
        fs::create_dir_all(home.join("signing")).unwrap();
        fs::write(
            home.join("signing/identity.json"),
            r#"{"label":"test.original.hub"}"#,
        )
        .unwrap();
        symlink(&f.release, home.join("current")).unwrap();
        let (mut s, m) = f.service_at(System::Darwin, "hub", &home, None);
        let original = json!({"Label":s.label,"ProgramArguments":["/bin/zsh",text(&home.join("current/ops/run.sh")).unwrap()]});
        let write = |value: &Value| {
            let mut bytes = Vec::new();
            plist::to_writer_xml(&mut bytes, value).unwrap();
            atomic(&s.config, &bytes).unwrap();
        };
        write(&original);
        assert_eq!(
            s.status(None, true).unwrap()["configured_build"]["version"],
            "0.2.0"
        );
        for (key, value) in [
            ("Program", json!("/usr/bin/false")),
            ("Label", json!("foreign.hub")),
            ("UserName", json!("other")),
            ("EnvironmentVariables", json!({"PATH":"/usr/bin"})),
            ("ProgramArguments", json!(["/bin/zsh", "-c", "true"])),
        ] {
            let mut changed = original.clone();
            changed[key] = value;
            write(&changed);
            assert!(
                s.state()
                    .unwrap_err()
                    .to_string()
                    .contains("another or unknown home")
            );
        }
        write(&original);
        m.lock().unwrap().calls.clear();
        let identity = fs::read(home.join("signing/identity.json")).unwrap();
        s.uninstall().unwrap();
        assert_eq!(
            fs::read(home.join("signing/identity.json")).unwrap(),
            identity
        );
        assert_eq!(fs::canonicalize(home.join("current")).unwrap(), f.release);
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
