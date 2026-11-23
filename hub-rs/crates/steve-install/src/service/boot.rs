//! Boot-time macOS Node. Only the system domain is implicitly loaded at boot.
//! Root installs the definition and controls that domain; the Node, including the
//! launcher that reads user-owned data, always runs as the original account.
use super::*;
use std::io::{BufRead, BufReader};
use steve_types::devices::NODE_BOOT_SERVICE_ENV;

// launchd starts this executable directly as the original account. macOS can
// attribute the Node and its ordinary children to the same responsible code.
pub(super) const RUNNER: &str = "services/node-boot-runner";
const RUNNER_ARG: &str = "__node-boot-run";

// One sudo command, including migration over SSH. The shell never sources a user file or
// executes the launcher. Only after launchctl detaches the old job does the unprivileged CLI
// update its own files and acknowledge readiness. EOF (failure or interruption) aborts.
const REGISTER: &str = r#"set -eu
export LC_ALL=C
definition=$1 config=$2 target=$3
shift 3
[ ! -e "$config" ] && [ ! -L "$config" ] || exit 1
for legacy; do
    if ! /bin/launchctl bootout "$legacy" >&2; then
        # gui/ and user/ may name the same job; the first bootout can remove both.
        state=$(/bin/launchctl print "$legacy" 2>&1) && exit 1
        case "$state" in
            *"Could not find service"*|*"Could not find domain"*|*"No such process"*) ;;
            *) exit 1 ;;
        esac
    fi
