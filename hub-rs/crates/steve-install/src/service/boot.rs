//! Boot-time macOS Node. Only the system domain is implicitly loaded at boot.
//! Root installs the definition and controls that domain; the Node, including the
//! launcher that reads user-owned data, always runs as the original account.
use super::*;
use crate::signing::{NodeSigningIdentity, Tools};
use std::io::{BufRead, BufReader};
use steve_types::devices::NODE_BOOT_SERVICE_ENV;

// launchd starts this executable directly as the original account. macOS can
// attribute the Node and its ordinary children to the same responsible code.
pub(super) const RUNNER: &str = "services/node-boot-runner";
const RUNNER_ARG: &str = "__node-boot-run";
const LOCK_PID_READ_LIMIT: usize = 64;
#[cfg(target_os = "macos")]
const PROC_PIDPATHINFO_MAXSIZE: usize = 4096;

fn parse_lock_pid(raw: &str) -> Option<u32> {
    let pid = raw.trim().parse::<u32>().ok()?;
    (pid > 1 && pid != std::process::id()).then_some(pid)
}

#[cfg(target_os = "macos")]
fn native_process_binary_path(pid: u32) -> Option<String> {
    unsafe extern "C" {
        fn proc_pidpath(
            pid: libc::c_int,
            buffer: *mut libc::c_void,
            buffersize: u32,
        ) -> libc::c_int;
    }
    let pid = libc::c_int::try_from(pid).ok()?;
    let mut buf = vec![0u8; PROC_PIDPATHINFO_MAXSIZE];
    let len = unsafe {
        proc_pidpath(
            pid,
            buf.as_mut_ptr().cast::<libc::c_void>(),
            buf.len() as u32,
        )
    };
    if len <= 0 {
        return None;
    }
    let path = std::str::from_utf8(&buf[..len as usize]).ok()?.trim();
    (!path.is_empty()).then(|| path.to_owned())
}

#[cfg(target_os = "linux")]
fn native_process_binary_path(pid: u32) -> Option<String> {
    let path = fs::read_link(format!("/proc/{pid}/exe")).ok()?;
    let text = path.to_str()?.trim();
    (!text.is_empty()).then(|| text.to_owned())
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn native_process_binary_path(_pid: u32) -> Option<String> {
    None
}

pub(super) trait NodeSigner: Send + Sync {
    fn ensure_identity(&self, home: &Path, label: &str) -> Result<NodeSigningIdentity>;
    fn load_identity(&self, home: &Path, label: &str) -> Result<NodeSigningIdentity>;
    fn sign_binary(
        &self,
        home: &Path,
        binary: &Path,
        identity: &NodeSigningIdentity,
        identifier: &str,
    ) -> Result<()>;
    fn verify_binary(&self, binary: &Path, requirement: &str) -> Result<()>;
    fn stage_runner(
        &self,
        home: &Path,
        source: &Path,
        destination_dir: &Path,
        identity: &NodeSigningIdentity,
    ) -> Result<PathBuf>;
    fn commit_runner(&self, staged: &Path, destination: &Path) -> Result<()> {
        let parent = destination.parent().context("runner has no parent")?;
        fs::rename(staged, destination)?;
        File::open(parent)?.sync_all()?;
        Ok(())
    }
}

pub(super) struct LocalNodeSigner(pub(super) Tools);

impl NodeSigner for LocalNodeSigner {
    fn ensure_identity(&self, home: &Path, label: &str) -> Result<NodeSigningIdentity> {
        crate::signing::ensure_node_identity(home, label, &self.0)
    }
    fn load_identity(&self, home: &Path, label: &str) -> Result<NodeSigningIdentity> {
        crate::signing::load_node_identity(home, label)
    }
    fn sign_binary(
        &self,
        home: &Path,
        binary: &Path,
        identity: &NodeSigningIdentity,
        identifier: &str,
    ) -> Result<()> {
        crate::signing::sign_node_binary(home, binary, identity, identifier, &self.0)
    }
    fn verify_binary(&self, binary: &Path, requirement: &str) -> Result<()> {
        crate::signing::verify_node_binary(binary, requirement, &self.0)
    }
    fn stage_runner(
        &self,
        home: &Path,
        source: &Path,
        destination_dir: &Path,
        identity: &NodeSigningIdentity,
    ) -> Result<PathBuf> {
        crate::signing::stage_signed_node_runner(home, source, destination_dir, identity, &self.0)
    }
}

struct StagedRunnerGuard {
    path: PathBuf,
    armed: bool,
}

impl StagedRunnerGuard {
    fn new(path: PathBuf) -> Self {
        Self { path, armed: true }
    }
    fn install_to(&mut self, signer: &dyn NodeSigner, destination: &Path) -> Result<()> {
        signer.commit_runner(&self.path, destination)?;
        self.armed = false;
        Ok(())
    }
}

impl Drop for StagedRunnerGuard {
    fn drop(&mut self) {
        if self.armed {
            let _ = fs::remove_file(&self.path);
        }
    }
}

// One sudo command. The shell never sources a user file or executes the launcher.
// Once authorized, the unprivileged CLI prepares its own files and acknowledges readiness.
// EOF (failure or interruption) aborts.
const REGISTER: &str = r#"set -eu
export LC_ALL=C
definition=$1 config=$2 target=$3
[ ! -e "$config" ] && [ ! -L "$config" ] || exit 1
printf '%s\n' detached
IFS= read -r ack && [ "$ack" = ready ] || exit 1
temp=$(/usr/bin/mktemp "${config%/*}/.steve-node.XXXXXXXX")
trap '/bin/rm -f -- "$temp"' EXIT
printf '%s' "$definition" > "$temp"
/usr/bin/plutil -lint -- "$temp" >&2
/usr/sbin/chown root:wheel "$temp"
/bin/chmod 0644 "$temp"
[ ! -e "$config" ] && [ ! -L "$config" ] || exit 1
/bin/ln -h "$temp" "$config"
/bin/launchctl enable "$target" >&2
/bin/launchctl bootstrap system "$config" >&2
"#;

// One sudo command to atomically update an existing LaunchDaemon definition (for example after
// refreshing the user's login-shell PATH) and reload it when it was running.
const UPDATE_BOOT_DEFINITION: &str = r#"set -eu
export LC_ALL=C
definition=$1 config=$2 target=$3 mode=$4
[ -f "$config" ] && [ ! -L "$config" ] || exit 1
if [ "$mode" = reload ]; then
    if ! /bin/launchctl bootout "$target" >&2; then
        rc=0
        state=$(/bin/launchctl print "$target" 2>&1) || rc=$?
        [ "$rc" -ne 0 ] || exit 1
        case "$rc" in
            3|113) ;;
            *) printf '%s\n' "$state" >&2; exit 1 ;;
        esac
    fi
