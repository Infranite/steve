//! Colocated local Node runner (`steve-node --local-node`): runs the Hub's own Node in a separate
//! process, linking over loopback and surviving Hub restarts/upgrades.

use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result, ensure};
use steve_node::{LocalNode, NodeConfig, PolicyCeiling, Root, Storage};
use steve_node_api::{
    Capacity, HarnessPreflight, LOCAL_SERVICE_LINK_FILE, LOCAL_SERVICE_PID_FILE, LocalServiceLink,
    LocalServicePid, NodeId,
};
use steve_node_wire::Identity;

use crate::config::{Home, Pairing, canonicalize_path};

const NODE_ID: &str = "local";
const KEY: &str = "service-identity.key";
const ROOTS: &str = "host-roots.json";
const INSTALLATION_CHECK: Duration = Duration::from_secs(1);
/// Concurrent command/push slots on the colocated Node.
const LOCAL_EXEC_CAPACITY: u32 = 16;

fn is_no_resume() -> bool {
    std::env::var("STEVE_NO_RESUME").ok().is_some_and(|v| {
        let s = v.trim();
        s == "1" || s.eq_ignore_ascii_case("true") || s.eq_ignore_ascii_case("yes")
    })
}

fn steve_home_from_env() -> Result<PathBuf> {
    if let Ok(h) = std::env::var("STEVE_HOME")
        && !h.is_empty()
    {
        return Ok(canonicalize_path(Path::new(&h)));
    }
    let home = std::env::var("HOME").context("HOME is not set")?;
    let p = Path::new(&home).join(".steve");
    Ok(canonicalize_path(&p))
}

fn data_dir_from_env(steve_home: &Path) -> PathBuf {
    if let Ok(d) = std::env::var("STEVE_DATA_DIR")
        && !d.trim().is_empty()
    {
        return canonicalize_path(Path::new(&d));
    }
    canonicalize_path(&steve_home.join("data"))
}

#[derive(Clone, Debug)]
struct WatchedPath {
    path: PathBuf,
    #[cfg(unix)]
    identity: Option<(u64, u64)>,
}

impl WatchedPath {
    fn capture(path: PathBuf) -> Self {
        #[cfg(unix)]
        let identity = {
            use std::os::unix::fs::MetadataExt;
            std::fs::metadata(&path).ok().map(|m| (m.dev(), m.ino()))
        };
        Self {
            path,
            #[cfg(unix)]
            identity,
        }
    }

    fn intact(&self) -> bool {
        match std::fs::metadata(&self.path) {
            Ok(meta) => {
                #[cfg(unix)]
                if let Some(id) = self.identity {
                    use std::os::unix::fs::MetadataExt;
                    return (meta.dev(), meta.ino()) == id;
                }
                let _ = meta;
                true
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => false,
            #[cfg(unix)]
            Err(e) if e.raw_os_error() == Some(libc::ENOTDIR) => false,
            Err(_) => true,
        }
    }
}

fn path_missing(path: &Path) -> bool {
    matches!(path.try_exists(), Ok(false))
}

fn retired(home: &WatchedPath, node: &LocalNode, executable: &Path) -> bool {
    if !home.intact() {
        tracing::info!(path = %home.path.display(), "the local Node's home is gone or replaced");
        return true;
    }
    if !node.home_intact() {
        tracing::info!("the local Node's lock file is gone or replaced");
        return true;
    }
    if path_missing(executable) {
        tracing::info!(path = %executable.display(), "the local Node's executable is gone");
        return true;
    }
    false
}

async fn wait_for_retirement(home: WatchedPath, node: LocalNode, executable: &Path) {
    let mut tick = tokio::time::interval(INSTALLATION_CHECK);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        tick.tick().await;
        if retired(&home, &node, executable) {
            return;
        }
    }
}

fn read_pid(home: &Path) -> Option<u32> {
    let text = std::fs::read_to_string(home.join(LOCAL_SERVICE_PID_FILE)).unwrap_or_default();
    LocalServicePid::parse(&text).map(|p| p.pid)
}

fn discovery_config(
    steve_home: &Path,
    acp: &steve_acp::AcpConfig,
) -> steve_node::history::DiscoveryConfig {
    let path_env = std::env::var_os("PATH");
    steve_node::history::DiscoveryConfig {
        homes: steve_node::history::Homes {
            user_home: acp.user_home.clone(),
            steve_home: steve_home.to_path_buf(),
            state_dirs: acp.discovery_state_dirs(),
        },
        binaries: steve_node::history::Binaries {
            bins: acp.discovery_binaries(|k| std::env::var_os(k), path_env.as_deref()),
        },
    }
}

/// Offline harness/adapter preflight run by `steve-node harness-probe` (invoked by
/// `steve-hub --check-config` and boot warnings), delegating all harness rules to `steve-acp`.
pub fn probe_harnesses(var: impl Fn(&str) -> Option<String>) -> HarnessPreflight {
    let user_home = var("HOME")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/"));
    let steve_home = var("STEVE_HOME")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| user_home.join(".steve"));
    let steve_home = std::path::absolute(&steve_home).unwrap_or(steve_home);
    let manifest_path = var("STEVE_HARNESS_MANIFEST")
        .filter(|v| !v.trim().is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| steve_home.join("harness.json"));
    steve_acp::probe_harnesses(&steve_home, &manifest_path, var)
}

