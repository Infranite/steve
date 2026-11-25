use super::*;

install! { prepare(reg, cx) [ports] => [declared, ports] {
    let Env { vars, assembly, .. } = cx.clone();
    // Feishu: the test base and host guard (STEVE_FEISHU_API_BASE, STEVE_FORBID_REAL_HOSTS),
    // whichever extensions are assembled (the directory sync below talks to Feishu too)
    use steve_channels::adapters::feishu;
    feishu::FeishuEnv::from_env(|k| vars(k)).install();
    // the extensions' adapters (built in: Feishu as the bot and as you, IMAP, GitHub); nothing
    // here talks to a provider
    let declared = assembly
        .declare(vars, ports.hooks.clone())
        .map_err(|e| BackendError::msg(e.to_string()))?;
}}

// Capture the extension context before the plugin and route constructors, as at the old root.
install! { context(reg, cx) [channels] => [channels, started] {
    let Env { bus, vars, .. } = cx.clone();
    let started = crate::extension::Started::new(&channels, bus, vars);
}}

install! { mount(reg, cx) [channels, declared, jobs, module_routes, started] => [channels, ext_jobs, jobs, module_routes, pending_lifecycle] {
    let Env { bus, assembly, .. } = cx.clone();
    let crate::extension::StartedAll {
        routes: module_routes,
        jobs: ext_jobs,
        hooks,
    } = assembly
        .start(&started, &declared, module_routes)
        .map_err(|e| BackendError::msg(e.to_string()))?;
    // what runs for each connection of an adapter with a hook: the events are tapped now, before
    // the runtime lists the connections
    let pending_lifecycle = (!hooks.is_empty()).then(|| {
        let mut rt = steve_channels::lifecycle::ConnectionRuntime::builder(channels.clone());
        for h in hooks {
            rt = rt.hook(&h.adapter, h.decl, h.hook);
        }
        (rt.build(), bus.tap(|event| matches!(event.as_domain(), Some(steve_types::DomainEvent::Channels(steve_types::ChannelsEvent::Connection { .. })))))
    });
}}

install! { start(reg, cx) [ext_jobs, mut jobs, pending_lifecycle] => [jobs, lifecycle] {
    let Env { bus, .. } = cx.clone();
    // the extensions' (built in: Feishu app registrations resumed) and the connection runtime
    // (built in: the Feishu long connections and self pollers)
    jobs.extend(ext_jobs);
    let lifecycle = pending_lifecycle.map(|(rt, events)| {
        let run = rt.clone();
        let bus = bus.clone();
        let initial = std::sync::Arc::new(std::sync::Mutex::new(Some(events)));
        jobs.push(Job::supervised("channels.lifecycle", move || {
            let run = run.clone();
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
            async move {
                run.run(events).await;
            }
        }));
        rt
    });
}}