fi
printf '%s\n' detached
IFS= read -r ack && [ "$ack" = ready ] || exit 1
temp=$(/usr/bin/mktemp "${config%/*}/.steve-node.XXXXXXXX")
trap '/bin/rm -f -- "$temp"' EXIT
printf '%s' "$definition" > "$temp"
/usr/bin/plutil -lint -- "$temp" >&2
/usr/sbin/chown root:wheel "$temp"
/bin/chmod 0644 "$temp"
[ -f "$config" ] && [ ! -L "$config" ] || exit 1
/bin/mv -f -- "$temp" "$config"
if [ "$mode" = reload ]; then
    /bin/launchctl enable "$target" >&2
    /bin/launchctl bootstrap system "$config" >&2
fi
"#;

pub(super) fn execute_register(
    args: &[String],
    finish: &mut dyn FnMut() -> Result<()>,
) -> Result<()> {
    // sudo still asks on the controlling terminal, not on this protocol pipe.
    let mut child = Command::new(&args[0])
        .args(&args[1..])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .context("authorize the macOS Node boot registration")?;
    let mut input = child.stdin.take().context("missing registration input")?;
    let mut output = BufReader::new(child.stdout.take().context("missing registration output")?);
    let result = (|| {
        let mut ready = String::new();
        output.read_line(&mut ready)?;
        ensure!(
            ready == "detached\n",
            "administrator authorization did not complete; run `steve node enable-boot` in an interactive terminal on this Mac (or verify `sudo` access in headless/SSH sessions) and retry"
        );
        finish()?;
        writeln!(input, "ready")?;
        Ok(())
    })();
    drop(input);
    let status = child.wait();
    result?;
    let status = status?;
    ensure!(
        status.success(),
        "system Node registration failed ({status}); if running over SSH without a desktop session, verify `sudo` access and retry `steve node enable-boot`"
    );
    Ok(())
}

fn boot_release(home: &Path) -> Result<PathBuf> {
    let record: Value =
        serde_json::from_slice(&maintenance_file(&home.join("services/node.json"))?)?;
    let release = PathBuf::from(
        record["boot_release"]
            .as_str()
            .context("missing boot release")?,
    );
    ensure!(release.is_absolute(), "invalid boot release path");
    Ok(release)
}

static STOP_BOOT_NODE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

extern "C" fn stop_boot_node(_: libc::c_int) {
    STOP_BOOT_NODE.store(true, std::sync::atomic::Ordering::Relaxed);
}

/// Internal entrypoint for the owner-owned copy. Only launchd controls registration; the
/// supervisor owns the child and never sends a signal to a PID obtained from external state.
pub fn run_boot_node() -> Result<()> {
    ensure!(
        unsafe { libc::getuid() } != 0,
        "the Node must not run as root"
    );
    let home = PathBuf::from(std::env::var_os("STEVE_HOME").context("missing Node home")?);
    unsafe {
        libc::umask(0o077);
        let mut action: libc::sigaction = std::mem::zeroed();
        action.sa_sigaction = stop_boot_node as *const () as libc::sighandler_t;
        libc::sigemptyset(&mut action.sa_mask);
        for signal in [libc::SIGTERM, libc::SIGINT] {
            if libc::sigaction(signal, &action, std::ptr::null_mut()) != 0 {
                return Err(std::io::Error::last_os_error())
                    .context("install Node supervisor signal handler");
            }
        }
    }
    supervise(&home, || {
        STOP_BOOT_NODE.load(std::sync::atomic::Ordering::Relaxed)
    })
}

