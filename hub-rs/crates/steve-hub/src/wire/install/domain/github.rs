use super::*;

install! { prepare(reg, _cx) [mut registry] => [gh_slot, registry] {
    // G3/G4/G5: PR open/merge and comment decisions, the wrap-up hook, the GitHub panels, the
    // `gh_repo` draft target
    let gh_slot = steve_github::accept::register(&mut registry);
}}

install! { install(reg, cx) [conns, gh_env, gh_slot] => [conns, gh] {
    let Env { core, git_nodes, .. } = cx.clone();
    let gh = steve_github::GitHub::new(conns.clone(), gh_env, cx.bus.caches().clone());
    gh_slot.bind(&gh);
    if let Some(n) = git_nodes {
        gh.set_git_nodes(n);
    }
    gh.set_core(Arc::new(hp::GhItems(core.clone())));
    gh.set_session_sink(Arc::new(hp::CoreSessions(core.clone())));
}}

install! { start(reg, _cx) [flow, gh, mut jobs, sink] => [flow, gh, gh_sink, jobs, sink] {
    // G2/G3: GitHub signals through S1, the “PR 检查失败” template, `{type:"gh"}` notices
    let gh_sink: Arc<dyn steve_github::events::EventSink> =
        Arc::new(steve_github::flow::FlowSink(sink.clone()));
    gh.set_event_sink(gh_sink.clone());
    gh.set_flow(flow.clone());
    {
        let gh = gh.clone();
        jobs.push(Job::new("github.poll", gh.run_jobs()));
    }
    // approved GitHub writes: running ones a restart cut off become unknown first
    {
        let gh = gh.clone();
        jobs.push(Job::new("github.effects", gh.run_effects_job()));
    }
}}

install! { routes(reg, _cx) [gh, mut module_routes] => [gh, module_routes] {
    module_routes = module_routes.merge(steve_github::manifest::routes(gh.clone()));
    module_routes = module_routes.merge(steve_github::repos::routes(gh.clone()));
    module_routes = module_routes.merge(steve_github::comment::routes(gh.clone()));
    module_routes = module_routes.merge(steve_github::issues::routes(gh.clone()));
}}

install! { root(reg, _cx) [gh, mut root_routes] => [gh, root_routes] {
    root_routes = root_routes.merge(steve_github::hooks::routes(gh.clone()));
}}
