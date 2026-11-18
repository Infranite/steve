use super::*;

install! { routes(reg, _cx) [mut module_routes] => [module_routes] {
    module_routes = module_routes.merge(steve_digest::routes());
}}

install! { start(reg, _cx) [modules] => [modules] {
    // Deterministic intake; the ordinary digest schedule consumes these packets through tools.
    drop(steve_digest::job::spawn_job(modules.clone()));
}}
