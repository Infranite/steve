//! Per-component checks and controlled upgrades on the execution host. Registry and Homebrew
//! reads are coalesced background work; writes use the Node's journal, process tracking, and
//! idle-boundary queuing so active harness sessions are never interrupted mid-turn.

use std::collections::HashMap;
use std::ffi::CString;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::process::{Command as StdCommand, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde::Deserialize;
use steve_acp::adapters::{self, DetectedInstall, InstallEvidence, UpdateComponent, UpgradeAction};
use steve_acp::{AcpConfig, managed};
use steve_node_api::*;

use crate::journal::Tx;
use crate::{Decided, Inner, LocalNode, adapter_setup, now};

#[cfg(test)]
mod tests;

/// Cached registry/brew version checks stay fresh for 24 hours unless explicitly refreshed.
const CHECK_FRESHNESS_MS: i64 = 86_400_000;
/// Maximum parallel version lookups during one background check round.
const MAX_CONCURRENT_CHECKS: usize = 4;
/// Bound for reading a local CLI's `--version`.
const VERSION_PROBE_TIMEOUT: Duration = Duration::from_secs(5);
/// Bound for one `brew info --json=v2` metadata lookup.
const BREW_INFO_TIMEOUT: Duration = Duration::from_secs(60);
/// Bound for one external package-manager or CLI self-update command.
const EXTERNAL_UPGRADE_TIMEOUT: Duration = Duration::from_secs(15 * 60);
/// Fallback poll cadence alongside `inner.wake` while a queued upgrade waits for active sessions.
const IDLE_POLL_INTERVAL: Duration = Duration::from_millis(250);
/// Poll interval inside blocking subprocess wait loops.
const SUBPROCESS_POLL_INTERVAL: Duration = Duration::from_millis(100);

#[derive(Clone)]
struct Check {
    latest: Option<String>,
    installed: Option<String>,
    at: i64,
    error: Option<ComponentUpdateReason>,
}

#[derive(Default)]
pub(crate) struct Updates {
    checks: Mutex<HashMap<(String, String), Check>>,
    checking: AtomicBool,
    finished: Mutex<Option<i64>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BrewKind {
    Cask,
    Formula,
}

#[derive(Deserialize)]
struct BrewInfoV2 {
    #[serde(default)]
    casks: Vec<BrewCaskInfo>,
    #[serde(default)]
    formulae: Vec<BrewFormulaInfo>,
}

#[derive(Deserialize)]
struct BrewCaskInfo {
    version: Option<String>,
}

#[derive(Deserialize)]
struct BrewFormulaInfo {
    versions: Option<BrewFormulaVersions>,
}

#[derive(Deserialize)]
struct BrewFormulaVersions {
    stable: Option<String>,
}

#[derive(Clone)]
struct ResolvedComponent {
    package: Option<&'static str>,
    present: bool,
    managed: bool,
    symlink_escape: bool,
    detected: DetectedInstall,
    action: Option<UpgradeAction>,
    unsupported_reason: Option<ComponentUpdateReason>,
}

#[derive(Clone)]
struct PlannedComponent {
    component_id: String,
    action: UpgradeAction,
    /// `Some(ver)` for registry/brew checked upgrades; `None` for check-on-upgrade self-updaters.
    target: Option<String>,
}

#[derive(Clone)]
struct UpgradePlan {
    harness: String,
    items: Vec<PlannedComponent>,
}

impl UpgradePlan {
    fn needs_managed_prefix(&self) -> bool {
        self.items.iter().any(|i| i.action.needs_managed_prefix())
    }

    fn needs_npm(&self) -> bool {
        self.items.iter().any(|i| i.action.needs_npm())
    }

    fn needs_brew(&self) -> bool {
        self.items.iter().any(|i| i.action.needs_brew())
    }
}

fn version(raw: &str) -> Option<String> {
    let v = raw
        .trim()
        .split(',')
        .next()?
        .trim()
        .trim_start_matches('v')
        .trim_end_matches(|c: char| matches!(c, ')' | ',' | ';'));
    let core = v.split(['-', '+']).next()?;
    (v.len() < 128
        && v.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b".+-".contains(&b))
        && core.split('.').count() == 3
        && core
            .split('.')
            .all(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit())))
    .then(|| v.to_string())
}

fn command(c: &UpdateComponent) -> Option<PathBuf> {
    let p = c.command.as_ref()?;
    if p.is_file() {
        return Some(p.clone());
    }
    adapter_setup::executable(p)
}

fn find_brew(node: &Path) -> Option<PathBuf> {
    let beside = node
        .parent()
        .map(|d| d.join("brew"))
        .filter(|p| steve_acp::is_executable_file(p));
    beside.or_else(|| steve_acp::which_in("brew", std::env::var_os("PATH").as_deref()))
}

fn global_npm_root(node: &Path) -> Option<PathBuf> {
    let bin = node.parent()?;
    let prefix = bin.parent()?;
    let candidate = prefix.join("lib/node_modules");
    candidate
        .is_dir()
        .then(|| candidate.canonicalize().unwrap_or(candidate))
}

fn path_writable(path: &Path) -> bool {
    let check = |p: &Path| {
        CString::new(p.as_os_str().as_bytes())
            .ok()
            .is_some_and(|c| unsafe { libc::access(c.as_ptr(), libc::W_OK) == 0 })
    };
    check(path) && path.parent().is_none_or(check)
}

/// Detect Pi's own managed-install layout (`<root>/managed-install.json` beside `releases/`).
fn is_pi_managed_install(real_path: &Path) -> bool {
    real_path.ancestors().any(|a| {
        a.file_name().and_then(|n| n.to_str()) == Some("releases")
            && a.parent()
                .is_some_and(|root| root.join("managed-install.json").is_file())
    })
}

/// A version belongs to the actual selected command, never to an unrelated package nearby.
fn installed_npm_version(c: &UpdateComponent) -> Option<String> {
    let path = command(c)?.canonicalize().ok()?;
    let package = c.package?;
    let root = path.ancestors().find(|p| p.ends_with(package))?;
    let v: serde_json::Value =
        serde_json::from_slice(&std::fs::read(root.join("package.json")).ok()?).ok()?;
    if v.get("name")
        .and_then(serde_json::Value::as_str)
        .is_some_and(|n| n != package)
    {
        return None;
    }
    version(v["version"].as_str()?)
}

