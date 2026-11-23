//! Trusted, asynchronous harness setup on the execution host. The journal is the source of
//! installing/failed/unknown, and the same admission lock excludes installs, updates and runs.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use steve_acp::adapters::{Probe, ProbeStatus};
use steve_acp::{AcpConfig, managed};
use steve_node_api::*;

use crate::{Inner, journal::Tx};

#[cfg(test)]
mod tests;

fn process_key() -> SessionId {
    SessionId::new("maintenance_adapters").expect("static id")
}

pub(crate) struct Lease {
    inner: Arc<Inner>,
    prefix: Arc<managed::PrefixLock>,
}
impl Lease {
    pub(crate) fn held(inner: Arc<Inner>, prefix: managed::PrefixLock) -> Self {
        Self {
            inner,
            prefix: Arc::new(prefix),
        }
    }
    pub(crate) fn prefix(&self) -> Arc<managed::PrefixLock> {
        self.prefix.clone()
    }
    pub(crate) fn reserve(inner: &Arc<Inner>, id: &str) -> Option<Self> {
        let mut busy = inner.adapter_maintenance.lock().unwrap();
        if busy.is_some()
            || inner.node_upgrade_pending()
            || inner.closed.load(std::sync::atomic::Ordering::SeqCst)
            || inner.j.read(idle).is_err()
        {
            return None;
        }
        let prefix = managed::prefix_lock(&inner.cfg.acp.steve_home, true).ok()?;
        *busy = Some(id.to_string());
        Some(Self::held(inner.clone(), prefix))
    }
}
impl Drop for Lease {
    fn drop(&mut self) {
        *self.inner.adapter_maintenance.lock().unwrap() = None;
    }
}

pub(crate) fn no_orphan_installer(t: &Tx) -> ApiResult<()> {
    if let Some(p) = t.proc_of(&process_key())? {
        if !crate::procs::state(p.pgid, p.started).gone() {
            return Err(Error::new(
                Code::Busy,
                "a previous adapter installer is still running or unknown; reconcile it before starting work",
            ));
        }
        t.drop_proc(&p.session, p.pgid)?;
    }
    Ok(())
}

pub(crate) fn executable(path: &Path) -> Option<PathBuf> {
    use std::os::unix::fs::PermissionsExt;
    let usable = |p: &Path| {
        p.metadata()
            .is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
    };
    if path.components().count() > 1 || path.is_absolute() {
        return usable(path).then(|| path.to_path_buf());
    }
    std::env::split_paths(&std::env::var_os("PATH")?)
        .map(|p| p.join(path))
        .find(|p| usable(p))
}

#[derive(Clone)]
pub(crate) struct Requirements {
    pub npm: Option<PathBuf>,
    pub node_error: Option<String>,
    pub npm_error: Option<String>,
}

