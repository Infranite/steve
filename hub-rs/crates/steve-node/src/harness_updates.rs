//! Per-component checks and controlled upgrades on the execution host. Registry reads are
//! coalesced background work; writes use the same admission, journal and prefix lease as setup.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use steve_acp::adapters::{self, UpdateComponent};
use steve_acp::{AcpConfig, managed};
use steve_node_api::*;

use crate::{Decided, Inner, LocalNode, adapter_setup, now};

#[cfg(test)]
mod tests;

#[derive(Clone)]
struct Check {
    latest: Option<String>,
    installed: Option<String>,
    at: i64,
    error: Option<String>,
}

#[derive(Default)]
pub(crate) struct Updates {
    checks: Mutex<HashMap<(String, String), Check>>,
    checking: AtomicBool,
    finished: Mutex<Option<i64>>,
}

fn version(raw: &str) -> Option<String> {
    let v = raw.trim().trim_start_matches('v');
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

/// A version belongs to the actual selected command, never to an unrelated package nearby.
fn installed(c: &UpdateComponent) -> Option<String> {
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

fn owned(cfg: &AcpConfig, id: &str, c: &UpdateComponent) -> bool {
    if !cfg.managed_adapter(id) {
        return false;
    }
    let Some(package) = c.package else {
        return false;
    };
    let root = managed::root(&cfg.steve_home);
    // Do not let a scoped package or one of its parents redirect npm outside the prefix.
    if root.ancestors().take(2).any(|p| p.is_symlink())
        || Path::new(package)
            .ancestors()
            .filter(|p| !p.as_os_str().is_empty())
            .any(|p| root.join(p).is_symlink())
    {
        return false;
    }
    let (Ok(root), Some(path)) = (
        root.canonicalize(),
        command(c).and_then(|p| p.canonicalize().ok()),
    ) else {
        return false;
    };
    path.starts_with(root.join(package)) && installed(c).is_some()
}

async fn executable_version(cfg: &AcpConfig, c: &UpdateComponent) -> Option<String> {
    if let Some(v) = installed(c) {
        return Some(v);
    }
    // Only the registry built-ins reach this function. Native CLIs without a supported
    // update source still report --version; manifest commands are excluded by schedule.
    let path = command(c)?;
    let mut cmd = if steve_acp::is_node_script(&path) {
        let mut cmd = tokio::process::Command::new(&cfg.node);
        cmd.arg(&path);
        cmd
    } else {
        tokio::process::Command::new(path)
    };
    let output = tokio::time::timeout(
        Duration::from_secs(5),
        cmd.arg("--version")
            .stdin(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
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

impl Updates {
    pub(crate) fn schedule(node: &LocalNode, fresh: bool) {
        let this = &node.0.harness_updates;
        if !fresh
            && (managed::UpdatePolicy::from_env() == managed::UpdatePolicy::Off
                || this
                    .finished
                    .lock()
                    .unwrap()
                    .is_some_and(|t| now() - t < 86_400_000))
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
            let mut tasks = tokio::task::JoinSet::new();
            let limit = Arc::new(tokio::sync::Semaphore::new(4));
            for id in cfg.adapters.keys() {
                // The manifest may name arbitrary programs; neither execute nor query them.
                if !cfg.adapter_spec(id).builtin || cfg.declared.contains_key(id) { continue; }
                for c in adapters::update_components(cfg, id) {
                    if command(&c).is_none() { continue; }
                    let (id, cfg, npm, limit) = (id.clone(), cfg.clone(), npm.clone(), limit.clone());
                    tasks.spawn(async move {
                        let _permit = limit.acquire_owned().await.expect("open semaphore");
                        let installed = executable_version(&cfg, &c).await;
                        let latest = match (npm, c.package) {
                            (Some(npm), Some(package)) => tokio::task::spawn_blocking(move || managed::latest(&npm, package))
                                .await.ok().and_then(Result::ok).and_then(|v| version(&v)),
                            _ => None,
                        };
                        let check = Check { error: (c.package.is_some() && latest.is_none()).then(|| "Cannot query the npm registry; check npm and network configuration on this Node".into()), latest, installed, at: now() };
                        ((id, c.id.to_string()), check)
                    });
                }
            }
            while let Some(Ok((key, check))) = tasks.join_next().await {
                inner.harness_updates.checks.lock().unwrap().insert(key, check);
            }
            *inner.harness_updates.finished.lock().unwrap() = Some(now());
        });
    }

    pub(crate) fn report(&self, inner: &Arc<Inner>) -> ApiResult<HarnessUpdateReport> {
        let cfg = &inner.cfg.acp;
        // Read the flag before the snapshot. A finishing check must not label an older cloned
        // map as complete; at worst a just-finished round asks the caller for one more read.
        let checking = self.checking.load(Ordering::SeqCst);
        let checks = self.checks.lock().unwrap().clone();
        let maintenance = inner.adapter_maintenance.lock().unwrap();
        let (busy, receipts) = inner.j.read(|t| {
            let receipts = cfg
                .adapters
                .keys()
                .map(|id| Ok((id.clone(), t.installation(&format!("upgrade:{id}"))?)))
                .collect::<ApiResult<HashMap<_, _>>>()?;
            Ok((adapter_setup::idle(t).err(), receipts))
        })?;
        let blocked = if !inner.cfg.ceiling.exec {
            Some("This Node's execution policy denies component upgrades".to_string())
        } else if inner.node_upgrade_pending() {
            Some("A Node release upgrade is in progress; finish it first".into())
        } else if inner.retiring.load(Ordering::SeqCst) {
            Some("This Node is being removed".into())
        } else if maintenance.is_some() {
            Some("Installation or upgrade is running; wait for its receipt".into())
        } else {
            busy.map(|e| e.safe_detail)
        };
        let mut harnesses = Vec::new();
        for id in cfg.adapters.keys() {
            let custom = cfg.declared.contains_key(id);
            let components = adapters::update_components(cfg, id).iter().map(|c| {
                let check = checks.get(&(id.clone(), c.id.into()));
                let present = command(c).is_some();
                let installed = installed(c).or_else(|| present.then(|| check.and_then(|v| v.installed.clone())).flatten());
                let managed = !custom && owned(cfg, id, c);
                let latest = check.and_then(|v| v.latest.clone());
                let state = if custom || c.package.is_none() { ComponentUpdateState::Unsupported }
                    else if !present { ComponentUpdateState::NotInstalled }
                    else if check.is_some_and(|v| v.error.is_some()) { ComponentUpdateState::CheckFailed }
                    else if installed.is_none() { ComponentUpdateState::Unsupported }
                    else if let Some(latest) = &latest {
                        if managed::newer(latest, installed.as_deref().unwrap()) { ComponentUpdateState::Available } else { ComponentUpdateState::Current }
                    } else { ComponentUpdateState::Unchecked };
                let reason = if custom || c.package.is_none() { Some("No supported official update source for this command; update it with its operator on the Node".into()) }
                    else if !present { Some("Component is not installed; installation is a separate explicit action".into()) }
                    else if !managed { Some("Externally managed or overridden command; update it on the Node with its original installer, then check again".into()) }
                    else { None };
                HarnessComponentUpdate { id: c.id.into(), name: c.name.clone(), package: c.package.map(String::from), managed, state, installed, latest, checked: check.map(|v| Timestamp(v.at)), reason: check.and_then(|v| v.error.clone()).or(reason) }
            }).collect::<Vec<_>>();
            let available = components
                .iter()
                .any(|c| c.managed && c.state == ComponentUpdateState::Available);
            harnesses.push(HarnessUpdateInfo {
                harness: id.clone(),
                name: cfg.adapter_spec(id).name,
                components,
                can_upgrade: available && blocked.is_none(),
                reason: blocked.clone(),
                operation: receipts.get(id).cloned().flatten(),
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
        let (components, packages) = upgrade_packages(&self.0, &c)?;
        let req = adapter_setup::requirements(cfg, id).await;
        if let Some(e) = req.node_error.or(req.npm_error) {
            return Err(Error::new(Code::Unsupported, e));
        }
        let mut prefix = None;
        let mut changed = false;
        let (receipt, fresh) = self.accept("upgrade_harness", &c, |t| {
            if self.0.node_upgrade_pending() {
                return Err(Error::new(
                    Code::Busy,
                    "A Node release upgrade is in progress",
                ));
            }
            adapter_setup::idle(t)?;
            prefix = Some(
                managed::prefix_lock(&cfg.steve_home, true)
                    .map_err(|e| Error::new(Code::Busy, e))?,
            );
            changed = recheck_components(&self.0, &components, &c.body)?;
            t.set_installation(&format!("upgrade:{id}"), &c.id)?;
            t.set_installation(id, &c.id)?;
            let mut d = Decided::accepted();
            d.state = if changed {
                Observed::Running
            } else {
                succeeded()
            };
            Ok(d)
        })?;
        if fresh && changed {
            let (inner, op, body) = (self.0.clone(), c.id.clone(), c.body.clone());
            let npm = req.npm.expect("checked npm");
            let lease = adapter_setup::Lease::held(inner.clone(), prefix.expect("reserved prefix"));
            let started = self.spawn_owned(async move {
                let _lease = lease;
                let prefix = _lease.prefix();
                let _io = inner.adapter_io.write().await;
                let process_key = SessionId::new("maintenance_adapters").expect("static id");
                let task_inner = inner.clone();
                let result = tokio::task::spawn_blocking(move || {
                    install_versions(&task_inner, &body, &packages, &npm, &prefix, &process_key)
                })
                .await
                .map_err(|_| {
                    "Upgrade task interrupted; inspect this Node before retrying".to_string()
                })
                .and_then(|r| r);
                let state = match result {
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
                inner.adapter_probes.forget().await;
                inner.wake.notify_one();
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

/// Validate the selected components against this Node's owned, recently checked versions.
fn upgrade_packages(
    inner: &Inner,
    c: &Command<UpgradeHarness>,
) -> ApiResult<(Vec<UpdateComponent>, BTreeMap<String, String>)> {
    let cfg = &inner.cfg.acp;
    let id = &c.body.harness;
    if !cfg.managed_adapter(id) || c.body.versions.is_empty() {
        return Err(Error::new(
            Code::Unsupported,
            "Only installed components in this Node's managed prefix may be upgraded",
        ));
    }
    let components = adapters::update_components(cfg, id);
    let mut packages = BTreeMap::new();
    {
        let checks = inner.harness_updates.checks.lock().unwrap();
        for (component, target) in &c.body.versions {
            let spec = components
                .iter()
                .find(|s| s.id == component)
                .ok_or_else(|| Error::new(Code::Invalid, "Unknown component id"))?;
            if !owned(cfg, id, spec) {
                return Err(Error::new(
                    Code::Forbidden,
                    "The selected component is external, missing or overridden; manage it on the Node",
                ));
            }
            let installed = installed(spec).ok_or_else(|| {
                Error::new(
                    Code::Stale,
                    "Component changed during the version read; refresh and retry",
                )
            })?;
            if installed != *target {
                let check = checks
                    .get(&(id.clone(), component.clone()))
                    .filter(|v| v.error.is_none() && now() - v.at < 86_400_000);
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
            packages.insert(spec.package.expect("owned").to_string(), target.clone());
        }
    }
    Ok((components, packages))
}

/// Recheck ownership and checked targets while admission holds the managed prefix lock.
fn recheck_components(
    inner: &Inner,
    components: &[UpdateComponent],
    body: &UpgradeHarness,
) -> ApiResult<bool> {
    let cfg = &inner.cfg.acp;
    let id = &body.harness;
    // Recheck ownership and versions after locking the prefix; a CLI may have raced
    // the registry read. Never downgrade or redirect an external command.
    let mut changed = false;
    for (component, target) in &body.versions {
        let spec = components
            .iter()
            .find(|s| s.id == component)
            .expect("validated");
        if !owned(cfg, id, spec) || installed(spec).is_some_and(|v| managed::newer(&v, target)) {
            return Err(Error::new(
                Code::Stale,
                "Components changed before admission; refresh and retry",
            ));
        }
        let current = installed(spec).ok_or_else(|| {
            Error::new(
                Code::Stale,
                "Component changed before admission; refresh and retry",
            )
        })?;
        if current != *target {
            let checks = inner.harness_updates.checks.lock().unwrap();
            let check = checks
                .get(&(id.clone(), component.clone()))
                .filter(|v| v.error.is_none() && now() - v.at < 86_400_000);
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
    }
    Ok(changed)
}

/// Observe the installer identity and exit before verifying every requested package version.
fn install_versions(
    task_inner: &Inner,
    body: &UpgradeHarness,
    packages: &BTreeMap<String, String>,
    npm: &Path,
    prefix: &managed::PrefixLock,
    process_key: &SessionId,
) -> Result<(), String> {
    let specs: Vec<_> = packages
        .iter()
        .map(|(p, v)| (p.as_str(), v.as_str()))
        .collect();
    let mut process = None;
    let result = managed::install_versions_observed(
        npm,
        &task_inner.cfg.acp.steve_home,
        &specs,
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
    result.map_err(|_| "npm upgrade failed or timed out; check this Node's network, npm configuration, disk space and Node.js version, then retry".to_string())?;
    for spec in adapters::update_components(&task_inner.cfg.acp, &body.harness) {
        if let Some(target) = body.versions.get(spec.id) {
            if !owned(&task_inner.cfg.acp, &body.harness, &spec)
                || installed(&spec).as_ref() != Some(target)
            {
                return Err("npm exited successfully but the requested component version was not installed; inspect this Node and retry".into());
            }
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
