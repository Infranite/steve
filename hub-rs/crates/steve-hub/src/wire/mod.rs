//! Integration point: builds the storage backend, the domain, `/auth` and `/mcp` from the sibling crates.
//!
//! Wired: steve-store (storage, [`store::StoreBackend`]), steve-auth (`/auth`, [`auth::AuthStore`]),
//! steve-core on the Nodes ([`core::CoreDomain`], [`core::CoreMcp`], [`core::RoutedRuntime`]), and the modules
//! ([`modules::build`]: contacts, accounts + connections, vault + passkeys, schedules, knowledge,
//! GitHub), whose MCP tools [`mcp_extra`] serves next to steve-mcp's.

pub mod acp_stats;
pub(crate) mod approval;
pub mod auth;
pub(crate) mod bot_dispatch;
pub(crate) mod bot_proposals;
pub mod bot_skills;
pub mod calendar;
pub mod chan;
pub mod core;
pub mod diagnostics;
pub mod digest_bot;
pub mod flow_cmd;
pub mod git_push;
pub mod host_node;
mod install;
pub mod item_git;
pub mod item_mcp;
pub mod library_mcp;
pub mod local_service;
pub mod mcp;
pub mod mcp_extra;
pub mod modules;
pub mod node_binding;
pub mod node_env;
pub mod node_files;
pub mod node_runtime;
pub mod node_secrets;
pub mod node_tools;
pub mod onboard_mcp;
pub mod outbox;
pub mod output_files;
pub mod outside_mcp;
pub mod ports;
pub mod preflight;
pub mod principal;
pub mod research;
pub mod research_mcp;
pub mod signals;
pub mod store;
pub mod tool_families;
pub mod tool_registry;
pub mod tool_service;
pub mod view;
pub mod workspaces;

use std::sync::Arc;

use axum::Router;

use crate::bus::Bus;
use crate::config::Config;
use crate::error::BackendError;

/// Localize a persisted item-gate blocker at the Hub edge.
pub(crate) fn item_gate_reason(b: steve_store::item_gate::GateBlocker) -> String {
    match b {
        steve_store::item_gate::GateBlocker::ItemPaused => {
            steve_i18n::t!("hub.item_gate.item_paused")
        }
        steve_store::item_gate::GateBlocker::DependencyWait => {
            steve_i18n::t!("hub.item_gate.dependency_wait")
        }
        steve_store::item_gate::GateBlocker::OpenPrerequisite => {
            steve_i18n::t!("hub.item_gate.open_prerequisite")
        }
    }
}

/// Everything the binary needs from the sibling crates.
pub struct Wired {
    pub backend: store::StoreBackend,
    pub domain: Arc<core::CoreDomain>,
    /// steve-auth's `/auth` router (login, callback, logout, dev).
    pub auth: Router,
    /// Single sign-on's switch: the settings put a configuration in force through it (A4b).
    pub oidc: Option<Arc<dyn crate::access::OidcSwitch>>,
    /// steve-mcp's streamable HTTP service, mounted behind the loopback gate.
    pub mcp: Option<Router>,
    /// The new modules (contacts, accounts, vault, schedules); `None` without them.
    pub modules: Option<steve_modules::Modules>,
    /// Their routes, merged under `/api` behind `require_user` + the `Caller` layer.
    pub module_routes: Router,
    /// Module routes at the root that read the session without requiring it (`/auth/connect/callback`).
    pub root_routes: Router,
    /// Background jobs to spawn after boot (the schedule scheduler).
    pub jobs: Vec<modules::Job>,
    /// The event entry points as wired (tests).
    pub seams: modules::Seams,
    /// What the modules' MCP tools run on (tests drive them directly).
    pub tools: modules::Tools,
    /// Remote Nodes (enabled by default): their routes and the coordinators sessions go to.
    pub nodes: Option<Arc<crate::nodes::NodeService>>,
    /// The local Node's coordinator (`STEVE_NODE_MODE=local`), for `/healthz`.
    pub node: Option<Arc<node_runtime::NodeRuntime>>,
    /// Local history discovery: session Node when present, otherwise the jobs Node.
    pub history: Option<Arc<dyn steve_node_api::Discovery>>,
    /// Where projects', items', sessions' and knowledge folders' files are (which Node).
    pub workspaces: Arc<workspaces::Workspaces>,
    pub harnesses: Arc<crate::harnesses::HubHarnesses>,
    /// What runs for each connection of an adapter with a connection hook (stopped by the
    /// domain's shutdown).
    pub lifecycle: Option<steve_channels::lifecycle::ConnectionRuntime>,
    /// Steve's tools as a Node reaches them (in-crate tests drive the bridge with them).
    #[cfg(test)]
    pub(crate) tool_service: Arc<tool_service::ToolService>,
    /// The core, whose model gateway in-crate tests call.
    #[cfg(test)]
    pub(crate) core: steve_core::Core,
}

