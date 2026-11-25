use super::*;

install! { install(reg, cx) [] => [plugins] {
    let Env { steve_home, vars, .. } = cx.clone();
    // the extensions' step two: their routes (merged with the modules' below) and jobs
    // plugins: the libraries, what is installed from them, one process per connected plugin
    let plugins = crate::plugins::Plugins::new(steve_home, vars(crate::plugins::LIBRARY_VAR))
        .map_err(|e| BackendError::msg(format!("plugins: {e}")))?;
    {
        let p = plugins.clone();
        crate::metrics::set_plugin_pids(move || p.running_pids());
    }
}}

install! { routes(reg, _cx) [conns, mut module_routes, plugins] => [conns, module_routes, plugins] {
    module_routes = module_routes.merge(crate::plugins::api::routes(plugins.clone(), conns.clone()));
}}

install! { jobs(reg, cx) [mut jobs, modules, plugins] => [jobs, modules, plugins] {
    let Env { bus, .. } = cx.clone();
    let initial = std::sync::Arc::new(std::sync::Mutex::new(Some(bus.tap(|event| {
        matches!(
            event.as_domain(),
            Some(steve_types::DomainEvent::Channels(
                steve_types::ChannelsEvent::Connection { .. }
            ))
        )
    }))));
    let bus = bus.clone();
    let plugins_job = plugins.clone();
    let modules_job = modules.clone();
    jobs.push(Job::supervised("plugins.supervise", move || {
        let events = initial
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take()
            .unwrap_or_else(|| {
                bus.tap(|event| {
                    matches!(
                        event.as_domain(),
                        Some(steve_types::DomainEvent::Channels(
                            steve_types::ChannelsEvent::Connection { .. }
                        ))
                    )
                })
            });
        plugins_job.clone().supervise(modules_job.clone(), events)
    }));
}}