pub(super) fn supervise(home: &Path, stopping: impl Fn() -> bool) -> Result<()> {
    if stopping() {
        return Ok(());
    }
    let release = boot_release(home)?;
    let log = OpenOptions::new()
        .append(true)
        .create(true)
        .mode(0o600)
        .open(home.join("logs/node.log"))?;
    let mut command = Command::new(release.join("bin/steve-node"));
    command
        .arg("run")
        .env("STEVE_HOME", home)
        .env(NODE_BOOT_SERVICE_ENV, "1")
        .stdin(Stdio::null())
        .stdout(log.try_clone()?)
        .stderr(log)
        .env_remove("STEVE_NODE_LOOPBACK_PLAINTEXT");
    if read_json(&home.join("node/node.json"))?["loopback"] == true {
        command.env("STEVE_NODE_LOOPBACK_PLAINTEXT", "1");
    }
    let mut node = command.spawn().context("start configured Node release")?;
    loop {
        if let Some(status) = node.try_wait()? {
            ensure!(status.success(), "Node process exited: {status}");
            return Ok(());
        }
        if stopping() || boot_release(home).is_ok_and(|next| next != release) {
            // We are the sole waiter. If the child has already exited, its PID cannot have been
            // recycled before we reap it. Let the Node close its link and journal before launchd
            // starts the next generation.
            if unsafe { libc::kill(node.id() as libc::pid_t, libc::SIGTERM) } != 0 {
                let error = std::io::Error::last_os_error();
                if error.raw_os_error() != Some(libc::ESRCH) {
                    return Err(error).context("stop Node child");
                }
            }
            node.wait().context("wait for Node child to exit")?;
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(500));
    }
}

struct BootOperationLock(File);

impl Drop for BootOperationLock {
    fn drop(&mut self) {
        // A concurrent fork may temporarily inherit the descriptor despite CLOEXEC.
        // Release the operation explicitly so a completed action never blocks the next.
        let _ = self.0.unlock();
    }
}

impl Service {
    pub(super) fn boot_node(&self) -> bool {
        self.system == System::Darwin && self.component == "node"
    }

