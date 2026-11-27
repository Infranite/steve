use super::*;

install! { install(reg, cx) [db] => [core_cfg, db, harnesses, host, mode, node_jobs, secrets] {
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
    let node_jobs = vec![];
    let secrets = node_secrets::NodeSecrets::new();
    // Validate STEVE_NODE_MODE up front before opening any listener or journal.
    // All sessions, model calls, research jobs, and host operations execute on the colocated
    // independent `steve-node --local-node` process (id `"local"`), never in the Hub process.
    let mode = node_mode(&vars, cfg.diagnostic())?;
    let reports = cx.bus.reports();
    if mode.is_some() {
        for warning in steve_i18n::with(steve_i18n::EN, || {
            crate::check::local_node_warnings(cfg, &vars)
        }) {
            tracing::warn!("{warning}");
        }
        reports.bind_model_node(local_service::NODE_ID);
    }
    let harnesses = crate::harnesses::HubHarnesses::of(cfg, reports);
}}