fn has_managed_symlink_escape(cfg: &AcpConfig, c: &UpdateComponent) -> bool {
    let Some(package) = c.package else {
        return false;
    };
    let root = managed::root(&cfg.steve_home);
    root.ancestors().take(2).any(|p| p.is_symlink())
        || Path::new(package)
            .ancestors()
            .filter(|p| !p.as_os_str().is_empty())
            .any(|p| root.join(p).is_symlink())
}

fn owned(cfg: &AcpConfig, id: &str, c: &UpdateComponent) -> bool {
    if !cfg.managed_adapter(id) {
        return false;
    }
    let Some(package) = c.package else {
        return false;
    };
    if has_managed_symlink_escape(cfg, c) {
        return false;
    }
    let root = managed::root(&cfg.steve_home);
    let (Ok(root), Some(path)) = (
        root.canonicalize(),
        command(c).and_then(|p| p.canonicalize().ok()),
    ) else {
        return false;
    };
    path.starts_with(root.join(package)) && installed_npm_version(c).is_some()
}

fn resolve_component(cfg: &AcpConfig, id: &str, c: &UpdateComponent) -> ResolvedComponent {
    let custom = cfg.declared.contains_key(id);
    let Some(original) = command(c) else {
        return ResolvedComponent {
            package: c.package,
            present: false,
            managed: false,
            symlink_escape: false,
            detected: DetectedInstall::External {
                reason: ComponentUpdateReason::NotInstalled,
            },
            action: None,
            unsupported_reason: Some(ComponentUpdateReason::NotInstalled),
        };
    };
    let symlink_escape = !custom && cfg.managed_adapter(id) && has_managed_symlink_escape(cfg, c);
    if custom || symlink_escape {
        let reason = if custom {
            ComponentUpdateReason::CustomManifest
        } else {
            ComponentUpdateReason::SymlinkEscape
        };
        return ResolvedComponent {
            package: c.package,
            present: true,
            managed: false,
            symlink_escape,
            detected: DetectedInstall::External {
                reason: reason.clone(),
            },
            action: None,
            unsupported_reason: Some(reason),
        };
    }

    let real = original.canonicalize().unwrap_or_else(|_| original.clone());
    let steve_managed = owned(cfg, id, c);
    let user_home = cfg
        .user_home
        .canonicalize()
        .unwrap_or_else(|_| cfg.user_home.clone());
    let npm_global = global_npm_root(&cfg.node);
    let writable_target = c
        .package
        .and_then(|pkg| real.ancestors().find(|a| a.ends_with(pkg)))
        .unwrap_or(&real);
    let writable = path_writable(writable_target);
    let pi_managed = is_pi_managed_install(&real);

    let (detected, action, unsupported_reason) = adapters::resolve_component_upgrade(
        id,
        c,
        &InstallEvidence {
            original_path: &original,
            real_path: &real,
            steve_managed,
            user_home: &user_home,
            npm_global_root: npm_global.as_deref(),
            pi_managed_install: pi_managed,
            writable,
        },
    );
    ResolvedComponent {
        package: c.package,
        present: true,
        managed: steve_managed,
        symlink_escape: false,
        detected,
        action,
        unsupported_reason,
    }
}

async fn executable_version(cfg: &AcpConfig, c: &UpdateComponent) -> Option<String> {
    if let Some(v) = installed_npm_version(c) {
        return Some(v);
    }
    // Only built-in catalog entries reach this function; manifest commands are excluded.
    let path = command(c)?;
    let mut cmd = if steve_acp::is_node_script(&path) && !steve_acp::is_executable_file(&path) {
        let mut cmd = tokio::process::Command::new(&cfg.node);
        cmd.arg(&path);
        cmd
    } else {
        tokio::process::Command::new(path)
    };
    let output = tokio::time::timeout(
        VERSION_PROBE_TIMEOUT,
        cmd.arg("--version")
            .stdin(Stdio::null())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .output(),
    )
    .await
    .ok()?
    .ok()?;
    if !output.status.success() {
        return None;
    }
    String::from_utf8_lossy(&output.stdout)
        .split_whitespace()
        .find_map(version)
}

fn run_subprocess_observed(
    program: &Path,
    args: &[&str],
    timeout: Duration,
    on_spawn: impl FnOnce(u32) -> Result<(), String>,
) -> Result<String, SubprocessError> {
    use std::os::unix::process::CommandExt;
    let mut cmd = StdCommand::new(program);
    let mut child = cmd
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0)
        .spawn()
        .map_err(|_| SubprocessError::SpawnFailed)?;
    if let Err(reason) = on_spawn(child.id()) {
        let _ = child.kill();
        let _ = child.wait();
        return Err(SubprocessError::Journal(reason));
    }
    let stdout_handle = child.stdout.take().map(|mut r| {
        std::thread::spawn(move || {
            let mut s = String::new();
            let _ = std::io::Read::read_to_string(&mut r, &mut s);
            s
        })
    });
    let stderr_handle = child.stderr.take().map(|mut r| {
        std::thread::spawn(move || {
            let mut s = String::new();
            let _ = std::io::Read::read_to_string(&mut r, &mut s);
            s
        })
    });
    let until = Instant::now() + timeout;
    let status = loop {
        if let Some(status) = child.try_wait().map_err(|_| SubprocessError::WaitFailed)? {
            break status;
        }
        if Instant::now() >= until {
            unsafe {
                libc::kill(-(child.id() as libc::pid_t), libc::SIGKILL);
            }
            let _ = child.kill();
            let _ = child.wait();
            return Err(SubprocessError::TimedOut);
        }
        std::thread::sleep(SUBPROCESS_POLL_INTERVAL);
    };
    let out = stdout_handle
        .and_then(|h| h.join().ok())
        .unwrap_or_default();
    let _err = stderr_handle
        .and_then(|h| h.join().ok())
        .unwrap_or_default();
    if status.success() {
        Ok(out)
    } else {
        Err(SubprocessError::NonZeroExit(status.code()))
    }
}

