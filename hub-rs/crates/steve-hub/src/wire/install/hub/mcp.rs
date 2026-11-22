use super::*;

install! { install(reg, cx) [built, core, db, fleet, mut jobs, mode, model_node, node_host, nodes, workspaces] => [built, core, db, jobs, local_node, local_runtime, mcp, nodes, tool_service, workspaces] {
    let HubEnv { cfg, vars, .. } = *cx;
    let extra = mcp_extra::Extra {
        cfg: cfg.clone(),
        caps: built.caps.clone(),
        store: db.clone(),
        modules: built.modules.clone(),
        ports: steve_modules::accounts::ports(),
        kb: built.kb.clone(),
        tools: built.tools.clone(),
        library: built.library.clone(),
    };
    // Periodic cleanup is an every callback, so each pass has its own run/duration metrics.
    {
        let library = built.library.clone();
        let clock = built.modules.clock.clone();
        jobs.push(Job::task("library.sweep", move || {
            steve_runtime::every("library.sweep", clock, std::time::Duration::from_secs(60), steve_runtime::FirstRun::Immediate, move || {
                let library = library.clone();
                async move { let _ = tokio::task::spawn_blocking(move || library.sweep()).await; }
            })
        }));
    }
    let tool_service = Arc::new(tool_service::ToolService {
        mcp: Arc::new(crate::wire::core::CoreMcp {
            public_url: cfg.public_url.clone(),
            store: db.clone(),
            core: core.clone(),
            home: cfg.home.clone(),
            workspaces: workspaces.clone(),
        }),
        extra: extra.clone(),
    });
    if let Some(f) = &fleet {
        f.set_service(tool_service.clone());
        jobs.push(Job::new("nodes.fleet", start_fleet(f.clone(), db.clone())));
    }
    // work waiting for a device that stays away goes on elsewhere (B5)
    if let Some(svc) = &nodes {
        jobs.push(Job::new("nodes.handover", crate::nodes::handover::run(
            svc.clone(),
            core.clone(),
            crate::nodes::handover::minutes(|k| vars(k)),
        )));
    }
    let local_node = model_node.as_ref().map(|h| h.node.clone());
    // how each machine is doing, for the devices page (B3)
    jobs.push(Job::new("nodes.vitals", crate::nodes::vitals::sample(
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
    if mode == Some(NodeMode::Service)
        && let Some(h) = &node_host
    {
        h.tools
            .install(tool_service.clone())
            .map_err(|e| BackendError::msg(e.to_string()))?;
    }
    let mcp = steve_mcp::router(Arc::new(crate::wire::core::CoreMcp {
        public_url: cfg.public_url.clone(),
        store: db.clone(),
        core: core.clone(),
        home: cfg.home.clone(),
        workspaces: workspaces.clone(),
    }))
    .layer(axum::middleware::from_fn_with_state(
        tool_service.clone(),
        mcp_extra::layer,
    ));
}}
