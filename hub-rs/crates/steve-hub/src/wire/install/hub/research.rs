use super::*;

install! { install(reg, cx) [core, db, mode, model_node, mut node_jobs, workspaces] => [core, db, mode, model_node, node_jobs, workspaces] {
    let HubEnv { cfg, bus, .. } = *cx;
    /* inline research (thread-routing R2): read-only runs on the local Node; each run's harness and
    models come from 设置 · 模型 with the request (none there: STEVE_MODELS_RESEARCH, else the
    harness's own default model) */
    let mut research = steve_research::ResearchConfig::new(cfg.scratch.join("research"));
    research.models = core.config().models_research.clone();
    let mut adapter = crate::wire::research::ResearchAdapter::new(
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
        let ws = if mode == Some(NodeMode::Local) {
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
            let initial = std::sync::Arc::new(std::sync::Mutex::new(Some(bus.tap(|event| {
                matches!(
                    event.as_domain(),
                    Some(
                        steve_types::DomainEvent::Config(steve_types::ConfigEvent::Project { .. })
                            | steve_types::DomainEvent::Knowledge(_)
                    )
                )
            }))));
            let bus = bus.clone();
            let ws_job = ws.clone();
            node_jobs.push(Job::supervised("research.roots", move || {
                let events = initial
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .take()
                    .unwrap_or_else(|| {
                        bus.tap(|event| {
                            matches!(
                                event.as_domain(),
                                Some(
                                    steve_types::DomainEvent::Config(
                                        steve_types::ConfigEvent::Project { .. }
                                    ) | steve_types::DomainEvent::Knowledge(_)
                                )
                            )
                        })
                    });
                ws_job.clone().follow("research.roots", events)
            }));
            ws
        };
        adapter = adapter.on_node(crate::wire::research::NodeResearch {
            runtime: h.runtime.clone(),
            roots: Arc::new(move || ws.local_roots().map_err(|e| e.to_string())),
        });
    }
    core.set_researcher(Arc::new(adapter));
}}
