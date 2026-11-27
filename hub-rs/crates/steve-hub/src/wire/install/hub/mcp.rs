use super::*;

install! { install(reg, cx) [built, core, db, fleet, harnesses, mut jobs, node_host, nodes, workspaces] => [built, core, db, harnesses, jobs, local_runtime, mcp, nodes, tool_service, workspaces] {
    let HubEnv { cfg, vars, .. } = *cx;
    let local_node = node_host.as_ref().map(|h| h.node.node());
    let extra = mcp_extra::Extra {
        cfg: cfg.clone(),
        caps: built.caps.clone(),
        store: db.clone(),
        modules: built.modules.clone(),
        ports: steve_modules::accounts::ports(),
        kb: built.kb.clone(),
        tools: built.tools.clone(),
        library: built.library.clone(),
        dispatch_facts: Some(research_mcp::DispatchFacts {
            harnesses: harnesses.clone(),
            nodes: nodes.clone(),
            local_node: local_node
                .clone()
                .map(|n| n as Arc<dyn steve_node_api::Discovery>),
        }),
    };
    // Periodic cleanup is an every callback, so each pass has its own run/duration metrics.
    {
        let library = built.library.clone();
        let clock = built.modules.clock.clone();
        jobs.push(Job::task("library.sweep", move || {
            steve_runtime::every("library.sweep", clock, std::time::Duration::from_secs(60), steve_runtime::FirstRun::Immediate, move || {
                let library = library.clone();
                async move { library.sweep().await; }
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
        let svc = svc.clone();
        let core = core.clone();
        let mins = crate::nodes::handover::minutes(|k| vars(k));
        jobs.push(Job::task("nodes.handover", move || {
            crate::nodes::handover::task(svc, core, mins)
        }));
    }
    // how each machine is doing, for the devices page (B3)
    {
        let db = db.clone();
        let local_for_vitals = local_node.clone();
        let nodes = nodes.clone();
        let low = cfg.disk.low;
        jobs.push(Job::task("nodes.vitals", move || {
            crate::nodes::vitals::task(db, local_for_vitals, nodes, low)
        }));
    }
    {
        let db = db.clone();
        let local_for_upgrades = local_node;
        let nodes = nodes.clone();
        let bus = cx.bus.clone();
        jobs.push(Job::task("nodes.harness_auto_upgrade", move || {
            crate::api::harness_updates::auto_upgrade_task(db, local_for_upgrades, nodes, bus)
        }));
    }
    let local_runtime = node_host.as_ref().map(|h| h.runtime.clone());
    if let Some(h) = &node_host {
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
