//! Integration point: builds the storage backend, the domain, `/auth` and `/mcp` from the sibling crates.
//!
//! Wired: steve-store (storage, [`store::StoreBackend`]), steve-auth (`/auth`, [`auth::AuthStore`]),
//! steve-core on the steve-acp runtime ([`core::CoreDomain`], [`core::CoreMcp`]), and the modules
//! ([`modules::build`]: contacts, accounts + connections, vault + passkeys, schedules, knowledge,
//! GitHub), whose MCP tools [`mcp_extra`] serves next to steve-mcp's.

pub mod acp_stats;
pub mod auth;
pub mod chan;
pub mod core;
pub mod mcp;
pub mod mcp_extra;
pub mod modules;
pub mod node_binding;
pub mod node_files;
pub mod node_runtime;
pub mod node_tools;
pub mod outbox;
pub mod ports;
pub mod research;
pub mod research_mcp;
pub mod signals;
pub mod store;
pub mod tool_service;

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
    /// Remote Nodes (`STEVE_NODE_LISTEN=1`): their routes and the coordinators sessions go to.
    pub nodes: Option<Arc<crate::nodes::NodeService>>,
    /// Steve's tools as a Node reaches them (in-crate tests drive the bridge with them).
    #[cfg(test)]
    pub(crate) tool_service: Arc<tool_service::ToolService>,
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

fn core_config(cfg: &Config) -> steve_core::CoreConfig {
    let mut c = steve_core::CoreConfig::for_home(&cfg.home);
    c.port = cfg.listen.port;
    c.claude_executable = cfg.claude_executable.clone().into();
    c.codex_executable = cfg.codex_executable.clone().into();
    c.codex_model = steve_core::models::codex_config_model(&cfg.codex_home);
    c.models_fast = cfg.models.fast.clone();
    c.models_smart = cfg.models.smart.clone();
    c.default_models = cfg.default_models.clone().into_iter().collect();
    c.steve_repo = cfg.steve_repo.clone();
    c.no_resume = cfg.no_resume;
    c.debug_route = cfg.debug_route;
    c.disk = cfg.disk;
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
        idle_release: std::time::Duration::from_millis(cfg.idle_release_ms),
        retry_delays: vec![
            std::time::Duration::from_secs(20),
            std::time::Duration::from_secs(60),
        ],
    }
}

/// Where the sibling crates read their environment variables from (the process environment, or
/// a map in tests).
pub type Vars = Arc<dyn Fn(&str) -> Option<String> + Send + Sync>;

/// The opt-in local host: the coordinator, and the tools port that gets its ToolService once
/// core/modules exist.
struct NodeHost {
    runtime: Arc<node_runtime::NodeRuntime>,
    /// The Node itself, for the same-machine host bridge (where a bound workspace is).
    node: Arc<steve_node::LocalNode>,
    tools: Arc<node_tools::NodeTools>,
}