/// The core's configuration: every model list is what the installation names, none by default (a
/// call without one runs on the harness's own default model).
fn core_config(cfg: &Config, vars: &Vars) -> steve_core::CoreConfig {
    let list = |k: &str| -> Vec<String> {
        vars(k)
            .unwrap_or_default()
            .split(',')
            .map(|x| x.trim().to_string())
            .filter(|x| !x.is_empty())
            .collect()
    };
    let mut c = steve_core::CoreConfig::for_home(&cfg.home);
    c.port = cfg.listen.port;
    c.models_fast = cfg.models.fast.clone();
    c.models_smart = cfg.models.smart.clone();
    /* the research and disclosure defaults of 设置 · 模型 (as steve-research and the channels read them) */
    c.models_research = list("STEVE_MODELS_RESEARCH");
    c.models_check = list("STEVE_MODELS_CHECK");
    c.default_models = cfg.default_models.clone().into_iter().collect();
    c.steve_repo = cfg.steve_repo.clone();
    c.no_resume = cfg.diagnostic();
    c.debug_route = cfg.debug_route;
    c.disk = cfg.disk;
    c.version = cfg.version.clone();
    c.commit = crate::build_identity().map(|(_, commit)| commit.to_owned());
    if let Some(n) = cfg.takeover_continues {
        c.takeover_continues = n;
    }
    if let Some(n) = cfg.takeover_same_refusals {
        c.takeover_same_refusals = n;
    }
    if let Some(g) = &cfg.gh {
        c.gh = g.clone();
    }
    c
}

fn acp_config(cfg: &Config) -> steve_acp::AcpConfig {
    steve_acp::AcpConfig {
        declared: cfg
            .declared_adapters
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect(),
        adapters: cfg
            .adapters
            .iter()
            .map(|(k, v)| (k.clone(), v.into()))
            .collect(),
        node: cfg.node.clone().into(),
        claude_executable: cfg.claude_executable.clone().into(),
        steve_home: cfg.home.clone(),
        codex_home: cfg.codex_home.clone(),
        user_home: crate::config::home_dir(),
        version: cfg.version.clone(),
    }
}

/// Where the sibling crates read their environment variables from (the process environment, or
/// a map in tests).
pub type Vars = Arc<dyn Fn(&str) -> Option<String> + Send + Sync>;

/// The opt-in local host: the coordinator, and the tools port that gets its ToolService once
/// core/modules exist.
/// The Node the Hub's sessions run on (in this process or in one of its own).
struct SessionHost {
    runtime: Arc<node_runtime::NodeRuntime>,
    node: Arc<dyn host_node::HostNode>,
    tools: Arc<node_tools::NodeTools>,
}

struct NodeHost {
    runtime: Arc<node_runtime::NodeRuntime>,
    /// The Node itself, for the same-machine host bridge (where a bound workspace is).
    node: Arc<steve_node::LocalNode>,
    tools: Arc<node_tools::NodeTools>,
}

/// Construct the local host in unit tests (`wire::node_binding::tests`).
#[cfg(test)]
fn node_runtime(
    cfg: &Config,
    store: &steve_store::Store,
    domain: Arc<dyn node_runtime::NodeDomain>,
    vars: &Vars,
    secrets: &Arc<node_secrets::NodeSecrets>,
    jobs: &mut Vec<modules::Job>,
) -> Result<Option<NodeHost>, BackendError> {
    let Some(mode) = node_mode(vars, cfg.diagnostic())? else {
        return Ok(None);
    };
    open_in_process_node(
        cfg,
        store,
        domain,
        vars,
        secrets,
        jobs,
        mode == NodeMode::Local,
    )
    .map(Some)
}

