use super::*;

install! { install(reg, cx) [core, db, node_host, workspaces] => [core, db, node_host, workspaces] {
    let HubEnv { cfg, .. } = *cx;
    /* inline research (thread-routing R2): read-only runs on the colocated Node; each run's harness
    and models come from 设置 · 模型 with the request (none there: STEVE_MODELS_RESEARCH, else the
    harness's own default model) */
    let mut research = crate::wire::research::policy::ResearchConfig::new();
    research.models = core.config().models_research.clone();
    let mut adapter = crate::wire::research::ResearchAdapter::new(
        research,
        steve_core::repo::Db::new(db.clone()),
        cfg.data_dir.clone(),
    );
    // Stream every research attempt through the colocated Node, reusing the single Workspaces root
    // tracker.
    if let Some(h) = &node_host {
        tracing::info!("research streams through the colocated Node");
        let ws = workspaces.clone();
        adapter = adapter.on_node(crate::wire::research::NodeResearch {
            runtime: h.runtime.clone(),
            roots: Arc::new(move || ws.local_roots().map_err(|e| e.to_string())),
        });
    }
    core.set_researcher(Arc::new(adapter));
}}
