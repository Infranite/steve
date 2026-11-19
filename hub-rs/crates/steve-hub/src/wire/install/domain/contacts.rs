use super::*;
use steve_channels::adapters::feishu;

install! { prepare(reg, _cx) [ports, mut registry] => [ports, registry] {
    steve_modules::contacts::changes::register(&mut registry, &ports);
    steve_modules::contacts::merge::register(&mut registry, &ports);
}}

install! { install(reg, cx) [conns, mut jobs, modules] => [conns, directory, jobs, modules] {
    let Env { bus, .. } = cx.clone();
    // C1/C3: channels on the Feishu bot; F5: its address book feeds the directory sync
    let directory = steve_modules::contacts::sync::TableConnections::new().with(
        feishu::directory::ADAPTER,
        feishu::directory::NAME,
        feishu::directory::factory(conns.clone()),
    );
    let directory =
        steve_modules::contacts::sync::DirectorySync::new(modules.clone(), Arc::new(directory));
    {
        let directory = directory.clone();
        jobs.push(Job::new("contacts.sync", directory.run()));
    }
    // connected, re-verified, a permission granted: sync the address book again soon
    let events = bus.tap(|event| matches!(event.as_domain(), Some(steve_types::DomainEvent::Channels(steve_types::ChannelsEvent::Connection { .. }))));
    {
        let directory = directory.clone();
        jobs.push(Job::new("contacts.watch", directory.watch(events)));
    }
}}

install! { finish(reg, _cx) [directory, mut ports] => [ports] {
    // contacts ports filled by the other modules, before the contacts routes take them
    ports.sync = Some(directory);
    ports.spaces = Some(Arc::new(steve_channels::spaces::Spaces));
    ports
        .timelines
        .push(steve_channels::messages::MessageTimeline::arc());
    ports
        .timelines
        .push(Arc::new(steve_mail::timeline::MailTimeline));
}}

install! { routes(reg, _cx) [mut module_routes, ports] => [module_routes] {
    module_routes = module_routes.merge(steve_modules::contacts::routes(ports));
}}