enum SubprocessError {
    SpawnFailed,
    WaitFailed,
    TimedOut,
    NonZeroExit(Option<i32>),
    Journal(String),
}

impl SubprocessError {
    fn safe_message(&self, label: &str) -> String {
        match self {
            Self::SpawnFailed => format!(
                "{label} could not be started; verify the binary and PATH on this Node, then retry"
            ),
            Self::WaitFailed => {
                format!("{label} exit could not be observed; inspect this Node before retrying")
            }
            Self::TimedOut => format!(
                "{label} timed out; check network and package manager state on this Node, then retry"
            ),
            Self::NonZeroExit(Some(code)) => format!(
                "{label} exited with code {code}; check package manager state, network and permissions on this Node, then retry"
            ),
            Self::NonZeroExit(None) => {
                format!("{label} was terminated by a signal; inspect this Node and retry")
            }
            Self::Journal(reason) => reason.clone(),
        }
    }
}

fn brew_latest(brew: &Path, kind: BrewKind, token: &str) -> Result<String, ComponentUpdateReason> {
    let args: Vec<&str> = match kind {
        BrewKind::Cask => vec!["info", "--json=v2", "--cask", token],
        BrewKind::Formula => vec!["info", "--json=v2", token],
    };
    let out = run_subprocess_observed(brew, &args, BREW_INFO_TIMEOUT, |_| Ok(()))
        .map_err(|_| ComponentUpdateReason::BrewInfoFailed)?;
    parse_brew_latest_json(kind, &out).ok_or(ComponentUpdateReason::BrewVersionMissing)
}

fn parse_brew_latest_json(kind: BrewKind, raw_json: &str) -> Option<String> {
    let info: BrewInfoV2 = serde_json::from_str(raw_json).ok()?;
    let raw = match kind {
        BrewKind::Cask => info.casks.into_iter().next().and_then(|c| c.version),
        BrewKind::Formula => info
            .formulae
            .into_iter()
            .next()
            .and_then(|f| f.versions)
            .and_then(|v| v.stable),
    };
    raw.as_deref().and_then(version)
}

impl Updates {
    pub(crate) fn schedule(node: &LocalNode, fresh: bool) {
        let this = &node.0.harness_updates;
        if !fresh
            && this
                .finished
                .lock()
                .unwrap()
                .is_some_and(|t| now() - t < CHECK_FRESHNESS_MS)
        {
            return;
        }
        if this.checking.swap(true, Ordering::SeqCst) {
            return;
        }
        let inner = node.0.clone();
        // The guard also clears checking if shutdown cancels the owned task.
        struct Guard(Arc<Inner>);
        impl Drop for Guard {
            fn drop(&mut self) {
                self.0
                    .harness_updates
                    .checking
                    .store(false, Ordering::SeqCst);
            }
        }
        let guard = Guard(inner.clone());
        node.spawn_owned(async move {
            let _guard = guard;
            let cfg = &inner.cfg.acp;
            let npm = managed::npm(Some(&cfg.node));
            let brew = find_brew(&cfg.node);
            let mut tasks = tokio::task::JoinSet::new();
            let limit = Arc::new(tokio::sync::Semaphore::new(MAX_CONCURRENT_CHECKS));
            for id in cfg.adapters.keys() {
                // The manifest may name arbitrary programs; neither execute nor query them.
                if !cfg.adapter_spec(id).builtin || cfg.declared.contains_key(id) {
                    continue;
                }
                for c in adapters::update_components(cfg, id) {
                    if command(&c).is_none() {
                        continue;
                    }
                    let resolved = resolve_component(cfg, id, &c);
                    let (id, cfg, npm, brew, limit) = (
                        id.clone(),
                        cfg.clone(),
                        npm.clone(),
                        brew.clone(),
                        limit.clone(),
                    );
                    tasks.spawn(async move {
                        let _permit = limit.acquire_owned().await.expect("open semaphore");
                        let installed = executable_version(&cfg, &c).await;
                        let (latest, error) =
                            query_latest(&resolved, npm.clone(), brew.clone()).await;
                        let check = Check {
                            latest,
                            installed,
                            at: now(),
                            error,
                        };
                        ((id, c.id.to_string()), check)
                    });
                }
            }
            while let Some(Ok((key, check))) = tasks.join_next().await {
                inner
                    .harness_updates
                    .checks
                    .lock()
                    .unwrap()
                    .insert(key, check);
            }
            *inner.harness_updates.finished.lock().unwrap() = Some(now());
        });
    }