/// Construct the opt-in local host. Initialization errors fail boot rather than silently routing
/// persisted Node sessions to legacy. ToolService is installed later, once core/modules exist;
/// the returned startup job connects its endpoint before dispatch workers begin.
fn node_runtime(
    cfg: &Config,
    store: &steve_store::Store,
    domain: Arc<dyn node_runtime::NodeDomain>,
    vars: &Vars,
    jobs: &mut Vec<modules::Job>,
) -> Result<Option<NodeHost>, BackendError> {
    if !local_node_mode(vars) {
        return Ok(None);
    }
    if cfg.no_resume {
        return Err(BackendError::msg(
            "STEVE_NODE_MODE=local is disabled on STEVE_NO_RESUME copies",
        ));
    }
    let node_id = steve_node_api::NodeId::new("local").expect("constant node id");
    let home = cfg.data_dir.join("node");
    let tools = node_tools::NodeTools::open(store.clone(), node_id.clone(), &home)
        .map_err(|e| BackendError::msg(e.to_string()))?;
    let node = Arc::new(
        steve_node::LocalNode::open(steve_node::NodeConfig {
            node: node_id.clone(),
            home,
            acp: acp_config(cfg),
            roots: node_binding::roots(store, cfg).map_err(|e| BackendError::msg(e.to_string()))?,
            capacity: steve_node_api::Capacity {
                sessions: 64,
                jobs: 0,
                execs: 0,
            },
        })
        .map_err(|e| BackendError::msg(e.to_string()))?,
    );
    let runtime = node_runtime::NodeRuntime::new(store.clone(), node.clone(), domain);
    // legacy's idle kill: an idle session's harness goes after STEVE_IDLE_RELEASE_MS (10 min)
    runtime.set_idle_release(cfg.idle_release_ms as i64);
    // an attempt's deadline (default 6 h); the end-to-end tests shorten it to see a card expire
    if let Some(ms) = turn_limit(vars) {
        runtime.set_turn_limit(ms);
    }
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
    Ok(Some(NodeHost {
        runtime,
        tools,
        node,
    }))
}

/// Whether the local Node is switched on: `STEVE_NODE_MODE=local`, or `STEVE_NODE=local` (how
/// 0.2.50 spelled it, kept as a deprecated alias when STEVE_NODE_MODE is unset).
fn local_node_mode(vars: &Vars) -> bool {
    match vars("STEVE_NODE_MODE").filter(|v| !v.trim().is_empty()) {
        Some(mode) => mode.trim() == "local",
        None if vars("STEVE_NODE").as_deref() == Some("local") => {
            tracing::warn!("STEVE_NODE=local is deprecated; set STEVE_NODE_MODE=local instead");
            true
        }
        None => false,
    }
}

/// An attempt's deadline when set (`STEVE_NODE_TURN_LIMIT_MS`, for tests); default 6 h.
fn turn_limit(vars: &Vars) -> Option<i64> {
    vars("STEVE_NODE_TURN_LIMIT_MS").and_then(|v| v.trim().parse::<i64>().ok())
}

/// Remote Nodes, only with `STEVE_NODE_LISTEN=1`: the service (pairing, listener) over a fleet of
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
    let bind = vars("STEVE_NODE_BIND").filter(|v| !v.trim().is_empty());
    if !crate::nodes::enabled(|k| vars(k)) {
        if bind.is_some() {
            tracing::warn!("STEVE_NODE_BIND is ignored without STEVE_NODE_LISTEN=1");
        }
        return (None, None);
    }
    let fleet =
        crate::nodes::NodeFleet::new(store.clone(), cfg.data_dir.clone(), Some(domain), None);
    if let Some(ms) = turn_limit(vars) {
        fleet.set_turn_limit(ms);
    }
    fleet.set_idle_release(cfg.idle_release_ms as i64);
    let svc = crate::nodes::from_vars(store, fleet, |k| vars(k));
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
    tools: Arc<node_tools::NodeTools>,
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

