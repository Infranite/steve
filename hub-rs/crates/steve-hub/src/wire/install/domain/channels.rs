use super::*;

install! { prepare(reg, _cx) [mut registry] => [registry] {
    // C8: the `space`, `persona` and `channel` draft targets; capi: the “新群” card
    steve_channels::drafts::register(&mut registry, Arc::new(steve_modules::SystemClock));
    steve_channels::space_decisions::register(&mut registry);
}}

install! { outbound(reg, _cx) [declared, mut registry] => [declared, disclosure, late, out_link, outbox, registry] {
    // C5/C6: the disclosure check (B1's outbound check), the `chan_message` approvals, the outbox
    // (sends as you through the declared adapters), back to the source when an item is done
    let disclosure = steve_channels::Disclosure::new();
    registry.outbound_check(Box::new(disclosure.clone()));
    let out_link = steve_channels::OutLink::new();
    registry.decision(steve_channels::respond::SUBJECT_MESSAGE, out_link.handler());
    let late = steve_channels::flow_actions::LateChannels::new();
    steve_channels::recovery::register(&mut registry, late.clone());
    let mut outbox = steve_channels::Outbox::new(out_link.clone());
    for a in &declared.act_as_user {
        outbox = outbox.act_as_user(a.clone());
    }
    let outbox = Arc::new(outbox);
    registry.item_hooks(Box::new(steve_channels::outbound::ReturnToSource::new(
        outbox.clone(),
        late.clone(),
    )));
}}

install! { install(reg, cx) [conns, declared, directory, flow, kstore, late, modules, outbox, ports, replies, sink, mut jobs] => [channels, conns, declared, directory, flow, modules, outbox, ports, jobs] {
    let Env { core, .. } = cx.clone();
    let mut channels = steve_channels::Channels::builder(modules.clone(), conns.clone(), sink);
    for a in &declared.channel_adapters {
        channels = channels.adapter(a.clone());
    }
    // the identities that look people up, keyed by adapter (which goes first is the names'
    // rule, not the order they come in)
    for p in &declared.profiles {
        channels = channels.profiles(p.clone());
    }
    // how the images and files of each adapter's messages are fetched
    for m in &declared.media {
        channels = channels.media(m.clone());
    }
    // how an agent reads each adapter's conversations on demand (`steve_im_read`)
    for c in &declared.connectors {
        channels = channels.connector(c.clone());
    }
    for h in &declared.history {
        channels = channels.history(h.clone());
    }
    // how each channel side's messages are told apart (provider ids, time precision, senders)
    for d in declared.decls() {
        channels = channels.decl(d.clone());
    }
    for l in &declared.links {
        channels = channels.links(l.clone());
    }
    for s in &declared.syntaxes {
        channels = channels.syntax(s.clone());
    }
    let channels = channels
        .burst_settle(std::time::Duration::from_millis(
            steve_channels::presence::BURST_SETTLE_MS,
        ))
        .party_hooks(ports.hooks.clone())
        .flow(flow.clone())
        .responder(replies)
        .other_events(Arc::new(crate::wire::signals::DirectoryEvents(directory.clone())))
        // “收到 → 完成” reactions, as the bot or (your `/bot`) as you
        .reactor(outbox.clone())
        // IM spaces converse through steve-core's thread (channels §2.12)
        .thread(Arc::new(crate::wire::chan::CoreThread(core.clone())))
        .build();
    channels.set_knowledge_store(kstore.clone());
    late.set(&channels);
    core.set_conv_host(Arc::new(crate::wire::chan::ChanConvHost(channels.downgrade())));
    {
        let (outbox, channels) = (outbox.clone(), channels.clone());
        jobs.push(Job::task("channels.send", move || outbox.clone().spawn_queue(&channels)));
    }
}}

install! { routes(reg, cx) [channels, mut module_routes, outbox] => [channels, module_routes, outbox] {
    module_routes = module_routes.merge(steve_channels::api::routes(channels.clone()));
    let backup: Arc<dyn steve_channels::PreDestructiveBackup> = Arc::new(
        crate::backup::ChannelBackup::new(cx.steve_home.to_owned(), cx.store.path().to_owned())
    );
    module_routes = module_routes.merge(steve_channels::channels_api::routes(channels.clone()).layer(axum::Extension(backup)));
    module_routes = module_routes.merge(crate::wire::chan::routes(channels.clone(), outbox.clone()));
}}

install! { notifications(reg, _cx) [channels, mut jobs, outbox] => [channels, jobs, outbox] {
    {
        let (ch, out) = (channels.clone(), outbox.clone());
        jobs.push(Job::supervised("channels.notifications", move || {
            steve_channels::notifications::run(ch.clone(), out.clone())
        }));
    }
    let (ch, out) = (channels.clone(), outbox.clone());
    jobs.push(Job::task("channels.trigger_report", move || steve_channels::trigger_report::run(ch, out)));
}}

install! { retention(reg, _cx) [mut jobs, modules] => [jobs, modules] {
    let m = modules.clone();
    jobs.push(Job::task("channels.retention", move || steve_channels::messages::retention(m)));
}}
