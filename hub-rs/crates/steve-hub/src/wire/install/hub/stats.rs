use super::*;

install! { install(reg, cx) [db, mut jobs] => [db, jobs, price_url] {
    let HubEnv { vars, .. } = *cx;
    // statistics: the item facts refresher runs after boot (after recover has closed the turns the
    // restart cut); the routes sit with the module routes behind the login (jobs already holds the
    // module and Node jobs)
    jobs.push(Job::new("stats.refresh", steve_stats::Refresher::new().run(db.clone())));
    let price_url = crate::price_sync::url(vars("STEVE_PRICE_CATALOG_URL").as_deref());
    jobs.push(Job::new("prices.sync", crate::price_sync::run(
        db.clone(),
        price_url.clone(),
    )));
}}

install! { routes(reg, cx) [mut built, db, harnesses, host, price_url, routed] => [built, db, harnesses, host, module_routes, routed] {
    let HubEnv { bus, cfg, .. } = *cx;
    let module_routes = std::mem::take(&mut built.routes)
        .merge(crate::api::stats::routes(
            crate::api::stats::StatsApi::new(db.clone(), bus.clone(), price_url)
                .with_harness_source(harnesses.source()),
        ))
        .merge(crate::api::approval::routes(db.clone(), routed.clone(), host.approval.clone(), cfg.diagnostic()));
}}
