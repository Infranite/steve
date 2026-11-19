use super::*;

install! { routes(reg, _cx) [mut module_routes] => [module_routes] {
    module_routes = module_routes.merge(steve_digest::routes());
}}

install! { start(reg, _cx) [mut jobs, modules] => [jobs, modules] {
    // Deterministic intake; the ordinary digest schedule consumes these packets through tools.
    {
        let modules = modules.clone();
        jobs.push(Job::new("digest.intake", steve_digest::job::run(modules)));
    }
}}