/// Opens the Hub's in-process Node: shared with agent sessions (`"local"`, `<data>/node`) only
/// when `sessions_here` is true (`STEVE_NODE_MODE=local`), and isolated on `"local-jobs"`
/// (`<data>/node-jobs`, zero session capacity) when sessions live in the service Node.
fn open_in_process_node(
    cfg: &Config,
    store: &steve_store::Store,
    domain: Arc<dyn node_runtime::NodeDomain>,
    vars: &Vars,
    secrets: &Arc<node_secrets::NodeSecrets>,
    jobs: &mut Vec<modules::Job>,
    sessions_here: bool,
) -> Result<NodeHost, BackendError> {
    /* a log line: in en whatever language the Hub answers people in */
    for warning in steve_i18n::with(steve_i18n::EN, || {
        crate::check::local_node_warnings(cfg, vars)
    }) {
        tracing::warn!("{warning}");
    }
    let (id, home) = if sessions_here {
        ("local", cfg.data_dir.join("node"))
    } else {
        ("local-jobs", cfg.data_dir.join("node-jobs"))
    };
    let node_id = steve_node_api::NodeId::new(id).expect("constant node id");
    let tools = node_tools::NodeTools::open(store.clone(), node_id.clone(), &home)
        .map_err(|e| BackendError::msg(e.to_string()))?;
    let roots = node_binding::roots(store, &workspaces::configured(cfg))
        .map_err(|e| BackendError::msg(e.to_string()))?;
    let mut node_cfg = local_node_config(cfg, node_id.clone(), home, roots, true);
    if !sessions_here {
        node_cfg.capacity.sessions = 0;
    }
    let node = Arc::new(
        steve_node::LocalNode::open(node_cfg).map_err(|e| BackendError::msg(e.to_string()))?,
    );
    local_node_extras(&node, cfg, true);
    let runtime = node_runtime::NodeRuntime::new(store.clone(), node.clone(), domain);
    runtime.set_configured_roots(workspaces::configured(cfg));
    runtime.set_principal_state(principal_state(vars));
    // an idle session's harness goes after STEVE_IDLE_RELEASE_MS (10 min)
    runtime.set_idle_release(cfg.idle_release_ms as i64);
    runtime.set_cost_caps(cost_caps(vars));
    // an attempt's deadline (default 6 h); the end-to-end tests shorten it to see a card expire
    if let Some(ms) = turn_limit(vars) {
        runtime.set_turn_limit(ms);
    }
    // vault uses' values, for this Node's commands only (N0e-E7)
    node.connect_secrets(secrets.for_node(node_id.clone()));
    node.connect(
        Arc::new(node_runtime::Sink(Arc::downgrade(&runtime))),
        Some(Arc::new(
            node_files::NodeFiles::new(store.clone(), node_id, cfg.data_dir.clone())
                .with_reply_source(
                    crate::deliveries::Service::new(
                        store.clone(),
                        cfg.home.clone(),
                        cfg.public_url.clone(),
                    ),
                    node.clone(),
                ),
        )),
    );
    jobs.push(crate::wire::modules::Job::new(
        "node.local",
        start_node(runtime.clone(), node.clone(), tools.clone()),
    ));
    Ok(NodeHost {
        runtime,
        tools,
        node,
    })
}

/// The Hub's own Node as the Hub configures it (in its process, or in one of its own, B1): no
/// ceiling, 32 concurrent turns until its saved device setting is synchronized, the Hub's harnesses
/// and roots, sessions, flow commands, and with `jobs` Steve's own model work (think and research).
pub(crate) fn local_node_config(
    cfg: &Config,
    node: steve_node_api::NodeId,
    home: std::path::PathBuf,
    roots: Vec<steve_node::Root>,
    jobs: bool,
) -> steve_node::NodeConfig {
    steve_node::NodeConfig {
        ceiling: steve_node::PolicyCeiling::unlimited(),
        node,
        home,
        acp: acp_config(cfg),
        roots,
        capacity: steve_node_api::Capacity {
            sessions: steve_node_api::Capacity::SESSIONS_DEFAULT,
            // think and research jobs share it
            jobs: if jobs { LOCAL_JOB_CAPACITY } else { 0 },
            // flow `cmd` steps (N0e-E6)
            execs: 16,
        },
        // a 256 MiB journal at most, 512 MiB left free on the disk, new work again below 80%;
        // no new item's worktree below Steve's disk floor
        storage: steve_node::Storage {
            worktree_floor: cfg.disk.floor,
            ..steve_node::Storage::default()
        },
    }
}