    pub(super) fn boot_dispatch(
        &mut self,
        action: &str,
        release: Option<&Path>,
        wait: u64,
    ) -> Result<Value> {
        if matches!(action, "status" | "doctor") {
            let mut report = self.status(release, action == "doctor")?;
            report["boot"] = json!(if report["service"]["installed"] == true {
                "registered"
            } else if self.record["boot_release"].is_string() && self.record["uninstalled"] != true
            {
                "enable_required"
            } else {
                "not_registered"
            });
            return Ok(report);
        }
        if action != "install" {
            self.require_registration(action)?;
        }
        if action == "install" {
            fs::create_dir_all(&self.home)?;
            directory(&self.home.join("services"))?;
        }
        let lock = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW)
            .open(self.home.join("services/node-boot.lock"))?;
        lock.try_lock()
            .context("another Node service operation is in progress")?;
        let _lock = BootOperationLock(lock);
        // Another CLI may have changed the receipt before this CLI acquired the lock.
        self.record = if exists(&self.record_path) {
            serde_json::from_slice(&maintenance_file(&self.record_path)?)?
        } else {
            json!({})
        };
        if action != "install" {
            self.require_registration(action)?;
        }
        ensure!(
            self.record
                .as_object()
                .is_some_and(|record| record.is_empty())
                || (self.record["component"] == "node" && self.record["label"] == self.label),
            "Node registration changed or belongs to another component or label"
        );
        match action {
            "enable-boot" => self.enable_boot(),
            "refresh-env" => self.refresh_boot_env(),
            "install" | "update" => self.prepare_boot(release, action == "update", wait),
            "start" | "restart" | "stop" => self.boot_action(action),
            "uninstall" => {
                if !exists(&self.config)
                    && self.record["uninstall"]["phase"] != "pending"
                    && self.record["uninstalled"] != true
                {
                    self.stopped()?;
                    self.record["uninstalled"] = json!(true);
                    atomic(&self.record_path, &json_bytes(&self.record)?)?;
                    Ok(
                        json!({"installed":false,"changed":true,"started":false,"home_preserved":true}),
                    )
                } else {
                    self.uninstall()
                }
            }
            _ => bail!("unsupported service action: {action}"),
        }
    }

    fn prepare_boot(&mut self, release: Option<&Path>, update: bool, wait: u64) -> Result<Value> {
        let release = release
            .map(Path::to_owned)
            .or_else(|| self.record["boot_release"].as_str().map(PathBuf::from))
            .unwrap_or_else(|| self.home.join("current"));
        let installed = exists(&self.config);
        if installed
            || (self.record["boot_release"].is_string() && self.record["uninstalled"] != true)
        {
            ensure!(
                self.record["uninstalled"] != true && self.record.get("uninstall").is_none(),
                "finish uninstall before changing the registered Node"
            );
        }
        let (source, info) = self.bundle(&release)?;
        let was_running = if installed {
            self.check_owner()?;
            if update {
                self.state()?["running"] == true
            } else {
                false
            }
        } else {
            false
        };
        let (release, info) = if update {
            self.ensure_managed_release(&source, &info)?
        } else {
            (source, info)
        };
        let identity = self.node_signing_identity()?;
        let requirement = identity.node_requirement();
        let runner = self.home.join(RUNNER);
        let runner_signed = !installed
            || self
                .node_signer
                .verify_binary(&runner, &identity.cli_requirement())
                .is_ok();
        let same = self.record["boot_release"].as_str() == release.to_str()
            && self.record["build"] == info
            && self.record["code_requirement"].as_str() == Some(requirement.as_str())
            && self.record["uninstalled"] != true
            && self.record.get("uninstall").is_none()
            && runner_signed;
        if installed
            || (self.record["boot_release"].is_string() && self.record["uninstalled"] != true)
        {
            if !same {
                let supervised = update && installed;
                if supervised {
                    self.check_owner()?;
                    ensure!(
                        self.record["boot_release"].as_str() != release.to_str(),
                        "extract the candidate into a separate release directory; no files changed"
                    );
                    // The owner-checked supervisor observes the receipt even without a
                    // GUI domain. A disabled system job stays disabled after the change.
                } else {
                    self.wait_unloaded("system", &self.config, wait)?;
                }
            } else {
                self.check_owner()?;
            }
        }
        if same {
            self.verify_release_signatures(&release, &identity)?;
        } else {
            self.sign_release_binaries(&release, &identity)?;
            if installed && !runner_signed {
                let cli = release.join("bin/steve");
                let source = if cli.is_file() {
                    cli
                } else if runner.is_file() {
                    runner.clone()
                } else {
                    std::env::current_exe()?
                };
                let mut staged = StagedRunnerGuard::new(self.node_signer.stage_runner(
                    &self.home,
                    &source,
                    &self.home.join("services"),
                    &identity,
                )?);
                staged.install_to(self.node_signer.as_ref(), &runner)?;
            }
            directory(&self.home.join("logs"))?;
            let previous = if update {
                self.record["boot_release"].clone()
            } else {
                self.record["previous"].clone()
            };
            self.write_boot_record(&release, &info, previous, requirement, None)?;
        }
        let started = update && installed && was_running;
        Ok(
            json!({"installed":installed,"prepared":true,"changed":!same,"started":started,
            "boot":if installed {"registered"} else {"enable_required"},
            "release":text(&release)?,"build":info}),
        )
    }

    pub(super) fn node_signing_identity(&self) -> Result<NodeSigningIdentity> {
        if let Some(pinned) = self.record["code_requirement"].as_str() {
            let identity = self.node_signer.load_identity(&self.home, &self.label)?;
            ensure!(
                identity.node_requirement() == pinned,
                "Node signing identity differs from registered code requirement"
            );
            Ok(identity)
        } else {
            self.node_signer.ensure_identity(&self.home, &self.label)
        }
    }

    fn sign_release_binaries(&self, release: &Path, identity: &NodeSigningIdentity) -> Result<()> {
        let node = release.join("bin/steve-node");
        self.node_signer
            .sign_binary(&self.home, &node, identity, &identity.node_identifier)?;
        self.node_signer
            .verify_binary(&node, &identity.node_requirement())?;
        let cli = release.join("bin/steve");
        if exists(&cli) {
            self.node_signer
                .sign_binary(&self.home, &cli, identity, &identity.cli_identifier)?;
            self.node_signer
                .verify_binary(&cli, &identity.cli_requirement())?;
        }
        Ok(())
    }

    pub(super) fn verify_release_signatures(
        &self,
        release: &Path,
        identity: &NodeSigningIdentity,
    ) -> Result<()> {
        self.node_signer.verify_binary(
            &release.join("bin/steve-node"),
            &identity.node_requirement(),
        )?;
        let cli = release.join("bin/steve");
        if exists(&cli) {
            self.node_signer
                .verify_binary(&cli, &identity.cli_requirement())?;
        }
        Ok(())
    }

    fn write_boot_record(
        &mut self,
        release: &Path,
        info: &Value,
        previous: Value,
        requirement: String,
        service_env: Option<&ServicePath>,
    ) -> Result<()> {
        // Deliberately not "release": an old Node can run here for migration/rollback but
        // must not advertise remote maintenance via its old gui-domain service helper.
        let release = text(release)?;
        ensure!(
            !release.contains(['\r', '\n']),
            "macOS Node release paths cannot contain newlines"
        );
        let mut record = json!({"component":"node","label":self.label,"boot_release":release,
            "build":info,"previous":previous,"code_requirement":requirement});
        if let Some(env) = service_env {
            record["service_env"] = env.to_json();
        } else if let Some(existing_env) = self.record.get("service_env").filter(|v| !v.is_null()) {
            record["service_env"] = existing_env.clone();
        }
        atomic(&self.record_path, &json_bytes(&record)?)?;
        self.record = record;
        Ok(())
    }

    /// Sign both staged executables with this installation's existing local self-signed identity
    /// and verify their pinned designated requirements before executing either binary.
    pub fn prepare_boot_upgrade(&self, candidate: &Path) -> Result<()> {
        ensure!(
            self.boot_node() && self.record["boot_release"].is_string(),
            "not a boot Node"
        );
        let identity = self.node_signer.load_identity(&self.home, &self.label)?;
        let node_req = self.record["code_requirement"]
            .as_str()
            .context("unverified Node signing identity")?;
        ensure!(
            node_req == identity.node_requirement(),
            "Node signing identity differs from registered code requirement"
        );
        self.node_signer
            .verify_binary(&self.home.join(RUNNER), &identity.cli_requirement())
            .context("unverified boot supervisor signing identity")?;
        self.node_signer.sign_binary(
            &self.home,
            &candidate.join("bin/steve-node"),
            &identity,
            &identity.node_identifier,
        )?;
        self.node_signer.sign_binary(
            &self.home,
            &candidate.join("bin/steve"),
            &identity,
            &identity.cli_identifier,
        )?;
        self.verify_boot_upgrade(candidate)
    }

    /// Check both staged executables against this installation's pinned local self-signed
    /// designated requirements (matching both the registered Node and the boot supervisor).
    pub fn verify_boot_upgrade(&self, candidate: &Path) -> Result<()> {
        ensure!(
            self.boot_node() && self.record["boot_release"].is_string(),
            "not a boot Node"
        );
        let identity = self.node_signer.load_identity(&self.home, &self.label)?;
        let node = self.record["code_requirement"]
            .as_str()
            .context("unverified Node signing identity")?;
        ensure!(
            node == identity.node_requirement(),
            "Node signing identity differs from registered code requirement"
        );
        let cli = identity.cli_requirement();
        self.node_signer
            .verify_binary(&self.home.join(RUNNER), &cli)
            .context("unverified boot supervisor signing identity")?;
        self.node_signer
            .verify_binary(&candidate.join("bin/steve-node"), node)?;
        self.node_signer
            .verify_binary(&candidate.join("bin/steve"), &cli)
    }

    fn sudo(&self, arguments: &[&str]) -> Result<Output> {
        self.command(
            std::iter::once("/usr/bin/sudo".into())
                .chain(arguments.iter().map(|s| s.to_string()))
                .collect(),
            true,
        )
    }

    fn system_launchctl(&self, arguments: &[&str]) -> Result<()> {
        let arguments = [&["/bin/launchctl"][..], arguments].concat();
        self.sudo(&arguments)?;
        Ok(())
    }

    pub(super) fn remove_boot_definition(&self) -> Result<()> {
        self.sudo(&["/bin/rm", "--", &text(&self.config)?])?;
        Ok(())
    }

    pub(super) fn account(&self) -> Result<String> {
        let output = self.command(vec!["/usr/bin/id".into(), "-un".into()], true)?;
        let name = output.stdout.trim();
        ensure!(
            !name.is_empty() && name != "root",
            "use the original non-root Node account"
        );
        Ok(name.into())
    }

    pub(super) fn boot_definition(&self, account: &str, path_env: &str) -> Result<Vec<u8>> {
        let mut output = Vec::new();
        plist::to_writer_xml(
            &mut output,
            &json!({
                "Label":self.label, "UserName":account, "InitGroups":true,
                "ProgramArguments":self.boot_arguments()?,
                "EnvironmentVariables":{
                    "HOME":text(&self.user_home)?, "STEVE_HOME":text(&self.home)?,
                    "PATH":path_env,
                },
                "RunAtLoad":true, "KeepAlive":true, "ExitTimeOut":600,
            }),
        )?;
        Ok(output)
    }

    pub(super) fn boot_arguments(&self) -> Result<Value> {
        Ok(json!([text(&self.home.join(RUNNER))?, RUNNER_ARG]))
    }

    fn replace_boot_definition(
        &mut self,
        account: &str,
        service_path: &ServicePath,
        reload: bool,
    ) -> Result<()> {
        let data = self.boot_definition(account, &service_path.path)?;
        let mode = if reload { "reload" } else { "static" };
        let args = vec![
            "/usr/bin/sudo".into(),
            "/bin/sh".into(),
            "-c".into(),
            UPDATE_BOOT_DEFINITION.into(),
            "steve-node-refresh-env".into(),
            std::str::from_utf8(&data)?.into(),
            text(&self.config)?,
            format!("system/{}", self.label),
            mode.into(),
        ];
        let env_json = service_path.to_json();
        let register = self.boot_registrar.clone();
        register(&args, &mut || {
            if reload {
                self.wait_unloaded("system", &self.config, 15)?;
                self.wait_node_exit(self.node_exit_wait)?;
            }
            self.record["service_env"] = env_json.clone();
            atomic(&self.record_path, &json_bytes(&self.record)?)?;
            Ok(())
        })?;
        ensure!(
            self.definition_bytes()? == data,
            "boot definition did not verify after environment refresh"
        );
        Ok(())
    }

    fn refresh_boot_env(&mut self) -> Result<Value> {
        ensure!(
            self.record["uninstalled"] != true && self.record.get("uninstall").is_none(),
            "finish uninstall and prepare an installation before refreshing the service environment"
        );
        ensure!(
            exists(&self.config),
            "macOS Node boot service is not installed; run steve node enable-boot"
        );
        self.check_owner()?;
        let state = self.state()?;
        ensure!(
            state["running"].is_boolean(),
            "cannot determine macOS Node service state: {}",
            state["diagnostic"]
                .as_str()
                .unwrap_or("unknown launchctl failure")
        );
        let account = self.account()?;
        let service_path = self.resolve_service_path();
        let expected = self.boot_definition(&account, &service_path.path)?;
        let same = self.definition_bytes()? == expected;
        let was_running = state["running"] == true;
        let was_loaded = state["loaded"] == true;
        let mut restarted = false;
        if same {
            self.record["service_env"] = service_path.to_json();
            atomic(&self.record_path, &json_bytes(&self.record)?)?;
        } else {
            let reload = was_running || was_loaded;
            self.replace_boot_definition(&account, &service_path, reload)?;
            restarted = reload;
        }
        let release = configured_release(&self.record)
            .map(str::to_owned)
            .unwrap_or_default();
        Ok(json!({
            "installed": true,
            "changed": !same,
            "restarted": restarted,
            "started": was_running || restarted,
            "boot": "registered",
            "release": release,
            "build": self.record["build"],
            "service_env": service_path.to_json(),
        }))
    }

    fn enable_boot(&mut self) -> Result<Value> {
        ensure!(
            self.record["uninstalled"] != true && self.record.get("uninstall").is_none(),
            "finish uninstall and prepare an installation before enabling boot"
        );
        let account = self.account()?;
        let release = PathBuf::from(
            configured_release(&self.record).context("Node has no prepared release")?,
        );
        self.check_owner()?;
        let service_path = self.resolve_service_path();
        if exists(&self.config) {
            let (release, info) = self.bundle(&release)?;
            let identity = self.node_signing_identity()?;
            self.sign_release_binaries(&release, &identity)?;
            let runner = self.home.join(RUNNER);
            if self
                .node_signer
                .verify_binary(&runner, &identity.cli_requirement())
                .is_err()
            {
                let mut staged = StagedRunnerGuard::new(self.node_signer.stage_runner(
                    &self.home,
                    &std::env::current_exe()?,
                    &self.home.join("services"),
                    &identity,
                )?);
                staged.install_to(self.node_signer.as_ref(), &runner)?;
            }
            let requirement = identity.node_requirement();
            if self.record["code_requirement"].as_str() != Some(requirement.as_str()) {
                self.write_boot_record(
                    &release,
                    &info,
                    self.record["previous"].clone(),
                    requirement,
                    None,
                )?;
            }
            let expected = self.boot_definition(&account, &service_path.path)?;
            if self.definition_bytes()? != expected {
                let state = self.state()?;
                ensure!(
                    state["running"].is_boolean(),
                    "cannot determine macOS Node service state: {}",
                    state["diagnostic"]
                        .as_str()
                        .unwrap_or("unknown launchctl failure")
                );
                let reload = state["running"] == true || state["loaded"] == true;
                self.replace_boot_definition(&account, &service_path, reload)?;
            }
        } else {
            let legacy_agent = self
                .user_home
                .join("Library/LaunchAgents")
                .join(format!("{}.plist", self.label));
            if exists(&legacy_agent) {
                let mut retry = format!(
                    "steve node enable-boot --home {}",
                    shell_quote(&text(&self.home)?)
                );
                if self.label != "io.github.infranite.steve.node" {
                    retry.push_str(&format!(" --label {}", shell_quote(&self.label)));
                }
                bail!(
                    "legacy LaunchAgent service definition still exists at {}; stop and remove it before enabling the system boot service:\n  launchctl bootout gui/$(id -u)/{} 2>/dev/null || true\n  rm -f {}\nthen re-run: {retry}",
                    legacy_agent.display(),
                    self.label,
                    shell_quote(&text(&legacy_agent)?)
                );
            }
            self.wait_unloaded("system", &self.config, 0)?;
            let (release, info) = self.bundle(&release)?;
            let identity = self.node_signing_identity()?;
            let requirement = identity.node_requirement();
            self.sign_release_binaries(&release, &identity)?;
            // Keep the supervisor independent of the selected (possibly older) runtime, and sign
            // it before requesting sudo so any signing failure aborts without touching launchd.
            let mut staged_runner = StagedRunnerGuard::new(self.node_signer.stage_runner(
                &self.home,
                &std::env::current_exe()?,
                &self.home.join("services"),
                &identity,
            )?);
            let data = self.boot_definition(&account, &service_path.path)?;
            let args = vec![
                "/usr/bin/sudo".into(),
                "/bin/sh".into(),
                "-c".into(),
                REGISTER.into(),
                "steve-node-register".into(),
                std::str::from_utf8(&data)?.into(),
                text(&self.config)?,
                format!("system/{}", self.label),
            ];
            let register = self.boot_registrar.clone();
            register(&args, &mut || {
                self.wait_node_exit(self.node_exit_wait)?;
                self.write_boot_record(
                    &release,
                    &info,
                    self.record["previous"].clone(),
                    requirement.clone(),
                    Some(&service_path),
                )?;
                directory(&self.home.join("logs"))?;
                staged_runner.install_to(self.node_signer.as_ref(), &self.home.join(RUNNER))?;
                Ok(())
            })?;
            ensure!(
                self.definition_bytes()? == data,
                "boot definition did not verify"
            );
            let mut status = self.status(None, false)?;
            status["registered_now"] = json!(true);
            return Ok(status);
        }
        if self.state()?["running"] == true {
            return self.status(None, false);
        }
        self.boot_action("start")
    }

    pub(super) fn wait_unloaded(&self, domain: &str, path: &Path, seconds: u64) -> Result<()> {
        let deadline = Instant::now()
            .checked_add(Duration::from_secs(seconds))
            .context("wait is too large")?;
        loop {
            let state = self.launch_state(domain, path, true)?;
            if state["loaded"] == false && state["running"] == false {
                return Ok(());
            }
            if state["running"].is_null() {
                bail!(
                    "service is loaded or its state is unknown ({domain}/{}: {}); no files changed by the stopped-state check. Stop it and try again",
                    self.label,
                    state["diagnostic"]
                        .as_str()
                        .unwrap_or("unknown launchctl failure")
                );
            }
            if Instant::now() >= deadline {
                bail!(
                    "service is loaded or its state is unknown; no files changed by the stopped-state check. Stop it and try again"
                );
            }
            std::thread::sleep(
                Duration::from_millis(200).min(deadline.saturating_duration_since(Instant::now())),
            );
        }
    }

    // Bootout may finish before the old process has released the runtime lock.
    // Check the existing lock (never create, truncate or remove it).
    fn wait_node_exit(&self, timeout: Duration) -> Result<()> {
        let lock_path = self.home.join("node/local/node.lock");
        let lock = OpenOptions::new()
            .read(true)
            .write(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
            .open(&lock_path);
        let mut lock = match lock {
            Ok(lock) => lock,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(e) => return Err(e).context("inspect existing Node runtime lock"),
        };
        let meta = lock.metadata()?;
        ensure!(
            meta.is_file() && meta.uid() == unsafe { libc::getuid() },
            "invalid Node runtime lock"
        );
        let until = Instant::now() + timeout;
        loop {
            match lock.try_lock() {
                Ok(()) => return lock.unlock().context("release existing Node runtime lock"),
                Err(std::fs::TryLockError::WouldBlock) if Instant::now() < until => {
                    std::thread::sleep(
                        Duration::from_millis(200)
                            .min(until.saturating_duration_since(Instant::now())),
                    );
                }
                Err(std::fs::TryLockError::WouldBlock) => {
                    let mut raw = [0u8; LOCK_PID_READ_LIMIT];
                    let recorded_pid = lock
                        .read(&mut raw)
                        .ok()
                        .and_then(|n| std::str::from_utf8(&raw[..n]).ok())
                        .and_then(parse_lock_pid);
                    // Close our descriptor before probing open-file holders so our own PID does not appear.
                    drop(lock);
                    let detail = self.describe_lock_holders(&lock_path, recorded_pid);
                    bail!(
                        "a Node process still owns the runtime lock{detail}; stop it before starting the boot service (for example, unload any legacy LaunchAgent with `launchctl bootout gui/$(id -u)/{}` or stop the listed PID, then re-run `steve node enable-boot`)",
                        self.label
                    );
                }
                Err(std::fs::TryLockError::Error(e)) => {
                    return Err(e).context("inspect existing Node runtime lock");
                }
            }
        }
    }

    fn describe_lock_holders(&self, lock_path: &Path, recorded_pid: Option<u32>) -> String {
        let mut pids = self.lock_holder_pids_via_lsof(lock_path);
        if pids.is_empty()
            && let Some(pid) = recorded_pid
        {
            pids.push(pid);
        }
        if pids.is_empty() {
            return String::new();
        }
        let holders: Vec<(u32, String)> = pids
            .into_iter()
            .map(|pid| {
                let binary = self
                    .process_binary_path(pid)
                    .unwrap_or_else(|| "unknown".into());
                (pid, binary)
            })
            .collect();
        if holders.len() == 1 {
            let (pid, binary) = &holders[0];
            format!(" (pid {pid}, binary {binary})")
        } else {
            let items = holders
                .iter()
                .map(|(pid, binary)| format!("pid {pid} (binary {binary})"))
                .collect::<Vec<_>>()
                .join(", ");
            format!(" ({items})")
        }
    }

    fn lock_holder_pids_via_lsof(&self, lock_path: &Path) -> Vec<u32> {
        let Ok(lock_text) = text(lock_path) else {
            return Vec::new();
        };
        let lsof = if self.system == System::Darwin {
            "/usr/sbin/lsof"
        } else {
            "lsof"
        };
        let Ok(out) = self.command(
            vec![lsof.into(), "-t".into(), "--".into(), lock_text],
            false,
        ) else {
            return Vec::new();
        };
        if out.code != 0 {
            return Vec::new();
        }
        let self_pid = std::process::id();
        let mut pids = Vec::new();
        for line in out.stdout.lines() {
            if let Ok(pid) = line.trim().parse::<u32>()
                && pid > 1
                && pid != self_pid
                && !pids.contains(&pid)
            {
                pids.push(pid);
            }
        }
        pids
    }

    fn process_binary_path(&self, pid: u32) -> Option<String> {
        if let Some(path) = native_process_binary_path(pid) {
            return Some(path);
        }
        let ps = if self.system == System::Darwin {
            "/bin/ps"
        } else {
            "ps"
        };
        let out = self
            .command(
                vec![
                    ps.into(),
                    "-p".into(),
                    pid.to_string(),
                    "-o".into(),
                    "comm=".into(),
                ],
                false,
            )
            .ok()?;
        (out.code == 0)
            .then(|| out.stdout.trim())
            .filter(|s| !s.is_empty())
            .map(str::to_owned)
    }

    fn boot_action(&self, action: &str) -> Result<Value> {
        let state = self.state()?;
        ensure!(
            state["running"].is_boolean(),
            "cannot determine macOS Node service state: {}",
            state["diagnostic"]
                .as_str()
                .unwrap_or("unknown launchctl failure")
        );
        if state["installed"] != true {
            ensure!(
                action == "stop" && state["loaded"] == false,
                "macOS Node boot service is not installed; run steve node enable-boot"
            );
            return Ok(state);
        }
        ensure!(
            self.record["boot_release"].is_string()
                && self.record["uninstalled"] != true
                && self.record.get("uninstall").is_none(),
            "boot service has no active Node registration"
        );
        let target = format!("system/{}", self.label);
        if matches!(action, "stop" | "restart") {
            // Persist an explicit stop across reboot, including in the middle of an upgrade.
            self.system_launchctl(&["disable", &target])?;
            if state["loaded"] == true {
                self.system_launchctl(&["bootout", &target])?;
            }
            self.wait_unloaded("system", &self.config, 15)?;
            self.wait_node_exit(self.node_exit_wait)?;
        }
        if matches!(action, "start" | "restart") {
            if action == "start" && state["loaded"] == false {
                self.wait_node_exit(self.node_exit_wait)?;
            }
            self.system_launchctl(&["enable", &target])?;
            if action == "restart" || state["loaded"] == false {
                self.system_launchctl(&["bootstrap", "system", &text(&self.config)?])?;
            } else if state["running"] == false {
                self.system_launchctl(&["kickstart", &target])?;
            }
        }
        self.status(None, false)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn privileged_registration_requires_both_authorization_and_user_ready() {
        for script in [REGISTER, UPDATE_BOOT_DEFINITION] {
            assert!(
                Command::new("/bin/sh")
                    .args(["-n", "-c", script])
                    .status()
                    .unwrap()
                    .success()
            );
        }
        let fixture = |script: &str| vec!["/bin/sh".into(), "-c".into(), script.into()];
        // A failed sudo/bootout writes no marker and must not reach owner-owned files.
        assert!(execute_register(&fixture("exit 1"), &mut || panic!("not authorized")).is_err());
        let handshake = fixture("printf 'detached\n'; IFS= read -r ack && [ \"$ack\" = ready ]");
        let failed = execute_register(&handshake, &mut || bail!("retirement was not safe"));
        assert_eq!(failed.unwrap_err().to_string(), "retirement was not safe");
        assert!(execute_register(&handshake, &mut || Ok(())).is_ok());
    }
}
