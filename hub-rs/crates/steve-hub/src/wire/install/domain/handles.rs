use super::*;

install! { install(reg, cx) [calendar, channels, flow, gh, gh_sink, mail, mail_events, outbox, uses] => [channels, flow, outbox, seams, tools] {
    let Env { core, .. } = cx.clone();
    let seams = Seams {
        flow: flow.clone(),
        github: gh_sink,
        mail: Arc::new(mail_events),
        channels: channels.clone(),
    };
    let tools = Tools {
        uses,
        gh,
        mail,
        calendar,
        flow: flow.clone(),
        core: core.clone(),
        channels: channels.clone(),
        outbox: outbox.clone(),
    };
}}

install! { finish(reg, _cx) [caps, channels, flow, jobs, kb, library, lifecycle, modules, outbox, plugins, root_routes, routes, seams, tools] => [] {
    reg.built = Some(Built {
        plugins,
        modules,
        flow,
        kb,
        routes,
        root_routes,
        seams,
        tools,
        jobs,
        recall: Arc::new(crate::wire::chan::ChanRecall {
            ch: channels.clone(),
            outbox: outbox.clone(),
        }),
        lifecycle,
        caps,
        library,
    });
}}
