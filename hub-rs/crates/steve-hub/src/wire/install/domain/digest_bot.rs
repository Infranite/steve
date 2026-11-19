use super::*;

install! { routes(reg, _cx) [flow, mut module_routes] => [flow, module_routes] {
    module_routes = module_routes.merge(crate::wire::digest_bot::routes(flow.clone()));
}}

install! { jobs(reg, cx) [flow, mut jobs, modules] => [flow, jobs, modules] {
    let mut changes = cx.bus.tap(crate::wire::digest_bot::wakes);
    let wake = Arc::new(tokio::sync::Notify::new());
    let m = modules.clone();
    let f = flow.clone();
    jobs.push(Job::new("digest.bot", async move {
        tokio::join!(crate::wire::digest_bot::run(m, f, wake.clone()), async move {
            while changes.recv().await.is_some() { wake.notify_one(); }
        });
    }));
}}
