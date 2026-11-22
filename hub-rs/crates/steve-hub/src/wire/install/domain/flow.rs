use super::*;

install! { prepare(reg, cx) [mut registry] => [registry, setup] {
    let Env { core, preflight, .. } = cx.clone();
    let setup = steve_flow::FlowSetup::new(Arc::new(FlowCore {
        core: core.clone(),
        preflight: preflight.clone(),
    }));
    setup.register(&mut registry);
}}

install! { hooks(reg, _cx) [mut registry] => [digest, item_events, registry] {
    // C2 + S10: personas and flow nodes follow a deleted Bot
    registry.bot_deleted(Box::new(hp::BotHeirs));
    // S10: item events and the report notes, bound to the flow once it exists
    let item_events = hp::FlowItemEvents::default();
    registry.item_hooks(Box::new(item_events.clone()));
    let digest = hp::FlowDigest::default();
    registry.digest_notes(Box::new(digest.clone()));
}}

install! { install(reg, cx) [conns, declared, digest, gh, item_events, late, mail, mail_events, modules, outbox, setup, vault] => [conns, declared, flow, gh, late, mail, mail_events, modules, outbox, replies, sink, vault] {
    let Env { cmd_node, .. } = cx.clone();
    // S1–S4, S10: the flow engine with GitHub as an event source, the code nodes (http with
    // `{{secrets}}`, cmd), the `bot` node, mail actions, GitHub's `item.link`
    let mut setup = setup
        .connections(conns.clone())
        .node(Arc::new(steve_flow::nodes::http::Http::new(vault.clone())))
        .node(Arc::new({
            let cmd = steve_flow::nodes::cmd::Cmd::new();
            match cmd_node {
                Some(n) => cmd.with_node(n),
                None => cmd,
            }
        }))
        .node(Arc::new(steve_flow::nodes::bot::BotNode));
    for a in &declared.event_sources {
        setup = setup.event_adapter(a.clone());
    }
    // C4: replies (sent through C6's outbox) and the `reply_as` / `send_to_space` actions
    let replies = Arc::new(
        steve_channels::Replies::new()
            .outbound(outbox.clone())
            .composer(
                steve_channels::compose::Composer::new()
                    .media(Arc::new(steve_channels::media_agent::FeishuMediaSource)),
            ),
    );
    let setup = setup
        .action(Arc::new(steve_channels::flow_actions::ReplyAs::new(
            replies.clone(),
            late.clone(),
        )))
        .action(Arc::new(steve_channels::flow_actions::SendToSpace::new(
            replies.clone(),
            late.clone(),
        )));
    let setup = steve_mail::nodes::register(setup, &mail)
        // after mail's: `item.link` with mail links and `by:"gh_pr"`
        .action(Arc::new(steve_mail::nodes::WithMailLinks(
            steve_github::flow::GhItemLink,
        )))
        .action(Arc::new(steve_github::automation::Analyze::new(&gh)))
        .item_finder("gh_pr", Arc::new(steve_github::flow::GhPrFinder))
        // a todo's reminder and snooze wake-up, a member's morning brief (`time_alarms`)
        .alarm_source(Arc::new(steve_digest::todo_alarms::TodoAlarms))
        .alarm_source(Arc::new(steve_digest::brief::BriefAlarms));
    let flow = setup.build(modules.clone(), Arc::new(steve_flow::GraphEngine));
    mail_events.bind(&flow);
    item_events.bind(flow.clone());
    let _ = digest.0.set(flow.clone());
    let sink: Arc<dyn steve_flow::EventSink> = Arc::new(flow.clone());
}}

install! { routes(reg, _cx) [flow, mut module_routes] => [flow, module_routes] {
    module_routes = module_routes.merge(steve_flow::routes(flow.clone()));
}}

install! { clocks(reg, _cx) [flow, mut jobs] => [flow, jobs] {
    jobs.push(Job::new("flow.followups", flow.clone().followups_task()));
    jobs.push(Job::new("flow.triggers", steve_flow::scheduler::Triggers::new(flow.clone()).run()));
    jobs.push(Job::new("flow.waits", steve_flow::scheduler::Waits::new(flow.clone()).run()));
    jobs.push(Job::new("flow.retries", steve_flow::scheduler::Retries::new(flow.clone()).run()));
    jobs.push(Job::new("flow.alarms", steve_flow::AlarmClock::new(flow.clone()).run()));
}}

install! { pollers(reg, _cx) [flow, mut jobs] => [flow, jobs] {
    jobs.push(Job::new("flow.ingest", steve_flow::Poller::new(flow.clone()).run()));
    let retention = flow.clone();
    jobs.push(Job::task("flow.retention", move || steve_flow::retention::run(retention)));
}}

install! { root(reg, _cx) [flow, gh, mut root_routes] => [flow, gh, root_routes] {
    root_routes = root_routes.merge(crate::wire::signals::routes(crate::wire::signals::Hooks::new(
        flow.clone(),
        gh.clone(),
    )));
}}