/// Opens the database (`cfg.db_path()`, migrations) with the store's change events going to `bus`, and
/// builds the runner, the core, `/auth` and `/mcp` on it. Must run inside the tokio runtime.
pub fn wire_with(cfg: &Arc<Config>, bus: &Bus, vars: Vars) -> Result<Wired, BackendError> {
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
        user_seqs: Default::default(),
        transcripts: Default::default(),
    });
    let acp = steve_acp::Acp::new(acp_config(cfg), host.clone());
    let mut core_cfg = core_config(cfg);
    /* the research and disclosure defaults of 设置 · 模型 (as steve-research and the channels read them) */
    let list = |k: &str| -> Vec<String> {
        vars(k)
            .unwrap_or_default()
            .split(',')
            .map(|x| x.trim().to_string())
            .filter(|x| !x.is_empty())
            .collect()
    };
    if !list("STEVE_MODELS_RESEARCH").is_empty() {
        core_cfg.models_research = list("STEVE_MODELS_RESEARCH");
    }
    core_cfg.models_check = list("STEVE_MODELS_CHECK");
    let gateway = Arc::new(steve_core::stats::RecordingGateway::new(
        Arc::new(steve_core::ClaudeCli {
            one: steve_core::LocalPrint {
                codex: Some(cfg.codex_executable.clone().into()),
                executable: cfg.claude_executable.clone().into(),
                cwd: cfg.scratch.clone(),
            },
            fast: cfg.models.fast.clone(),
            smart: cfg.models.smart.clone(),
            models: Some(steve_core::models::Models::new(
                db.clone(),
                steve_core::models::Defaults::of(&core_cfg),
            )),
        }),
        db.clone(),
    ));
    let mut node_jobs = vec![];
    let node_host = node_runtime(cfg, &db, host.clone(), &vars, &mut node_jobs)?;
    if let Some(h) = &node_host {
        let _ = host.local_node.set(h.node.clone());
    }
    let (nodes, bind_remote) = remote_nodes(cfg, &db, host.clone(), &vars);
    let fleet = nodes.as_ref().map(|s| s.fleet.clone());
    let core = steve_core::Core::new(
        steve_core::repo::Db::new(db.clone()),
        Arc::new(core::RoutedRuntime {
            legacy: core::AcpRuntime(acp.clone()),
            node: node_host.as_ref().map(|h| h.runtime.clone()),
            fleet: fleet.clone(),
            bind_remote,
            store: db.clone(),
            host: host.clone(),
        }),
        gateway,
        core_cfg,
    );
    let _ = host.core.set(core.clone());
    /* inline research (thread-routing R2): read-only claude runs; each run's models come from
    设置 · 模型 (default STEVE_MODELS_RESEARCH) with the request */
    let mut research = steve_research::ResearchConfig::new(
        cfg.claude_executable.clone(),
        cfg.scratch.join("research"),
    );
    research.models = core.config().models_research.clone();
    research.codex_executable = Some(cfg.codex_executable.clone().into());
    core.set_researcher(Arc::new(research::ResearchAdapter::new(
        research,
        steve_core::repo::Db::new(db.clone()),
        cfg.data_dir.clone(),
    )));
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
    })?;
    let _ = host.flow.set(built.flow.clone());
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
    let local_node = node_host.as_ref().map(|h| h.node.clone());
    if let Some(NodeHost { tools, .. }) = node_host {
        tools
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
    // statistics: the one-time backfill and the item facts refresher run after boot (after recover
    // has closed the turns the restart cut); the routes sit with the module routes behind the login
    // (jobs already holds the module jobs and the YOLO loops)
    let stats_db = db.clone();
    let codex_home = cfg.codex_home.clone();
    jobs.push(Box::pin(async move {
        if let Err(e) = steve_stats::backfill::spawn(stats_db.clone()).await {
            tracing::error!("stats backfill task: {e}");
        }
        acp_stats::repair_codex(&stats_db, &codex_home).await;
    }));
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
        domain: Arc::new(core::CoreDomain {
            core,
            acp,
            node: local_node,
        }),
        auth: auth.router(),
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
    fn local_node_mode_keeps_the_0_2_50_spelling() {
        assert!(local_node_mode(&vars(&[("STEVE_NODE_MODE", "local")])));
        // a 0.2.50 config upgraded in place keeps its local Node
        assert!(local_node_mode(&vars(&[("STEVE_NODE", "local")])));
        // a Node.js path is not the switch, and STEVE_NODE_MODE wins when both are set
        assert!(!local_node_mode(&vars(&[(
            "STEVE_NODE",
            "/opt/x/bin/node"
        )])));
        assert!(!local_node_mode(&vars(&[
            ("STEVE_NODE", "local"),
            ("STEVE_NODE_MODE", "off")
        ])));
        assert!(!local_node_mode(&vars(&[])));
    }
}
