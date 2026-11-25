//! The Hub's own Node in a process of its own (`STEVE_NODE_MODE=service`, B1): a Hub restart or
//! upgrade leaves its turns running, as a remote Node's do.
//!
//! - The Node keeps its id (`local`) and its home (`<data>/node`): sessions bound to it before
//!   carry on there, their journal and harness state where they were.
//! - It is `steve-hub --local-node`, built from the Hub's own configuration
//!   ([`super::local_node_config`]) by the same binary, and it links to the Hub over loopback like a
//!   paired Node: the Hub registers it (`nodes`, `local`, active) with a key it writes into the
//!   Node's home, and the Node checks the Hub's signed welcome. A listener on
//!   `127.0.0.1:<STEVE_LOCAL_NODE_PORT>` (default: the Hub's port + 3) answers it, whatever
//!   addresses the Hub's own port is on; a listener handed to the Hub as `local-node`
//!   ([`crate::config::Handed`]) instead, on its own port.
//! - The Hub drives it with a coordinator of its own (trusted as the in-process Node is, none of a
//!   remote Node's limits), and answers the host bridges from the roots it writes to the Node's
//!   home ([`super::host_node::ServiceNode`]).
//! - Steve's think and research jobs stay in the Hub's process (`local-jobs`, `<data>/node-jobs`).
//! - The Hub starts the process when none is linked: in a session of its own (and, under systemd,
//!   in a scope of its own), so stopping the Hub's service does not stop it. A Node of another
//!   build is restarted once it has no turn running.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::json;
use steve_node_api::{Node, NodeId};
use steve_node_wire::Identity;
use steve_store::Store;

use super::host_node::{HostNode, ServiceNode};
use super::node_runtime::NodeRuntime;
use super::node_tools::NodeTools;
use crate::config::Config;
use crate::error::BackendError;

pub const NODE_ID: &str = "local";
const KEY: &str = "service-identity.key";
const LINK: &str = "service.json";
const PID: &str = "service.pid";
const INSTALLATION_CHECK: Duration = Duration::from_secs(1);
/// Maximum consecutive restarts before assuming the newly spawned binary still fails to match
/// the Hub and refusing to enter a kill loop.
const MAX_CONSECUTIVE_RESTARTS: u32 = 3;

/// The Hub's side of its service Node: what routing and workspaces use.
pub(crate) struct ServiceHost {
    pub runtime: Arc<NodeRuntime>,
    pub node: Arc<dyn HostNode>,
    pub tools: Arc<NodeTools>,
}

pub fn home(cfg: &Config) -> PathBuf {
    cfg.data_dir.join("node")
}

pub fn port(cfg: &Config, vars: &super::Vars) -> u16 {
    vars("STEVE_LOCAL_NODE_PORT")
        .and_then(|v| v.trim().parse::<u16>().ok())
        .filter(|p| *p != 0)
        .unwrap_or(cfg.listen.port.saturating_add(3))
}

fn identity(home: &Path) -> std::io::Result<Identity> {
    let path = home.join(KEY);
    match Identity::load(&path) {
        Ok(id) => Ok(id),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            std::fs::create_dir_all(home)?;
            let id = Identity::generate()?;
            id.save(&path)?;
            Ok(id)
        }
        Err(e) => Err(e),
    }
}

/// The Node's row: this machine's Node, active, its key the one in its home (made again, the row
/// follows it).
fn register(store: &Store, id: &Identity) -> Result<(), steve_store::StoreError> {
    let pubkey = id.public();
    let fingerprint = steve_node_wire::identity::fingerprint(&pubkey);
    let name = format!("{} · Hub", hostname());
    store.write(|w| {
        let owner = steve_store::UserRows(w)
            .first_owner()?
            .unwrap_or_else(|| "system".into());
        let node = steve_store::nodes::NewNode {
            node_id: NODE_ID,
            name: &name,
            pubkey: &pubkey,
            fingerprint: &fingerprint,
        };
        steve_store::node_directory::register_local(w, &node, &owner, crate::nodes::now())
    })
}