/// Research (on the Node that runs jobs) and discovery: the history and harnesses the Hub would
/// read itself, its own homes and binaries.
pub(crate) fn local_node_extras(node: &steve_node::LocalNode, cfg: &Config, jobs: bool) {
    if jobs {
        node.enable_research(steve_research::ResearchConfig::new(
            cfg.scratch.join("research"),
        ));
    }
    let homes = crate::resume::Homes::of(cfg);
    node.set_discovery(steve_node::history::DiscoveryConfig {
        homes: steve_node::history::Homes {
            user_home: homes.home,
            steve_home: cfg.home.clone(),
            claude_dir: homes.claude_dir,
            codex_home: homes.codex_home,
        },
        binaries: steve_node::history::Binaries {
            claude: cfg.claude_executable.clone().into(),
            codex: cfg.codex_executable.clone().into(),
        },
    });
}

/// Where the Hub's own Node runs sessions (`STEVE_NODE_MODE`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum NodeMode {
    /// In the Hub's process: its turns stop when the Hub does.
    Local,
    /// In a process of its own on this machine (B1): a Hub restart or upgrade leaves its turns
    /// running.
    Service,
}

/// The Hub runtime decides whether this installation runs any Nodes.
pub(crate) fn node_mode(vars: &Vars, diagnostic: bool) -> Result<Option<NodeMode>, BackendError> {
    let mode = vars("STEVE_NODE_MODE")
        .map(|v| v.trim().to_ascii_lowercase())
        .filter(|v| !v.is_empty());
    let selected = match mode.as_deref() {
        None | Some("service") => NodeMode::Service,
        Some("local") => NodeMode::Local,
        Some(other) => {
            return Err(BackendError::msg(format!(
                "STEVE_NODE_MODE={other:?} is not a mode: use service (the default) or local"
            )));
        }
    };
    Ok((!diagnostic).then_some(selected))
}

/// Think jobs the local Node may run at once: no limit, as in process. A bound here would turn a
/// burst of calls into start_jobs refused as exhausted and backed off (2 s doubling) with nothing
/// waking them when a slot frees, so a call that answers in process today could time out and fall
/// back instead; a start left unknown after a refused abandon would also hold its slot until the
/// next boot.
pub const LOCAL_JOB_CAPACITY: u32 = u32::MAX;

// The stored scheduler setting and the executor's actual limiter use the same defaults/range.
const _: () = assert!(
    steve_node_api::Capacity::SESSIONS_MAX as i64 == steve_store::runtime::SESSIONS_MAX
        && steve_node_api::Capacity::SESSIONS_DEFAULT as i64
            == steve_store::runtime::SESSIONS_DEFAULT
);

/// New sessions get their principal's own harness directory by default. An explicit `off`
/// keeps the Node's shared harness state; existing sessions retain their recorded directory.
pub(crate) fn principal_state(vars: &Vars) -> bool {
    match vars("STEVE_PRINCIPAL_STATE").as_deref().map(str::trim) {
        None | Some("") | Some("on") => true,
        Some("off") => false,
        Some(_) => {
            tracing::warn!("STEVE_PRINCIPAL_STATE must be on or off; principal state disabled");
            false
        }
    }
}

/// An attempt's deadline when set (`STEVE_NODE_TURN_LIMIT_MS`, for tests); by default none in practice.
fn turn_limit(vars: &Vars) -> Option<i64> {
    vars("STEVE_NODE_TURN_LIMIT_MS").and_then(|v| v.trim().parse::<i64>().ok())
}

/// What one usage report and one attempt of a Node may add to a session's cost at most
/// (`STEVE_NODE_COST_REPORT_MAX`, `STEVE_NODE_COST_ATTEMPT_MAX`; 100 and 1000 unless set).
fn cost_caps(vars: &Vars) -> node_runtime::CostCaps {
    let d = node_runtime::CostCaps::default();
    let num = |k: &str, d: f64| {
        vars(k)
            .and_then(|v| v.trim().parse::<f64>().ok())
            .unwrap_or(d)
    };
    node_runtime::CostCaps::new(
        num("STEVE_NODE_COST_REPORT_MAX", d.report),
        num("STEVE_NODE_COST_ATTEMPT_MAX", d.attempt),
    )
}