/// Probe the actual configured executable, not npm metadata or the Hub's process environment.
pub(crate) async fn requirements(cfg: &AcpConfig, id: &str) -> Requirements {
    let mut r = Requirements {
        npm: managed::npm(Some(&cfg.node)).and_then(|p| executable(&p)),
        node_error: None,
        npm_error: None,
    };
    let Some(node) = executable(&cfg.node) else {
        r.node_error = Some("Node.js is missing; install Node.js and set STEVE_NODE in the Node service environment".into());
        return r;
    };
    let output = tokio::time::timeout(
        Duration::from_secs(5),
        tokio::process::Command::new(node)
            .arg("--version")
            .stdin(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .kill_on_drop(true)
            .output(),
    )
    .await;
    let version = match output {
        Ok(Ok(o)) if o.status.success() => String::from_utf8_lossy(&o.stdout)
            .trim()
            .trim_start_matches('v')
            .split('.')
            .map(str::parse::<u64>)
            .collect::<Result<Vec<_>, _>>()
            .ok(),
        _ => None,
    };
    let min = steve_harness_spec::components(id)
        .iter()
        .map(|c| c.node_min)
        .max()
        .unwrap_or((20, 0, 0));
    if !version.is_some_and(|v| v.len() == 3 && (v[0], v[1], v[2]) >= min) {
        r.node_error = Some(format!(
            "Node.js >= {}.{}.{} is required; check STEVE_NODE and the Node service PATH",
            min.0, min.1, min.2
        ));
    }
    if r.npm.is_none() {
        r.npm_error = Some(
            "npm is missing; install npm beside Node.js or add it to the Node service PATH".into(),
        );
    }
    r
}

/// Prefix changes wait for all accepted execution and live harness processes. This inspection
/// runs under `adapter_maintenance` and the journal transaction used for command admission.
pub(crate) fn idle(t: &Tx) -> ApiResult<()> {
    for p in t.procs()? {
        if crate::procs::state(p.pgid, p.started).gone() {
            t.drop_proc(&p.session, p.pgid)?;
        } else {
            return Err(Error::new(
                Code::Busy,
                "a harness or installer still runs (or its outcome is unknown); release idle sessions and wait for running work",
            ));
        }
    }
    if t.ops()?.iter().any(|(_, o)| {
        !o.receipt.state.is_finished()
            && !matches!(o.method.as_str(), "install_harness" | "upgrade_harness")
    }) {
        return Err(Error::new(
            Code::Busy,
            "execution or workspace work is active or unknown; wait for it before installing adapters",
        ));
    }
    Ok(())
}

fn components(cfg: &AcpConfig, id: &str) -> Vec<HarnessComponent> {
    let root = cfg.component_root(id);
    steve_harness_spec::components(id)
        .iter()
        .map(|c| {
            let path = cfg.component_command(id, c);
            let metadata = root
                .as_ref()
                .filter(|r| {
                    path.as_ref().is_some_and(|p| {
                        c.id == "adapter"
                            || *p == r.join(c.entry)
                            || p.canonicalize()
                                .is_ok_and(|real| real.starts_with(r.join(c.package)))
                    })
                })
                .and_then(|r| {
                    let v: serde_json::Value = serde_json::from_slice(
                        &std::fs::read(r.join(c.package).join("package.json")).ok()?,
                    )
                    .ok()?;
                    Some(v)
                });
            let exists = path.as_ref().is_some_and(|p| p.is_file());
            let required = root.as_ref().map(|r| {
                c.required
                    .iter()
                    .all(|f| r.join(c.package).join(f).is_file())
            });
            // An opaque executable may have proved ACP/session without exposing npm metadata.
            // That is unknown package integrity, not missing software and not installed=true.
            let status = if !exists || required == Some(false) {
                ComponentStatus::Missing
            } else if metadata.is_some() && required == Some(true) {
                ComponentStatus::Installed
            } else {
                ComponentStatus::Unknown
            };
            let version = metadata.and_then(|v| v["version"].as_str().map(String::from));
            let detected = cfg.detected_component(id, c).map(|p| {
                let version = p.canonicalize().ok().and_then(|real| {
                    let package = real.ancestors().find(|a| a.ends_with(c.package))?;
                    let v: serde_json::Value =
                        serde_json::from_slice(&std::fs::read(package.join("package.json")).ok()?)
                            .ok()?;
                    v["version"].as_str().map(String::from)
                });
                DetectedComponent {
                    path: p.to_string_lossy().into_owned(),
                    version,
                }
            });
            HarnessComponent {
                id: c.id.into(),
                name: c.name.into(),
                package: c.package.into(),
                status,
                detected,
                version,
                path: path.map(|p| p.to_string_lossy().into_owned()),
            }
        })
        .collect()
}

pub(crate) fn setup(
    inner: &Inner,
    id: &str,
    probe: Option<&Probe>,
    req: &Requirements,
) -> ApiResult<Option<HarnessSetup>> {
    if steve_harness_spec::components(id).is_empty() {
        return Ok(None);
    }
    let cfg = &inner.cfg.acp;
    let maintenance = inner.adapter_maintenance.lock().unwrap();
    let (last, busy) = inner.j.read(|t| Ok((t.installation(id)?, idle(t).err())))?;
    let managed = cfg.managed_adapter(id);
    let prefix_error = if managed && maintenance.is_none() && busy.is_none() {
        managed::prefix_lock(&cfg.steve_home, true).err()
    } else {
        None
    };
    let can_install = managed
        && maintenance.is_none()
        && busy.is_none()
        && prefix_error.is_none()
        && req.node_error.is_none()
        && req.npm_error.is_none()
        && inner.cfg.ceiling.exec;
    let mut s = HarnessSetup { state: SetupState::Unknown, components: components(cfg,id), can_install, reason: None, error: None, help: Some("Install the built-in components here, then configure a model provider in the harness's state directory and refresh discovery. Custom commands and separate Nodes are managed by their operator.".into()) };
    if maintenance.as_deref() == Some(id)
        || last.as_ref().is_some_and(|r| {
            matches!(
                r.state,
                Observed::Accepted | Observed::Running | Observed::Waiting { .. }
            )
        })
    {
        s.state = SetupState::Installing;
        s.can_install = false;
        s.reason = Some("Installation or upgrade is running on the Node; discovery will refresh when it finishes".into());
    } else if let Some(Receipt {
        state: Observed::Unknown { .. },
        ..
    }) = &last
    {
        s.reason = Some("The Node restarted during installation; the outcome is unknown. Retry explicitly once the previous installer is confirmed stopped".into());
    } else if let Some(Receipt {
        state: Observed::Finished { error: Some(e), .. },
        ..
    }) = &last
    {
        s.state = SetupState::Unavailable;
        s.error = Some(e.safe_detail.clone());
        s.reason = Some("Installation failed; correct the cause and retry".into());
    // Installer requirements apply to the managed packages. For an external command,
    // successful ACP+session is stronger evidence than an unrelated STEVE_NODE/npm setting.
    // A JS override with a missing actual interpreter still fails its real probe.
    } else if let Some(e) = req.node_error.as_ref().filter(|_| {
        cfg.managed_adapter(id)
            && cfg
                .adapters
                .get(id)
                .is_some_and(|p| steve_acp::is_node_script(p))
    }) {
        s.state = SetupState::Unavailable;
        s.reason = Some(e.clone());
    } else if managed && s.components.iter().any(|c| !c.installed()) {
        s.state = SetupState::NotInstalled;
        s.reason = Some("One or more required components are missing".into());
    } else if let Some((name, min)) = steve_harness_spec::components(id).iter().find_map(|c| {
        let min = c.minimum?;
        s.components
            .iter()
            .find(|v| v.id == c.id)
            .and_then(|v| v.version.as_deref())
            .filter(|v| managed::newer(min, v))
            .map(|_| (c.name, min))
    }) {
        s.state = SetupState::Unavailable;
        s.reason = Some(format!(
            "{name} >= {min} is required for MCP forwarding; upgrade the installed components"
        ));
    } else if let Some(p) = probe {
        if p.status == ProbeStatus::Ready && p.session.is_some() {
            s.state = SetupState::Ready;
        } else {
            s.state = SetupState::Unavailable;
            s.reason = p.session_error.clone().or_else(|| p.error.clone());
            s.help = Some("Configure the harness's model provider/API key or login in its state directory, check the custom command if any, and refresh. Installation alone does not configure credentials.".into());
        }
    }
    if s.state != SetupState::Installing {
        if !managed {
            s.can_install = false;
            s.help = Some("This command is external or overridden; Steve will not replace it. Manage its components and provider on the Node, then refresh discovery.".into());
        } else if let Some(e) = req
            .npm_error
            .as_ref()
            .or_else(|| busy.as_ref().map(|e| &e.safe_detail))
            .or(prefix_error.as_ref())
        {
            s.reason = Some(match s.reason {
                Some(before) => format!("{before}. {e}"),
                None => e.clone(),
            });
        } else if maintenance.is_some() {
            s.reason = Some(
                "Another adapter installation or upgrade is running; wait for it to finish".into(),
            );
        }
    }
    Ok(Some(s))
}

pub(crate) fn complete(cfg: &AcpConfig, id: &str) -> Result<(), String> {
    if components(cfg, id).iter().all(|c| c.installed()) {
        Ok(())
    } else {
        Err("npm exited successfully but required component entry points are missing; retry installation or inspect the Node's npm configuration".into())
    }
}

/// Invoked only after admission reserved the entire prefix. A durable process identity prevents
/// a forced Node restart from admitting another install over an orphaned npm process.
pub(crate) async fn install_packages(
    inner: Arc<Inner>,
    npm: PathBuf,
    packages: Vec<&'static str>,
    lease: Arc<managed::PrefixLock>,
) -> Result<(), String> {
    tokio::task::spawn_blocking(move || {
        let key = process_key();
        let mut process = None;
        let result = managed::install_observed(&npm, &inner.cfg.acp.steve_home, &packages, &lease, |pid| {
            let started = crate::procs::start_time(i64::from(pid));
            inner.j.tx(|t| t.put_proc(&key, i64::from(pid), 0, inner.incarnation, "adapter-install", started)).map_err(|_| "Could not record installer process identity".to_string())?;
            process = Some((i64::from(pid), started));
            Ok(())
        });
        if let Some((pid, started)) = process {
            if !crate::procs::wait_gone(pid, started).gone() { return Err("Installer process exit could not be confirmed; reconcile the Node before retrying".into()); }
            inner.j.tx(|t| t.drop_proc(&key, pid)).map_err(|_| "Could not record installer exit".to_string())?;
        }
        // npm errors can contain authenticated registry URLs and local log paths. Keep them out
        // of API/journal responses; the operator can run the trusted CLI for npm diagnostics.
        result.map_err(|_| "npm installation failed or timed out; check network/registry access, disk space and package Node.js requirements, then retry (steve adapters install <id> for local npm diagnostics)".into())
    }).await.map_err(|_| "Installer task interrupted; inspect the Node before retrying".to_string())?
}