    pub(crate) async fn report(&self, inner: &Arc<Inner>) -> ApiResult<HarnessUpdateReport> {
        let cfg = &inner.cfg.acp;
        // Read the flag before the snapshot so a finishing check never labels an older map complete.
        let checking = self.checking.load(Ordering::SeqCst);
        let checks = self.checks.lock().unwrap().clone();
        let maintenance = inner.adapter_maintenance.lock().unwrap().clone();
        let (active_sessions, receipts) = inner.j.read(|t| {
            let receipts = cfg
                .adapters
                .keys()
                .map(|id| Ok((id.clone(), t.installation(&format!("upgrade:{id}"))?)))
                .collect::<ApiResult<HashMap<_, _>>>()?;
            Ok((t.active_sessions_by_harness()?, receipts))
        })?;
        let hard_blocked = if !inner.cfg.ceiling.exec {
            Some(HarnessUpgradeBlockReason::ExecDenied)
        } else if inner.node_upgrade_pending() {
            Some(HarnessUpgradeBlockReason::NodeUpgradeInProgress)
        } else if inner.retiring.load(Ordering::SeqCst) {
            Some(HarnessUpgradeBlockReason::NodeRemoving)
        } else if maintenance.is_some() {
            Some(HarnessUpgradeBlockReason::MaintenanceBusy)
        } else {
            None
        };
        let has_brew = find_brew(&cfg.node).is_some();
        let has_npm = managed::npm(Some(&cfg.node))
            .and_then(|p| adapter_setup::executable(&p))
            .is_some();

        let mut harnesses = Vec::new();
        for id in cfg.adapters.keys() {
            let custom = cfg.declared.contains_key(id);
            let specs = adapters::update_components(cfg, id);
            let resolved_list: Vec<_> = specs
                .iter()
                .map(|c| (c, resolve_component(cfg, id, c)))
                .collect();
            let needs_managed_req = resolved_list.iter().any(|(_, r)| {
                r.action
                    .as_ref()
                    .is_some_and(UpgradeAction::needs_managed_prefix)
            });
            let managed_req = if needs_managed_req {
                Some(adapter_setup::requirements(cfg, id).await)
            } else {
                None
            };
            let managed_req_reason = managed_req.as_ref().and_then(|r| {
                if r.npm_error.is_some() {
                    Some(ComponentUpdateReason::ToolMissing { tool: "npm".into() })
                } else if r.node_error.is_some() {
                    Some(ComponentUpdateReason::NodeRuntimeIncompatible)
                } else {
                    None
                }
            });

            let mut components = Vec::with_capacity(resolved_list.len());
            for (c, r) in &resolved_list {
                let check = checks.get(&(id.clone(), c.id.into()));
                let installed = installed_npm_version(c).or_else(|| {
                    r.present
                        .then(|| check.and_then(|v| v.installed.clone()))
                        .flatten()
                });
                let latest = check.and_then(|v| v.latest.clone());
                let action_tool_reason = match &r.action {
                    Some(a) if a.needs_managed_prefix() => managed_req_reason.clone(),
                    Some(a) if a.needs_npm() && !has_npm => {
                        Some(ComponentUpdateReason::ToolMissing { tool: "npm".into() })
                    }
                    Some(a) if a.needs_brew() && !has_brew => {
                        Some(ComponentUpdateReason::ToolMissing {
                            tool: "brew".into(),
                        })
                    }
                    _ => None,
                };
                let is_self_check_only = matches!(r.action, Some(UpgradeAction::SelfUpdate { .. }))
                    && (c.package.is_none() || (!has_npm && latest.is_none()));
                let state = if custom {
                    ComponentUpdateState::Unsupported
                } else if !r.present {
                    ComponentUpdateState::NotInstalled
                } else if check.is_some_and(|v| v.error.is_some()) {
                    ComponentUpdateState::CheckFailed
                } else if is_self_check_only {
                    if installed.is_some() {
                        ComponentUpdateState::CheckOnUpgrade
                    } else {
                        ComponentUpdateState::Unchecked
                    }
                } else if c.package.is_none()
                    && !matches!(
                        r.detected,
                        DetectedInstall::HomebrewCask { .. }
                            | DetectedInstall::HomebrewFormula { .. }
                    )
                {
                    ComponentUpdateState::Unsupported
                } else if installed.is_none() {
                    ComponentUpdateState::Unsupported
                } else if let Some(latest) = &latest {
                    if managed::newer(latest, installed.as_deref().unwrap()) {
                        ComponentUpdateState::Available
                    } else {
                        ComponentUpdateState::Current
                    }
                } else {
                    ComponentUpdateState::Unchecked
                };
                let comp_can_upgrade = r.present
                    && r.action.is_some()
                    && action_tool_reason.is_none()
                    && !matches!(
                        state,
                        ComponentUpdateState::NotInstalled | ComponentUpdateState::Unsupported
                    );
                let reason = check
                    .and_then(|v| v.error.clone())
                    .or(action_tool_reason)
                    .or_else(|| r.unsupported_reason.clone());
                components.push(HarnessComponentUpdate {
                    id: c.id.into(),
                    name: c.name.clone(),
                    package: c.package.map(String::from),
                    managed: r.managed,
                    source: r.detected.source(),
                    upgrade_method: r
                        .action
                        .as_ref()
                        .map_or(UpgradeMethod::Unsupported, UpgradeAction::method),
                    upgrade_command: r.action.as_ref().map(UpgradeAction::display_command),
                    can_upgrade: comp_can_upgrade,
                    state,
                    installed,
                    latest,
                    checked: check.map(|v| Timestamp(v.at)),
                    reason,
                });
            }
            let has_upgradable_target = components.iter().any(|c| {
                c.can_upgrade
                    && matches!(
                        c.state,
                        ComponentUpdateState::Available | ComponentUpdateState::CheckOnUpgrade
                    )
            });
            let op = receipts.get(id).cloned().flatten();
            let op_in_flight = op.as_ref().is_some_and(|r| {
                matches!(
                    r.state,
                    Observed::Accepted | Observed::Running | Observed::Waiting { .. }
                )
            });
            let harness_sessions = active_sessions.get(id).copied().unwrap_or(0);
            let blocked_reason = hard_blocked
                .or_else(|| op_in_flight.then_some(HarnessUpgradeBlockReason::AlreadyRunning));
            harnesses.push(HarnessUpdateInfo {
                harness: id.clone(),
                name: cfg.adapter_spec(id).name,
                components,
                can_upgrade: has_upgradable_target && hard_blocked.is_none() && !op_in_flight,
                active_sessions: harness_sessions,
                blocked_reason,
                operation: op,
            });
        }
        harnesses.sort_by(|a, b| a.harness.cmp(&b.harness));
        Ok(HarnessUpdateReport {
            node: inner.cfg.node.clone(),
            checking,
            harnesses,
        })
    }
}