/// Builds the Hub's side and starts keeping the process alive. `svc`: the Node listener (the
/// service Node links through it).
pub(crate) fn host(
    cfg: &Config,
    vars: &super::Vars,
    store: &Store,
    domain: Arc<dyn super::node_runtime::NodeDomain>,
    svc: &Arc<crate::nodes::NodeService>,
    jobs: &mut Vec<crate::wire::modules::Job>,
) -> Result<ServiceHost, BackendError> {
    let err = |e: &dyn std::fmt::Display| BackendError::msg(format!("the local Node service: {e}"));
    let home = home(cfg);
    let id = identity(&home).map_err(|e| err(&e))?;
    register(store, &id).map_err(|e| err(&e))?;
    // a listener handed to the Hub when it started (config::Handed) says the port itself
    let handed = cfg
        .listen
        .handed
        .local_node()
        .transpose()
        .map_err(|e| err(&e))?;
    let listener = match handed {
        Some(listener) => listener,
        None => {
            let listener =
                std::net::TcpListener::bind(("127.0.0.1", port(cfg, vars))).map_err(|e| err(&e))?;
            listener.set_nonblocking(true).map_err(|e| err(&e))?;
            tokio::net::TcpListener::from_std(listener).map_err(|e| err(&e))?
        }
    };
    let addr = listener.local_addr().map_err(|e| err(&e))?;
    let port = addr.port();
    let _ = svc.local_listener.set(addr);
    let hub_key = svc.identity.get().map(|k| hex::encode(k.public()));
    std::fs::write(
        home.join(LINK),
        serde_json::to_vec_pretty(&json!({
            "hub_url": format!("http://127.0.0.1:{port}"),
            "hub_id": svc.hub_id,
            "hub_key": hub_key,
        }))
        .unwrap_or_default(),
    )
    .map_err(|e| err(&e))?;
    let node_id = NodeId::new(NODE_ID).expect("constant node id");
    let remote = svc.fleet.remote(&node_id);
    // on this machine: jobs and secrets written to a file stand as the Node says
    remote.set_colocated(true);
    let node = ServiceNode::new(node_id.clone(), remote.clone() as Arc<dyn Node>, &home);
    node.set_roots(
        super::node_binding::roots(store, &super::workspaces::configured(cfg))
            .map_err(|e| err(&e))?,
    );
    let tools = NodeTools::open(store.clone(), node_id.clone(), &home).map_err(|e| err(&e))?;
    let runtime = NodeRuntime::new(store.clone(), remote, domain);
    runtime.require_transport_link();
    runtime.set_configured_roots(super::workspaces::configured(cfg));
    runtime.set_principal_state(super::principal_state(vars));
    runtime.set_idle_release(cfg.idle_release_ms as i64);
    runtime.set_cost_caps(super::cost_caps(vars));
    if let Some(ms) = super::turn_limit(vars) {
        runtime.set_turn_limit(ms);
    }
    svc.fleet.adopt(&node_id, runtime.clone(), tools.clone());
    // the Node's own way in: loopback, the Node routes only
    let app = crate::nodes::node_router_at(svc.clone(), crate::nodes::link::Door::Colocated);
    let listen = async move {
        use axum::serve::ListenerExt as _;
        let l = listener
            .tap_io(|tcp: &mut tokio::net::TcpStream| steve_node_wire::sock::low_water(tcp));
        let _ = axum::serve(
            l,
            app.into_make_service_with_connect_info::<std::net::SocketAddr>(),
        )
        .await;
    };
    jobs.push(crate::wire::modules::Job::new(
        "node.service.listen",
        listen,
    ));
    let rt = runtime.clone();
    jobs.push(crate::wire::modules::Job::new(
        "node.service.runtime",
        async move { rt.start() },
    ));
    let svc_for_supervise = svc.clone();
    let rt_for_supervise = runtime.clone();
    let cfg_version = cfg.version.clone();
    jobs.push(crate::wire::modules::Job::task(
        "node.service.supervise",
        move || keep_running_task(home, svc_for_supervise, rt_for_supervise, cfg_version),
    ));
    Ok(ServiceHost {
        runtime,
        node,
        tools,
    })
}

fn hostname() -> String {
    let mut buf = [0u8; 256];
    // SAFETY: gethostname writes at most `buf.len()` bytes into it
    let ok = unsafe { libc::gethostname(buf.as_mut_ptr().cast(), buf.len()) } == 0;
    let end = buf.iter().position(|b| *b == 0).unwrap_or(buf.len());
    ok.then(|| String::from_utf8_lossy(&buf[..end]).into_owned())
        .filter(|h| !h.is_empty())
        .unwrap_or_else(|| "this machine".into())
}

