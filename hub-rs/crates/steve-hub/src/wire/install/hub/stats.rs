use super::*;

install! { install(reg, cx) [db, mut jobs] => [db, jobs, price_url] {
    let HubEnv { vars, .. } = *cx;
    // statistics: the item facts refresher runs after boot (after recover has closed the turns the
    // restart cut); the routes sit with the module routes behind the login (jobs already holds the
    // module jobs and the YOLO loops)
    jobs.push(Box::pin(steve_stats::Refresher::new().run(db.clone())));
    let price_url = crate::price_sync::url(vars("STEVE_PRICE_CATALOG_URL").as_deref());
    jobs.push(Box::pin(crate::price_sync::run(
        db.clone(),
        price_url.clone(),
    )));
}}

install! { routes(reg, cx) [mut built, db, harnesses, price_url] => [built, db, harnesses, module_routes] {
    let HubEnv { bus, .. } = *cx;
    let module_routes = std::mem::take(&mut built.routes)
        .merge(crate::api::stats::routes(
            crate::api::stats::StatsApi::new(db.clone(), bus.clone(), price_url)
                .with_harness_source(harnesses.source()),
        ));
}}