/// Remote Nodes, unless `STEVE_NODE_LISTEN=0` or a NO_RESUME copy: pairing and a listener over a
/// fleet of coordinators reporting to `domain`.
fn remote_nodes(
    cfg: &Config,
    store: &steve_store::Store,
    domain: Arc<dyn node_runtime::NodeDomain>,
    vars: &Vars,
) -> Option<Arc<crate::nodes::NodeService>> {
    // A diagnostic database copy must never accept real Nodes or reconcile their stored sessions,
    // even when the copied environment explicitly enabled the listener.
    if cfg.diagnostic() || !crate::nodes::enabled(|k| vars(k)) {
        return None;
    }
    let fleet =
        crate::nodes::NodeFleet::new(store.clone(), cfg.data_dir.clone(), Some(domain), None);
    fleet.set_deliveries(crate::deliveries::Service::new(
        store.clone(),
        cfg.home.clone(),
        cfg.public_url.clone(),
    ));
    if let Some(ms) = turn_limit(vars) {
        fleet.set_turn_limit(ms);
    }
    fleet.set_idle_release(cfg.idle_release_ms as i64);
    fleet.set_principal_state(principal_state(vars));
    fleet.set_cost_caps(cost_caps(vars));
    let svc = crate::nodes::from_vars(store, fleet, |k| vars(k));
    if let Some(svc) = &svc {
        // a Hub that cannot keep a key still runs; its welcomes go unsigned and say so in the log
        match crate::nodes::identity_at(&cfg.home) {
            Ok(id) => {
                let _ = svc.identity.set(Arc::new(id));
            }
            Err(e) => tracing::error!("the hub's identity key: {e}; node welcomes go unsigned"),
        }
    }
    if let Some(svc) = &svc {
        let port = vars("STEVE_TAILCAT_PORT")
            .and_then(|v| v.trim().parse::<u16>().ok())
            .filter(|p| *p != 0)
            .unwrap_or(cfg.listen.port.saturating_add(2));
        let t = crate::nodes::tailcat::Tailcat::new(&cfg.home, port, svc);
        let _ = svc.tailcat.set(t.clone());
        let _ = svc.installs.set(crate::nodes::install::Installs::new(
            &cfg.home,
            &cfg.version,
            &cfg.public_url,
        ));
        let up = crate::nodes::upgrade::Upgrades::new(
            store.clone(),
            cfg.data_dir.clone(),
            Arc::new(crate::nodes::upgrade::LiveFleet {
                fleet: svc.fleet.clone(),
                registry: svc.registry.clone(),
            }),
            crate::nodes::upgrade::Timing::from_vars(|k| vars(k)),
        );
        let _ = svc.upgrades.set(up.clone());
        let rm = crate::nodes::removal::Removals::new(
            store.clone(),
            Arc::new(crate::nodes::removal::LiveFleet {
                svc: Arc::downgrade(svc),
            }),
            crate::nodes::removal::Timing::from_vars(|k| vars(k)),
        );
        let _ = svc.removals.set(rm.clone());
        if let Ok(rt) = tokio::runtime::Handle::try_current() {
            rt.spawn(crate::nodes::tailcat::restore(t, store.clone()));
            rt.spawn(up.follow());
            rt.spawn(rm.settle_open());
        }
    }
    svc
}

/// Every remote Node that has sessions bound to it gets its coordinator at boot: it reconciles
/// (once the Node links) and sends what waited, without a new message to wake it.
async fn start_fleet(fleet: Arc<crate::nodes::NodeFleet>, store: steve_store::Store) {
    let nodes = tokio::task::spawn_blocking(move || {
        store.read(|r| steve_store::node_directory::nodes_with_sessions(r))
    })
    .await;
    match nodes {
        Ok(Ok(nodes)) => {
            for n in nodes {
                if let Ok(id) = steve_node_api::NodeId::new(n) {
                    fleet.start(&id);
                }
            }
        }
        Ok(Err(e)) => tracing::error!("remote Nodes to start: {e}"),
        Err(e) => tracing::error!("remote Nodes to start: {e}"),
    }
}

