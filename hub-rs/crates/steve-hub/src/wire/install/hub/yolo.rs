use super::*;

install! { install(reg, cx) [core, db, host, node_jobs, yolo_forced] => [core, db, engine, jobs, yolo] {
    let HubEnv { cfg, bus, .. } = *cx;
    let yolo = steve_core::yolo::Yolo::new(
        steve_core::repo::Db::new(db.clone()),
        yolo_forced,
        &cfg.home,
    );
    core.set_yolo(yolo.clone());
    let engine = steve_core::yolo::Engine::new(core.clone(), yolo.clone());
    engine.set_recall(reg.built.as_ref().expect("modules installed").recall.clone());
    let _ = host.yolo.set(engine.clone());
    let mut jobs = std::mem::take(&mut reg.built.as_mut().expect("modules installed").jobs);
    jobs.extend(node_jobs);
    jobs.push(Box::pin(
        engine.clone().run_events(bus.tap(&["decision", "yolo"])),
    ));
    jobs.push(Box::pin(engine.clone().run_ticks()));
}}

install! { routes(reg, _cx) [engine, mut module_routes] => [engine, module_routes] {
    module_routes = module_routes.merge(crate::api::yolo::routes(engine.clone()));
}}