done
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
            "administrator authorization or legacy detach did not complete"
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
        "system Node registration failed ({status}); run enable-boot again"
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

    pub(super) fn legacy_config(&self) -> PathBuf {
        self.user_home
            .join("Library/LaunchAgents")
            .join(format!("{}.plist", self.label))
    }

    pub(super) fn boot_dispatch(
        &mut self,
        action: &str,
        release: Option<&Path>,
        wait: u64,
    ) -> Result<Value> {
        if matches!(action, "status" | "doctor") {
            let mut report = self.status(release, action == "doctor")?;
            report["boot"] = json!(if self.legacy_registration() {
                "migration_required"
            } else if report["service"]["installed"] == true {
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
        if matches!(action, "install" | "update") {
            self.require_no_agent()?;
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
            "install" | "update" => self.prepare_boot(release, action == "update", wait),
            "start" | "restart" | "stop" => {
                self.require_no_agent()?;
                self.boot_action(action)
            }
            "uninstall" => {
                self.require_no_agent()?;
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

    fn legacy_registration(&self) -> bool {
        (self.record["release"].is_string()
            && self.record["boot_release"].is_null()
            && self.record["uninstalled"] != true)
            || exists(&self.legacy_config())
    }

    fn require_no_agent(&self) -> Result<()> {
        ensure!(
            !self.legacy_registration(),
            "legacy macOS Node service; finish sessions and run steve node enable-boot to migrate it first"
        );
        Ok(())
    }

    fn prepare_boot(&mut self, release: Option<&Path>, update: bool, wait: u64) -> Result<Value> {
        self.require_no_agent()?;
        let release = release
            .map(Path::to_owned)
            .or_else(|| self.record["boot_release"].as_str().map(PathBuf::from))
            .unwrap_or_else(|| self.home.join("current"));
        let (release, info, requirement) = self.boot_bundle(&release)?;
        let same = self.record["boot_release"].as_str() == release.to_str()
            && self.record["build"] == info
            && self.record["uninstalled"] != true
            && self.record.get("uninstall").is_none();
        let installed = exists(&self.config);
        if installed
            || (self.record["boot_release"].is_string() && self.record["uninstalled"] != true)
        {
            ensure!(
                self.record["uninstalled"] != true && self.record.get("uninstall").is_none(),
                "finish uninstall before changing the registered Node"
            );
            if !same {
                let supervised = update && installed;
                if supervised {
                    self.check_owner()?;
                    ensure!(
                        self.record["boot_release"].as_str() != release.to_str(),
                        "extract the candidate into a separate release directory; no files changed"
                    );
                    ensure!(
                        self.state()?["running"].is_boolean(),
                        "cannot determine macOS Node service state; no files changed"
                    );
                } else {
                    self.wait_unloaded("system", &self.config, wait)?;
                }
            } else {
                self.check_owner()?;
            }
        }
        directory(&self.home.join("logs"))?;
        let previous = if update && !same {
            self.record["boot_release"].clone()
        } else {
            self.record["previous"].clone()
        };
        // Identical requests must not replace a receipt or trigger a restart.
        if !same {
            self.write_boot_record(&release, &info, previous, requirement)?;
        }
        Ok(
            json!({"installed":installed,"prepared":true,"changed":!same,"started":false,
            "boot":if installed {"registered"} else {"enable_required"},
            "release":text(&release)?,"build":info}),
        )
    }

    fn boot_bundle(&self, release: &Path) -> Result<(PathBuf, Value, Option<String>)> {
        let release = resolve(release)?;
        let requirement = self.node_requirement(&release.join("bin/steve-node"))?;
        let (release, info) = self.bundle(&release)?;
        Ok((release, info, requirement))
    }

    fn write_boot_record(
        &mut self,
        release: &Path,
        info: &Value,
        previous: Value,
        requirement: Option<String>,
    ) -> Result<()> {
        // Deliberately not "release": an old Node can run here for migration/rollback but
        // must not advertise remote maintenance via its old gui-domain service helper.
        let release = text(release)?;
        ensure!(
            !release.contains(['\r', '\n']),
            "macOS Node release paths cannot contain newlines"
        );
        let record = json!({"component":"node","label":self.label,"boot_release":release,
            "build":info,"previous":previous,"code_requirement":requirement});
        atomic(&self.record_path, &json_bytes(&record)?)?;
        self.record = record;
        Ok(())
    }

    fn node_requirement(&self, candidate: &Path) -> Result<Option<String>> {
        let candidate = text(candidate)?;
        let requirement = self.record["code_requirement"].as_str().map(str::to_owned);
        let requirement = match requirement {
            Some(requirement) => Some(requirement),
            None => {
                let signature = self.command(
                    vec![
                        "/usr/bin/codesign".into(),
                        "--display".into(),
                        "-r-".into(),
                        "--verbose=4".into(),
                        candidate.clone(),
                    ],
                    false,
                )?;
                let developer_id = signature
                    .stderr
                    .lines()
                    .any(|line| line.starts_with("Authority=Developer ID Application:"));
                if signature.code != 0 || !developer_id {
                    return Ok(None);
                }
                Some(
                    signature
                        .stderr
                        .lines()
                        .find_map(|line| line.strip_prefix("designated => "))
                        .context("signed Node has no designated code requirement")?
                        .to_owned(),
                )
            }
        };
        if let Some(requirement) = &requirement {
            self.command(
                vec![
                    "/usr/bin/codesign".into(),
                    "--verify".into(),
                    "--strict".into(),
                    "-R".into(),
                    requirement.clone(),
                    candidate,
                ],
                true,
            )
            .context("Node signature must satisfy the installation's designated requirement")?;
        }
        Ok(requirement)
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

    fn boot_definition(&self, account: &str) -> Result<Vec<u8>> {
        let mut output = Vec::new();
        plist::to_writer_xml(
            &mut output,
            &json!({
                "Label":self.label, "UserName":account, "InitGroups":true,
                "ProgramArguments":self.boot_arguments()?,
                "EnvironmentVariables":{
                    "HOME":text(&self.user_home)?, "STEVE_HOME":text(&self.home)?,
                    "PATH":std::env::var("PATH").unwrap_or_else(|_| "/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin".into()),
                },
                "RunAtLoad":true, "KeepAlive":true, "ExitTimeOut":600,
            }),
        )?;
        Ok(output)
    }

    pub(super) fn boot_arguments(&self) -> Result<Value> {
        Ok(json!([text(&self.home.join(RUNNER))?, RUNNER_ARG]))
    }

    fn enable_boot(&mut self) -> Result<Value> {
        ensure!(
            self.record["uninstalled"] != true && self.record.get("uninstall").is_none(),
            "finish uninstall and prepare an installation before enabling boot"
        );
        let account = self.account()?;
        let release = configured_release(&self.record).context("Node has no prepared release")?;
        let (release, info, requirement) = self.boot_bundle(Path::new(release))?;
        self.check_owner()?;
        if exists(&self.config) {
            self.require_no_agent()?;
        } else {
            self.wait_unloaded("system", &self.config, 0)?;
            let (before, loaded) = self.legacy_before()?;
            let data = self.boot_definition(&account)?;
            let args = [
                "/usr/bin/sudo".into(),
                "/bin/sh".into(),
                "-c".into(),
                REGISTER.into(),
                "steve-node-register".into(),
                std::str::from_utf8(&data)?.into(),
                text(&self.config)?,
                format!("system/{}", self.label),
            ]
            .into_iter()
            .chain(loaded)
            .collect::<Vec<_>>();
            let register = self.boot_registrar.clone();
            register(&args, &mut || {
                self.retire_agent(before.as_deref())?;
                self.wait_node_exit(15)?;
                if self.record["boot_release"].is_null() {
                    self.write_boot_record(
                        &release,
                        &info,
                        self.record["previous"].clone(),
                        requirement.clone(),
                    )?;
                }
                directory(&self.home.join("logs"))?;
                // Keep the supervisor independent of the selected (possibly older) runtime.
                atomic_mode(
                    &self.home.join(RUNNER),
                    &fs::read(std::env::current_exe()?)?,
                    0o700,
                )?;
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

    fn legacy_domains() -> [String; 2] {
        let uid = unsafe { libc::getuid() };
        [format!("gui/{uid}"), format!("user/{uid}")]
    }

    fn legacy_before(&self) -> Result<(Option<Vec<u8>>, Vec<String>)> {
        let agent = self.legacy_config();
        let before = if exists(&agent) {
            let bytes = maintenance_file(&agent)?;
            ensure!(
                self.record["release"].is_string() && self.record["boot_release"].is_null(),
                "unexpected LaunchAgent with this label; inspect it before enabling boot"
            );
            self.check_launch_definition(&bytes)?;
            Some(bytes)
        } else {
            None
        };
        let mut loaded = Vec::new();
        for domain in Self::legacy_domains() {
            let state = self.launch_state(&domain, &agent, true)?;
            ensure!(
                state["running"].is_boolean(),
                "cannot determine legacy launchd state"
            );
            if state["loaded"] == true {
                ensure!(
                    before.is_some(),
                    "found a legacy job without an owned definition; inspect it first"
                );
                loaded.push(format!("{domain}/{}", self.label));
            }
        }
        Ok((before, loaded))
    }

    fn retire_agent(&self, before: Option<&[u8]>) -> Result<()> {
        let agent = self.legacy_config();
        for domain in Self::legacy_domains() {
            self.wait_unloaded(&domain, &agent, 15)?;
        }
        if let Some(bytes) = before {
            let saved = self.home.join("services/node-launchagent.previous");
            atomic(&saved, bytes)?;
            ensure!(
                maintenance_file(&agent)? == bytes,
                "legacy definition changed during migration"
            );
            fs::remove_file(&agent)?;
        }
        ensure!(
            !exists(&agent),
            "legacy definition reappeared during migration"
        );
        for domain in Self::legacy_domains() {
            self.wait_unloaded(&domain, &agent, 0)?;
        }
        Ok(())
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
            if state["running"].is_null() || Instant::now() >= deadline {
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
    fn wait_node_exit(&self, seconds: u64) -> Result<()> {
        let lock = OpenOptions::new()
            .write(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
            .open(self.home.join("node/local/node.lock"));
        let lock = match lock {
            Ok(lock) => lock,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(e) => return Err(e).context("inspect existing Node runtime lock"),
        };
        let meta = lock.metadata()?;
        ensure!(
            meta.is_file() && meta.uid() == unsafe { libc::getuid() },
            "invalid Node runtime lock"
        );
        let until = Instant::now() + Duration::from_secs(seconds);
        loop {
            match lock.try_lock() {
                Ok(()) => return lock.unlock().context("release existing Node runtime lock"),
                Err(std::fs::TryLockError::WouldBlock) if Instant::now() < until => {
                    std::thread::sleep(Duration::from_millis(200));
                }
                Err(std::fs::TryLockError::WouldBlock) => {
                    bail!(
                        "a Node process still owns the runtime lock; stop it before starting the boot service"
                    );
                }
                Err(std::fs::TryLockError::Error(e)) => {
                    return Err(e).context("inspect existing Node runtime lock");
                }
            }
        }
    }

    fn boot_action(&self, action: &str) -> Result<Value> {
        let state = self.state()?;
        ensure!(
            state["running"].is_boolean(),
            "cannot determine macOS Node service state"
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
            self.wait_node_exit(15)?;
        }
        if matches!(action, "start" | "restart") {
            if action == "start" && state["loaded"] == false {
                self.wait_node_exit(15)?;
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
        assert!(
            Command::new("/bin/sh")
                .args(["-n", "-c", REGISTER])
                .status()
                .unwrap()
                .success()
        );
        let fixture = |script: &str| vec!["/bin/sh".into(), "-c".into(), script.into()];
        // A failed sudo/bootout writes no marker and must not reach owner-owned files.
        assert!(execute_register(&fixture("exit 1"), &mut || panic!("not authorized")).is_err());
        let handshake = fixture("printf 'detached\n'; IFS= read -r ack && [ \"$ack\" = ready ]");
        let failed = execute_register(&handshake, &mut || bail!("retirement was not safe"));
        assert_eq!(failed.unwrap_err().to_string(), "retirement was not safe");
        assert!(execute_register(&handshake, &mut || Ok(())).is_ok());
    }
}
