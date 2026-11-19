use super::*;

install! { install(reg, _cx) [auth, backend, built, core, engine, harnesses, mut jobs, local_node, local_runtime, mcp, module_routes, nodes, routed, workspaces, yolo] => [] {
    let history = local_runtime.as_ref()
        .map(|r| r.node() as Arc<dyn steve_node_api::Discovery>)
        .or_else(|| local_node.clone().map(|n| n as Arc<dyn steve_node_api::Discovery>));
    let tasks = Arc::new(crate::jobs::Tasks::default());
    for job in &mut jobs { job.bind(&tasks); }
    reg.wired = Some(Wired {
        history,
        backend,
        #[cfg(test)]
        core: core.clone(),
        domain: Arc::new(crate::wire::core::CoreDomain {
            core,
            jobs: tasks,
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