fn pid_alive(pid: u32) -> bool {
    // SAFETY: signal 0 only checks that the process exists and may be signalled
    pid > 0 && unsafe { libc::kill(pid as libc::pid_t, 0) } == 0
}

/// The Node's `service.pid`: its pid, then the build it runs.
fn read_service(home: &Path) -> (Option<u32>, Option<String>) {
    let text = std::fs::read_to_string(home.join(PID)).unwrap_or_default();
    let mut lines = text.lines();
    let pid = lines.next().and_then(|l| l.trim().parse().ok());
    let version = lines
        .next()
        .map(|l| l.trim().to_owned())
        .filter(|v| !v.is_empty());
    (pid, version)
}

/// The running local service Node's version from `service.pid`, when that process is still alive.
pub(crate) fn running_version(home: &Path) -> Option<String> {
    let (pid, built) = read_service(home);
    pid.filter(|p| pid_alive(*p))?;
    built
}

fn send_sigterm(pid: u32) -> std::io::Result<()> {
    // SAFETY: a plain signal to the Node's process, which shuts down as on Ctrl-C
    if unsafe { libc::kill(pid as libc::pid_t, libc::SIGTERM) } == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error())
    }
}

#[derive(Default)]
struct SuperviseState {
    /// PID already sent SIGTERM, so each mismatched process is signaled at most once.
    signaled_pid: Option<u32>,
    /// Consecutive restart signals issued since the last healthy normal link.
    consecutive_restarts: u32,
    /// True once we logged the restart-cap error for the current unhealthy streak.
    restart_cap_logged: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SuperviseAction {
    Idle,
    Spawned,
    SpawnFailed,
    WaitingForTurns,
    Signaled(u32),
    SignalFailed(u32),
    AwaitingExit(u32),
    RestartCapReached(u32),
}

struct SuperviseObservation {
    linked: bool,
    upgrade_only: bool,
    running_turns: usize,
    pid: Option<u32>,
    built: Option<String>,
}

/// Deterministic supervision step: restarts the local service Node without touching ordinary
/// protocol RPCs whenever its link is upgrade-only or its recorded build differs from the Hub.
fn supervise_step(
    state: &mut SuperviseState,
    obs: SuperviseObservation,
    hub_version: &str,
    signal_term: impl FnOnce(u32) -> std::io::Result<()>,
    spawn_node: impl FnOnce() -> std::io::Result<()>,
) -> SuperviseAction {
    if state.signaled_pid.is_some_and(|old| obs.pid != Some(old)) {
        state.signaled_pid = None;
    }

    if !obs.linked && obs.pid.is_none() {
        return match spawn_node() {
            Ok(()) => SuperviseAction::Spawned,
            Err(e) => {
                tracing::error!(error = %e, "starting the local Node failed");
                SuperviseAction::SpawnFailed
            }
        };
    }

    let version_mismatch = obs.built.as_deref() != Some(hub_version);
    let needs_restart = obs.upgrade_only || version_mismatch;
    if !needs_restart {
        if obs.linked {
            state.consecutive_restarts = 0;
            state.restart_cap_logged = false;
        }
        return SuperviseAction::Idle;
    }

    let Some(pid) = obs.pid else {
        return SuperviseAction::Idle;
    };
    if obs.running_turns > 0 {
        return SuperviseAction::WaitingForTurns;
    }
    if state.signaled_pid == Some(pid) {
        return SuperviseAction::AwaitingExit(pid);
    }
    if state.consecutive_restarts >= MAX_CONSECUTIVE_RESTARTS {
        if !state.restart_cap_logged {
            state.restart_cap_logged = true;
            tracing::error!(
                pid,
                built = obs.built.as_deref().unwrap_or("unknown"),
                hub = hub_version,
                upgrade_only = obs.upgrade_only,
                restarts = state.consecutive_restarts,
                "local Node still mismatches Hub after repeated restarts; stopping restart loop"
            );
        }
        return SuperviseAction::RestartCapReached(pid);
    }

    state.signaled_pid = Some(pid);
    state.consecutive_restarts += 1;
    tracing::info!(
        pid,
        built = obs.built.as_deref().unwrap_or("unknown"),
        hub = hub_version,
        linked = obs.linked,
        upgrade_only = obs.upgrade_only,
        attempt = state.consecutive_restarts,
        "restarting the local Node to match this Hub"
    );
    if let Err(e) = signal_term(pid) {
        tracing::error!(pid, error = %e, "failed to signal the local Node to restart");
        return SuperviseAction::SignalFailed(pid);
    }
    SuperviseAction::Signaled(pid)
}

fn observe_local_service(
    home: &Path,
    svc: &crate::nodes::NodeService,
    runtime: &NodeRuntime,
    is_alive: impl FnOnce(u32) -> bool,
) -> SuperviseObservation {
    let conn = svc.registry.conn(NODE_ID).filter(|c| !c.is_closed());
    let linked = conn.is_some();
    let upgrade_only = conn.is_some_and(|c| c.is_upgrade());
    let (pid, built) = read_service(home);
    SuperviseObservation {
        linked,
        upgrade_only,
        running_turns: runtime.running_turns(),
        pid: pid.filter(|p| is_alive(*p)),
        built,
    }
}

/// Starts the Node's process when none runs, and replaces one of another build once it is idle.
fn keep_running_task(
    home: PathBuf,
    svc: Arc<crate::nodes::NodeService>,
    runtime: Arc<NodeRuntime>,
    version: String,
) -> steve_runtime::Task {
    let state = Arc::new(Mutex::new(SuperviseState::default()));
    // a moment for a Node still running from before this Hub to link again
    steve_runtime::every_after(
        "node.service.supervise",
        Arc::new(steve_runtime::SystemClock),
        Duration::from_secs(10),
        steve_runtime::FirstRun::After(Duration::from_secs(3)),
        move || {
            let (home, svc, runtime, version, state) = (
                home.clone(),
                svc.clone(),
                runtime.clone(),
                version.clone(),
                state.clone(),
            );
            async move {
                let obs = observe_local_service(&home, &svc, &runtime, pid_alive);
                let mut guard = state.lock().unwrap();
                supervise_step(&mut guard, obs, &version, send_sigterm, || spawn(&home));
            }
        },
    )
}

/// `steve-hub --local-node`, detached from the Hub: its own session, and its own systemd scope
/// when the Hub runs under systemd, so the Hub's service stopping leaves it running.
fn spawn(home: &Path) -> std::io::Result<()> {
    let exe = std::env::current_exe()?;
    // only the Hub's own binary has `--local-node` (a test harness wiring the Hub in its own
    // process starts the Node itself, or runs without it)
    if !exe
        .file_name()
        .is_some_and(|n| n.to_string_lossy().starts_with("steve-hub"))
    {
        return Err(std::io::Error::other(format!(
            "{} is not steve-hub: start `steve-hub --local-node` yourself",
            exe.display()
        )));
    }
    let log_dir = home.join("logs");
    std::fs::create_dir_all(&log_dir)?;
    let log = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(log_dir.join("local-node.log"))?;
    let systemd =
        std::env::var_os("INVOCATION_ID").is_some() && Path::new("/usr/bin/systemd-run").exists();
    let mut cmd = if systemd {
        let mut c = std::process::Command::new("/usr/bin/systemd-run");
        c.args(["--user", "--scope", "--collect", "--quiet"])
            .arg(format!("--unit=steve-local-node-{}", std::process::id()))
            .arg(&exe);
        c
    } else {
        std::process::Command::new(&exe)
    };
    cmd.arg("--local-node")
        .stdin(std::process::Stdio::null())
        .stdout(log.try_clone()?)
        .stderr(log);
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        // SAFETY: setsid only, between fork and exec
        unsafe {
            cmd.pre_exec(|| {
                libc::setsid();
                Ok(())
            });
        }
    }
    let child = cmd.spawn()?;
    tracing::info!(
        pid = child.id(),
        "started the local Node in a process of its own"
    );
    Ok(())
}

