use super::*;

install! { install(reg, cx) [declared, disclosure, kb_hooks, out_link, registry, uses] => [declared, modules, uses] {
    let Env { store, core, .. } = cx.clone();
    let mut registry = registry;
    core.register_change_ops(&mut registry);
    let modules = Modules::new(store.clone(), core.module_gateway(), registry);
    // the handle systems the extensions declared, there before anything names a sender in one
    if !declared.systems.is_empty() {
        store
            .write(|w| -> steve_store::Result<()> {
                for (id, label, ext) in &declared.systems {
                    steve_store::Systems(w).upsert_declared(id, label, ext, steve_store::now())?;
                }
                Ok(())
            })
            .map_err(|e| BackendError::msg(format!("declared handle systems: {e}")))?;
    }
    kb_hooks.bind(modules.clone());
    // B1: the core calls the modules' hooks, prompt contexts and decision handlers from here on
    core.attach_modules(modules.clone());
    uses.start();
    disclosure.bind(modules.store.clone(), modules.gateway.clone());
    out_link.bind_clock(modules.clock.clone());
}}

install! { routes(reg, _cx) [module_routes, modules] => [modules, routes] {
    let routes = module_routes.with_state(modules.clone());
}}
