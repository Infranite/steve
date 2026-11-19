//! Integration point: builds the storage backend, the domain, `/auth` and `/mcp` from the sibling crates.
//!
//! Wired: steve-store (storage, [`store::StoreBackend`]), steve-auth (`/auth`, [`auth::AuthStore`]),
//! steve-core on the Nodes ([`core::CoreDomain`], [`core::CoreMcp`], [`core::RoutedRuntime`]), and the modules
//! ([`modules::build`]: contacts, accounts + connections, vault + passkeys, schedules, knowledge,
//! GitHub), whose MCP tools [`mcp_extra`] serves next to steve-mcp's.

pub mod acp_stats;
pub mod auth;
pub(crate) mod bot_dispatch;
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

use crate::backend::{Backend, BackendError, Domain};
use crate::bus::Bus;
use crate::config::Config;

/// Everything the binary needs from the sibling crates.
pub struct Wired {
    pub backend: Arc<dyn Backend>,
    pub domain: Arc<dyn Domain>,
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
    /// The YOLO switch (its routes are in `module_routes`).
    pub yolo: steve_core::yolo::Yolo,
    /// The YOLO executor (its event and tick loops are in `jobs`).
    pub yolo_engine: steve_core::yolo::Engine,
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

/// Process-wide SQLite settings, before the first connection: no memory statistics. With them on (the
/// default), every SQLite allocation takes one global mutex, which serializes the read pool.
pub fn tune_sqlite() {
    // SAFETY: sqlite3_config is only valid before sqlite3_initialize; this runs first thing in main
    let rc = unsafe {
        rusqlite::ffi::sqlite3_config(
            rusqlite::ffi::SQLITE_CONFIG_MEMSTATUS,
            0 as std::os::raw::c_int,
        )
    };
    if rc != rusqlite::ffi::SQLITE_OK {
        tracing::warn!("sqlite3_config(MEMSTATUS) = {rc}");
    }
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
    c.no_resume = cfg.no_resume;
    c.dynamic_workflows = cfg.dynamic_workflows;
    c.debug_route = cfg.debug_route;
    c.disk = cfg.disk;
    c.version = cfg.version.clone();
    // Each root item tree has its own collaboration budget; Node capacity is separate.
    c.max_workflow_parallel = vars("STEVE_WORKFLOW_MAX_PARALLEL")
        .and_then(|v| v.trim().parse::<usize>().ok())
        .filter(|n| *n > 0)
        .unwrap_or(4);
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
    let mode = node_mode(vars, cfg.no_resume)?;
    if mode == NodeMode::Off {
        return Ok(None);
    }
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
/// (`<data>/node-jobs`, zero session capacity) whenever sessions live in the service Node or are
/// switched off (`STEVE_NODE_MODE=off`).
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
    // legacy's idle kill: an idle session's harness goes after STEVE_IDLE_RELEASE_MS (10 min)
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
    Off,
    /// In the Hub's process: its turns stop when the Hub does.
    Local,
    /// In a process of its own on this machine (B1): a Hub restart or upgrade leaves its turns
    /// running.
    Service,
}

/// `STEVE_NODE_MODE`: `service` (the default, B1), `local`, or `off`. A `STEVE_NO_RESUME` copy
/// defaults to off and refuses `service` and `local`.
pub(crate) fn node_mode(vars: &Vars, no_resume: bool) -> Result<NodeMode, BackendError> {
    let mode = vars("STEVE_NODE_MODE")
        .map(|v| v.trim().to_ascii_lowercase())
        .filter(|v| !v.is_empty());
    match mode.as_deref() {
        None if no_resume => Ok(NodeMode::Off),
        None | Some("service") => {
            if no_resume {
                return Err(BackendError::msg(
                    "STEVE_NODE_MODE=service is disabled on STEVE_NO_RESUME copies",
                ));
            }
            Ok(NodeMode::Service)
        }
        _ => Ok(if local_node_mode(vars, no_resume)? {
            NodeMode::Local
        } else {
            NodeMode::Off
        }),
    }
}

/// Whether the Hub's own Node is switched on (in its process or as a service). It is by default;
/// with `off`, sessions already bound to the Node are parked and a new session is refused. A
/// `STEVE_NO_RESUME` copy (a DB copy that must not touch the live harnesses) resolves to off by
/// default; an explicit `local` there is a boot error. Any other value is a boot error too,
/// rather than a guess.
pub(crate) fn local_node_mode(vars: &Vars, no_resume: bool) -> Result<bool, BackendError> {
    let explicit_local = match vars("STEVE_NODE_MODE")
        .map(|v| v.trim().to_ascii_lowercase())
        .filter(|v| !v.is_empty())
    {
        Some(mode) => match mode.as_str() {
            "local" | "service" => true,
            "off" => return Ok(false),
            other => {
                return Err(BackendError::msg(format!(
                    "STEVE_NODE_MODE={other:?} is not a mode: use service (the default), local or off"
                )));
            }
        },
        // unset: on (as a service, see `node_mode`)
        None => return Ok(!no_resume),
    };
    if explicit_local && no_resume {
        return Err(BackendError::msg(
            "STEVE_NODE_MODE=local is disabled on STEVE_NO_RESUME copies",
        ));
    }
    Ok(explicit_local)
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

/// Remote Nodes, unless `STEVE_NODE_LISTEN=0` or a NO_RESUME copy: pairing and a listener over a fleet of
/// coordinators reporting to `domain`, and the Node new sessions are bound to (`STEVE_NODE_BIND`,
/// a paired Node's id; unset: none, sessions go where they went before).
fn remote_nodes(
    cfg: &Config,
    store: &steve_store::Store,
    domain: Arc<dyn node_runtime::NodeDomain>,
    vars: &Vars,
) -> (
    Option<Arc<crate::nodes::NodeService>>,
    Option<steve_node_api::NodeId>,
) {
    // A diagnostic database copy must never accept real Nodes or reconcile their stored sessions,
    // even when the copied environment explicitly enabled the listener and bound a remote Node.
    if cfg.no_resume {
        return (None, None);
    }
    let bind = vars("STEVE_NODE_BIND").filter(|v| !v.trim().is_empty());
    if !crate::nodes::enabled(|k| vars(k)) {
        if bind.is_some() {
            tracing::warn!("STEVE_NODE_BIND is ignored while the remote Node listener is disabled");
        }
        return (None, None);
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
    let bind = match bind.map(|v| steve_node_api::NodeId::new(v.trim())) {
        Some(Ok(id)) if svc.is_some() => Some(id),
        Some(Ok(_)) => None,
        Some(Err(e)) => {
            tracing::error!("STEVE_NODE_BIND: {e}; new sessions are not bound to a remote Node");
            None
        }
        None => None,
    };
    (svc, bind)
}

/// Every remote Node that has sessions bound to it gets its coordinator at boot: it reconciles
/// (once the Node links) and sends what waited, without a new message to wake it.
async fn start_fleet(fleet: Arc<crate::nodes::NodeFleet>, store: steve_store::Store) {
    let nodes = tokio::task::spawn_blocking(move || {
        store.reader(|c| {
            let mut st = c.prepare(
                "SELECT DISTINCT r.node_id FROM runtime_sessions r JOIN nodes n ON n.node_id=r.node_id",
            )?;
            let v = st
                .query_map([], |r| r.get::<_, String>(0))?
                .collect::<Result<Vec<_>, _>>()?;
            Ok::<_, steve_store::StoreError>(v)
        })
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
    bus.use_caches(cfg.caches.clone());
    let cx = install::hub::HubEnv {
        cfg,
        bus,
        vars: &vars,
        asm,
    };
    let mut registry = install::hub::HubRegistry::default();
    registry.install(&cx)?;
    Ok(registry.finish())
}

// Keep the existing Node boundary entry here; installers select it only for an inert copy.
fn in_process_model(
    cfg: &Config,
    models: Option<steve_core::models::Models>,
) -> steve_core::InProcess {
    steve_core::InProcess {
        one: steve_core::LocalAcp {
            acp: acp_config(cfg),
            cwd: cfg.scratch.clone(),
        },
        fast: cfg.models.fast.clone(),
        smart: cfg.models.smart.clone(),
        models,
    }
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
    async fn remote_listener_defaults_on_without_registering_devices_and_copies_stay_inert() {
        steve_modules::net::forbid_in_this_process();
        for (listen, copy, bind, expected) in [
            (None, false, None, true),
            (Some("0"), false, None, false),
            (Some("off"), false, None, false),
            (None, true, None, false),
            (Some("1"), true, Some("remote-test"), false),
        ] {
            let tmp = tempfile::tempdir().unwrap();
            let mut kv = std::collections::HashMap::from([
                ("STEVE_HOME".to_string(), tmp.path().display().to_string()),
                ("HOME".to_string(), tmp.path().display().to_string()),
                ("STEVE_NODE_MODE".into(), "off".into()),
                ("STEVE_VAULT_KEYSTORE".into(), "file".into()),
                ("STEVE_ADAPTER_CLAUDE".into(), "/usr/bin/false".into()),
                ("STEVE_ADAPTER_CODEX".into(), "/usr/bin/false".into()),
            ]);
            if let Some(value) = listen {
                kv.insert("STEVE_NODE_LISTEN".into(), value.into());
            }
            if let Some(value) = bind {
                kv.insert("STEVE_NODE_BIND".into(), value.into());
            }
            if copy {
                kv.insert("STEVE_NO_RESUME".into(), "1".into());
            }
            let get: Vars = Arc::new(move |k| kv.get(k).cloned());
            let cfg = Arc::new(Config::from_vars(|k| get(k)).unwrap());
            for d in [&cfg.data_dir, &cfg.worktrees, &cfg.scratch] {
                std::fs::create_dir_all(d).unwrap();
            }
            let w = wire_with(&cfg, &crate::bus::Bus::new(), get).unwrap();
            assert_eq!(
                w.nodes.is_some(),
                expected,
                "listen={listen:?}, copy={copy}"
            );
            assert!(
                w.node.is_none(),
                "local execution remains explicitly disabled"
            );
            let store =
                steve_store::Store::open(cfg.db_path(), steve_store::FnPublisher(|_| {})).unwrap();
            let registered: i64 = store
                .reader(|c| {
                    c.query_row("SELECT COUNT(*) FROM nodes", [], |r| r.get(0))
                        .map_err(steve_store::StoreError::from)
                })
                .unwrap();
            assert_eq!(registered, 0, "enabling a listener never registers a Node");
            w.domain.shutdown().await;
        }
    }

    /// Conversation model calls use ACP on the Node even on a config that predates the Node
    /// switch. Existing agent-session routing follows its own switch.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn think_streams_on_the_node_without_an_opt_in_switch() {
        steve_modules::net::forbid_in_this_process();
        for mode in [None, Some("off")] {
            let dir = tempfile::tempdir().unwrap();
            let h = dir.path().canonicalize().unwrap();
            let adapter = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("tests/fixtures/streaming-acp.js");
            let mut vars: std::collections::HashMap<&str, String> = [
                ("HOME", h.display().to_string()),
                ("STEVE_HOME", h.join("steve").display().to_string()),
                ("STEVE_HOST", "127.0.0.1".into()),
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
            // the local Node by default: STEVE_NODE_MODE unset
            if let Some(mode) = mode {
                vars.insert("STEVE_NODE_MODE", mode.into());
            }
            let get = move |k: &str| vars.get(k).cloned();
            let cfg = Arc::new(crate::config::Config::from_vars(&get).unwrap());
            for d in [&cfg.data_dir, &cfg.worktrees, &cfg.scratch] {
                std::fs::create_dir_all(d).unwrap();
            }
            let w = wire_with(&cfg, &crate::bus::Bus::new(), Arc::new(get)).unwrap();
            assert_eq!(w.node.is_some(), mode != Some("off"));
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
            let n: i64 = store
                .reader(|c| {
                    Ok::<_, steve_store::StoreError>(c.query_row(
                        "SELECT COUNT(*) FROM runtime_jobs",
                        [],
                        |r| r.get(0),
                    )?)
                })
                .unwrap();
            assert_eq!(n, 1, "{mode:?}");
            w.domain.shutdown().await;
        }
    }

    #[test]
    fn the_local_node_is_the_default() {
        let mode = |kv: &'static [(&'static str, &'static str)], no_resume: bool| {
            local_node_mode(&vars(kv), no_resume).map_err(|e| e.to_string())
        };
        // unset → local; off → off
        assert_eq!(mode(&[], false), Ok(true));
        assert_eq!(mode(&[("STEVE_NODE_MODE", "")], false), Ok(true));
        assert_eq!(mode(&[("STEVE_NODE_MODE", "off")], false), Ok(false));
        assert_eq!(mode(&[("STEVE_NODE_MODE", " Local ")], false), Ok(true));
        // a STEVE_NO_RESUME copy: off by default, an explicit local fails boot
        assert_eq!(mode(&[], true), Ok(false));
        assert_eq!(mode(&[("STEVE_NODE_MODE", "off")], true), Ok(false));
        assert!(
            mode(&[("STEVE_NODE_MODE", "local")], true)
                .unwrap_err()
                .contains("STEVE_NO_RESUME")
        );
        // a typo is not silently off (which would park every Node session) nor silently local
        assert!(
            mode(&[("STEVE_NODE_MODE", "loacl")], false)
                .unwrap_err()
                .contains("loacl")
        );
    }

    /// With `STEVE_NODE_MODE=off`, the in-process model-job Node runs on `local-jobs`
    /// (`<data>/node-jobs`), never opening `<data>/node` or reconciling/draining sessions
    /// bound to `local` (even when messages arrive in `queued` before startup reconciliation
    /// finishes, or when a detached service Node still holds `<data>/node/node.lock`).
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn steve_node_mode_off_keeps_local_sessions_parked_and_untouched() {
        steve_modules::net::forbid_in_this_process();
        let dir = tempfile::tempdir().unwrap();
        let h = dir.path().canonicalize().unwrap();
        let repo = h.join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        let fake = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/streaming-acp.js");
        let make_vars = |home_sub: &str, mode: &str| -> Vars {
            let kv: std::collections::HashMap<String, String> = [
                ("HOME", h.display().to_string()),
                ("STEVE_HOME", h.join(home_sub).display().to_string()),
                ("STEVE_HOST", "127.0.0.1".into()),
                ("STEVE_PUBLIC_URL", "http://127.0.0.1:9".into()),
                ("STEVE_DEV_LOGIN", "1".into()),
                ("STEVE_REPO", repo.display().to_string()),
                ("STEVE_NODE_LISTEN", "0".into()),
                ("STEVE_NODE_MODE", mode.into()),
                ("STEVE_VAULT_KEYSTORE", "file".into()),
                ("STEVE_FORBID_REAL_HOSTS", "1".into()),
                ("STEVE_ADAPTER_CLAUDE", fake.display().to_string()),
                ("STEVE_ADAPTER_CODEX", fake.display().to_string()),
                ("STEVE_WEB_DIR", h.join("no-web").display().to_string()),
            ]
            .into_iter()
            .map(|(k, v)| (k.into(), v))
            .collect();
            Arc::new(move |k| kv.get(k).cloned())
        };

        // 1. Persist a session `s1` already bound to `local` (as left behind before rollback).
        let off_vars = make_vars("steve", "off");
        let cfg = Arc::new(crate::config::Config::from_vars(|k| off_vars(k)).unwrap());
        for d in [&cfg.data_dir, &cfg.worktrees, &cfg.scratch] {
            std::fs::create_dir_all(d).unwrap();
        }
        {
            let store = steve_store::Store::open(cfg.db_path(), steve_store::NoPublisher).unwrap();
            store.ensure_workspace().unwrap();
            store
                .transaction::<_, steve_store::StoreError>(|tx| {
                    tx.execute(
                        "INSERT INTO items(id,workspace_id,no,title,status,stage,created,updated) \
                         VALUES ('i1','default',1,'test','run','执行',1,1)",
                        [],
                    )?;
                    tx.execute(
                        "INSERT INTO sessions(id,workspace_id,item_id,harness,cwd,status,created,updated) \
                         VALUES ('s1','default','i1','claude-code','','active',1,1)",
                        [],
                    )?;
                    Ok(())
                })
                .unwrap();
            let b = node_binding::binding_for(&store, "s1", None, &|_| true).unwrap();
            let ws_ref = serde_json::json!({ "node": "local", "id": "w_s1" });
            store
                .transaction::<_, steve_store::StoreError>(|tx| {
                    steve_store::runtime::bind_session_in(
                        tx,
                        &steve_store::runtime::SessionBinding {
                            session: &b.session,
                            node: "local",
                            workspace: &ws_ref,
                            node_session: &b.session,
                            config_revision: b.policy.revision.as_str(),
                        },
                        steve_store::now(),
                    )?;
                    tx.execute(
                        "UPDATE runtime_sessions SET phase='released' WHERE session_id='s1'",
                        [],
                    )?;
                    Ok(())
                })
                .unwrap();
        }

        // 2. Boot with STEVE_NODE_MODE=off, park a message BEFORE startup jobs reconcile (the exact
        //    Ubuntu CI window in node_e2e), then run startup jobs + a gateway think call.
        let off_vars = make_vars("steve", "off");
        let mut w2 = wire_with(&cfg, &crate::bus::Bus::new(), off_vars).unwrap();
        assert!(w2.node.is_none(), "session Node is disabled");
        let prompted = w2
            .domain
            .session_message("s1", "@wire-check", "", vec![])
            .await
            .unwrap();
        assert_eq!(prompted, "queued");
        for job in std::mem::take(&mut w2.jobs) {
            job.start(w2.domain.background_tasks().unwrap().as_ref());
        }
        let ans = w2
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
        assert_eq!(ans.text, "答案");
        // Completing `gateway().call()` proves `start_node -> runtime.start() -> reconcile()` on
        // `model_node` has already run; if `model_node` had opened `"local"`, `plan_waiting` would
        // already have popped `s1` from `queued`.
        let store = w2.core.store().store();
        let ran: i64 = store
            .reader(|c| {
                Ok::<_, steve_store::StoreError>(c.query_row(
                    "SELECT COUNT(*) FROM events WHERE session_id='s1' AND format='steve.v1' AND data LIKE '%答案%'",
                    [],
                    |r| r.get(0),
                )?)
            })
            .unwrap();
        let queued: i64 = store
            .reader(|c| {
                Ok::<_, steve_store::StoreError>(c.query_row(
                    "SELECT COUNT(*) FROM queued WHERE session_id='s1'",
                    [],
                    |r| r.get(0),
                )?)
            })
            .unwrap();
        assert_eq!(ran, 0, "no runner may execute a parked session while off");
        assert_eq!(queued, 1, "parked message stays queued until Node returns");
        assert!(
            !cfg.data_dir.join("node").exists(),
            "off never opens <data>/node"
        );
        assert!(
            cfg.data_dir.join("node-jobs").exists(),
            "model jobs run on local-jobs"
        );
        w2.domain.shutdown().await;
        drop(w2);

        // 3. Holding `<data>/node/node.lock` and `<data>/node/host-tools.lock` (as a detached
        //    service Node does) does not block `STEVE_NODE_MODE=off`.
        let svc_vars = make_vars("steve-service", "off");
        let svc_cfg = Arc::new(crate::config::Config::from_vars(|k| svc_vars(k)).unwrap());
        for d in [&svc_cfg.data_dir, &svc_cfg.worktrees, &svc_cfg.scratch] {
            std::fs::create_dir_all(d).unwrap();
        }
        std::fs::create_dir_all(svc_cfg.data_dir.join("node")).unwrap();
        let mut held_locks = Vec::new();
        for name in ["node.lock", "host-tools.lock"] {
            let f = std::fs::OpenOptions::new()
                .create(true)
                .read(true)
                .write(true)
                .open(svc_cfg.data_dir.join("node").join(name))
                .unwrap();
            assert_eq!(
                unsafe {
                    libc::flock(
                        std::os::fd::AsRawFd::as_raw_fd(&f),
                        libc::LOCK_EX | libc::LOCK_NB,
                    )
                },
                0,
                "test holds data/node/{name}"
            );
            held_locks.push(f);
        }
        let off_locked = wire_with(&svc_cfg, &crate::bus::Bus::new(), svc_vars).unwrap();
        off_locked.domain.shutdown().await;
        drop(held_locks);

        // 4. An invalid STEVE_NODE_MODE fails before touching `<data>/node` or `<data>/node-jobs`.
        let bad_vars = make_vars("steve-bad", "loacl");
        let bad_cfg = Arc::new(crate::config::Config::from_vars(|k| bad_vars(k)).unwrap());
        for d in [&bad_cfg.data_dir, &bad_cfg.worktrees, &bad_cfg.scratch] {
            std::fs::create_dir_all(d).unwrap();
        }
        let err = match wire_with(&bad_cfg, &crate::bus::Bus::new(), bad_vars) {
            Ok(_) => panic!("typo mode must fail"),
            Err(e) => e,
        };
        assert!(err.to_string().contains("loacl"), "{err}");
        assert!(
            !bad_cfg.data_dir.join("node").exists() && !bad_cfg.data_dir.join("node-jobs").exists(),
            "invalid mode must not open any Node home"
        );
    }
}

#[cfg(test)]
mod chan_tests;
#[cfg(test)]
mod workflow_tests;