/// The local Node's startup job: its tools endpoint, then the coordinator's workers. A Node
/// whose tools cannot connect runs nothing: the coordinator is stopped (its sessions refuse
/// work visibly instead of queueing commands nobody sends) and the Node shut down.
async fn start_node(
    runtime: Arc<node_runtime::NodeRuntime>,
    node: Arc<steve_node::LocalNode>,
    tools: Arc<dyn steve_node_api::HubTools>,
) {
    match node.connect_tools(tools).await {
        Ok(()) => runtime.start(),
        Err(e) => {
            tracing::error!("local Node tools failed; dispatch remains stopped: {e}");
            runtime.stop(&format!("its tools did not connect ({e})"));
            node.shutdown().await;
        }
    }
}

/// [`wire_with`] on the process environment.
pub fn wire(cfg: &Arc<Config>, bus: &Bus) -> Result<Wired, BackendError> {
    wire_with(cfg, bus, Arc::new(|k: &str| std::env::var(k).ok()))
}

/// [`wire_assembly`] with the built-in extensions.
pub fn wire_with(cfg: &Arc<Config>, bus: &Bus, vars: Vars) -> Result<Wired, BackendError> {
    wire_assembly(cfg, bus, vars, &crate::extension::Assembly::builtin())
}

/// Opens the database (`cfg.db_path()`, migrations) with the store's change events going to `bus`, and
/// builds the runtime, the core, `/auth` and `/mcp` on it, with the adapters of `asm`. Must run
/// inside the tokio runtime.
pub fn wire_assembly(
    cfg: &Arc<Config>,
    bus: &Bus,
    vars: Vars,
    asm: &crate::extension::Assembly,
) -> Result<Wired, BackendError> {
    let cx = install::hub::HubEnv {
        cfg,
        bus,
        vars: &vars,
        asm,
    };
    let mut registry = install::hub::HubRegistry::default();
    registry.install(&cx)?;
    let mut wired = registry.finish();
    // The host owns and stops metric sampling alongside its other background jobs.
    let caches = bus.caches().clone();
    let clock = wired
        .modules
        .as_ref()
        .map(|modules| modules.clock.clone())
        .unwrap_or_else(|| Arc::new(steve_runtime::SystemClock));
    wired.jobs.push(modules::Job::task("cache.gauges", move || {
        steve_runtime::jobs::every(
            "cache.gauges",
            clock,
            std::time::Duration::from_secs(15),
            steve_runtime::FirstRun::AfterPeriod,
            move || {
                caches.sample_gauges();
                std::future::ready(())
            },
        )
    }));
    Ok(wired)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vars(kv: &'static [(&'static str, &'static str)]) -> Vars {
        Arc::new(move |k| kv.iter().find(|(n, _)| *n == k).map(|(_, v)| v.to_string()))
    }

    #[test]
    fn principal_state_defaults_on_and_respects_explicit_off() {
        assert!(principal_state(&vars(&[])));
        assert!(principal_state(&vars(&[("STEVE_PRINCIPAL_STATE", "on")])));
        assert!(!principal_state(&vars(&[("STEVE_PRINCIPAL_STATE", "off")])));
        assert!(!principal_state(&vars(&[(
            "STEVE_PRINCIPAL_STATE",
            "typo"
        )])));
    }

    /// The core's own lists are fixtures: an installation that names no model has none for any
    /// purpose, research included.
    #[test]
    fn an_installation_names_no_model_unless_told() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().display().to_string();
        let cfg = |kv: &[(&str, &str)]| {
            let get = |k: &str| match k {
                "STEVE_HOME" | "HOME" => Some(home.clone()),
                _ => kv.iter().find(|(n, _)| *n == k).map(|(_, v)| v.to_string()),
            };
            Config::from_vars(get).unwrap()
        };
        let c = core_config(&cfg(&[]), &vars(&[]));
        assert!(c.models_fast.is_empty() && c.models_smart.is_empty());
        assert!(c.models_research.is_empty() && c.models_check.is_empty());
        let named = [("STEVE_MODELS_RESEARCH", "a, b,")];
        let c = core_config(&cfg(&named), &vars(&[("STEVE_MODELS_RESEARCH", "a, b,")]));
        assert_eq!(c.models_research, ["a", "b"]);
    }

    #[tokio::test]
    async fn diagnostic_copy_refuses_models_before_admission_or_node_construction() {
        use std::os::unix::fs::PermissionsExt;
        use std::time::Duration;

        let tmp = tempfile::tempdir().unwrap();
        let spawn_log = tmp.path().join("spawned");
        let adapter = tmp.path().join("adapter");
        std::fs::write(
            &adapter,
            format!("#!/bin/sh\nprintf spawned > '{}'\n", spawn_log.display()),
        )
        .unwrap();
        std::fs::set_permissions(&adapter, std::fs::Permissions::from_mode(0o700)).unwrap();
        let db_home = tmp.path().join("home");
        let get = move |k: &str| match k {
            "HOME" | "STEVE_HOME" => Some(db_home.display().to_string()),
            "STEVE_NO_RESUME" | "STEVE_NODE_LISTEN" => Some("1".into()),
            "STEVE_VAULT_KEYSTORE" => Some("file".into()),
            "STEVE_ADAPTER_CLAUDE" | "STEVE_ADAPTER_CODEX" => Some(adapter.display().to_string()),
            _ => None,
        };
        let cfg = Arc::new(Config::from_vars(&get).unwrap());
        for dir in [&cfg.data_dir, &cfg.worktrees, &cfg.scratch] {
            std::fs::create_dir_all(dir).unwrap();
        }
        let bus = crate::bus::Bus::new(Arc::new(steve_runtime::cache::Caches::new(
            Default::default(),
        )));
        let w = wire_with(&cfg, &bus, Arc::new(get)).unwrap();
        assert!(w.node.is_none() && w.nodes.is_none());
        let store = w.core.store().store();
        store.maintenance_set(true).unwrap();
        let gateway = w.core.gateway();
        let call = gateway.attempt(
            "unused",
            steve_core::Tier::Smart,
            Duration::from_secs(60),
            &steve_core::gateway::ThinkTools::None,
            Some(&|_| panic!("diagnostic execution streamed text")),
        );
        let refused = tokio::time::timeout(Duration::ZERO, call).await.unwrap();
        assert_eq!(refused.attempts, 0);
        let steve_core::CoreError::Contract(error) = refused.result.unwrap_err() else {
            panic!("diagnostic refusal must use the shared error contract");
        };
        assert_eq!(error.code, steve_types::Code::Forbidden);
        assert_eq!(error.params, steve_types::Params::default());
        assert!(
            error
                .reason
                .unwrap()
                .is(steve_types::ConfigReason::DiagnosticModelDisabled)
        );
        assert!(
            local_service::run(&cfg)
                .await
                .unwrap_err()
                .to_string()
                .contains("STEVE_NO_RESUME")
        );
        assert!(!cfg.data_dir.join("node").exists());
        assert!(!cfg.data_dir.join("node-jobs").exists());
        assert!(!spawn_log.exists());
        w.domain.shutdown().await;
    }

    #[tokio::test]
    async fn remote_listener_defaults_on_without_registering_devices_and_copies_stay_inert() {
        steve_modules::net::forbid_in_this_process();
        for (listen, copy, expected) in [
            (None, false, true),
            (Some("0"), false, false),
            (Some("off"), false, false),
            (None, true, false),
            (Some("1"), true, false),
        ] {
            let tmp = tempfile::tempdir().unwrap();
            let mut kv = std::collections::HashMap::from([
                ("STEVE_HOME".to_string(), tmp.path().display().to_string()),
                ("HOME".to_string(), tmp.path().display().to_string()),
                ("STEVE_VAULT_KEYSTORE".into(), "file".into()),
                ("STEVE_ADAPTER_CLAUDE".into(), "/usr/bin/false".into()),
                ("STEVE_ADAPTER_CODEX".into(), "/usr/bin/false".into()),
            ]);
            if let Some(value) = listen {
                kv.insert("STEVE_NODE_LISTEN".into(), value.into());
            }
            if copy {
                kv.insert("STEVE_NO_RESUME".into(), "1".into());
            } else {
                kv.insert("STEVE_NODE_MODE".into(), "local".into());
            }
            let get: Vars = Arc::new(move |k| kv.get(k).cloned());
            let cfg = Arc::new(Config::from_vars(|k| get(k)).unwrap());
            for d in [&cfg.data_dir, &cfg.worktrees, &cfg.scratch] {
                std::fs::create_dir_all(d).unwrap();
            }
            let w = wire_with(
                &cfg,
                &crate::bus::Bus::new(std::sync::Arc::new(steve_runtime::cache::Caches::new(
                    Default::default(),
                ))),
                get,
            )
            .unwrap();
            assert_eq!(
                w.nodes.is_some(),
                expected,
                "listen={listen:?}, copy={copy}"
            );
            assert_eq!(w.node.is_none(), copy);
            let store =
                steve_store::Store::open(cfg.db_path(), steve_store::FnPublisher(|_| {})).unwrap();
            let registered = steve_store::testkit::hub::hub_fixtures::node_count(&store).unwrap();
            assert_eq!(registered, 0, "enabling a listener never registers a Node");
            w.domain.shutdown().await;
        }
    }

    /// The default Hub runs conversation model calls through the Node job runtime.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn think_streams_on_the_node_without_an_opt_in_switch() {
        steve_modules::net::forbid_in_this_process();
        let dir = tempfile::tempdir().unwrap();
        let h = dir.path().canonicalize().unwrap();
        let adapter = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/streaming-acp.js");
        let service_port = std::net::TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port();
        let vars: std::collections::HashMap<&str, String> = [
            ("HOME", h.display().to_string()),
            ("STEVE_HOME", h.join("steve").display().to_string()),
            ("STEVE_HOST", "127.0.0.1".into()),
            ("STEVE_LOCAL_NODE_PORT", service_port.to_string()),
            ("STEVE_PUBLIC_URL", "http://127.0.0.1:9".into()),
            ("STEVE_DEV_LOGIN", "1".into()),
            ("STEVE_REPO", "/nonexistent".into()),
            ("STEVE_VAULT_KEYSTORE", "file".into()),
            ("STEVE_FORBID_REAL_HOSTS", "1".into()),
            ("STEVE_ADAPTER_CLAUDE", adapter.display().to_string()),
            ("STEVE_ADAPTER_CODEX", adapter.display().to_string()),
            ("STEVE_WEB_DIR", h.join("no-web").display().to_string()),
        ]
        .into_iter()
        .collect();
        let get = move |k: &str| vars.get(k).cloned();
        let cfg = Arc::new(crate::config::Config::from_vars(&get).unwrap());
        for d in [&cfg.data_dir, &cfg.worktrees, &cfg.scratch] {
            std::fs::create_dir_all(d).unwrap();
        }
        let w = wire_with(
            &cfg,
            &crate::bus::Bus::new(std::sync::Arc::new(steve_runtime::cache::Caches::new(
                Default::default(),
            ))),
            Arc::new(get),
        )
        .unwrap();
        assert!(w.node.is_some());
        for job in w.jobs {
            job.start(w.domain.background_tasks().unwrap().as_ref());
        }
        let p = w
            .core
            .gateway()
            .call(
                "@wire-check",
                steve_core::Tier::Smart,
                std::time::Duration::from_secs(20),
                &steve_core::gateway::ThinkTools::None,
            )
            .await
            .unwrap();
        assert_eq!(p.text, "答案");
        let store =
            steve_store::Store::open(cfg.db_path(), steve_store::FnPublisher(|_| {})).unwrap();
        let n = steve_store::testkit::hub::hub_fixtures::job_count(&store).unwrap();
        assert_eq!(n, 1);
        w.domain.shutdown().await;
    }

    #[test]
    fn node_mode_defaults_to_service_and_diagnostic_copies_disable_it() {
        let mode = |value: Option<&'static str>, copy| {
            let get: Vars = Arc::new(move |_| value.map(String::from));
            node_mode(&get, copy)
        };
        assert_eq!(mode(None, false).unwrap(), Some(NodeMode::Service));
        assert_eq!(mode(Some(""), false).unwrap(), Some(NodeMode::Service));
        assert_eq!(mode(Some(" Local "), false).unwrap(), Some(NodeMode::Local));
        assert!(mode(Some("off"), false).is_err());
        assert_eq!(mode(None, true).unwrap(), None);
        for explicit in ["local", "service"] {
            assert_eq!(mode(Some(explicit), true).unwrap(), None);
        }
        assert!(
            mode(Some("loacl"), false)
                .unwrap_err()
                .to_string()
                .contains("loacl")
        );
    }
}

#[cfg(test)]
mod chan_tests;
#[cfg(test)]
mod decisions_acceptance;
