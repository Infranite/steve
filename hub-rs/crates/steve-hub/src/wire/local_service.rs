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
use std::sync::Arc;
use std::time::Duration;

use serde_json::json;
use steve_node_api::{Discovery, Node, NodeId};
use steve_node_wire::Identity;
use steve_store::Store;

use super::host_node::{HostNode, ServiceNode};
use super::node_runtime::NodeRuntime;
use super::node_tools::NodeTools;
use crate::backend::BackendError;
use crate::config::Config;

pub const NODE_ID: &str = "local";
const KEY: &str = "service-identity.key";
const LINK: &str = "service.json";
const PID: &str = "service.pid";
const INSTALLATION_CHECK: Duration = Duration::from_secs(1);

/// The Hub's side of its service Node: what routing, YOLO and workspaces use.
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
    store.transaction(|tx| {
        let owner: String = tx
            .query_row(
                "SELECT id FROM users WHERE role='owner' ORDER BY created, rowid LIMIT 1",
                [],
                |r| r.get(0),
            )
            .unwrap_or_else(|_| "system".into());
        tx.execute(
            "INSERT INTO nodes(node_id,owner,name,pubkey,fingerprint,state,local,created) VALUES (?1,?2,?3,?4,?5,'active',1,?6)
             ON CONFLICT(node_id) DO UPDATE SET pubkey=excluded.pubkey, fingerprint=excluded.fingerprint, state='active', local=1, revoked_at=NULL, revoked_by=NULL",
            rusqlite::params![NODE_ID, owner, name, &pubkey[..], fingerprint, crate::nodes::now()],
        )?;
        Ok(())
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
    jobs.push(crate::wire::modules::Job::new(
        "node.service.supervise",
        keep_running(home, svc.clone(), runtime.clone(), cfg.version.clone()),
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

/// Starts the Node's process when none runs, and replaces one of another build once it is idle.
async fn keep_running(
    home: PathBuf,
    svc: Arc<crate::nodes::NodeService>,
    runtime: Arc<NodeRuntime>,
    version: String,
) {
    // a moment for a Node still running from before this Hub to link again
    tokio::time::sleep(Duration::from_secs(3)).await;
    let node_id = NodeId::new(NODE_ID).expect("constant node id");
    loop {
        let linked = svc.registry.current(NODE_ID).is_some();
        let (pid, built) = read_service(&home);
        let pid = pid.filter(|p| pid_alive(*p));
        if !linked && pid.is_none() {
            if let Err(e) = spawn(&home) {
                tracing::error!("starting the local Node: {e}");
            }
        } else if !linked
            && let Some(pid) = pid
            && built.as_deref() != Some(version.as_str())
        {
            // A Node of another build cannot link to this Hub once their protocols differ, so
            // waiting for it to link first would leave it running forever.
            tracing::info!(
                "the local Node runs {}, this Hub {version}, and it is not linked: restarting it",
                built.as_deref().unwrap_or("an unknown build")
            );
            // SAFETY: a plain signal to the Node's process, which shuts down as on Ctrl-C
            unsafe { libc::kill(pid as libc::pid_t, libc::SIGTERM) };
        } else if linked
            && runtime.running_turns() == 0
            && let Some(pid) = pid
            && let Ok(caps) = svc.fleet.remote(&node_id).probe(false).await
            && !caps.version.is_empty()
            && caps.version != version
        {
            tracing::info!(
                "the local Node runs {}, this Hub {version}: restarting it while it is idle",
                caps.version
            );
            // SAFETY: a plain signal to the Node's process, which shuts down as on Ctrl-C
            unsafe { libc::kill(pid as libc::pid_t, libc::SIGTERM) };
        }
        tokio::time::sleep(Duration::from_secs(10)).await;
    }
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
