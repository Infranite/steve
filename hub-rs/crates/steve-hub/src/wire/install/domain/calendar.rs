use super::*;

install! { install(reg, _cx) [conns, declared, modules] => [calendar, conns, declared, modules] {
    // the calendar read cache: the built-in sources, and every connector that declares `calendar.v1`
    let mut calendar = steve_calendar::Calendar::builder(modules.clone(), conns.clone());
    for c in &declared.calendars {
        calendar = calendar.source(c.clone());
    }
    for c in &declared.connectors {
        calendar = calendar.source(crate::wire::calendar::ProfileSource::source(c.clone()));
    }
    let calendar = calendar.build();
}}

install! { routes(reg, _cx) [calendar, mut module_routes] => [calendar, module_routes] {
    module_routes = module_routes.merge(steve_calendar::api::routes(calendar.clone()));
}}

install! { jobs(reg, _cx) [calendar, mut jobs] => [calendar, jobs] {
    let cal = calendar.clone();
    jobs.push(Job::task("calendar.sync", move || cal.task()));
}}
