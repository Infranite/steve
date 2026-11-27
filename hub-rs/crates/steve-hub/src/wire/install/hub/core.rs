use super::*;

struct HubHousekeeping(Arc<workspaces::Workspaces>);

#[async_trait::async_trait]
impl steve_core::workspace::NodeHousekeeping for HubHousekeeping {
    async fn disk_inspect(
        &self,
        req: steve_node_api::DiskInspectRequest,
    ) -> steve_node_api::ApiResult<steve_node_api::DiskInspectResponse> {
        let node = self.0.local_node().ok_or_else(|| {
            steve_node_api::Error::new(
                steve_node_api::Code::Unavailable,
                "local Node is not attached",
            )
        })?;
        node.disk_inspect(req).await
    }

    async fn disk_reclaim(
        &self,
        req: steve_node_api::DiskReclaimRequest,
    ) -> steve_node_api::ApiResult<steve_node_api::DiskReclaimResponse> {
        let node = self.0.local_node().ok_or_else(|| {
            steve_node_api::Error::new(
                steve_node_api::Code::Unavailable,
                "local Node is not attached",
            )
        })?;
        node.disk_reclaim(req).await
    }
}

install! { install(reg, cx) [core_cfg, db, fleet, gateway, harnesses, host, node_host, routed, workspaces] => [core, db, fleet, harnesses, host, node_host, routed, workspaces] {
    let core = steve_core::Core::new(
        steve_core::repo::Db::new(db.clone()),
        routed.clone(),
        gateway,
        core_cfg,
    );
    core.models().set_harnesses(harnesses.source());
    core.models().set_bot_harnesses(harnesses.bot_source(db.clone()));
    // once Steve's harness has said what it offers, untouched seeded bots reconcile if needed and
    // the cheap purposes get its quick model (once)
    let models = core.models().clone();
    let core_for_probe = core.clone();
    harnesses.on_update(move || {
        let m = models.clone();
        let c = core_for_probe.clone();
        tokio::task::spawn_blocking(move || {
            let _ = c.reconcile_seeded_bots();
            m.migrate_all();
        });
    });
    let _ = host.core.set(core.clone());
    match crate::workflow_step::StepRunner::current_exe_confined(
        &cx.cfg.home,
        &cx.cfg.data_dir.join("node"),
    ) {
        Ok(runner) => core.set_workflow_executor(Arc::new(runner)),
        Err(e) => tracing::warn!("workflow step subprocess runner unavailable: {e}"),
    }
    /* an item bound to a Node has its git read there; disk housekeeping runs on the colocated Node */
    if node_host.is_some() || fleet.is_some() {
        core.set_node_git(Arc::new(item_git::ItemGit::new(workspaces.clone())));
    }
    if node_host.is_some() {
        core.set_housekeeping(Arc::new(HubHousekeeping(workspaces.clone())));
    }
}}
