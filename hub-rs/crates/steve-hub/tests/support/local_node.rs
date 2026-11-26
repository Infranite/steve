//! Shared test helper for integration tests that run `steve_hub::wire::wire_with` in-process and
//! attach a real `steve_node::LocalNode("local")` over an in-memory `steve_node_wire::testkit::pair()`
//! (no TCP socket required, full wire protocol, zero `STEVE_NODE_MODE=local`).

#![allow(dead_code)]

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use steve_hub::config::Config;
use steve_hub::jobs::Tasks;
use steve_hub::wire::Wired;
use steve_hub::wire::host_node::roots_file;
use steve_hub::wire::local_service;
use steve_hub::wire::modules::Job;
use steve_node_api::NodeId;

pub struct AttachedLocalNode {
    pub node: Arc<steve_node::LocalNode>,
    _hub_conn: Arc<steve_node_wire::Conn>,
    _node_conn: Arc<steve_node_wire::Conn>,
}

impl AttachedLocalNode {
    pub async fn shutdown(self) {
        self.node.shutdown().await;
    }
}

fn adapters_from_vars(cfg: &Config, vars: &HashMap<String, String>) -> HashMap<String, PathBuf> {
    #[derive(serde::Deserialize)]
    struct Manifest {
        #[serde(default)]
        adapters: HashMap<String, steve_harness_spec::AdapterEntry>,
    }
    let mut out = HashMap::new();
    if let Ok(text) = std::fs::read_to_string(&cfg.harness_manifest)
        && let Ok(manifest) = serde_json::from_str::<Manifest>(&text)
    {
        let (commands, _) = steve_harness_spec::split_entries(manifest.adapters);
        out.extend(commands);
    }
    for (env_key, harness) in [
        ("STEVE_ADAPTER_CLAUDE", "claude-code"),
        ("STEVE_ADAPTER_CODEX", "codex"),
        ("STEVE_ADAPTER_PI", "pi"),
        ("STEVE_ADAPTER_OPENCODE", "opencode"),
        ("STEVE_ADAPTER_GEMINI", "gemini"),
        ("STEVE_ADAPTER_GOOSE", "goose"),
    ] {
        if let Some(p) = vars.get(env_key).filter(|s| !s.is_empty()) {
            out.insert(harness.to_string(), PathBuf::from(p));
        }
    }
    out
}

/// Starts all wired Hub background jobs except the external local-node supervisor/listener when an
/// in-memory paired test `LocalNode` is attached instead.
pub fn start_jobs(jobs: Vec<Job>, tasks: &Tasks) {
    for job in jobs {
        if matches!(job.name, "node.service.supervise" | "node.service.listen") {
            continue;
        }
        job.start(tasks);
    }
}

/// Attaches a real `steve_node::LocalNode("local")` to `w.nodes` over `steve_node_wire::testkit::pair()`.
/// Returns `None` when the Hub was wired without a local Node (e.g. `STEVE_NO_RESUME=1`).
pub async fn attach(
    w: &Wired,
    cfg: &Config,
    vars: &HashMap<String, String>,
) -> Option<AttachedLocalNode> {
    if w.node.is_none() {
        return None;
    }
    let svc = w.nodes.as_ref()?;
    let node_id = NodeId::new(local_service::NODE_ID).expect("constant node id");
    let home = local_service::home(cfg);
    let rf = roots_file(&home);
    let roots: Vec<steve_node::Root> = std::fs::read(&rf)
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default();
    let user_home = vars
        .get("HOME")
        .filter(|s| !s.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| cfg.home.clone());
    let claude_executable = vars
        .get("CLAUDE_CODE_EXECUTABLE")
        .filter(|s| !s.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/usr/bin/false"));
    let codex_home = vars
        .get("CODEX_HOME")
        .filter(|s| !s.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| user_home.join(".codex"));
    let claude_config_dir = vars
        .get("CLAUDE_CONFIG_DIR")
        .filter(|s| !s.is_empty())
        .map(PathBuf::from);
    let node_cfg = steve_node::NodeConfig {
        ceiling: steve_node::PolicyCeiling::unlimited(),
        node: node_id.clone(),
        home,
        acp: steve_acp::AcpConfig {
            declared: cfg
                .declared_adapters
                .iter()
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect(),
            adapters: adapters_from_vars(cfg, vars),
            node: "node".into(),
            claude_executable,
            claude_config_dir,
            steve_home: cfg.home.clone(),
            codex_home,
            user_home,
            version: cfg.version.clone(),
        },
        roots,
        capacity: steve_node_api::Capacity {
            sessions: steve_node_api::Capacity::SESSIONS_DEFAULT,
            jobs: u32::MAX,
            execs: 16,
        },
        storage: steve_node::Storage {
            worktree_floor: cfg.disk.floor,
            ..Default::default()
        },
    };
    let local = Arc::new(steve_node::LocalNode::open(node_cfg).expect("open test LocalNode"));
    local.follow_roots_file(rf);
    local.enable_research(steve_research::ResearchConfig::new(
        cfg.scratch.join("research"),
    ));

    let (hub_end, node_end) = steve_node_wire::testkit::pair();
    let hub_server = Arc::new(steve_node_wire::ServeHub {
        sink: svc.fleet.sink_for(&node_id),
        tools: svc.fleet.tools_for(&node_id),
        files: Some(svc.fleet.files_for(&node_id)),
        secrets: svc.secrets.get().map(|s| s.for_node(node_id.clone())),
    });
    let node_server = steve_node_wire::NodeServer::new(local.clone());
    let hub_conn = steve_node_wire::Conn::spawn(
        hub_end.sink,
        hub_end.stream,
        1,
        hub_server,
        steve_node_wire::ConnOptions {
            side: "h",
            ..Default::default()
        },
    );
    let node_conn = steve_node_wire::Conn::spawn(
        node_end.sink,
        node_end.stream,
        1,
        node_server,
        steve_node_wire::ConnOptions {
            side: "n",
            ..Default::default()
        },
    );
    let hub_link = steve_node_wire::HubLink::new();
    hub_link.set(Some(node_conn.clone()));
    local.connect(
        Arc::new(steve_node_wire::RemoteSink(hub_link.clone())),
        Some(Arc::new(steve_node_wire::RemoteFiles(hub_link.clone()))),
    );
    local.connect_secrets(Arc::new(steve_node_wire::RemoteSecrets(hub_link.clone())));
    local
        .connect_tools(Arc::new(steve_node_wire::RemoteTools(hub_link)))
        .await
        .expect("connect test local tools");

    assert!(
        svc.registry
            .install(local_service::NODE_ID, 1, hub_conn.clone())
    );
    let remote = svc.fleet.remote(&node_id);
    assert!(remote.attach(hub_conn.clone(), false));
    svc.fleet.linked(&node_id, steve_node_api::PROTOCOL);

    // Give the runtime a moment to catch up if it was started, or start it if not.
    if let Some(rt) = &w.node {
        rt.start();
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        while !rt.is_reconciled() && tokio::time::Instant::now() < deadline {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }
    let _ = w.domain.harnesses(true).await;

    Some(AttachedLocalNode {
        node: local,
        _hub_conn: hub_conn,
        _node_conn: node_conn,
    })
}
