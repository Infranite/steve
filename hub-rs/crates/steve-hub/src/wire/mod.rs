//! Integration point: builds the storage backend, the domain, `/auth` and `/mcp` from the sibling crates.
//!
//! Wired: steve-store (storage, [`store::StoreBackend`]), steve-auth (`/auth`, [`auth::AuthStore`]),
//! steve-core on the Nodes ([`core::CoreDomain`], [`core::CoreMcp`], [`core::RoutedRuntime`]), and the modules
//! ([`modules::build`]: contacts, accounts + connections, vault + passkeys, schedules, knowledge,
//! GitHub), whose MCP tools [`mcp_extra`] serves next to steve-mcp's.

pub mod acp_stats;
pub mod auth;
pub mod chan;
pub mod core;
pub mod diagnostics;
pub mod flow_cmd;
pub mod git_push;
pub mod host_node;
pub mod item_git;
pub mod item_mcp;
pub mod local_service;
pub mod mcp;
pub mod mcp_extra;
pub mod modules;
pub mod mq_host;
pub mod mq_node;
pub mod node_binding;
pub mod node_env;
pub mod node_files;
pub mod node_runtime;
pub mod node_secrets;
pub mod node_tools;
pub mod onboard_mcp;
pub mod outbox;
pub mod ports;
pub mod principal;
pub mod research;
pub mod research_mcp;
pub mod signals;
pub mod store;
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
    /// Where projects', items', sessions' and knowledge folders' files are (which Node).
    pub workspaces: Arc<workspaces::Workspaces>,
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
    c.debug_route = cfg.debug_route;
    c.disk = cfg.disk;
    c.version = cfg.version.clone();
    c.mq = steve_core::config::mq_from_env();
    c.commit = crate::build_identity().map(|(_, commit)| commit.to_owned());
    if let Some(n) = cfg.takeover_continues {
        c.takeover_continues = n;
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
        Some(Arc::new(node_files::NodeFiles::new(
            store.clone(),
            node_id,
            cfg.data_dir.clone(),
        ))),
    );
    jobs.push(Box::pin(start_node(
        runtime.clone(),
        node.clone(),
        tools.clone(),
    )));
    Ok(NodeHost {
        runtime,
        tools,
        node,
    })
}

