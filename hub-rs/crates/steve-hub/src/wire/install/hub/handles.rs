use super::*;

install! { install(reg, cx) [auth, backend, built, core, db, engine, harnesses, jobs, local_node, local_runtime, mcp, nodes, price_url, routed, workspaces, yolo] => [] {
    let HubEnv { bus, .. } = *cx;
    let module_routes = built
        .routes
        .merge(crate::api::stats::routes(
            crate::api::stats::StatsApi::new(db.clone(), bus.clone(), price_url)
                .with_harness_source(harnesses.source()),
        ))
        .merge(crate::api::yolo::routes(engine.clone()));
    reg.wired = Some(Wired {
        backend,
        #[cfg(test)]
        core: core.clone(),
        domain: Arc::new(crate::wire::core::CoreDomain {
            core,
            routed,
            harnesses: harnesses.clone(),
            node: local_node,
            nodes: nodes.clone(),
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
        harnesses,
        lifecycle: built.lifecycle,
        #[cfg(test)]
        tool_service: reg.tool_service.take().expect("MCP tools installed"),
    });
}}
