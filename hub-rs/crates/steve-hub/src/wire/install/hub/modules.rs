use super::*;

install! { install(reg, cx) [auth_cfg, backend, core, db, host, node_host, node_jobs, preflight, secrets, workspaces] => [auth_cfg, backend, built, core, db, host, jobs, node_host, workspaces] {
    let HubEnv { cfg, bus, vars, asm, .. } = *cx;
    let mut built = crate::wire::modules::build(crate::wire::modules::Env {
        store: &db,
        core: &core,
        backend: backend.clone(),
        steve_home: &cfg.home,
        data_dir: &cfg.data_dir,
        public_urls: &auth_cfg.public_urls,
        bus,
        vars,
        openviking: cfg.openviking.clone(),
        assembly: asm,
        workspaces: node_host.as_ref().map(|_| workspaces.clone()),
        // with the local Node on, `cmd` steps run on the Node that holds the project
        cmd_node: node_host.as_ref().map(|_| {
            flow_cmd::FlowCmd::new(db.clone(), workspaces.clone())
                as Arc<dyn steve_flow::nodes::cmd::CmdNode>
        }),
        // and secret uses on the Node the session works on
        use_nodes: node_host.as_ref().map(|_| {
            node_secrets::VaultNodes::new(db.clone(), workspaces.clone(), secrets.clone())
                as Arc<dyn steve_modules::vault::UseNodes>
        }),
        // and GitHub's pushes from the Node that holds the item (N0e-E8)
        git_nodes: node_host.as_ref().map(|_| {
            git_push::GitPush::new(workspaces.clone(), secrets.clone())
                as Arc<dyn steve_github::push::GitNodes>
        }),
        preflight: Some(preflight.clone()),
    })?;
    let mut jobs = std::mem::take(&mut built.jobs);
    jobs.extend(node_jobs);
    let _ = host.flow.set(built.flow.clone());
}}
