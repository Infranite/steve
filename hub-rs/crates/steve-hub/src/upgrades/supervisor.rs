//! The service manager owns this process, which owns the replaceable Hub child.
use super::*;
use crate::metrics::backup::{self as telemetry, Event, SupervisorOutcome, SupervisorStep};
use std::sync::Arc;
use std::time::{Duration, Instant};
use steve_modules::vault::keystore::KEYHELPER_TIMEOUT;
use steve_runtime::process::{
    BoundedOptions, CommandError, PROCESS_KILL_REAP_TIMEOUT, exited_unreaped, run_bounded_async,
};
#[cfg(test)]
use steve_runtime::testkit::process::{AsyncCommand as Command, AsyncProcessGuard as Child};
#[cfg(not(test))]
use tokio::process::{Child, Command};

/// Graceful `SIGTERM` window before forcing `SIGKILL` on the Hub's process group (allows active
/// HTTP/SSE connections and SQLite WAL checkpoints to settle).
const STOP_GRACE: Duration = Duration::from_secs(15);
/// Maximum wait after `SIGKILL` to observe exit and reap the Hub child (unified with
/// `steve_runtime::process::PROCESS_KILL_REAP_TIMEOUT`).
const STOP_KILL_TIMEOUT: Duration = PROCESS_KILL_REAP_TIMEOUT;
/// Upper bound for candidate `--build-info` queries (matches `steve_install::signing::BUILD_INFO_TIMEOUT`).
const CANDIDATE_BUILD_INFO_TIMEOUT: Duration = Duration::from_secs(15);
/// Upper bound for candidate `--check-config` validation (unified at 120s with `release.rs`,
/// `setup.rs`, and `reconfigure.rs`).
const CANDIDATE_CHECK_CONFIG_TIMEOUT: Duration = Duration::from_secs(120);
/// Per-stream output ceiling for candidate `--build-info` / `--check-config` (JSON reports are
/// <10 KiB; 1 MiB bounds rogue stdout/stderr without truncating real diagnostics).
const CANDIDATE_CHECK_MAX_OUTPUT_BYTES: usize = 1024 * 1024;
/// Maximum wait for active work to drain during maintenance before aborting the upgrade while the
/// current version stays untouched.
const DRAIN_TIMEOUT: Duration = Duration::from_secs(120);
/// Polling interval while waiting for `/internal/upgrade/drain` to report `ready: true`.
const DRAIN_POLL_INTERVAL: Duration = Duration::from_millis(500);
/// Loopback HTTP timeout for supervisor <-> Hub `/internal/upgrade/drain` control requests
/// (matches `steve_install::release::CONTROL_HTTP_TIMEOUT`).
const MAINTENANCE_REQUEST_TIMEOUT: Duration = Duration::from_secs(10);
/// Health verification window without forward migration progress (`migration::Budget` extends up
/// to `TOTAL_TIMEOUT = 15min` while SQLite `vm_steps` advances).
const HEALTH_IDLE_TIMEOUT: Duration = Duration::from_secs(60);
/// Polling interval for loopback `/healthz` checks during startup and upgrade verification.
const HEALTH_POLL_INTERVAL: Duration = Duration::from_millis(500);
/// Single loopback `/healthz` HTTP probe timeout (matches `steve_install::release::HEALTH_PROBE_TIMEOUT`).
const HEALTH_PROBE_TIMEOUT: Duration = Duration::from_secs(2);
/// Periodic steady-state `/healthz` liveness probe timeout (tolerates transient Hub load spikes).
const LIVENESS_PROBE_TIMEOUT: Duration = Duration::from_secs(5);
/// Steady-state supervisor loop cadence for checking queued upgrades and Hub exit.
const SUPERVISOR_TICK_INTERVAL: Duration = Duration::from_millis(500);
/// Headroom added above `probes * KEYHELPER_TIMEOUT` in `startup_gate` for SQLite reads, KEK
/// unwraps, and blocking-pool scheduling so single- and multi-workspace probes never run with zero
/// margin against `KEYHELPER_TIMEOUT`.
const VAULT_PROBE_OVERHEAD: Duration = Duration::from_secs(15);
/// Ceiling on aggregate `startup_gate` vault probe duration across many workspaces so a wedged
/// keychain cannot block candidate verification indefinitely.
const VAULT_PROBE_MAX_TIMEOUT: Duration = Duration::from_secs(180);
/// Polling interval inside `startup_gate` while waiting for the supervisor to write `committed`.
const STARTUP_GATE_POLL_INTERVAL: Duration = Duration::from_millis(100);
/// Maximum wait for maintenance HTTP servers to close idle sockets before starting business routes.
const STARTUP_GATE_SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(5);
/// Loopback TCP connect timeout for supervisor <-> Hub control HTTP clients.
const LOCAL_CONNECT_TIMEOUT: Duration = Duration::from_secs(2);

/// Compute the aggregate `startup_gate` vault-probe timeout for `probe_count` workspace KEKs.
fn vault_probe_timeout(probe_count: usize) -> Duration {
    let count = u32::try_from(probe_count.max(1)).unwrap_or(u32::MAX);
    KEYHELPER_TIMEOUT
        .saturating_mul(count)
        .saturating_add(VAULT_PROBE_OVERHEAD)
        .min(VAULT_PROBE_MAX_TIMEOUT)
}

/// Strongly typed errors emitted at supervisor subprocess and gate boundaries.
#[derive(Debug, thiserror::Error)]
pub(super) enum SupervisorError {
    #[error("candidate {step} timed out after {:.1}s", .timeout.as_secs_f64())]
    CandidateCheckTimedOut {
        step: &'static str,
        timeout: Duration,
    },
    #[error("candidate {step} {stream} pipe did not close within {}ms after exit", .grace.as_millis())]
    CandidatePipeDrainTimedOut {
        step: &'static str,
        stream: &'static str,
        grace: Duration,
    },
    #[error("Hub child (pid {pid}) did not exit within {:.1}s after SIGKILL", .timeout.as_secs_f64())]
    StopTimedOut { pid: u32, timeout: Duration },
    #[error("upgrade_drain_timed_out: maintenance drain did not settle within {:.0}s; current version retained without stopping or switching", .timeout.as_secs_f64())]
    DrainTimedOut { timeout: Duration },
    #[error("upgrade startup gate timed out after {:.0}s waiting for commit", .timeout.as_secs_f64())]
    StartupGateTimedOut { timeout: Duration },
}

/// Loopback-only HTTP client for supervisor <-> Hub control calls: never routes loopback through
/// ambient proxy variables and disables keep-alive pooling so maintenance servers shut down promptly.
fn local_client() -> Result<reqwest::Client> {
    Ok(reqwest::Client::builder()
        .no_proxy()
        .connect_timeout(LOCAL_CONNECT_TIMEOUT)
        .pool_max_idle_per_host(0)
        .build()?)
}

struct HubChild {
    process: Child,
    group: u32,
}
impl std::ops::Deref for HubChild {
    type Target = Child;
    fn deref(&self) -> &Child {
        &self.process
    }
}
impl std::ops::DerefMut for HubChild {
    fn deref_mut(&mut self) -> &mut Child {
        &mut self.process
    }
}

