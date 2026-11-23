use super::*;

install! { install(reg, cx) [db] => [core_cfg, db, harnesses, host, mode, model_node, node_jobs, secrets] {
    let HubEnv { cfg, vars, .. } = *cx;
    let host = Arc::new(crate::wire::core::HubHost {
        store: db.clone(),
        data_dir: cfg.data_dir.clone(),
        core: std::sync::OnceLock::new(),
        flow: std::sync::OnceLock::new(),
        approval: crate::wire::approval::ApprovalModes(Arc::new(cfg.declared_adapters.clone())),
        local_node: std::sync::OnceLock::new(),
    });
    let core_cfg = core_config(cfg, &vars);
    let mut node_jobs = vec![];
    let secrets = node_secrets::NodeSecrets::new();
    // Validate STEVE_NODE_MODE up front before opening any Node directory or journal.
    // Model calls always execute on an in-process Node except on an inert NO_RESUME copy;
    // only `NodeMode::Local` shares `"local"` (`<data>/node`) with agent sessions, whereas
    // `Service` isolates jobs on `"local-jobs"` (`<data>/node-jobs`).
    let mode = node_mode(&vars, cfg.diagnostic())?;
    let model_node = mode
        .map(|mode| open_in_process_node(
            cfg,
            &db,
            host.clone(),
            &vars,
            &secrets,
            &mut node_jobs,
            mode == NodeMode::Local,
        ))
        .transpose()?;
    // the harness adapters this Hub knows, with what the Nodes say of them (kept fresh)
    let reports = cx.bus.reports();
    if let Some(host) = &model_node {
        reports.bind_model_node(steve_node_api::Node::id(host.node.as_ref()).as_str());
    }
    let harnesses = crate::harnesses::HubHarnesses::of(cfg, reports);
}}