/// The Hub's own Node as the Hub configures it (in its process, or in one of its own, B1): no
/// ceiling, the Hub's harnesses and roots, sessions, flow commands, and with `jobs` Steve's own
/// model work (think and research).
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
            sessions: 64,
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
        if let Ok(rt) = tokio::runtime::Handle::try_current() {
            rt.spawn(crate::nodes::tailcat::restore(t, store.clone()));
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
    asm.check().map_err(|e| BackendError::msg(e.to_string()))?;
    let db = steve_store::Store::open(cfg.db_path(), store::BusPublisher(bus.clone()))
        .map_err(|e| BackendError::msg(format!("open {}: {e}", cfg.db_path().display())))?;
    let lookup = db.clone();
    bus.set_session_workspace(move |sid| {
        lookup
            .sessions()
            .get(sid)
            .ok()
            .flatten()
            .map(|s| s.workspace_id)
    });

    let host = Arc::new(core::HubHost {
        store: db.clone(),
        data_dir: cfg.data_dir.clone(),
        core: std::sync::OnceLock::new(),
        flow: std::sync::OnceLock::new(),
        yolo: std::sync::OnceLock::new(),
        local_node: std::sync::OnceLock::new(),
    });
    let core_cfg = core_config(cfg, &vars);
    let mut node_jobs = vec![];
    let secrets = node_secrets::NodeSecrets::new();
    // Validate STEVE_NODE_MODE up front before opening any Node directory or journal.
    // Model calls always execute on an in-process Node except on an inert NO_RESUME copy;
    // only `NodeMode::Local` shares `"local"` (`<data>/node`) with agent sessions, whereas
    // `Service` and `Off` isolate jobs on `"local-jobs"` (`<data>/node-jobs`).
    let mode = node_mode(&vars, cfg.no_resume)?;
    let model_node = if cfg.no_resume {
        None
    } else {
        Some(open_in_process_node(
            cfg,
            &db,
            host.clone(),
            &vars,
            &secrets,
            &mut node_jobs,
            mode == NodeMode::Local,
        )?)
    };
    // the harness adapters this Hub knows, with what the local Node says of them (kept fresh)
    let harnesses = crate::harnesses::HubHarnesses::of(cfg);
    if let Some(h) = &model_node {
        node_jobs.push(Box::pin(
            harnesses
                .clone()
                .follow(h.node.clone() as Arc<dyn steve_node_api::Node>),
        ));
    }
    let models = || {
        let m = steve_core::models::Models::new(
            db.clone(),
            steve_core::models::Defaults::of(&core_cfg),
        );
        m.set_harnesses(harnesses.source());
        Some(m)
    };
    // One Node job per model tried; the fallback stays on the Hub. Only an inert NO_RESUME
    // copy retains the legacy gateway, without opening a copied Node journal.
    let one_model: Arc<dyn steve_core::ModelGateway> = match &model_node {
        Some(h) => {
            tracing::info!("conversation model calls stream through local Node jobs");
            Arc::new(steve_core::Fallback {
                one: node_runtime::NodePrint {
                    runtime: h.runtime.clone(),
                    data_dir: cfg.data_dir.clone(),
                },
                fast: cfg.models.fast.clone(),
                smart: cfg.models.smart.clone(),
                models: models(),
            })
        }
        _ => Arc::new(steve_core::InProcess {
            one: steve_core::LocalAcp {
                acp: acp_config(cfg),
                cwd: cfg.scratch.clone(),
            },
            fast: cfg.models.fast.clone(),
            smart: cfg.models.smart.clone(),
            models: models(),
        }),
    };
    let gateway = Arc::new(steve_core::stats::RecordingGateway::new(
        one_model,
        db.clone(),
    ));
    let (nodes, bind_remote) = remote_nodes(cfg, &db, host.clone(), &vars);
    if let Some(s) = &nodes {
        let _ = s.secrets.set(secrets.clone());
    }
    let fleet = nodes.as_ref().map(|s| s.fleet.clone());
    // where the Hub's own Node runs sessions: in this process, or in one of its own (B1)
    let node_host: Option<SessionHost> = match mode {
        NodeMode::Off => None,
        NodeMode::Local => model_node.as_ref().map(|h| SessionHost {
            runtime: h.runtime.clone(),
            node: h.node.clone(),
            tools: h.tools.clone(),
        }),
        NodeMode::Service => {
            let svc = nodes.as_ref().ok_or_else(|| {
                BackendError::msg(
                    "STEVE_NODE_MODE=service needs the Node listener (STEVE_NODE_LISTEN is off)",
                )
            })?;
            let s = local_service::host(cfg, &vars, &db, host.clone(), svc, &mut node_jobs)?;
            Some(SessionHost {
                runtime: s.runtime,
                node: s.node,
                tools: s.tools,
            })
        }
    };
    if let Some(h) = &node_host {
        let _ = host.local_node.set(h.node.clone());
    }
    let workspaces = workspaces::Workspaces::new(
        db.clone(),
        node_host.as_ref().map(|h| workspaces::LocalFace {
            node: h.node.clone(),
            runtime: h.runtime.clone(),
        }),
        fleet.clone(),
        workspaces::configured(cfg),
    );
    if node_host.is_some() {
        // the local roots follow projects and knowledge folders, with no restart
        node_jobs.push(Box::pin(
            workspaces.clone().follow(bus.tap(&["project", "kb"])),
        ));
    }
    let routed = Arc::new(core::RoutedRuntime {
        node: node_host.as_ref().map(|h| h.runtime.clone()),
        fleet: fleet.clone(),
        bind_remote,
        // `spread` is the default; `local` keeps new sessions on this machine
        spread: !vars("STEVE_NODE_PLACEMENT")
            .is_some_and(|v| v.trim().eq_ignore_ascii_case("local")),
        store: db.clone(),
        host: host.clone(),
        workspaces: workspaces.clone(),
        worktrees: node_host.as_ref().map(|_| cfg.worktrees.clone()),
    });
    let core = steve_core::Core::new(
        steve_core::repo::Db::new(db.clone()),
        routed.clone(),
        gateway,
        core_cfg,
    );
    core.models().set_harnesses(harnesses.source());
    // once Steve's harness has said what it offers, the cheap purposes get its quick model (once)
    let models = core.models().clone();
    harnesses.on_update(move || {
        let m = models.clone();
        tokio::task::spawn_blocking(move || m.migrate_all());
    });
    let _ = host.core.set(core.clone());
    /* an item bound to a Node has its git read there (N0e-E11b); without any Node, nothing changes */
    if node_host.is_some() || fleet.is_some() {
        core.set_node_git(Arc::new(item_git::ItemGit::new(workspaces.clone())));
    }
    /* the merge queue's staging is on the Hub's own Node (STEVE_MQ=1 turns the queue on) */
    let mq_nodes = node_host
        .as_ref()
        .map(|_| mq_node::MqNodes::new(workspaces.clone(), secrets.clone()));
    if let Some(n) = &mq_nodes {
        core.set_mq_node(n.clone());
    }
    /* inline research (thread-routing R2): read-only runs on the local Node; each run's harness and
    models come from 设置 · 模型 with the request (none there: STEVE_MODELS_RESEARCH, else the
    harness's own default model) */
    let mut research = steve_research::ResearchConfig::new(cfg.scratch.join("research"));
    research.models = core.config().models_research.clone();
    let mut adapter = research::ResearchAdapter::new(
        research.clone(),
        steve_core::repo::Db::new(db.clone()),
        cfg.data_dir.clone(),
    );
    // Stream every research attempt through the model Node, retaining admission, model fallback,
    // shutdown, citations and accounting in the existing research adapter.
    if let Some(h) = &model_node {
        tracing::info!("research streams through local Node jobs");
        h.node.enable_research(research);
        // the jobs' Node follows the roots itself when the sessions' Node is another
        let ws = if mode == NodeMode::Local {
            workspaces.clone()
        } else {
            let ws = workspaces::Workspaces::new(
                db.clone(),
                Some(workspaces::LocalFace {
                    node: h.node.clone(),
                    runtime: h.runtime.clone(),
                }),
                None,
                workspaces::configured(cfg),
            );
            node_jobs.push(Box::pin(ws.clone().follow(bus.tap(&["project", "kb"]))));
            ws
        };
        adapter = adapter.on_node(research::NodeResearch {
            runtime: h.runtime.clone(),
            roots: Arc::new(move || ws.local_roots().map_err(|e| e.to_string())),
        });
    }
    core.set_researcher(Arc::new(adapter));
    let auth_cfg = steve_auth::AuthConfig::from_vars(|k| vars(k))
        .map_err(|e| BackendError::msg(format!("auth config: {e}")))?;
    let backend: Arc<dyn Backend> = Arc::new(store::StoreBackend::new(db.clone())?);
    // YOLO is off by force on a production copy (STEVE_NO_RESUME=1) and with STEVE_YOLO=off
    let yolo_forced =
        steve_core::yolo::Forced::from_env(vars("STEVE_YOLO").as_deref(), cfg.no_resume);
    let built = modules::build(modules::Env {
        store: &db,
        core: &core,
        backend: backend.clone(),
        steve_home: &cfg.home,
        data_dir: &cfg.data_dir,
        public_urls: &auth_cfg.public_urls,
        bus,
        vars: &vars,
        yolo_forced,
        openviking: cfg.openviking.clone(),
        assembly: asm,
        workspaces: node_host.as_ref().map(|_| workspaces.clone()),
        // with the local Node on, `cmd` steps run on the Node that holds the project
        cmd_node: node_host.as_ref().map(|_| {
            flow_cmd::FlowCmd::new(db.clone(), workspaces.clone())
                as Arc<dyn steve_flow::nodes::cmd::CmdNode>
        }),
        // and secret uses on the Node the session works on
        use_nodes: node_host.as_ref().map(|_| {
            node_secrets::VaultNodes::new(db.clone(), workspaces.clone(), secrets.clone())
                as Arc<dyn steve_modules::vault::UseNodes>
        }),
        // and GitHub's pushes from the Node that holds the item (N0e-E8)
        git_nodes: node_host.as_ref().map(|_| {
            git_push::GitPush::new(workspaces.clone(), secrets.clone())
                as Arc<dyn steve_github::push::GitNodes>
        }),
    })?;
    let _ = host.flow.set(built.flow.clone());
    /* the merge queue's host: pull requests and pushes go through the project's GitHub connection */
    if let Some(n) = &mq_nodes {
        let h = Arc::new(mq_host::GitHubHost::new(db.clone(), built.tools.gh.clone()));
        n.set_host(h.clone());
        core.set_mq_host(h);
    }
    let yolo = steve_core::yolo::Yolo::new(
        steve_core::repo::Db::new(db.clone()),
        yolo_forced,
        &cfg.home,
    );
    core.set_yolo(yolo.clone());
    let engine = steve_core::yolo::Engine::new(core.clone(), yolo.clone());
    engine.set_recall(built.recall.clone());
    let _ = host.yolo.set(engine.clone());
    let mut jobs = built.jobs;
    jobs.extend(node_jobs);
    jobs.push(Box::pin(
        engine.clone().run_events(bus.tap(&["decision", "yolo"])),
    ));
    jobs.push(Box::pin(engine.clone().run_ticks()));

    let auth = steve_auth::Auth::new(auth::AuthStore(db.clone()), auth_cfg)
        .map_err(|e| BackendError::msg(e.to_string()))?;
    /* single sign-on set up in the settings (A4b), unless the environment configures it */
    if !auth.oidc_enabled()
        && let Err(e) = crate::access::restore_oidc(&db, &cfg.home, &auth)
    {
        tracing::warn!("single sign-on from the settings: {e}");
    }
    let extra = mcp_extra::Extra {
        store: db.clone(),
        modules: built.modules.clone(),
        ports: steve_modules::accounts::ports(),
        kb: built.kb.clone(),
        tools: built.tools.clone(),
    };
    let tool_service = Arc::new(tool_service::ToolService {
        mcp: Arc::new(core::CoreMcp {
            store: db.clone(),
            core: core.clone(),
        }),
        extra: extra.clone(),
    });
    if let Some(f) = &fleet {
        f.set_service(tool_service.clone());
        jobs.push(Box::pin(start_fleet(f.clone(), db.clone())));
    }
    // work waiting for a device that stays away goes on elsewhere (B5)
    if let Some(svc) = &nodes {
        jobs.push(Box::pin(crate::nodes::handover::run(
            svc.clone(),
            core.clone(),
            crate::nodes::handover::minutes(|k| vars(k)),
        )));
    }
    let local_node = model_node.as_ref().map(|h| h.node.clone());
    // how each machine is doing, for the devices page (B3)
    jobs.push(Box::pin(crate::nodes::vitals::sample(
        db.clone(),
        local_node
            .clone()
            .map(|n| n as Arc<dyn steve_node_api::Node>),
        nodes.clone(),
        cfg.disk.low,
    )));
    // Health's Node rollout switch describes agent-session routing, including its rollback.
    // The model-job host still participates in domain shutdown regardless of that switch.
    let local_runtime = node_host.as_ref().map(|h| h.runtime.clone());
    if let Some(h) = &model_node {
        h.tools
            .install(tool_service.clone())
            .map_err(|e| BackendError::msg(e.to_string()))?;
    }
    if mode == NodeMode::Service
        && let Some(h) = &node_host
    {
        h.tools
            .install(tool_service.clone())
            .map_err(|e| BackendError::msg(e.to_string()))?;
    }
    let mcp = steve_mcp::router(Arc::new(core::CoreMcp {
        store: db.clone(),
        core: core.clone(),
    }))
    .layer(axum::middleware::from_fn_with_state(
        extra,
        mcp_extra::layer,
    ));
    // statistics: the item facts refresher runs after boot (after recover has closed the turns the
    // restart cut); the routes sit with the module routes behind the login (jobs already holds the
    // module jobs and the YOLO loops)
    jobs.push(Box::pin(steve_stats::Refresher::new().run(db.clone())));
    let price_url = crate::price_sync::url(vars("STEVE_PRICE_CATALOG_URL").as_deref());
    jobs.push(Box::pin(crate::price_sync::run(
        db.clone(),
        price_url.clone(),
    )));
    let module_routes = built
        .routes
        .merge(crate::api::stats::routes(crate::api::stats::StatsApi::new(
            db.clone(),
            bus.clone(),
            price_url,
        )))
        .merge(crate::api::yolo::routes(engine.clone()));
    Ok(Wired {
        backend,
        #[cfg(test)]
        core: core.clone(),
        domain: Arc::new(core::CoreDomain {
            core,
            routed,
            harnesses: harnesses.clone(),
            node: local_node,
            lifecycle: built.lifecycle.clone(),
        }),
        auth: auth.router(),
        oidc: Some(Arc::new(auth.clone())),
        mcp: Some(mcp),
        modules: Some(built.modules),
        module_routes,
        root_routes: built.root_routes,
        seams: built.seams,
        tools: built.tools,
        jobs,
        yolo,
        yolo_engine: engine,
        nodes,
        node: local_runtime,
        workspaces,
        lifecycle: built.lifecycle,
        #[cfg(test)]
        tool_service,
    })
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
                tokio::spawn(job);
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
        let mut handles = Vec::new();
        for job in std::mem::take(&mut w2.jobs) {
            handles.push(tokio::spawn(job));
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
        for handle in handles {
            handle.abort();
        }
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