fn switch(home: &Path, target: &Path) -> Result<()> {
    let tmp = home.join(format!(".current-{}", uuid::Uuid::new_v4()));
    std::os::unix::fs::symlink(target, &tmp)?;
    fs::rename(&tmp, home.join("current"))?;
    File::open(home)?.sync_all()?;
    Ok(())
}
fn control_token(cfg: &Config) -> Result<String> {
    let path = cfg.home.join("updates/control-token.json");
    if path.exists() {
        return read(&path);
    }
    let token = format!("{}{}", uuid::Uuid::new_v4(), uuid::Uuid::new_v4());
    atomic(&path, &token)?;
    Ok(token)
}
async fn maintenance(
    cfg: &Config,
    client: &reqwest::Client,
    method: reqwest::Method,
) -> Result<Value> {
    Ok(client
        .request(
            method,
            format!(
                "http://{}:{}/internal/upgrade/drain",
                cfg.listen.local_host(),
                cfg.listen.port
            ),
        )
        .bearer_auth(control_token(cfg)?)
        .timeout(MAINTENANCE_REQUEST_TIMEOUT)
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?)
}
async fn drain(
    cfg: &Config,
    u: &Updates,
    client: &reqwest::Client,
    op: &mut Operation,
) -> Result<()> {
    let started = Instant::now();
    let result: Result<()> = async {
        maintenance(cfg, client, reqwest::Method::POST).await?;
        let deadline = tokio::time::Instant::now() + DRAIN_TIMEOUT;
        while tokio::time::Instant::now() < deadline {
            let observed = maintenance(cfg, client, reqwest::Method::GET).await?;
            if op.drain.as_ref() != Some(&observed) {
                op.drain = Some(observed.clone());
                u.progress(op, "waiting", None)?;
            }
            if observed["ready"] == true {
                return Ok(());
            }
            tokio::time::sleep(DRAIN_POLL_INTERVAL).await;
        }
        Err(SupervisorError::DrainTimedOut {
            timeout: DRAIN_TIMEOUT,
        }
        .into())
    }
    .await;
    let outcome = match &result {
        Ok(()) => SupervisorOutcome::Ok,
        Err(err)
            if matches!(
                err.downcast_ref::<SupervisorError>(),
                Some(SupervisorError::DrainTimedOut { .. })
            ) =>
        {
            SupervisorOutcome::Timeout
        }
        Err(_) => SupervisorOutcome::Error,
    };
    telemetry::emit(
        &cfg.home,
        Event::Supervisor {
            step: SupervisorStep::Drain,
            outcome,
            elapsed: started.elapsed(),
        },
    );
    result
}
async fn release_maintenance(cfg: &Config, client: &reqwest::Client) -> Result<()> {
    maintenance(cfg, client, reqwest::Method::DELETE).await?;
    Ok(())
}
/// `bundle`'s Hub in the environment a child of this supervisor gets.
fn hub_command(cfg: &Config, bundle: &Path, manifest: &Manifest) -> Command {
    let mut c = Command::new(bundle.join("bin/steve-hub"));
    c.env_remove("STEVE_UPGRADE_CONTROL_TOKEN")
        .env("STEVE_PORT", cfg.listen.port.to_string())
        .env("STEVE_HOST", cfg.listen.env_value())
        .env("STEVE_HOME", &cfg.home)
        .env("STEVE_VERSION", &manifest.version)
        .env("STEVE_WEB_DIR", bundle.join("web/dist"))
        .kill_on_drop(true);
    c
}

fn candidate_std_command(
    bin: &Path,
    cfg: Option<(&Config, &Path, &Manifest)>,
) -> std::process::Command {
    let mut c = std::process::Command::new(bin);
    if let Some((cfg, bundle, manifest)) = cfg {
        c.env_remove("STEVE_UPGRADE_CONTROL_TOKEN")
            .env_remove("STEVE_UPGRADE_ID")
            .env("STEVE_PORT", cfg.listen.port.to_string())
            .env("STEVE_HOST", cfg.listen.env_value())
            .env("STEVE_HOME", &cfg.home)
            .env("STEVE_VERSION", &manifest.version)
            .env("STEVE_WEB_DIR", bundle.join("web/dist"));
    }
    c
}

async fn run_candidate_command(
    home: &Path,
    step: SupervisorStep,
    command: std::process::Command,
    timeout: Duration,
) -> Result<std::process::Output> {
    let started = Instant::now();
    let step_code = step.code();
    let result = run_bounded_async(
        command,
        format!("candidate {step_code}"),
        BoundedOptions {
            max_output_bytes: CANDIDATE_CHECK_MAX_OUTPUT_BYTES,
            ..BoundedOptions::capture(step_code, timeout)
        },
    )
    .await;
    let outcome = match &result {
        Ok(_) => SupervisorOutcome::Ok,
        Err(CommandError::TimedOut { .. }) => SupervisorOutcome::Timeout,
        Err(CommandError::PipeDrainTimedOut { .. }) => SupervisorOutcome::PipeDrainTimeout,
        Err(_) => SupervisorOutcome::Error,
    };
    telemetry::emit(
        home,
        Event::Supervisor {
            step,
            outcome,
            elapsed: started.elapsed(),
        },
    );
    result.map_err(|err| match err {
        CommandError::TimedOut { timeout, .. } => SupervisorError::CandidateCheckTimedOut {
            step: step_code,
            timeout,
        }
        .into(),
        CommandError::PipeDrainTimedOut { stream, grace, .. } => {
            SupervisorError::CandidatePipeDrainTimedOut {
                step: step_code,
                stream,
                grace,
            }
            .into()
        }
        other => anyhow::Error::new(other).context(format!("candidate {step_code}")),
    })
}

/// The candidate's own startup checks (`--check-config`, check.rs) on the environment it would
/// start with, before anything is drained: a setting the new Hub refuses fails the upgrade while
/// the current version keeps serving. A candidate from before `--check-config` is not asked.
pub(super) async fn check_candidate(
    cfg: &Config,
    bundle: &Path,
    manifest: &Manifest,
) -> Result<()> {
    check_candidate_with_timeouts(
        cfg,
        bundle,
        manifest,
        CANDIDATE_BUILD_INFO_TIMEOUT,
        CANDIDATE_CHECK_CONFIG_TIMEOUT,
    )
    .await
}

