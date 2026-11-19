use super::*;

install! { prepare(reg, cx) [mut registry] => [kb, kb_hooks, kstore, ov, registry] {
    let Env { store, steve_home, data_dir, bus, openviking, workspaces, .. } = cx.clone();
    // knowledge (K1–K5): candidate decisions, item documents on finish, prompt context, drafts
    let kb = knowledge(steve_home, data_dir, bus);
    if let Some(w) = workspaces {
        kb.set_folder_places(Arc::new(crate::wire::workspaces::KbFolders(w)));
    }
    // one knowledge store for every reader (knowledge tools, prompt context, mail and channel replies)
    // OpenViking (user decision 2026-11-04): keywords from the Hub's index, meaning from it; the
    // index is mirrored to it through the outbox (`kb.openviking`)
    let ov = openviking.map(steve_knowledge::openviking::OpenViking::new);
    let kstore: Arc<dyn steve_knowledge::port::KnowledgeStore> = match &ov {
        Some(ov) => Arc::new(steve_knowledge::openviking::Hybrid {
            keywords: steve_knowledge::port::SqliteStore,
            meaning: ov.clone(),
        }),
        None => Arc::new(steve_knowledge::port::SqliteStore),
    };
    kb.set_store(kstore.clone());
    let kb_hooks = steve_knowledge::sources::items::KnowledgeHooks::new(kb.clone());
    registry.decision(
        steve_knowledge::candidates::SUBJECT,
        Box::new(steve_knowledge::candidates::MemDecisions),
    );
    registry.item_hooks(Box::new(kb_hooks.clone()));
    registry.prompt_context(steve_knowledge::context::provider(
        store.clone(),
        kb.clone(),
    ));
    steve_knowledge::drafts::register(&mut registry, kb.clone());
}}

install! { routes(reg, _cx) [kb, mut module_routes] => [kb, module_routes] {
    module_routes = module_routes.merge(steve_knowledge::routes(kb.clone()));
}}

install! { start(reg, _cx) [kb, modules] => [kb, modules] {
    // the index queue and the memory sweeper start now (they spawn themselves)
    drop(steve_knowledge::index::spawn_worker(
        modules.clone(),
        kb.clone(),
    ));
    drop(steve_knowledge::sync::spawn_sweeper(modules.clone()));
    // folder scans, the 30-day candidate expiry and the main-thread chat documents
    drop(steve_knowledge::spawn_scans(modules.clone(), kb.clone()));
    drop(steve_knowledge::candidates::spawn_expirer(modules.clone()));
    drop(steve_knowledge::sources::chat::spawn_job(
        modules.clone(),
        kb.clone(),
    ));
}}

install! { sync(reg, cx) [mut jobs, kb, modules] => [jobs, kb, modules] {
    let Env { bus, .. } = cx.clone();
    // memories and Bot memories are re-indexed when the store says they changed
    let mut changes = bus.tap(|event| matches!(event.as_domain(), Some(steve_types::DomainEvent::Config(steve_types::ConfigEvent::Mm { .. } | steve_types::ConfigEvent::Bot { .. }))));
    let m = modules.clone();
    jobs.push(Box::pin(async move {
        while let Some(event) = changes.recv().await {
            let m = m.clone();
            match tokio::task::spawn_blocking(move || steve_knowledge::sync::on_event(&m, &event.event)).await {
                Ok(Ok(())) => {}
                Ok(Err(e)) => tracing::warn!("kb sync: {e}"),
                Err(e) => tracing::warn!("kb sync: {e}"),
            }
        }
    }));
}}
