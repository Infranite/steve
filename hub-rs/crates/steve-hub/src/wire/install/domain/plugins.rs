use super::*;

install! { install(reg, cx) [] => [plugins] {
    let Env { steve_home, vars, .. } = cx.clone();
    // the extensions' step two: their routes (merged with the modules' below) and jobs
    // plugins: the libraries, what is installed from them, one process per connected plugin
    let plugins = crate::plugins::Plugins::new(steve_home, vars(crate::plugins::LIBRARY_VAR))
        .map_err(|e| BackendError::msg(format!("plugins: {e}")))?;
}}

install! { routes(reg, _cx) [conns, mut module_routes, plugins] => [conns, module_routes, plugins] {
    module_routes = module_routes.merge(crate::plugins::api::routes(plugins.clone(), conns.clone()));
}}

install! { jobs(reg, cx) [mut jobs, modules, plugins] => [jobs, modules] {
    let Env { bus, .. } = cx.clone();
    jobs.push(Box::pin(plugins.supervise(modules.clone(), bus.tap(&["connection"]))));
}}