async fn check_candidate_with_timeouts(
    cfg: &Config,
    bundle: &Path,
    manifest: &Manifest,
    build_info_timeout: Duration,
    check_config_timeout: Duration,
) -> Result<()> {
    let bin = bundle.join("bin/steve-hub");
    let mut info = candidate_std_command(&bin, None);
    info.arg("--build-info");
    // A candidate that hangs on --build-info fails fast; an older/broken binary that exits
    // quickly without JSON is left to the startup gate after the switch, which still rolls back.
    let asks = match run_candidate_command(
        &cfg.home,
        SupervisorStep::BuildInfo,
        info,
        build_info_timeout,
    )
    .await
    {
        Ok(out) => serde_json::from_slice::<Value>(&out.stdout)
            .is_ok_and(|info| info["check_config"] == true),
        Err(error) if error.is::<SupervisorError>() => return Err(error),
        Err(_) => false,
    };
    if !asks {
        return Ok(());
    }
    let mut c = candidate_std_command(&bin, Some((cfg, bundle, manifest)));
    c.arg("--check-config");
    let out = run_candidate_command(
        &cfg.home,
        SupervisorStep::CheckConfig,
        c,
        check_config_timeout,
    )
    .await?;
    if out.status.success() {
        return Ok(());
    }
    let errors = serde_json::from_slice::<Value>(&out.stdout)
        .ok()
        .and_then(|v| v["errors"].as_array().cloned())
        .map(|e| {
            e.iter()
                .filter_map(Value::as_str)
                .collect::<Vec<_>>()
                .join("; ")
        })
        .unwrap_or_else(|| String::from_utf8_lossy(&out.stderr).trim().to_string());
    bail!(
        "{} 按当前环境（steve.env）启动不了，没有排空也没有切换，当前版本照常运行。改好 steve.env 里的以下设置，重启 Steve 服务（托管模式只在启动时读取 steve.env）后再升级：{errors}",
        manifest.version
    )
}
fn spawn(cfg: &Config, bundle: &Path, op: Option<&Operation>) -> Result<HubChild> {
    let manifest: Manifest = read(&bundle.join("release.json"))?;
    let _ = control_token(cfg)?;
    let mut c = hub_command(cfg, bundle, &manifest);
    c.env("STEVE_SUPERVISOR_PID", std::process::id().to_string())
        .env("STEVE_MANAGED", "1")
        .process_group(0);

    if let Some(op) = op {
        c.env("STEVE_UPGRADE_ID", &op.id);
    } else {
        c.env_remove("STEVE_UPGRADE_ID");
    }
    let process = c.spawn()?;
    let group = process.id().context("spawned child has no PID")?;
    Ok(HubChild { process, group })
}
/// Must run to completion: do not put this future in a cancellable select/timeout context.
/// The blocking waitid can return only after the child exits and cannot be aborted with the future.
async fn stop(home: Option<&Path>, child: &mut HubChild, grace: Duration) -> Result<()> {
    let started = Instant::now();
    let result: Result<()> = async {
        let Some(pid) = child.id() else {
            // A previous wait already released this identity; never signal its cached group.
            return Ok(());
        };
        let group = child.group as i32;
        if !exited_unreaped(pid, true)? {
            unsafe {
                libc::kill(-group, libc::SIGTERM);
            }
            // Wait in the blocking pool without polling timers or reaping the leader.
            let mut observed = tokio::task::spawn_blocking(move || exited_unreaped(pid, false));
            match tokio::time::timeout(grace, &mut observed).await {
                Ok(result) => {
                    result??;
                }
                Err(_) => {
                    // An observation error must not turn a cached group number into a signal.
                    exited_unreaped(pid, true)?;
                    unsafe {
                        libc::kill(-group, libc::SIGKILL);
                        // The unreaped leader still pins its PID even if it left the recorded group.
                        libc::kill(pid as i32, libc::SIGKILL);
                    }
                    tokio::time::timeout(STOP_KILL_TIMEOUT, &mut observed)
                        .await
                        .map_err(|_| SupervisorError::StopTimedOut {
                            pid,
                            timeout: STOP_KILL_TIMEOUT,
                        })???;
                }
            }
        }
        // The live or zombie leader still pins the group. Clear descendants before reaping it.
        unsafe {
            libc::kill(-group, libc::SIGKILL);
        }
        tokio::time::timeout(STOP_KILL_TIMEOUT, child.wait())
            .await
            .map_err(|_| SupervisorError::StopTimedOut {
                pid,
                timeout: STOP_KILL_TIMEOUT,
            })??;
        Ok(())
    }
    .await;
    if let Some(home) = home {
        let outcome = match &result {
            Ok(()) => SupervisorOutcome::Ok,
            Err(err)
                if matches!(
                    err.downcast_ref::<SupervisorError>(),
                    Some(SupervisorError::StopTimedOut { .. })
                ) =>
            {
                SupervisorOutcome::Timeout
            }
            Err(_) => SupervisorOutcome::Error,
        };
        telemetry::emit(
            home,
            Event::Supervisor {
                step: SupervisorStep::Stop,
                outcome,
                elapsed: started.elapsed(),
            },
        );
    }
    result
}

fn saved_snapshot(op: &Operation) -> Result<&crate::backup::SnapshotId> {
    op.backup_snapshot
        .as_ref()
        .context("verified upgrade snapshot missing")
}
/// Timeout for verifying the candidate release's sibling `bin/steve-node --build-info`.
const CANDIDATE_NODE_INFO_TIMEOUT: Duration = Duration::from_secs(15);

/// Verifies that the candidate release bundle ships an executable `bin/steve-node` whose
/// `--build-info` matches the staged release manifest (fail closed before commit so rollback runs).
async fn verify_candidate_node(bundle: &Path, manifest: &Manifest) -> Result<()> {
    let bin = bundle.join("bin/steve-node");
    if !bin.is_file() {
        bail!("candidate release is missing bin/steve-node");
    }
    let mut cmd = candidate_std_command(&bin, None);
    cmd.arg("--build-info");
    let out = run_bounded_async(
        cmd,
        "candidate node build_info",
        BoundedOptions {
            max_output_bytes: CANDIDATE_CHECK_MAX_OUTPUT_BYTES,
            ..BoundedOptions::capture("node_build_info", CANDIDATE_NODE_INFO_TIMEOUT)
        },
    )
    .await
    .map_err(|err| match err {
        CommandError::TimedOut { .. } => {
            anyhow::anyhow!("candidate bin/steve-node --build-info timed out")
        }
        other => {
            anyhow::Error::new(other).context("failed to run candidate bin/steve-node --build-info")
        }
    })?;
    if !out.status.success() {
        bail!(
            "candidate bin/steve-node --build-info failed: {}",
            out.status
        );
    }
    let info: Value = serde_json::from_slice(&out.stdout)
        .context("candidate bin/steve-node --build-info returned invalid JSON")?;
    match (
        option_env!("STEVE_BUILD_VERSION"),
        option_env!("STEVE_BUILD_COMMIT"),
    ) {
        (Some(_), Some(_)) => {
            if info["version"].as_str() != Some(manifest.version.as_str())
                || info["commit"].as_str() != Some(manifest.commit.as_str())
            {
                bail!("candidate bin/steve-node identity does not match release manifest");
            }
        }
        (None, None) if cfg!(debug_assertions) => {
            if !info.is_object() {
                bail!("candidate bin/steve-node --build-info must return a JSON object");
            }
        }
        _ => bail!("candidate bin/steve-node identity cannot be verified"),
    }
    Ok(())
}

async fn healthy(cfg: &Config, client: &reqwest::Client, version: &str, id: Option<&str>) -> bool {
    let host = cfg.listen.local_host();
    let Ok(r) = client
        .get(format!("http://{host}:{}/healthz", cfg.listen.port))
        .timeout(HEALTH_PROBE_TIMEOUT)
        .send()
        .await
        .and_then(reqwest::Response::error_for_status)
    else {
        return false;
    };
    let Ok(v) = r.json::<Value>().await else {
        return false;
    };
    let expected = read::<Manifest>(&cfg.home.join("current/release.json")).ok();
    let identity_ok = match v["build_commit"].as_str() {
        Some(commit) => expected.as_ref().is_some_and(|m| m.commit == commit),
        None => cfg!(debug_assertions),
    };
    let node_ok = match v.get("node").filter(|n| n["enabled"] == true) {
        Some(node) => node.get("linked").and_then(Value::as_bool) != Some(false),
        None => true,
    };
    identity_ok
        && node_ok
        && v["version"] == version
        && v["web_version"] == version
        && match id {
            Some(id) => v["upgrade_id"] == id && v["maintenance"] == true,
            None => v["maintenance"] != true,
        }
}
/// How often the running Hub is asked whether it still answers, and how many unanswered probes in a
/// row make it hung: it is stopped, and the service manager starts the supervisor (and it) again, as
/// when the Hub exits. Only once it has answered since it started, and never during an upgrade.
const LIVENESS_EVERY: Duration = Duration::from_secs(30);
const LIVENESS_MISSES: u32 = 4;

/// The Hub answers `/healthz` at all (its version and maintenance do not matter here).
async fn answers(cfg: &Config, client: &reqwest::Client) -> bool {
    let host = cfg.listen.local_host();
    client
        .get(format!("http://{host}:{}/healthz", cfg.listen.port))
        .timeout(LIVENESS_PROBE_TIMEOUT)
        .send()
        .await
        .is_ok_and(|r| r.status().is_success())
}

/// Counts unanswered liveness probes; [`Liveness::hung`] once there were too many in a row.
#[derive(Default)]
struct Liveness {
    answered: bool,
    misses: u32,
}

impl Liveness {
    /// Takes one probe's outcome; true when the Hub is to be taken for hung.
    fn probe(&mut self, ok: bool) -> bool {
        if ok {
            self.answered = true;
            self.misses = 0;
        } else if self.answered {
            self.misses += 1;
        }
        self.misses >= LIVENESS_MISSES
    }
}

