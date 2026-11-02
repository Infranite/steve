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
pub mod outbox;
pub mod ports;
pub mod research;
pub mod research_mcp;
pub mod signals;
pub mod store;

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
        user_seqs: Default::default(),
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
            codex: Some(cfg.codex_executable.clone().into()),
            executable: cfg.claude_executable.clone().into(),
            fast: cfg.models.fast.clone(),
            smart: cfg.models.smart.clone(),
            cwd: cfg.scratch.clone(),
            models: Some(steve_core::models::Models::new(
                db.clone(),
                steve_core::models::Defaults::of(&core_cfg),
            )),
        }),
        db.clone(),
    ));
    let core = steve_core::Core::new(
        steve_core::repo::Db::new(db.clone()),
        Arc::new(core::AcpRuntime(acp.clone())),
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
    let module_routes = built
        .routes
        .merge(crate::api::stats::routes(crate::api::stats::StatsApi::new(
            db.clone(),
            bus.clone(),
        )))
        .merge(crate::api::yolo::routes(engine.clone()));
    Ok(Wired {
        backend,
        domain: Arc::new(core::CoreDomain { core, acp }),
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
    })
}
