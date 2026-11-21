use super::*;

install! { routes(reg, _cx) [flow, mut module_routes] => [flow, module_routes] {
    module_routes = module_routes.merge(crate::wire::digest_bot::routes(flow.clone()));
}}

install! { jobs(reg, _cx) [flow, mut jobs, modules] => [flow, jobs, modules] {
    let m = modules.clone();
    let f = flow.clone();
    jobs.push(Job::new("digest.bot", crate::wire::digest_bot::run(m, f)));
}}
