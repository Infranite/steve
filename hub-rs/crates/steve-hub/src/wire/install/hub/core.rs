use super::*;

install! { install(reg, _cx) [core_cfg, db, fleet, gateway, harnesses, host, node_host, routed, workspaces] => [core, db, fleet, harnesses, host, node_host, routed, workspaces] {
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
    /* an item bound to a Node has its git read there (N0e-E11b); without any Node, nothing changes */
    if node_host.is_some() || fleet.is_some() {
        core.set_node_git(Arc::new(item_git::ItemGit::new(workspaces.clone())));
    }
}}