/// Run the Hub's colocated local Node until stopped by signal or installation retirement.
pub async fn run() -> Result<()> {
    ensure!(
        !is_no_resume(),
        "--local-node is disabled on STEVE_NO_RESUME copies"
    );
    let steve_home = steve_home_from_env()?;
    let executable = std::env::current_exe().context("the local Node's executable")?;
    let executable = executable.canonicalize().unwrap_or(executable);
    if path_missing(&steve_home) || path_missing(&executable) {
        tracing::info!("the local Node's installation is gone");
        return Ok(());
    }
    let watched_home = WatchedPath::capture(steve_home.clone());
    let data_dir = data_dir_from_env(&steve_home);
    let node_home = data_dir.join("node");
    let link: LocalServiceLink = serde_json::from_slice(
        &std::fs::read(node_home.join(LOCAL_SERVICE_LINK_FILE))
            .context("the Hub has not set up its local Node yet")?,
    )
    .context("invalid local Node service link")?;
    let id = Identity::load(&node_home.join(KEY)).context("the local Node's key")?;
    let roots_file = node_home.join(ROOTS);
    let roots: Vec<Root> = std::fs::read(&roots_file)
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default();
    let acp = Home(steve_home.clone()).acp()?;
    let version = acp.version.clone();
    let discovery = discovery_config(&steve_home, &acp);
    let worktree_floor = link.disk_floor;
    let node_id = NodeId::new(NODE_ID).expect("constant node id");
    let node = LocalNode::open(NodeConfig {
        ceiling: PolicyCeiling::unlimited(),
        node: node_id,
        home: node_home.clone(),
        acp,
        roots,
        capacity: Capacity {
            sessions: Capacity::SESSIONS_DEFAULT,
            jobs: steve_node::LOCAL_JOB_CAPACITY,
            execs: LOCAL_EXEC_CAPACITY,
        },
        storage: Storage {
            worktree_floor,
            ..Storage::default()
        },
    })
    .map_err(|e| anyhow::anyhow!("{e}"))?;
    node.follow_roots_file(roots_file);
    node.enable_host_ops();
    node.enable_research_in(steve_home.join("scratch").join("research"));
    node.enable_capability_sandbox();
    node.set_discovery(discovery);
    let pairing = Pairing {
        hub_url: link.hub_url,
        node_id: NODE_ID.into(),
        hub_id: link.hub_id,
        pin: None,
        loopback: true,
        hub_key: link.hub_key,
        name: NODE_ID.into(),
    };
    tracing::info!(
        "the local Node, in a process of its own, links to {}",
        pairing.hub_url
    );
    let pid_record = LocalServicePid {
        pid: std::process::id(),
        version: Some(version),
    };
    std::fs::write(node_home.join(LOCAL_SERVICE_PID_FILE), pid_record.format())?;
    let retirement_node = node.clone();
    let r = crate::dial::serve_until_stopped(
        node,
        Some(&node_home),
        &pairing,
        &id,
        &steve_node_wire::tls::Trust::LoopbackPlaintext,
        wait_for_retirement(watched_home, retirement_node, &executable),
    )
    .await;
    if read_pid(&node_home) == Some(std::process::id()) {
        let _ = std::fs::remove_file(node_home.join(LOCAL_SERVICE_PID_FILE));
    }
    r
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_node(steve_home: &Path, node_home: &Path) -> LocalNode {
        LocalNode::open(NodeConfig {
            ceiling: PolicyCeiling::unlimited(),
            node: NodeId::new(NODE_ID).unwrap(),
            home: node_home.to_path_buf(),
            acp: steve_acp::AcpConfig {
                adapters: Default::default(),
                declared: Default::default(),
                node: PathBuf::from("/usr/bin/false"),
                claude_executable: PathBuf::from("/usr/bin/false"),
                claude_config_dir: None,
                steve_home: steve_home.to_path_buf(),
                codex_home: steve_home.join("codex"),
                user_home: steve_home.to_path_buf(),
                version: "0.14.0".into(),
            },
            roots: vec![],
            capacity: Capacity {
                sessions: 1,
                jobs: 0,
                execs: 0,
            },
            storage: Storage::default(),
        })
        .unwrap()
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn retired_detects_missing_and_recreated_home_lock_and_executable() {
        let tmp = tempfile::tempdir().unwrap();
        let steve_home = tmp.path().join("steve");
        let node_home = steve_home.join("data/node");
        std::fs::create_dir_all(&node_home).unwrap();
        let exe = tmp.path().join("steve-node");
        std::fs::write(&exe, b"bin").unwrap();

        let watched = WatchedPath::capture(steve_home.clone());
        let node = test_node(&steve_home, &node_home);
        assert!(!retired(&watched, &node, &exe));

        // 1. Renaming STEVE_HOME and recreating an empty directory at the old path must retire.
        let moved_home = tmp.path().join("retired-steve");
        std::fs::rename(&steve_home, &moved_home).unwrap();
        assert!(retired(&watched, &node, &exe));
        std::fs::create_dir_all(&steve_home).unwrap();
        assert!(retired(&watched, &node, &exe));

        // Restore original STEVE_HOME inode and verify retirement clears again.
        std::fs::remove_dir_all(&steve_home).unwrap();
        std::fs::rename(&moved_home, &steve_home).unwrap();
        assert!(!retired(&watched, &node, &exe));

        // 2. Replacing node.lock triggers retirement via node.home_intact().
        let moved_lock = node_home.join("node.lock.moved");
        std::fs::rename(node_home.join("node.lock"), &moved_lock).unwrap();
        assert!(retired(&watched, &node, &exe));
        std::fs::write(node_home.join("node.lock"), "").unwrap();
        assert!(retired(&watched, &node, &exe));

        // Restore original lock inode and verify executable removal triggers retirement.
        std::fs::rename(&moved_lock, node_home.join("node.lock")).unwrap();
        assert!(!retired(&watched, &node, &exe));
        std::fs::remove_file(&exe).unwrap();
        assert!(retired(&watched, &node, &exe));

        node.shutdown().await;
    }
}
