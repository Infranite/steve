//! Colocated local Node runner (`steve-node --local-node`): runs the Hub's own Node in a separate
//! process, linking over loopback and surviving Hub restarts/upgrades.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result, ensure};
use steve_node::{LocalNode, NodeConfig, PolicyCeiling, Root, Storage};
use steve_node_api::{Capacity, NodeId};
use steve_node_wire::Identity;

use crate::config::{Home, Pairing, canonicalize_path};

const NODE_ID: &str = "local";
const KEY: &str = "service-identity.key";
const LINK: &str = "service.json";
const PID: &str = "service.pid";
const ROOTS: &str = "host-roots.json";
const INSTALLATION_CHECK: Duration = Duration::from_secs(1);
/// Default worktree disk floor (10 GiB), matching `steve_core::workspace::DiskPolicy::default`.
const DEFAULT_DISK_FLOOR_BYTES: u64 = 10 << 30;
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

fn read_pid(home: &Path) -> Option<u32> {
    let text = std::fs::read_to_string(home.join(PID)).unwrap_or_default();
    text.lines().next().and_then(|l| l.trim().parse().ok())
}

fn discovery_config(
    steve_home: &Path,
    acp: &steve_acp::AcpConfig,
) -> steve_node::history::DiscoveryConfig {
    let path_env = std::env::var_os("PATH");
    let mut state_dirs = BTreeMap::new();
    let mut bins = BTreeMap::new();
    for b in steve_harness_spec::builtins() {
        if let Some(state) = &b.properties.state {
            let dir = match state.env.as_str() {
                "CLAUDE_CONFIG_DIR" => Some(
                    acp.claude_config_dir
                        .clone()
                        .unwrap_or_else(|| acp.user_home.join(&state.default)),
                ),
                "CODEX_HOME" => Some(acp.codex_home.clone()),
                _ => None,
            };
            if let Some(dir) = dir {
                state_dirs.insert(b.id.to_string(), dir);
            }
        }
        if b.properties.executable_env.as_deref() == Some("CLAUDE_CODE_EXECUTABLE") {
            if !acp.claude_executable.as_os_str().is_empty() {
                bins.insert(b.id.to_string(), acp.claude_executable.clone());
            }
        } else if b.properties.usage_fallback == steve_harness_spec::UsageFallback::CodexRollout {
            let detected = std::env::var_os("CODEX_EXECUTABLE")
                .filter(|v| !v.is_empty())
                .map(PathBuf::from)
                .or_else(|| {
                    b.detect_bins
                        .iter()
                        .find_map(|name| steve_acp::which_in(name, path_env.as_deref()))
                });
            if let Some(bin) = detected {
                bins.insert(b.id.to_string(), bin);
            }
        }
    }
    steve_node::history::DiscoveryConfig {
        homes: steve_node::history::Homes {
            user_home: acp.user_home.clone(),
            steve_home: steve_home.to_path_buf(),
            state_dirs,
        },
        binaries: steve_node::history::Binaries { bins },
    }
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
    if retired(&steve_home, &executable) {
        return Ok(());
    }
    let data_dir = data_dir_from_env(&steve_home);
    let node_home = data_dir.join("node");
    let link: serde_json::Value = serde_json::from_slice(
        &std::fs::read(node_home.join(LINK))
            .context("the Hub has not set up its local Node yet")?,
    )?;
    let id = Identity::load(&node_home.join(KEY)).context("the local Node's key")?;
    let roots_file = node_home.join(ROOTS);
    let roots: Vec<Root> = std::fs::read(&roots_file)
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default();
    let acp = Home(steve_home.clone()).acp()?;
    let version = acp.version.clone();
    let discovery = discovery_config(&steve_home, &acp);
    let worktree_floor = link["disk_floor"]
        .as_u64()
        .unwrap_or(DEFAULT_DISK_FLOOR_BYTES);
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
    node.enable_research_in(steve_home.join("scratch").join("research"));
    node.enable_capability_sandbox();
    node.set_discovery(discovery);
    let pairing = Pairing {
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
        node_home.join(PID),
        format!("{}\n{}\n", std::process::id(), version),
    )?;
    let r = crate::dial::serve_until_stopped(
        node,
        Some(&node_home),
        &pairing,
        &id,
        &steve_node_wire::tls::Trust::LoopbackPlaintext,
        wait_for_retirement(&steve_home, &executable),
    )
    .await;
    if read_pid(&node_home) == Some(std::process::id()) {
        let _ = std::fs::remove_file(node_home.join(PID));
    }
    r
}
