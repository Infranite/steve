//! Boot-time macOS Node. Only the system domain is implicitly loaded at boot.
//! Root installs the definition and controls that domain; the Node, including the
//! launcher that reads user-owned data, always runs as the original account.
use super::*;

// Stable across releases: the atomic service receipt is the only release pointer.
// Do not use launchd's log paths here: open the user's log *after* the UID drop.
const LAUNCH: &str = r#"umask 077
exec >>"$STEVE_HOME/logs/node.log" 2>&1
release=$(/usr/bin/plutil -extract boot_release raw -expect string -o - "$STEVE_HOME/services/node.json") || exit 1
case "$release" in /*) ;; *) exit 1 ;; esac
if [ "$(/usr/bin/plutil -extract loopback raw -expect bool -o - "$STEVE_HOME/node/node.json" 2>/dev/null)" = true ]; then
    export STEVE_NODE_LOOPBACK_PLAINTEXT=1
fi
exec "$release/bin/steve-node" run
"#;

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
        let (release, info) = self.bundle(&release)?;
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
                self.wait_unloaded("system", &self.config, wait)?;
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
        self.write_boot_record(&release, &info, previous)?;
        Ok(
            json!({"installed":installed,"prepared":true,"changed":!same,"started":false,
            "boot":if installed {"registered"} else {"enable_required"},
            "release":text(&release)?,"build":info}),
        )
    }

    fn write_boot_record(&mut self, release: &Path, info: &Value, previous: Value) -> Result<()> {
        // Deliberately not "release": an old Node can run here for migration/rollback but
        // must not advertise remote maintenance via its old gui-domain service helper.
        let release = text(release)?;
        ensure!(
            !release.contains(['\r', '\n']),
            "macOS Node release paths cannot contain newlines"
        );
        let record = json!({"component":"node","label":self.label,"boot_release":release,
            "build":info,"previous":previous});
        atomic(&self.record_path, &json_bytes(&record)?)?;
        self.record = record;
        Ok(())
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
                "ProgramArguments":["/bin/sh","-c",LAUNCH],
                "EnvironmentVariables":{
                    "HOME":text(&self.user_home)?, "STEVE_HOME":text(&self.home)?,
                    "PATH":std::env::var("PATH").unwrap_or_else(|_| "/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin".into()),
                },
                "RunAtLoad":true, "KeepAlive":true, "ExitTimeOut":600,
            }),
        )?;
        Ok(output)
    }

    fn enable_boot(&mut self) -> Result<Value> {
        ensure!(
            self.record["uninstalled"] != true && self.record.get("uninstall").is_none(),
            "finish uninstall and prepare an installation before enabling boot"
        );
        let account = self.account()?;
        let release = configured_release(&self.record).context("Node has no prepared release")?;
        let (release, info) = self.bundle(Path::new(release))?;
        self.check_owner()?;
        if exists(&self.config) {
            self.require_no_agent()?;
        } else {
            self.wait_unloaded("system", &self.config, 0)?;
            // A denied admin prompt must not take the old job down.
            self.sudo(&["-v"])?;
            self.retire_agent()?;
            self.wait_node_exit(15)?;
            if self.record["boot_release"].is_null() {
                self.write_boot_record(&release, &info, self.record["previous"].clone())?;
            }
            directory(&self.home.join("logs"))?;
            let data = self.boot_definition(&account)?;
            // Temporary non-loadable file. No user-owned program executes through sudo.
            let temp = tempfile::tempdir()?;
            let path = temp.path().join("definition");
            atomic(&path, &data)?;
            self.sudo(&[
                "/usr/bin/install",
                "-S",
                "-o",
                "root",
                "-g",
                "wheel",
                "-m",
                "0644",
                &text(&path)?,
                &text(&self.config)?,
            ])?;
            ensure!(
                self.definition_bytes()? == data,
                "boot definition did not verify"
            );
        }
        self.boot_action("start")
    }

    fn retire_agent(&self) -> Result<()> {
        let agent = self.legacy_config();
        let saved = self.home.join("services/node-launchagent.previous");
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
        let uid = unsafe { libc::getuid() };
        let domains = [format!("gui/{uid}"), format!("user/{uid}")];
        for domain in &domains {
            let state = self.launch_state(domain, &agent, true)?;
            ensure!(
                state["running"].is_boolean(),
                "cannot determine legacy launchd state"
            );
            if state["loaded"] == true {
                ensure!(
                    before.is_some()
                        || (exists(&saved) && {
                            self.check_launch_definition(&maintenance_file(&saved)?)?;
                            true
                        }),
                    "found a legacy job without an owned definition; inspect it first"
                );
                // The CLI may be running in SSH's audit session rather than the GUI session.
                self.system_launchctl(&["bootout", &format!("{domain}/{}", self.label)])?;
                self.wait_unloaded(domain, &agent, 15)?;
            }
        }
        if let Some(bytes) = before {
            atomic(&saved, &bytes)?;
            ensure!(
                maintenance_file(&agent)? == bytes,
                "legacy definition changed during migration"
            );
            fs::remove_file(&agent)?;
        }
        // The GUI domain might have been created during the migration. Never load the
        // system daemon until both old targets are confirmed absent.
        for domain in &domains {
            self.wait_unloaded(domain, &agent, 0)?;
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
