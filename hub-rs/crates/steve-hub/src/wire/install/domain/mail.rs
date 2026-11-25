use super::*;

install! { prepare(reg, _cx) [mut registry] => [draft_link, mail_hooks, registry] {
    // E3: mail drafts' approval; E4/E5: mail hooks and the item panel
    let draft_link = steve_mail::DraftLink::new();
    registry.decision(steve_mail::drafts::SUBJECT_TYPE, draft_link.handler());
    let mail_hooks = steve_mail::nodes::MailHooks::new();
    registry.item_hooks(mail_hooks.boxed());
    registry.item_panel(Box::new(steve_mail::items::MailPanel));
}}

install! { targets(reg, _cx) [mut registry] => [registry] {
    // E6: the `mail_account` draft target
    steve_mail::config::targets::register(&mut registry);
}}

install! { install(reg, cx) [conns, declared, draft_link, kb, mail_cfg, mail_hooks, modules] => [conns, declared, kb, mail, mail_events, modules] {
    let Env { core, backend, data_dir, .. } = cx.clone();
    // E1/E3/E4/E5: mail, before the flow (its triage actions hold it); events bound after
    let mail_events = steve_mail::flow_events::FlowEvents::new();
    let mut mail = steve_mail::Mail::builder(modules.clone(), conns.clone(), mail_cfg);
    for a in &declared.mail {
        mail = mail.adapter(a.clone());
    }
    let mail = mail
        .events(Arc::new(mail_events.clone()))
        .drafts(draft_link)
        .hooks(Arc::new(hp::MailDraftHooks(core.clone())))
        .files(Arc::new(hp::HubFiles {
            backend,
            data_dir: data_dir.to_path_buf(),
        }))
        .items(Arc::new(hp::MailItems(core.clone())))
        .knowledge(kb.clone())
        .build();
    mail_hooks.bind(&mail);
}}

install! { start(reg, _cx) [mut jobs, mail] => [jobs, mail] {
    {
        let mail = mail.clone();
        jobs.push(Job::task("mail.sync", move || mail.jobs_task()));
    }
    {
        let mail = mail.clone();
        jobs.push(Job::task("mail.send", move || mail.send_queue_task()));
    }
}}

install! { routes(reg, _cx) [mail, mut module_routes] => [mail, module_routes] {
    module_routes = module_routes.merge(steve_mail::api::routes(mail.clone()));
}}

install! { watch(reg, cx) [flow, mut jobs, tools] => [flow, jobs, tools] {
    let Env { bus, .. } = cx.clone();
    // new mailboxes get the triage template
    let initial = std::sync::Arc::new(std::sync::Mutex::new(Some(bus.tap(|event| {
        matches!(
            event.as_domain(),
            Some(steve_types::DomainEvent::Channels(
                steve_types::ChannelsEvent::Connection { .. }
            ))
        )
    }))));
    let bus = bus.clone();
    let flow_job = flow.clone();
    let mail = tools.mail.clone();
    jobs.push(Job::supervised("mail.mailboxes", move || {
        let changes = initial
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
        crate::wire::signals::watch_mailboxes(changes, flow_job.clone(), mail.clone())
    }));
}}