fn retired(home: &Path, executable: &Path) -> bool {
    if let Some(missing) = [home, executable]
        .into_iter()
        .find(|path| matches!(path.try_exists(), Ok(false)))
    {
        tracing::info!(path = %missing.display(), "the local Node's installation is gone");
        return true;
    }
    false
}

async fn wait_for_retirement(home: &Path, executable: &Path) {
    let mut tick = tokio::time::interval(INSTALLATION_CHECK);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        tick.tick().await;
        if retired(home, executable) {
            return;
        }
    }
}

/// The Hub's own Node, which reconnects across Hub restarts while its installation exists.
pub async fn run(cfg: &Config) -> anyhow::Result<()> {
    anyhow::ensure!(
        !cfg.diagnostic(),
        "--local-node is disabled on STEVE_NO_RESUME copies"
    );
    use anyhow::Context;
    let executable = std::env::current_exe().context("the local Node's executable")?;
    let executable = executable.canonicalize().unwrap_or(executable);
    if retired(&cfg.home, &executable) {
        return Ok(());
    }
    let home = home(cfg);
    let link: serde_json::Value = serde_json::from_slice(
        &std::fs::read(home.join(LINK)).context("the Hub has not set up its local Node yet")?,
    )?;
    let id = Identity::load(&home.join(KEY)).context("the local Node's key")?;
    let roots_file = super::host_node::roots_file(&home);
    let roots: Vec<steve_node::Root> = std::fs::read(&roots_file)
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default();
    let node_id = NodeId::new(NODE_ID).expect("constant node id");
    let node = steve_node::LocalNode::open(super::local_node_config(
        cfg,
        node_id.clone(),
        home.clone(),
        roots,
        false,
    ))
    .map_err(|e| anyhow::anyhow!("{e}"))?;
    node.follow_roots_file(roots_file);
    super::local_node_extras(&node, cfg, false);
    let pairing = steve_node_cli::config::Pairing {
        hub_url: link["hub_url"].as_str().context("hub_url")?.to_string(),
        node_id: NODE_ID.into(),
        hub_id: link["hub_id"].as_str().context("hub_id")?.to_string(),
        pin: None,
        loopback: true,
        hub_key: link["hub_key"].as_str().map(str::to_string),
        name: NODE_ID.into(),
    };
    tracing::info!(
        "the local Node, in a process of its own, links to {}",
        pairing.hub_url
    );
    std::fs::write(
        home.join(PID),
        format!("{}\n{}\n", std::process::id(), cfg.version),
    )?;
    let r = steve_node_cli::dial::serve_until_stopped(
        node,
        Some(&home),
        &pairing,
        &id,
        &steve_node_wire::tls::Trust::LoopbackPlaintext,
        wait_for_retirement(&cfg.home, &executable),
    )
    .await;
    if read_service(&home).0 == Some(std::process::id()) {
        let _ = std::fs::remove_file(home.join(PID));
    }
    r
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use steve_node_api::{PROTOCOL, upgrade_v1};
    use steve_node_wire::{Conn, ConnOptions, NodeServer};

    struct NoopDomain;
    impl crate::wire::node_runtime::NodeDomain for NoopDomain {
        fn turn_ended(&self, _: &str, _: Option<&str>, _: &str, _: Option<String>) {}
    }

    #[tokio::test]
    async fn upgrade_only_local_service_node_is_restarted_once_and_respawned_after_exit() {
        let dir = tempfile::tempdir().unwrap();
        let node_home = dir.path().join("node");
        std::fs::create_dir_all(&node_home).unwrap();
        std::fs::write(node_home.join(PID), "4242\n0.9.0\n").unwrap();

        let store =
            Store::open(dir.path().join("steve.sqlite3"), steve_store::NoPublisher).unwrap();
        store.ensure_workspace().unwrap();
        let fleet =
            crate::nodes::NodeFleet::new(store.clone(), dir.path().to_path_buf(), None, None);
        let svc = crate::nodes::NodeService::new(store.clone(), fleet.clone(), None).unwrap();
        let node_id = NodeId::new(NODE_ID).unwrap();
        let remote = fleet.remote(&node_id);
        let runtime = NodeRuntime::new(store, remote.clone(), Arc::new(NoopDomain));

        // Link the local service Node on an older protocol so Conn enters upgrade-only mode.
        let (hub, peer) = steve_node_wire::testkit::pair();
        let fake = steve_node_api::testkit::FakeNode::new(NODE_ID);
        let mode = upgrade_v1::Mode::between(PROTOCOL, PROTOCOL - 1);
        assert_eq!(mode, upgrade_v1::Mode::Upgrade);
        let hub_conn = Conn::spawn(
            hub.sink,
            hub.stream,
            1,
            NodeServer::new(fake.clone()),
            ConnOptions {
                side: "h",
                mode,
                ..Default::default()
            },
        );
        let _peer_conn = Conn::spawn(
            peer.sink,
            peer.stream,
            1,
            NodeServer::new(fake),
            ConnOptions {
                side: "n",
                mode,
                ..Default::default()
            },
        );
        assert!(svc.registry.install(NODE_ID, 1, hub_conn.clone()));
        assert!(remote.attach(hub_conn.clone(), false));

        let mut state = SuperviseState::default();
        let signals = Arc::new(Mutex::new(Vec::<u32>::new()));
        let spawns = Arc::new(AtomicUsize::new(0));

        // First tick: linked in upgrade-only mode, idle -> SIGTERM sent without calling probe().
        let obs = observe_local_service(&node_home, &svc, &runtime, |p| p == 4242);
        assert!(obs.linked && obs.upgrade_only);
        let s1 = signals.clone();
        let sp1 = spawns.clone();
        assert_eq!(
            supervise_step(
                &mut state,
                obs,
                "0.11.1",
                move |pid| {
                    s1.lock().unwrap().push(pid);
                    Ok(())
                },
                move || {
                    sp1.fetch_add(1, Ordering::SeqCst);
                    Ok(())
                },
            ),
            SuperviseAction::Signaled(4242)
        );
        assert_eq!(*signals.lock().unwrap(), vec![4242]);

        // Second tick while old PID 4242 is still alive -> debounced, no duplicate SIGTERM.
        let obs = observe_local_service(&node_home, &svc, &runtime, |p| p == 4242);
        let s2 = signals.clone();
        assert_eq!(
            supervise_step(
                &mut state,
                obs,
                "0.11.1",
                move |pid| {
                    s2.lock().unwrap().push(pid);
                    Ok(())
                },
                || Ok(()),
            ),
            SuperviseAction::AwaitingExit(4242)
        );
        assert_eq!(*signals.lock().unwrap(), vec![4242]);

        // Old process exits and link closes -> existing spawn path brings up the new version.
        hub_conn.close(None, steve_node_wire::close::GOODBYE, "restarting");
        svc.registry.remove(NODE_ID, 1);
        remote.detach(1);
        let obs = observe_local_service(&node_home, &svc, &runtime, |_| false);
        assert!(!obs.linked && obs.pid.is_none());
        let sp2 = spawns.clone();
        assert_eq!(
            supervise_step(
                &mut state,
                obs,
                "0.11.1",
                |_| panic!("must not signal when process is gone"),
                move || {
                    sp2.fetch_add(1, Ordering::SeqCst);
                    Ok(())
                },
            ),
            SuperviseAction::Spawned
        );
        assert_eq!(spawns.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn supervise_waits_for_running_turns_reports_spawn_errors_and_caps_restart_loops() {
        let mut state = SuperviseState::default();

        // Active turns defer restart even when upgrade-only or mismatched.
        assert_eq!(
            supervise_step(
                &mut state,
                SuperviseObservation {
                    linked: true,
                    upgrade_only: true,
                    running_turns: 2,
                    pid: Some(100),
                    built: Some("0.9.0".into()),
                },
                "0.11.1",
                |_| panic!("must not signal while turns are running"),
                || panic!("must not spawn while old process runs"),
            ),
            SuperviseAction::WaitingForTurns
        );

        // Spawn failure reports error without panicking or looping kills.
        assert_eq!(
            supervise_step(
                &mut state,
                SuperviseObservation {
                    linked: false,
                    upgrade_only: false,
                    running_turns: 0,
                    pid: None,
                    built: None,
                },
                "0.11.1",
                |_| panic!("no pid to signal"),
                || Err(std::io::Error::other("exec failed")),
            ),
            SuperviseAction::SpawnFailed
        );

        // Repeated restarts of distinct mismatched PIDs stop at MAX_CONSECUTIVE_RESTARTS.
        for i in 0..MAX_CONSECUTIVE_RESTARTS {
            let pid = 200 + i;
            assert_eq!(
                supervise_step(
                    &mut state,
                    SuperviseObservation {
                        linked: true,
                        upgrade_only: true,
                        running_turns: 0,
                        pid: Some(pid),
                        built: Some("0.9.0".into()),
                    },
                    "0.11.1",
                    |_| Ok(()),
                    || Ok(()),
                ),
                SuperviseAction::Signaled(pid)
            );
        }
        assert_eq!(
            supervise_step(
                &mut state,
                SuperviseObservation {
                    linked: true,
                    upgrade_only: true,
                    running_turns: 0,
                    pid: Some(999),
                    built: Some("0.9.0".into()),
                },
                "0.11.1",
                |_| panic!("must not keep killing past the restart cap"),
                || Ok(()),
            ),
            SuperviseAction::RestartCapReached(999)
        );
    }
}