async fn wait_health(
    cfg: &Config,
    client: &reqwest::Client,
    child: &mut HubChild,
    version: &str,
    id: Option<&str>,
) -> Result<()> {
    let mut budget =
        steve_install::migration::Budget::new(id.unwrap_or(""), child.group, HEALTH_IDLE_TIMEOUT);
    loop {
        if let Some(status) = child.try_wait()? {
            bail!("candidate exited: {status}")
        };
        budget.check(id.and_then(|id| steve_install::migration::read(&cfg.home, id)))?;
        if healthy(cfg, client, version, id).await {
            return Ok(());
        };
        tokio::time::sleep(HEALTH_POLL_INTERVAL).await;
    }
}
async fn apply(
    cfg: &Config,
    u: &Updates,
    client: &reqwest::Client,
    child: &mut HubChild,
    op: &mut Operation,
) -> Result<()> {
    let previous = fs::canonicalize(cfg.home.join("current"))?;
    let target = PathBuf::from(op.target.as_ref().context("staged target missing")?);
    let manifest: Manifest = read(&target.join("release.json"))?;
    super::client::validate_manifest(&manifest, &op.version)?;
    check_candidate(cfg, &target, &manifest).await?;
    op.previous = Some(previous.to_string_lossy().into());
    if let Ok(old) = read::<Manifest>(&previous.join("release.json")) {
        op.previous_version = Some(old.version);
    }
    u.phase(op, "draining", None)?;
    if let Err(e) = drain(cfg, u, client, op).await {
        let cancel = release_maintenance(cfg, client).await;
        return Err(anyhow::anyhow!(
            "{e:#}; maintenance cancellation: {cancel:?}"
        ));
    }
    u.phase(op, "stopping", None)?;
    stop(Some(&cfg.home), child, STOP_GRACE).await?;
    u.phase(op, "backing_up", None)?;
    let copied = (|| {
        let _instance = super::instance_lock(cfg)?;
        crate::backup::create(
            &cfg.home,
            &cfg.db_path(),
            crate::backup::Purpose::PreUpgrade,
        )
    })();
    if let Err(e) = &copied {
        *child = spawn(cfg, &previous, None)?;
        let old: Manifest = read(&previous.join("release.json"))?;
        wait_health(cfg, client, child, &old.version, None).await?;
        release_maintenance(cfg, client).await?;
        return Err(anyhow::anyhow!("{e:#}"));
    }
    op.backup_snapshot = Some(copied?.snapshot_id);
    u.phase(op, "restarting", None)?;
    switch(&cfg.home, &target)?;
    *child = spawn(cfg, &target, Some(op))?;
    u.phase(op, "verifying", None)?;
    if let Err(e) = wait_health(cfg, client, child, &op.version, Some(&op.id)).await {
        rollback(cfg, u, client, child, op, format!("{e:#}")).await?;
        return Ok(());
    }
    // This is the irreversible business boundary: after commit never restore an old database.
    u.phase(op, "committed", None)?;
    match wait_health(cfg, client, child, &op.version, None).await {
        Ok(()) => {
            release_maintenance(cfg, client).await?;
            if let Err(error) =
                steve_install::service::sync_hub_service_definition(&cfg.home, &target)
            {
                tracing::warn!("sync hub service definition after managed upgrade: {error:#}");
            }
            u.phase(op, "succeeded", None)?;
        }
        Err(e) => u.fail_committed(
            op,
            format!("升级已提交，业务启动失败；保留新数据库，禁止自动回滚：{e:#}"),
        )?,
    };
    Ok(())
}
async fn rollback(
    cfg: &Config,
    u: &Updates,
    client: &reqwest::Client,
    child: &mut HubChild,
    op: &mut Operation,
    error: String,
) -> Result<()> {
    u.phase(op, "rolling_back", Some(error.clone()))?;
    stop(Some(&cfg.home), child, STOP_GRACE).await?;
    {
        let _instance = super::instance_lock(cfg)?;
        crate::backup::restore_database_within(
            &cfg.home,
            &cfg.db_path(),
            saved_snapshot(op)?,
            crate::backup::ROLLBACK_RESTORE_BUDGET,
        )?;
    }
    let previous = PathBuf::from(
        op.previous
            .as_ref()
            .context("rollback missing previous bundle")?,
    );
    switch(&cfg.home, &previous)?;
    u.phase(op, "rolled_back_pending_health", Some(error.clone()))?;
    *child = spawn(cfg, &previous, None)?;
    let manifest: Manifest = read(&previous.join("release.json"))?;
    wait_health(cfg, client, child, &manifest.version, None).await?;
    release_maintenance(cfg, client).await?;
    u.phase(op, "rolled_back", Some(error))?;
    Ok(())
}
pub async fn run(cfg: Config) -> Result<()> {
    let mut u = Updates::new(&cfg)?;
    u.managed = true;
    if let Some(reason) = u.execution_reason() {
        bail!("{reason}")
    }
    let client = local_client()?;
    fs::create_dir_all(u.dir())?;
    let lock = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(u.dir().join("supervisor.lock"))?;
    lock.try_lock()
        .context("another supervisor already owns this installation")?;
    let instance = super::instance_lock(&cfg)?;
    // Service-manager restart must stop the complete service cgroup/job before invoking a new supervisor.
    // On a standalone launch, refuse an occupied port instead of killing an unrelated process.
    let probe = cfg
        .listen
        .bind()
        .await
        .context("Hub port occupied; stop the previous managed instance first")?;
    drop(probe);
    if let Some(mut op) = u.active()? {
        match op.phase.as_str() {
            "restarting" | "verifying" | "rolling_back" => {
                let previous =
                    PathBuf::from(op.previous.as_ref().context("missing rollback bundle")?);
                crate::backup::restore_database_within(
                    &cfg.home,
                    &cfg.db_path(),
                    saved_snapshot(&op)?,
                    crate::backup::ROLLBACK_RESTORE_BUDGET,
                )?;
                switch(&cfg.home, &previous)?;
                u.phase(
                    &mut op,
                    "rolled_back_pending_health",
                    Some("supervisor restarted before health commit".into()),
                )?;
            }
            "draining" | "stopping" | "backing_up" => u.phase(
                &mut op,
                "failed",
                Some(
                    "supervisor restarted before version switch; retained original database".into(),
                ),
            )?,
            "staging" => u.phase(
                &mut op,
                "failed",
                Some("download interrupted; retry with a new idempotency key".into()),
            )?,
            _ => {}
        }
    }
    let current = fs::canonicalize(cfg.home.join("current"))?;
    drop(instance);
    let mut child = spawn(&cfg, &current, None)?;
    let initial_manifest: Manifest = read(&current.join("release.json"))?;
    wait_health(&cfg, &client, &mut child, &initial_manifest.version, None).await?;
    release_maintenance(&cfg, &client).await?;
    if let Some(mut op) = u.active()?
        && matches!(
            op.phase.as_str(),
            "committed" | "rolled_back_pending_health" | "failed"
        )
    {
        let current_manifest: Manifest = read(&current.join("release.json"))?;
        match wait_health(&cfg, &client, &mut child, &current_manifest.version, None).await {
            Ok(()) => {
                release_maintenance(&cfg, &client).await?;
                if op.phase == "committed" {
                    u.phase(&mut op, "succeeded", None)?
                } else if op.phase == "rolled_back_pending_health" {
                    let error = op.error.clone();
                    u.phase(&mut op, "rolled_back", error)?
                }
            }
            Err(e) => {
                u.phase(
                    &mut op,
                    "failed",
                    Some(format!("version startup failed: {e:#}")),
                )?;
                return Err(e);
            }
        }
    }
    let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
    let mut live = Liveness::default();
    let mut probed = tokio::time::Instant::now();
    loop {
        tokio::select! {_ = term.recv()=>{stop(Some(&cfg.home), &mut child, STOP_GRACE).await?;return Ok(())},_ = tokio::signal::ctrl_c()=>{stop(Some(&cfg.home), &mut child, STOP_GRACE).await?;return Ok(())},_ = tokio::time::sleep(SUPERVISOR_TICK_INTERVAL)=>{}}
        if let Some(mut op) = u.active()?
            && op.phase == "ready"
            && let Err(e) = apply(&cfg, &u, &client, &mut child, &mut op).await
        {
            // If we crossed the switch but not commit, make a best-effort rollback under the closed gate.
            if matches!(op.phase.as_str(), "restarting" | "verifying") {
                if let Err(rb) =
                    rollback(&cfg, &u, &client, &mut child, &mut op, format!("{e:#}")).await
                {
                    u.phase(
                        &mut op,
                        "failed",
                        Some(format!("upgrade: {e:#}; rollback: {rb:#}")),
                    )?;
                    return Err(rb);
                }
            } else {
                u.phase(&mut op, "failed", Some(format!("{e:#}")))?;
            }
        }
        if let Some(status) = child.try_wait()? {
            bail!("Hub child exited: {status}; supervisor must be restarted by service manager")
        }
        /* a Hub that runs but no longer answers is as gone as one that exited (2026-11-09) */
        if probed.elapsed() >= LIVENESS_EVERY {
            probed = tokio::time::Instant::now();
            if u.active()?.is_none() && live.probe(answers(&cfg, &client).await) {
                tracing::error!(
                    "Hub did not answer /healthz {LIVENESS_MISSES} times in a row; stopping it"
                );
                stop(Some(&cfg.home), &mut child, STOP_GRACE).await?;
                bail!("Hub stopped answering; supervisor must be restarted by service manager")
            }
        }
    }
}
/// Serve maintenance on the Hub's existing listeners before starting any business code.
pub async fn startup_gate(cfg: &Config, listeners: &[tokio::net::TcpListener]) -> Result<()> {
    use std::os::fd::AsFd;

    let supervisor_pid = unsafe { libc::getppid() };
    let Ok(id) = std::env::var("STEVE_UPGRADE_ID") else {
        return Ok(());
    };
    if supervisor_pid <= 1 {
        bail!("upgrade supervisor is no longer alive")
    }
    let u = Updates::new(cfg)?;
    let op = u.operation(&id)?;
    if op.phase == "committed" || op.phase == "succeeded" {
        return Ok(());
    }
    if op.version != cfg.version || !matches!(op.phase.as_str(), "restarting" | "verifying") {
        bail!("invalid upgrade gate state")
    }
    let target_dir = PathBuf::from(op.target.context("gate target missing")?);
    let manifest: Manifest = read(&target_dir.join("release.json"))?;
    super::client::validate_manifest(&manifest, &cfg.version)?;
    verify_candidate_node(&target_dir, &manifest).await?;
    match (
        option_env!("STEVE_BUILD_VERSION"),
        option_env!("STEVE_BUILD_COMMIT"),
    ) {
        (Some(version), Some(commit))
            if version == manifest.version && commit == manifest.commit => {}
        (None, None) if cfg!(debug_assertions) => {}
        _ => bail!("candidate compiled identity does not match release manifest"),
    }

    let keys = steve_modules::vault::keystore::from_env(&cfg.home, |k| std::env::var(k).ok())
        .map_err(|e| anyhow::anyhow!("vault unavailable: {e}"))?;
    let progress = std::sync::Mutex::new(steve_install::migration::Reporter::new(&cfg.home, &id)?);
    let store = steve_store::Store::open_with_migration_progress(
        cfg.db_path(),
        steve_store::NoPublisher,
        Arc::new(move |snapshot| {
            if let Err(error) = progress.lock().unwrap_or_else(|e| e.into_inner()).report(
                snapshot.migration,
                snapshot.target,
                snapshot.vm_steps,
                snapshot.complete,
            ) {
                tracing::warn!(%error, "could not report upgrade migration progress");
            }
        }),
    )?;
    let schema = u32::try_from(steve_store::db::inspect_schema(&cfg.db_path())?.schema)?;
    if schema != manifest.schema.target
        || schema < manifest.schema.min_read
        || schema > manifest.schema.max_read
    {
        bail!("candidate database schema incompatible with manifest")
    }
    let probes = store.read(|r| steve_store::vault_keys::upgrade_probes(r))?;
    drop(store);
    let probe_timeout = vault_probe_timeout(probes.len());
    let vault_started = Instant::now();
    let vault_result = tokio::time::timeout(
        probe_timeout,
        tokio::task::spawn_blocking(move || -> Result<()> {
            for probe in probes {
                let key = keys
                    .get(&probe.keychain_account)
                    .map_err(|e| anyhow::anyhow!("vault probe: {e}"))?
                    .context("vault probe: existing KEK missing")?;
                if steve_modules::vault::crypto::fingerprint(&key) != probe.fingerprint {
                    bail!("vault probe: KEK fingerprint mismatch")
                }
                if let Some(sample) = probe.sample {
                    steve_modules::vault::crypto::unwrap_dek(
                        &key,
                        &probe.workspace,
                        &sample.item_id,
                        sample.kek_version,
                        &sample.dek_wrapped,
                    )
                    .map_err(|_| anyhow::anyhow!("vault probe: existing data key cannot unwrap"))?;
                }
            }
            Ok(())
        }),
    )
    .await;
    let vault_outcome = match &vault_result {
        Ok(Ok(Ok(()))) => SupervisorOutcome::Ok,
        Err(_) => SupervisorOutcome::Timeout,
        Ok(_) => SupervisorOutcome::Error,
    };
    telemetry::emit(
        &cfg.home,
        Event::Supervisor {
            step: SupervisorStep::VaultProbe,
            outcome: vault_outcome,
            elapsed: vault_started.elapsed(),
        },
    );
    vault_result.context("vault probe timeout")???;
    if crate::web::web_version(cfg) != cfg.version || !cfg.web_dir.join("index.html").is_file() {
        bail!("candidate Web version missing or incompatible")
    }
    let version = cfg.version.clone();
    let gate_id = id.clone();
    let app = axum::Router::new()
        .route(
            "/healthz",
            axum::routing::get(move || {
                let (version, id) = (version.clone(), gate_id.clone());
                async move {
                    (
                        axum::http::StatusCode::OK,
                        [(axum::http::header::CONNECTION, "close")],
                        axum::Json(json!({
                            "ok": true,
                            "version": version,
                            "web_version": version,
                            "upgrade_id": id,
                            "maintenance": true,
                            "build_commit": option_env!("STEVE_BUILD_COMMIT"),
                        })),
                    )
                }
            }),
        )
        .fallback(|| async {
            (
                axum::http::StatusCode::SERVICE_UNAVAILABLE,
                [(axum::http::header::CONNECTION, "close")],
                "升级验证中，请稍后重试",
            )
        });
    let (done, rx) = tokio::sync::watch::channel(false);
    let mut servers = tokio::task::JoinSet::new();
    for listener in listeners {
        // Duplicate the descriptor with close-on-exec; the already nonblocking socket
        // stays bound while the maintenance servers shut down and the business router starts.
        let listener = std::net::TcpListener::from(listener.as_fd().try_clone_to_owned()?);
        let listener = tokio::net::TcpListener::from_std(listener)?;
        let (app, mut rx) = (app.clone(), rx.clone());
        servers.spawn(async move {
            axum::serve(listener, app)
                .with_graceful_shutdown(async move {
                    let _ = rx.wait_for(|d| *d).await;
                })
                .await
        });
    }
    drop(rx);
    let gate_started = Instant::now();
    let gate_deadline = tokio::time::Instant::now() + steve_install::migration::TOTAL_TIMEOUT;
    loop {
        if tokio::time::Instant::now() >= gate_deadline {
            let _ = done.send(true);
            servers.abort_all();
            telemetry::emit(
                &cfg.home,
                Event::Supervisor {
                    step: SupervisorStep::StartupGate,
                    outcome: SupervisorOutcome::Timeout,
                    elapsed: gate_started.elapsed(),
                },
            );
            return Err(SupervisorError::StartupGateTimedOut {
                timeout: steve_install::migration::TOTAL_TIMEOUT,
            }
            .into());
        }
        tokio::time::sleep(STARTUP_GATE_POLL_INTERVAL).await;
        if unsafe { libc::getppid() } != supervisor_pid {
            let _ = done.send(true);
            servers.abort_all();
            telemetry::emit(
                &cfg.home,
                Event::Supervisor {
                    step: SupervisorStep::StartupGate,
                    outcome: SupervisorOutcome::Error,
                    elapsed: gate_started.elapsed(),
                },
            );
            bail!("upgrade supervisor disappeared before commit")
        }
        let op = u.operation(&id)?;
        if op.phase == "committed" {
            break;
        }
        if !matches!(op.phase.as_str(), "restarting" | "verifying") {
            let _ = done.send(true);
            servers.abort_all();
            telemetry::emit(
                &cfg.home,
                Event::Supervisor {
                    step: SupervisorStep::StartupGate,
                    outcome: SupervisorOutcome::Error,
                    elapsed: gate_started.elapsed(),
                },
            );
            bail!("upgrade gate aborted: {}", op.phase)
        }
    }
    let _ = done.send(true);
    let shutdown = tokio::time::timeout(STARTUP_GATE_SHUTDOWN_TIMEOUT, async {
        while let Some(r) = servers.join_next().await {
            r??;
        }
        Ok::<(), anyhow::Error>(())
    })
    .await;
    match shutdown {
        Ok(res) => res?,
        Err(_) => {
            tracing::warn!(
                timeout_secs = STARTUP_GATE_SHUTDOWN_TIMEOUT.as_secs(),
                "maintenance server graceful shutdown timed out; aborting remaining connections"
            );
            servers.abort_all();
        }
    }
    telemetry::emit(
        &cfg.home,
        Event::Supervisor {
            step: SupervisorStep::StartupGate,
            outcome: SupervisorOutcome::Ok,
            elapsed: gate_started.elapsed(),
        },
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;
    fn fixture(version: &str) -> Manifest {
        Manifest {
            format_version: 1,
            version: version.into(),
            commit: "a".repeat(40),
            hub_version: version.into(),
            web_version: version.into(),
            api_protocol: 1,
            schema: Schema {
                min_read: 1,
                max_read: 1,
                target: 1,
            },
            assets: vec![],
        }
    }
    fn bundle(home: &Path, version: &str, broken: bool) -> PathBuf {
        let dir = home.join("releases").join(version);
        fs::create_dir_all(dir.join("bin")).unwrap();
        fs::create_dir_all(dir.join("web/dist")).unwrap();
        atomic(&dir.join("release.json"), &fixture(version)).unwrap();
        let script = if broken {
            "#!/bin/sh\nexit 1\n".to_string()
        } else {
            r#"#!/usr/bin/env python3
import os,json,http.server,socketserver,sys,errno
# Build metadata is a CLI query, not a request to start a server. In particular,
# never bind an inherited STEVE_PORT when these tests run inside a live Node.
if '--build-info' in sys.argv:
 print(json.dumps(dict(check_config=False))); sys.exit(0)
home=os.environ['STEVE_HOME']; version=os.environ['STEVE_VERSION']; op=os.environ.get('STEVE_UPGRADE_ID')
class Server(http.server.ThreadingHTTPServer):
 allow_reuse_address=True
 daemon_threads=True
 def server_bind(self):
  # Skip socket.getfqdn('127.0.0.1') which can stall on macOS reverse DNS/mDNS.
  socketserver.TCPServer.server_bind(self)
  self.server_name='127.0.0.1'
  self.server_port=self.server_address[1]
class Handler(http.server.BaseHTTPRequestHandler):
 timeout=5
 def log_message(self,*args): pass
 def do_POST(self): self.do_GET()
 def do_DELETE(self): self.do_GET()
 def do_GET(self):
  maintenance=False
  if op:
   with open(home+'/updates/operations/'+op+'.json') as f: maintenance=json.load(f)['phase'] not in ['committed','succeeded']
  body=json.dumps(dict(ok=True,ready=True,version=version,web_version=version,upgrade_id=op,maintenance=maintenance)).encode()
  self.send_response(200);self.send_header('Content-Length',str(len(body)));self.send_header('Connection','close');self.end_headers();self.wfile.write(body)
# the port was chosen before this binds it: say so when something else took it meanwhile
try: server=Server(('127.0.0.1',int(os.environ['STEVE_PORT'])),Handler)
except OSError as e:
 if e.errno==errno.EADDRINUSE: open(home+'/port-taken','w').close()
 raise
server.serve_forever()
"#.into()
        };
        fs::write(dir.join("bin/steve-hub"), script).unwrap();
        fs::set_permissions(dir.join("bin/steve-hub"), fs::Permissions::from_mode(0o755)).unwrap();
        let node_script = format!(
            "#!/bin/sh\nprintf '%s\\n' '{{\"version\":\"{version}\",\"commit\":\"{}\"}}'\n",
            "a".repeat(40)
        );
        fs::write(dir.join("bin/steve-node"), node_script).unwrap();
        fs::set_permissions(
            dir.join("bin/steve-node"),
            fs::Permissions::from_mode(0o755),
        )
        .unwrap();
        dir
    }
    #[tokio::test]
    async fn exit_observation_keeps_the_leader_waitable() {
        let mut child = Command::new("/bin/sh")
            .args(["-c", "exit 23"])
            .spawn()
            .unwrap();
        let pid = child.id().unwrap();
        steve_runtime::testkit::eventually_sync("leader exited without reaping", || {
            exited_unreaped(pid, true).unwrap()
        })
        .await;
        assert!(
            exited_unreaped(pid, true).unwrap(),
            "a second observation still sees it"
        );
        assert_eq!(child.wait().await.unwrap().code(), Some(23));
        assert_eq!(
            exited_unreaped(pid, true).unwrap_err().raw_os_error(),
            Some(libc::ECHILD)
        );
    }

    #[tokio::test]
    async fn stop_clears_descendants_before_reaping_and_ignores_a_reused_group() {
        let home = tempfile::tempdir().unwrap();
        let ready = home.path().join("descendant");
        let process = Command::new("/bin/sh")
            .args([
                "-c",
                "/bin/sleep 600 & printf '%s' \"$!\" > \"$1\"; exit 0",
                "fixture",
            ])
            .arg(&ready)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .unwrap();
        let group = process.id().unwrap();
        let mut child = HubChild { process, group };
        steve_runtime::testkit::eventually_sync("leader exited with a live descendant", || {
            ready.exists() && exited_unreaped(group, true).unwrap()
        })
        .await;
        let descendant: i32 = fs::read_to_string(ready).unwrap().parse().unwrap();
        assert_eq!(unsafe { libc::kill(descendant, 0) }, 0);
        stop(None, &mut child, Duration::from_millis(50))
            .await
            .unwrap();
        assert!(child.id().is_none());
        steve_runtime::testkit::eventually_sync(
            "the old group and its descendant are gone",
            || unsafe { libc::kill(descendant, 0) == -1 && libc::kill(-(group as i32), 0) == -1 },
        )
        .await;
        let mut canary = Command::new("/bin/sleep").arg("600").spawn().unwrap();
        child.group = canary.id().unwrap();
        stop(None, &mut child, Duration::from_millis(50))
            .await
            .unwrap();
        assert!(canary.try_wait().unwrap().is_none());
        canary.kill().await.unwrap();
    }

    #[tokio::test]
    async fn stop_forces_a_term_ignoring_child_outside_the_recorded_group() {
        let home = tempfile::tempdir().unwrap();
        let ready = home.path().join("ready");
        let process = Command::new("/bin/sh")
            .args([
                "-c",
                "trap '' TERM; printf ready > \"$1\"; exec /bin/sleep 600",
                "fixture",
            ])
            .arg(&ready)
            .spawn()
            .unwrap();
        let leader = process.id().unwrap();
        // Model a leader outside the recorded group using another independently owned fixture.
        // Keep that group's leader unreaped too, so this never signals a reusable/fabricated ID.
        let mut former_group = Command::new("/bin/sleep").arg("600").spawn().unwrap();
        let group = former_group.id().unwrap();
        let mut child = HubChild { process, group };
        steve_runtime::testkit::eventually_sync("child ignores TERM", || ready.exists()).await;
        // Test-only deadline: on failure the guarded children drop and release blocking waitid.
        tokio::time::timeout(
            steve_runtime::testkit::EVENTUALLY,
            stop(None, &mut child, Duration::from_millis(50)),
        )
        .await
        .expect("forced shutdown must finish before the shared test deadline")
        .unwrap();
        assert!(child.id().is_none());
        former_group.wait().await.unwrap();
        steve_runtime::testkit::eventually_sync("forced shutdown removed both groups", || unsafe {
            libc::kill(-(group as i32), 0) == -1 && libc::kill(-(leader as i32), 0) == -1
        })
        .await;
    }

    #[tokio::test]
    async fn health_rejects_http_failure_even_when_maintenance_identity_matches() {
        let home = tempfile::tempdir().unwrap();
        let candidate = bundle(home.path(), "0.2.0", false);
        switch(home.path(), &candidate).unwrap();
        for status in [
            axum::http::StatusCode::SERVICE_UNAVAILABLE,
            axum::http::StatusCode::OK,
        ] {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let port = listener.local_addr().unwrap().port();
            let cfg = Config::from_vars(|key| match key {
                "STEVE_HOME" => Some(home.path().to_string_lossy().into()),
                "STEVE_PORT" => Some(port.to_string()),
                "STEVE_ADAPTER_CLAUDE" | "STEVE_ADAPTER_CODEX" => Some("unused".into()),
                _ => None,
            })
            .unwrap();
            let app = axum::Router::new().route("/healthz", axum::routing::get(move || async move {
                (status, axum::Json(json!({"ok":true,"maintenance":true,"version":"0.2.0","web_version":"0.2.0","upgrade_id":"health-fixture","build_commit":"a".repeat(40)})))
            }));
            let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
            let client = local_client().unwrap();
            let healthy = healthy(&cfg, &client, "0.2.0", Some("health-fixture")).await;
            server.abort();
            assert_eq!(
                healthy,
                status.is_success(),
                "a matching JSON identity cannot make an HTTP failure healthy"
            );
        }
    }

    /// Hard upper bound for a full real-process upgrade/rollback scenario test so a regression
    /// fails with diagnostics instead of burning a CI shard timeout.
    const REAL_PROCESS_SCENARIO_TIMEOUT: Duration = Duration::from_secs(120);

    #[derive(Default)]
    struct ScenarioDiag {
        attempt: usize,
        step: &'static str,
        home: Option<PathBuf>,
        op_id: Option<String>,
        port: u16,
    }

    impl ScenarioDiag {
        fn summary(&self, elapsed: Duration) -> String {
            let op_json = self
                .home
                .as_ref()
                .zip(self.op_id.as_ref())
                .and_then(|(h, id)| {
                    fs::read_to_string(h.join("updates/operations").join(format!("{id}.json"))).ok()
                })
                .unwrap_or_else(|| "<unavailable>".into());
            let port_taken = self
                .home
                .as_ref()
                .is_some_and(|h| h.join("port-taken").exists());
            format!(
                "attempt={} step={} elapsed={:?} port={} port_taken={} op={}",
                self.attempt, self.step, elapsed, self.port, port_taken, op_json
            )
        }
    }

    #[tokio::test]
    async fn health_requires_local_node_linked_when_node_is_enabled() {
        let home = tempfile::tempdir().unwrap();
        let candidate = bundle(home.path(), "0.2.0", false);
        switch(home.path(), &candidate).unwrap();
        let client = local_client().unwrap();
        for linked in [false, true] {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let port = listener.local_addr().unwrap().port();
            let cfg = Config::from_vars(|key| match key {
                "STEVE_HOME" => Some(home.path().to_string_lossy().into()),
                "STEVE_PORT" => Some(port.to_string()),
                "STEVE_ADAPTER_CLAUDE" | "STEVE_ADAPTER_CODEX" => Some("unused".into()),
                _ => None,
            })
            .unwrap();
            let app = axum::Router::new().route(
                "/healthz",
                axum::routing::get(move || async move {
                    axum::Json(json!({
                        "ok": true,
                        "maintenance": false,
                        "version": "0.2.0",
                        "web_version": "0.2.0",
                        "build_commit": "a".repeat(40),
                        "node": { "enabled": true, "linked": linked }
                    }))
                }),
            );
            let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
            let ok = healthy(&cfg, &client, "0.2.0", None).await;
            server.abort();
            assert_eq!(
                ok, linked,
                "post-switch health must require local Node linked when enabled"
            );
        }
        // Missing bin/steve-node fails pre-commit verification.
        fs::remove_file(candidate.join("bin/steve-node")).unwrap();
        let manifest = fixture("0.2.0");
        assert!(
            verify_candidate_node(&candidate, &manifest).await.is_err(),
            "missing bin/steve-node must fail startup gate verification"
        );
    }
    /// An upgrade between two fixture Hubs, on a port chosen before they bind it: another test
    /// can take it first, or between one child and the next. A fixture that finds it taken says so
    /// (`port-taken`), and the case runs again on another port, three times at most (#429).
    async fn scenario(broken: bool) {
        let diag = Arc::new(std::sync::Mutex::new(ScenarioDiag::default()));
        let started = std::time::Instant::now();
        let fut = async {
            for attempt in 1..=3 {
                diag.lock().unwrap().attempt = attempt;
                if scenario_on_a_free_port(broken, &diag).await {
                    return;
                }
            }
            panic!("the fixtures' port was taken three times");
        };
        if tokio::time::timeout(REAL_PROCESS_SCENARIO_TIMEOUT, fut)
            .await
            .is_err()
        {
            let info = diag.lock().unwrap().summary(started.elapsed());
            panic!(
                "real_process scenario (broken={broken}) timed out after {:?}: {info}",
                REAL_PROCESS_SCENARIO_TIMEOUT
            );
        }
    }
    /// The case, or `false` when a fixture's port was taken.
    async fn scenario_on_a_free_port(
        broken: bool,
        diag: &Arc<std::sync::Mutex<ScenarioDiag>>,
    ) -> bool {
        let note = |step: &'static str| {
            diag.lock().unwrap().step = step;
        };
        note("setup");
        let home = tempfile::tempdir().unwrap();
        let taken = || home.path().join("port-taken").exists();
        let port = std::net::TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port();
        {
            let mut d = diag.lock().unwrap();
            d.home = Some(home.path().to_path_buf());
            d.port = port;
        }
        let cfg = Config::from_vars(|k| match k {
            "STEVE_HOME" => Some(home.path().to_string_lossy().into()),
            "STEVE_PORT" => Some(port.to_string()),
            "STEVE_ADAPTER_CLAUDE" | "STEVE_ADAPTER_CODEX" => Some("unused".into()),
            _ => None,
        })
        .unwrap();
        fs::create_dir_all(&cfg.data_dir).unwrap();
        steve_store::testkit::hub::backup::seed_evidence(&cfg.db_path(), "before-upgrade").unwrap();
        let old = bundle(home.path(), "0.1.0", false);
        let new = bundle(home.path(), "0.2.0", broken);
        switch(home.path(), &old).unwrap();
        let client = local_client().unwrap();
        let u = Updates {
            home: home.path().into(),
            repository: "Infranite/steve".into(),
            current_hub: "0.1.0".into(),
            current_web: "0.1.0".into(),
            managed: true,
            blocked_reason: None,
            client: client.clone(),
            api_base: String::new(),
            web_base: String::new(),
            repo: PathBuf::new(),
            kind: None,
            launch: crate::upgrades::runner::Launch::Launchd,
            check_timeout: CHECK_HTTP_TIMEOUT,
            check_backoff: default_check_backoff(),
        };
        let mut op = u.request("0.2.0", "integration", "owner", true).unwrap();
        diag.lock().unwrap().op_id = Some(op.id.clone());
        op.target = Some(new.to_string_lossy().into());
        u.phase(&mut op, "ready", None).unwrap();
        note("spawn_old");
        let mut child = spawn(&cfg, &old, None).unwrap();
        note("wait_health_old");
        let up = wait_health(&cfg, &client, &mut child, "0.1.0", None).await;
        if taken() {
            let _ = stop(None, &mut child, STOP_GRACE).await;
            return false;
        }
        up.unwrap();
        note("apply");
        let applied = apply(&cfg, &u, &client, &mut child, &mut op).await;
        if taken() {
            let _ = stop(None, &mut child, STOP_GRACE).await;
            return false;
        }
        applied.unwrap();
        note("assert");
        assert_eq!(op.phase, if broken { "rolled_back" } else { "succeeded" });
        assert_eq!(
            fs::canonicalize(home.path().join("current")).unwrap(),
            fs::canonicalize(if broken { old } else { new }).unwrap()
        );
        assert_eq!(
            steve_store::testkit::hub::backup::evidence(&cfg.db_path()).unwrap(),
            "before-upgrade"
        );
        note("stop_final");
        stop(None, &mut child, STOP_GRACE).await.unwrap();
        true
    }

    #[tokio::test]
    async fn check_candidate_times_out_and_kills_process_group() {
        let home = tempfile::tempdir().unwrap();
        let cfg = Config::from_vars(|k| match k {
            "STEVE_HOME" => Some(home.path().to_string_lossy().into()),
            "STEVE_ADAPTER_CLAUDE" | "STEVE_ADAPTER_CODEX" => Some("unused".into()),
            _ => None,
        })
        .unwrap();
        let dir = home.path().join("hanging");
        fs::create_dir_all(dir.join("bin")).unwrap();
        let pid_file = home.path().join("hanging.pid");
        let bin = dir.join("bin/steve-hub");
        fs::write(
            &bin,
            format!(
                "#!/bin/sh\n/bin/sleep 60 & printf '%s' \"$!\" > '{}'\nwait\n",
                pid_file.display()
            ),
        )
        .unwrap();
        fs::set_permissions(&bin, fs::Permissions::from_mode(0o755)).unwrap();
        let started = std::time::Instant::now();
        let err = check_candidate_with_timeouts(
            &cfg,
            &dir,
            &fixture("0.2.0"),
            Duration::from_millis(250),
            Duration::from_millis(250),
        )
        .await
        .expect_err("hanging candidate --build-info must time out");
        assert!(
            matches!(
                err.downcast_ref::<SupervisorError>(),
                Some(SupervisorError::CandidateCheckTimedOut {
                    step: "build_info",
                    ..
                })
            ),
            "expected typed CandidateCheckTimedOut, got {err:?}"
        );
        assert!(started.elapsed() < Duration::from_secs(5));
        if let Ok(raw) = fs::read_to_string(&pid_file)
            && let Ok(pid) = raw.trim().parse::<libc::pid_t>()
        {
            steve_runtime::testkit::eventually_sync(
                "hanging candidate grandchild killed",
                || unsafe { libc::kill(pid, 0) == -1 },
            )
            .await;
        }
    }

    #[tokio::test]
    async fn check_candidate_recovers_from_grandchild_holding_pipes() {
        let home = tempfile::tempdir().unwrap();
        let cfg = Config::from_vars(|k| match k {
            "STEVE_HOME" => Some(home.path().to_string_lossy().into()),
            "STEVE_ADAPTER_CLAUDE" | "STEVE_ADAPTER_CODEX" => Some("unused".into()),
            _ => None,
        })
        .unwrap();
        let dir = home.path().join("pipe-holder");
        fs::create_dir_all(dir.join("bin")).unwrap();
        let pid_file = home.path().join("holder.pid");
        let bin = dir.join("bin/steve-hub");
        fs::write(
            &bin,
            format!(
                "#!/bin/sh\nif [ \"$1\" = --build-info ]; then\n  /bin/sleep 60 & printf '%s' \"$!\" > '{}'\n  printf '{{\"check_config\":false}}'\n  exit 0\nfi\nexit 0\n",
                pid_file.display()
            ),
        )
        .unwrap();
        fs::set_permissions(&bin, fs::Permissions::from_mode(0o755)).unwrap();
        let started = std::time::Instant::now();
        check_candidate_with_timeouts(
            &cfg,
            &dir,
            &fixture("0.2.0"),
            Duration::from_secs(5),
            Duration::from_secs(5),
        )
        .await
        .expect("pipe-holding grandchild must be killed without hanging check_candidate");
        assert!(started.elapsed() < Duration::from_secs(5));
        let pid: libc::pid_t = fs::read_to_string(&pid_file)
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        steve_runtime::testkit::eventually_sync("pipe-holding grandchild killed", || unsafe {
            libc::kill(pid, 0) == -1
        })
        .await;
    }
    #[tokio::test]
    async fn fixture_build_info_does_not_start_a_server_or_need_runtime_env() {
        let home = tempfile::tempdir().unwrap();
        let dir = bundle(home.path(), "0.2.0", false);
        let out = tokio::time::timeout(
            Duration::from_secs(5),
            Command::new(dir.join("bin/steve-hub"))
                .arg("--build-info")
                .env_remove("STEVE_HOME")
                .env_remove("STEVE_VERSION")
                .env("STEVE_PORT", "must-not-bind")
                .kill_on_drop(true)
                .output(),
        )
        .await
        .expect("metadata must exit without starting a server")
        .unwrap();
        assert!(out.status.success(), "{:?}", out.stderr);
        assert_eq!(
            serde_json::from_slice::<Value>(&out.stdout).unwrap()["check_config"],
            false
        );
    }
    #[tokio::test]
    async fn real_process_upgrade_commits_pair() {
        scenario(false).await
    }
    #[tokio::test]
    async fn real_process_bad_candidate_restores_database_and_old_pair() {
        scenario(true).await
    }
    #[test]
    fn liveness_counts_misses_only_after_an_answer() {
        let mut l = Liveness::default();
        /* still starting: never answered, never hung */
        for _ in 0..10 {
            assert!(!l.probe(false));
        }
        assert!(!l.probe(true));
        for _ in 1..LIVENESS_MISSES {
            assert!(!l.probe(false));
        }
        /* one answer in between starts again */
        assert!(!l.probe(true));
        for _ in 1..LIVENESS_MISSES {
            assert!(!l.probe(false));
        }
        assert!(
            l.probe(false),
            "hung after {LIVENESS_MISSES} misses in a row"
        );
    }

    #[test]
    fn vault_probe_timeout_leaves_headroom_and_scales_with_workspaces() {
        assert!(vault_probe_timeout(0) > KEYHELPER_TIMEOUT);
        assert!(vault_probe_timeout(1) > KEYHELPER_TIMEOUT);
        assert!(vault_probe_timeout(3) > KEYHELPER_TIMEOUT * 3);
        assert_eq!(vault_probe_timeout(50), VAULT_PROBE_MAX_TIMEOUT);
    }
}