async fn query_latest(
    resolved: &ResolvedComponent,
    npm: Option<PathBuf>,
    brew: Option<PathBuf>,
) -> (Option<String>, Option<ComponentUpdateReason>) {
    match &resolved.detected {
        DetectedInstall::HomebrewCask { cask } => {
            let Some(brew) = brew else {
                return (
                    None,
                    Some(ComponentUpdateReason::ToolMissing {
                        tool: "brew".into(),
                    }),
                );
            };
            let cask = cask.clone();
            match tokio::task::spawn_blocking(move || brew_latest(&brew, BrewKind::Cask, &cask))
                .await
            {
                Ok(Ok(v)) => (Some(v), None),
                Ok(Err(e)) => (None, Some(e)),
                Err(_) => (None, Some(ComponentUpdateReason::BrewInfoFailed)),
            }
        }
        DetectedInstall::HomebrewFormula { formula } => {
            let Some(brew) = brew else {
                return (
                    None,
                    Some(ComponentUpdateReason::ToolMissing {
                        tool: "brew".into(),
                    }),
                );
            };
            let formula = formula.clone();
            match tokio::task::spawn_blocking(move || {
                brew_latest(&brew, BrewKind::Formula, &formula)
            })
            .await
            {
                Ok(Ok(v)) => (Some(v), None),
                Ok(Err(e)) => (None, Some(e)),
                Err(_) => (None, Some(ComponentUpdateReason::BrewInfoFailed)),
            }
        }
        _ => match (npm, resolved.package) {
            (Some(npm), Some(package)) => {
                let latest = tokio::task::spawn_blocking(move || managed::latest(&npm, package))
                    .await
                    .ok()
                    .and_then(Result::ok)
                    .and_then(|v| version(&v));
                let err = latest
                    .is_none()
                    .then_some(ComponentUpdateReason::NpmRegistryFailed);
                (latest, err)
            }
            (None, Some(_)) => {
                // Standalone self-updater can still run without npm installed.
                if matches!(resolved.action, Some(UpgradeAction::SelfUpdate { .. })) {
                    (None, None)
                } else {
                    (
                        None,
                        Some(ComponentUpdateReason::ToolMissing { tool: "npm".into() }),
                    )
                }
            }
            (_, None) => (None, None),
        },
    }
}

fn harness_work_active(t: &Tx, harness: &str, needs_managed_prefix: bool) -> ApiResult<bool> {
    if needs_managed_prefix {
        return Ok(adapter_setup::idle(t).is_err());
    }
    let sessions = t.active_sessions_by_harness()?;
    Ok(sessions.get(harness).copied().unwrap_or(0) > 0)
}

impl LocalNode {
    pub(crate) async fn upgrade_components(
        &self,
        c: Command<UpgradeHarness>,
    ) -> ApiResult<Receipt> {
        self.0.cfg.ceiling.check_exec()?;
        // Same-id retries always observe the old command, including after a restart.
        if self.0.j.read(|t| t.op(&c.id))?.is_some() {
            return self
                .accept("upgrade_harness", &c, |_| unreachable!())
                .map(|(r, _)| r);
        }
        let cfg = &self.0.cfg.acp;
        let id = &c.body.harness;
        let plan = build_upgrade_plan(&self.0, &c).await?;

        // Validate only the tools actually required by the planned actions.
        let mut npm = None;
        if plan.needs_managed_prefix() {
            let req = adapter_setup::requirements(cfg, id).await;
            if let Some(e) = req.node_error.or(req.npm_error) {
                return Err(Error::new(Code::Unsupported, e));
            }
            npm = req.npm;
        } else if plan.needs_npm() {
            npm = managed::npm(Some(&cfg.node)).and_then(|p| adapter_setup::executable(&p));
            if npm.is_none() {
                return Err(Error::new(
                    Code::Unsupported,
                    "npm is missing; install npm beside Node.js or add it to the Node service PATH",
                ));
            }
        }
        let brew = if plan.needs_brew() {
            let b = find_brew(&cfg.node);
            if b.is_none() {
                return Err(Error::new(
                    Code::Unsupported,
                    "brew is missing; install Homebrew or add it to the Node service PATH",
                ));
            }
            b
        } else {
            None
        };

        let needs_prefix = plan.needs_managed_prefix();
        let mut prefix = None;
        let mut mode = AdmissionMode::NoChange;
        let (receipt, fresh) = self.accept("upgrade_harness", &c, |t| {
            if self.0.node_upgrade_pending() {
                return Err(Error::new(
                    Code::Busy,
                    "A Node release upgrade is in progress",
                ));
            }
            if let Some(prior) = t.installation(&format!("upgrade:{id}"))?
                && matches!(
                    prior.state,
                    Observed::Accepted | Observed::Running | Observed::Waiting { .. }
                )
            {
                return Err(Error::new(
                    Code::Busy,
                    "An upgrade for this harness is already queued or running",
                ));
            }
            let busy = harness_work_active(t, id, needs_prefix)?;
            let changed = recheck_plan_sync(&self.0, &plan)?;
            if !changed {
                t.set_installation(&format!("upgrade:{id}"), &c.id)?;
                if needs_prefix {
                    t.set_installation(id, &c.id)?;
                }
                let mut d = Decided::accepted();
                d.state = succeeded();
                return Ok(d);
            }
            if busy {
                // Active sessions/work exist: queue to idle boundary instead of refusing.
                t.set_installation(&format!("upgrade:{id}"), &c.id)?;
                mode = AdmissionMode::Queued;
                let mut d = Decided::accepted();
                d.state = Observed::Waiting { on: vec![] };
                return Ok(d);
            }
            if needs_prefix {
                prefix = Some(
                    managed::prefix_lock(&cfg.steve_home, true)
                        .map_err(|e| Error::new(Code::Busy, e))?,
                );
            }
            t.set_installation(&format!("upgrade:{id}"), &c.id)?;
            if needs_prefix {
                t.set_installation(id, &c.id)?;
            }
            mode = AdmissionMode::Immediate;
            let mut d = Decided::accepted();
            d.state = Observed::Running;
            Ok(d)
        })?;

        if fresh && mode != AdmissionMode::NoChange {
            let (inner, op, plan) = (self.0.clone(), c.id.clone(), plan);
            let immediate_lease = (mode == AdmissionMode::Immediate)
                .then(|| adapter_setup::Lease::held_optional(inner.clone(), prefix));
            let started = self.spawn_owned(async move {
                let lease = match immediate_lease {
                    Some(lease) => lease,
                    None => match wait_for_idle_boundary(&inner, &op, &plan).await {
                        Ok(lease) => lease,
                        Err(state) => {
                            if inner.j.tx(|t| t.set_state(&op, &state)).is_err() {
                                inner.fault();
                            }
                            inner.wake.notify_one();
                            return;
                        }
                    },
                };
                run_planned_upgrade(inner, op, plan, npm, brew, lease).await;
            });
            if !started {
                self.0.j.tx(|t| {
                    t.set_state(
                        &c.id,
                        &Observed::Unknown {
                            reason: "Node shut down before upgrade started".into(),
                        },
                    )
                })?;
            }
        }
        Ok(receipt)
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum AdmissionMode {
    NoChange,
    Immediate,
    Queued,
}

async fn wait_for_idle_boundary(
    inner: &Arc<Inner>,
    op: &OperationId,
    plan: &UpgradePlan,
) -> Result<adapter_setup::Lease, Observed> {
    let needs_prefix = plan.needs_managed_prefix();
    loop {
        if inner.closed.load(Ordering::SeqCst) {
            return Err(Observed::Unknown {
                reason: "Node shut down while upgrade waited for active sessions to finish".into(),
            });
        }
        if inner.retiring.load(Ordering::SeqCst) {
            return Err(failed(
                Code::Busy,
                "Node is being removed before the queued upgrade could start",
            ));
        }
        let acquired = {
            let mut maintenance = inner.adapter_maintenance.lock().unwrap();
            if maintenance.is_none() && !inner.node_upgrade_pending() {
                let attempt: Result<Option<Option<managed::PrefixLock>>, Error> = inner.j.tx(|t| {
                    if adapter_setup::no_orphan_installer(t).is_err() {
                        return Ok(None);
                    }
                    if harness_work_active(t, &plan.harness, needs_prefix)? {
                        return Ok(None);
                    }
                    let prefix_lock = if needs_prefix {
                        match managed::prefix_lock(&inner.cfg.acp.steve_home, true) {
                            Ok(lock) => Some(lock),
                            Err(_) => return Ok(None),
                        }
                    } else {
                        None
                    };
                    if let Err(e) = recheck_plan_sync(inner, plan) {
                        return Err(e);
                    }
                    if needs_prefix {
                        t.set_installation(&plan.harness, op)?;
                    }
                    t.set_state(op, &Observed::Running)?;
                    Ok(Some(prefix_lock))
                });
                match attempt {
                    Ok(Some(lock)) => {
                        *maintenance = Some(plan.harness.clone());
                        Some(Ok(adapter_setup::Lease::held_optional(inner.clone(), lock)))
                    }
                    Ok(None) => None,
                    Err(e) => Some(Err(Observed::Finished {
                        outcome: Outcome::Failed,
                        result: None,
                        error: Some(e),
                    })),
                }
            } else {
                None
            }
        };
        if let Some(res) = acquired {
            return res;
        }
        tokio::select! {
            _ = inner.wake.notified() => {},
            _ = tokio::time::sleep(IDLE_POLL_INTERVAL) => {},
        }
    }
}

async fn run_planned_upgrade(
    inner: Arc<Inner>,
    op: OperationId,
    plan: UpgradePlan,
    npm: Option<PathBuf>,
    brew: Option<PathBuf>,
    lease: adapter_setup::Lease,
) {
    let _lease = lease;
    let prefix = _lease.optional_prefix();
    let _io = inner.adapter_io.write().await;
    let process_key = SessionId::new("maintenance_adapters").expect("static id");
    let task_inner = inner.clone();
    let task_plan = plan.clone();
    let exec_result = tokio::task::spawn_blocking(move || {
        execute_plan(
            &task_inner,
            &task_plan,
            npm.as_deref(),
            brew.as_deref(),
            prefix.as_deref(),
            &process_key,
        )
    })
    .await
    .map_err(|_| "Upgrade task interrupted; inspect this Node before retrying".to_string())
    .and_then(|r| r);

    let verified = match exec_result {
        Ok(()) => verify_and_refresh_checks(&inner, &plan).await,
        Err(e) => Err(e),
    };
    let state = match verified {
        Ok(()) => succeeded(),
        Err(e) => Observed::Finished {
            outcome: Outcome::Failed,
            result: None,
            error: Some(Error::new(Code::Internal, e)),
        },
    };
    if inner.j.tx(|t| t.set_state(&op, &state)).is_err() {
        inner.fault();
    }
    // Always invalidate cached adapter and CLI binary probes after any upgrade attempt.
    inner.adapter_probes.forget().await;
    inner.probes.forget().await;
    inner.wake.notify_one();
}

/// Validate the selected components against this Node's resolved install sources and checked versions.
async fn build_upgrade_plan(inner: &Inner, c: &Command<UpgradeHarness>) -> ApiResult<UpgradePlan> {
    let cfg = &inner.cfg.acp;
    let id = &c.body.harness;
    if c.body.versions.is_empty() && c.body.self_update.is_empty() {
        return Err(Error::new(
            Code::Unsupported,
            "No upgradable components selected",
        ));
    }
    if !cfg.adapter_spec(id).builtin || cfg.declared.contains_key(id) {
        return Err(Error::new(
            Code::Unsupported,
            "Only built-in components without custom manifest overrides may be upgraded",
        ));
    }
    let components = adapters::update_components(cfg, id);
    let mut items = Vec::new();

    for (component, target) in &c.body.versions {
        let spec = components
            .iter()
            .find(|s| s.id == component)
            .ok_or_else(|| Error::new(Code::Invalid, "Unknown component id"))?;
        let resolved = resolve_component(cfg, id, spec);
        if resolved.symlink_escape {
            return Err(Error::new(
                Code::Forbidden,
                "The selected component is external, missing or overridden; manage it on the Node",
            ));
        }
        let Some(action) = resolved.action else {
            let code = if cfg.managed_adapter(id) {
                Code::Forbidden
            } else {
                Code::Unsupported
            };
            return Err(Error::new(
                code,
                resolved
                    .unsupported_reason
                    .as_ref()
                    .map_or("custom_install_path", ComponentUpdateReason::code),
            ));
        };
        let installed = executable_version(cfg, spec).await.ok_or_else(|| {
            Error::new(
                Code::Stale,
                "Component changed during the version read; refresh and retry",
            )
        })?;
        if installed != *target {
            let checks = inner.harness_updates.checks.lock().unwrap();
            let check = checks
                .get(&(id.clone(), component.clone()))
                .filter(|v| v.error.is_none() && now() - v.at < CHECK_FRESHNESS_MS);
            if version(target).as_ref() != Some(target)
                || check.and_then(|v| v.latest.as_ref()) != Some(target)
                || !managed::newer(target, &installed)
            {
                return Err(Error::new(
                    Code::Stale,
                    "Component versions changed or have not been checked on this Node; refresh before upgrading",
                ));
            }
        }
        items.push(PlannedComponent {
            component_id: component.clone(),
            action,
            target: Some(target.clone()),
        });
    }

    for component in &c.body.self_update {
        if c.body.versions.contains_key(component) {
            return Err(Error::new(
                Code::Invalid,
                "Duplicate component in versions and self_update",
            ));
        }
        let spec = components
            .iter()
            .find(|s| s.id == component)
            .ok_or_else(|| Error::new(Code::Invalid, "Unknown component id"))?;
        let resolved = resolve_component(cfg, id, spec);
        match resolved.action {
            Some(action @ UpgradeAction::SelfUpdate { .. }) => {
                items.push(PlannedComponent {
                    component_id: component.clone(),
                    action,
                    target: None,
                });
            }
            _ => {
                return Err(Error::new(
                    Code::Unsupported,
                    "Self-update is not supported for this component",
                ));
            }
        }
    }

    Ok(UpgradePlan {
        harness: id.clone(),
        items,
    })
}

/// Fast synchronous recheck under the journal lock: detects install-source tampering and npm races
/// without spawning subprocesses while holding SQLite write locks.
fn recheck_plan_sync(inner: &Inner, plan: &UpgradePlan) -> ApiResult<bool> {
    let cfg = &inner.cfg.acp;
    let components = adapters::update_components(cfg, &plan.harness);
    let checks = inner.harness_updates.checks.lock().unwrap();
    let mut changed = false;
    for item in &plan.items {
        let spec = components
            .iter()
            .find(|s| s.id == item.component_id)
            .ok_or_else(|| Error::new(Code::Stale, "Components changed before admission"))?;
        let resolved = resolve_component(cfg, &plan.harness, spec);
        if resolved.action.as_ref() != Some(&item.action) {
            return Err(Error::new(
                Code::Stale,
                "Component installation changed before admission; refresh and retry",
            ));
        }
        let Some(target) = &item.target else {
            changed = true;
            continue;
        };
        if item.action.needs_npm() {
            let current = installed_npm_version(spec).ok_or_else(|| {
                Error::new(
                    Code::Stale,
                    "Component changed before admission; refresh and retry",
                )
            })?;
            if managed::newer(&current, target) {
                return Err(Error::new(
                    Code::Stale,
                    "Components changed before admission; refresh and retry",
                ));
            }
            if current != *target {
                let check = checks
                    .get(&(plan.harness.clone(), item.component_id.clone()))
                    .filter(|v| v.error.is_none() && now() - v.at < CHECK_FRESHNESS_MS);
                if check.and_then(|v| v.latest.as_ref()) != Some(target)
                    || !managed::newer(target, &current)
                {
                    return Err(Error::new(
                        Code::Stale,
                        "Refresh component versions before retrying",
                    ));
                }
                changed = true;
            }
        } else {
            // Non-npm binary (Homebrew or standalone self-update): compare against last check.
            let check = checks
                .get(&(plan.harness.clone(), item.component_id.clone()))
                .filter(|v| v.error.is_none() && now() - v.at < CHECK_FRESHNESS_MS);
            if check.and_then(|v| v.latest.as_ref()) != Some(target) {
                return Err(Error::new(
                    Code::Stale,
                    "Refresh component versions before retrying",
                ));
            }
            if check.and_then(|v| v.installed.as_deref()) != Some(target.as_str()) {
                changed = true;
            }
        }
    }
    Ok(changed)
}

fn execute_plan(
    task_inner: &Inner,
    plan: &UpgradePlan,
    npm: Option<&Path>,
    brew: Option<&Path>,
    prefix: Option<&managed::PrefixLock>,
    process_key: &SessionId,
) -> Result<(), String> {
    // 1. Run all Steve-managed npm packages in one batched `npm install --prefix`.
    let managed_specs: Vec<(&str, &str)> = plan
        .items
        .iter()
        .filter_map(|item| match (&item.action, &item.target) {
            (UpgradeAction::SteveManagedNpm { package }, Some(target)) => {
                Some((*package, target.as_str()))
            }
            _ => None,
        })
        .collect();
    if !managed_specs.is_empty() {
        let npm = npm.ok_or_else(|| "npm is missing on this Node".to_string())?;
        let prefix = prefix.ok_or_else(|| "managed prefix lock missing".to_string())?;
        run_managed_npm(task_inner, npm, &managed_specs, prefix, process_key)?;
    }

    // 2. Run external official upgraders one by one with durable process registration.
    for item in &plan.items {
        match &item.action {
            UpgradeAction::SteveManagedNpm { .. } => {}
            UpgradeAction::HomebrewCask { cask } => {
                let brew = brew.ok_or_else(|| "brew is missing on this Node".to_string())?;
                run_tracked_command(
                    task_inner,
                    process_key,
                    brew,
                    &["upgrade", "--cask", cask.as_str()],
                    "Homebrew cask upgrade",
                )?;
            }
            UpgradeAction::HomebrewFormula { formula } => {
                let brew = brew.ok_or_else(|| "brew is missing on this Node".to_string())?;
                run_tracked_command(
                    task_inner,
                    process_key,
                    brew,
                    &["upgrade", formula.as_str()],
                    "Homebrew formula upgrade",
                )?;
            }
            UpgradeAction::NpmGlobal {
                prefix: npm_prefix,
                package,
            } => {
                let npm = npm.ok_or_else(|| "npm is missing on this Node".to_string())?;
                let target = item.target.as_deref().ok_or_else(|| {
                    "Global npm upgrade requires a checked target version".to_string()
                })?;
                let prefix_str = npm_prefix.to_string_lossy().into_owned();
                let spec = format!("{package}@{target}");
                run_tracked_command(
                    task_inner,
                    process_key,
                    npm,
                    &[
                        "install",
                        "-g",
                        "--prefix",
                        &prefix_str,
                        "--no-audit",
                        "--no-fund",
                        &spec,
                    ],
                    "Global npm upgrade",
                )?;
            }
            UpgradeAction::SelfUpdate { executable, args } => {
                let (program, full_args): (&Path, Vec<String>) =
                    if steve_acp::is_node_script(executable)
                        && !steve_acp::is_executable_file(executable)
                    {
                        let mut v = vec![executable.to_string_lossy().into_owned()];
                        v.extend(args.iter().map(|s| (*s).to_string()));
                        (&task_inner.cfg.acp.node, v)
                    } else {
                        (
                            executable.as_path(),
                            args.iter().map(|s| (*s).to_string()).collect(),
                        )
                    };
                let arg_refs: Vec<&str> = full_args.iter().map(String::as_str).collect();
                run_tracked_command(
                    task_inner,
                    process_key,
                    program,
                    &arg_refs,
                    "CLI self-update",
                )?;
            }
        }
    }
    Ok(())
}

fn run_managed_npm(
    task_inner: &Inner,
    npm: &Path,
    specs: &[(&str, &str)],
    prefix: &managed::PrefixLock,
    process_key: &SessionId,
) -> Result<(), String> {
    let mut process = None;
    let result = managed::install_versions_observed(
        npm,
        &task_inner.cfg.acp.steve_home,
        specs,
        prefix,
        |pid| {
            let started = crate::procs::start_time(i64::from(pid));
            task_inner
                .j
                .tx(|t| {
                    t.put_proc(
                        process_key,
                        i64::from(pid),
                        0,
                        task_inner.incarnation,
                        "adapter-upgrade",
                        started,
                    )
                })
                .map_err(|_| "Could not record installer identity".to_string())?;
            process = Some((i64::from(pid), started));
            Ok(())
        },
    );
    if let Some((pid, started)) = process {
        if !crate::procs::wait_gone(pid, started).gone() {
            return Err("Installer exit is unknown; reconcile this Node before retrying".into());
        }
        task_inner
            .j
            .tx(|t| t.drop_proc(process_key, pid))
            .map_err(|_| "Could not record installer exit".to_string())?;
    }
    result.map_err(|_| {
        "npm upgrade failed or timed out; check this Node's network, npm configuration, disk space and Node.js version, then retry".to_string()
    })
}

fn run_tracked_command(
    task_inner: &Inner,
    process_key: &SessionId,
    program: &Path,
    args: &[&str],
    label: &str,
) -> Result<(), String> {
    let mut process = None;
    let result = run_subprocess_observed(program, args, EXTERNAL_UPGRADE_TIMEOUT, |pid| {
        let started = crate::procs::start_time(i64::from(pid));
        task_inner
            .j
            .tx(|t| {
                t.put_proc(
                    process_key,
                    i64::from(pid),
                    0,
                    task_inner.incarnation,
                    "adapter-upgrade",
                    started,
                )
            })
            .map_err(|_| "Could not record installer identity".to_string())?;
        process = Some((i64::from(pid), started));
        Ok(())
    });
    if let Some((pid, started)) = process {
        if !crate::procs::wait_gone(pid, started).gone() {
            return Err("Installer exit is unknown; reconcile this Node before retrying".into());
        }
        task_inner
            .j
            .tx(|t| t.drop_proc(process_key, pid))
            .map_err(|_| "Could not record installer exit".to_string())?;
    }
    result.map(|_| ()).map_err(|e| e.safe_message(label))
}

async fn verify_and_refresh_checks(inner: &Inner, plan: &UpgradePlan) -> Result<(), String> {
    let cfg = &inner.cfg.acp;
    let specs = adapters::update_components(cfg, &plan.harness);
    for item in &plan.items {
        let spec = specs
            .iter()
            .find(|s| s.id == item.component_id)
            .ok_or_else(|| "Upgraded component disappeared from adapter catalog".to_string())?;
        let post_version = executable_version(cfg, spec).await;
        match (&item.action, &item.target) {
            (UpgradeAction::SteveManagedNpm { .. }, Some(target)) => {
                if !owned(cfg, &plan.harness, spec) || post_version.as_deref() != Some(target) {
                    return Err(
                        "npm exited successfully but the requested component version was not installed; inspect this Node and retry"
                            .into(),
                    );
                }
            }
            (UpgradeAction::NpmGlobal { .. }, Some(target)) => {
                if post_version.as_deref() != Some(target) {
                    return Err(
                        "Global npm exited successfully but the requested version was not installed; inspect this Node and retry"
                            .into(),
                    );
                }
            }
            (_, Some(target)) => {
                let Some(actual) = post_version.as_deref() else {
                    return Err(
                        "Upgrade command exited successfully but the updated CLI version could not be read back; inspect this Node"
                            .into(),
                    );
                };
                // External upgraders (brew / self-update) install latest, which may equal or exceed target.
                if managed::newer(target, actual) {
                    return Err(format!(
                        "Upgrade command exited successfully but installed version is still {actual} (expected >= {target})"
                    ));
                }
            }
            (UpgradeAction::SelfUpdate { .. }, None) => {
                if command(spec).is_none() || post_version.is_none() {
                    return Err(
                        "CLI self-update exited successfully but the binary version could not be verified; inspect this Node"
                            .into(),
                    );
                }
            }
            _ => {}
        }
        // Reflect the verified post-upgrade version immediately in the check cache.
        if let Some(actual) = post_version {
            let mut checks = inner.harness_updates.checks.lock().unwrap();
            let entry = checks
                .entry((plan.harness.clone(), item.component_id.clone()))
                .or_insert_with(|| Check {
                    latest: None,
                    installed: None,
                    at: now(),
                    error: None,
                });
            entry.installed = Some(actual.clone());
            if entry
                .latest
                .as_deref()
                .is_some_and(|l| managed::newer(&actual, l))
            {
                entry.latest = Some(actual);
            }
            entry.error = None;
            entry.at = now();
        }
    }
    Ok(())
}

fn succeeded() -> Observed {
    Observed::Finished {
        outcome: Outcome::Succeeded,
        result: Some(OperationResult::Unit),
        error: None,
    }
}

fn failed(code: Code, detail: impl Into<String>) -> Observed {
    Observed::Finished {
        outcome: Outcome::Failed,
        result: None,
        error: Some(Error::new(code, detail)),
    }
}
