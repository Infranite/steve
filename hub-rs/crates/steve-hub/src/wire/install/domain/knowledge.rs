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
    registry.decision(
        steve_knowledge::proposals::SUBJECT,
        Box::new(steve_knowledge::proposals::AgentsProposalDecisions),
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

install! { start(reg, _cx) [mut jobs, kb, modules] => [jobs, kb, modules] {
    // Register the index worker, memory sweeper, folder scans and chat documents.
    let index = steve_knowledge::index::prepare_worker(modules.clone(), kb.clone());
    jobs.push(Job::task("kb.index", move || match index {
        Some(start) => start(),
        None => steve_runtime::spawn("kb.index", async {}),
    }));
    {
        let modules = modules.clone();
        jobs.push(Job::task("kb.sweep", move || steve_knowledge::sync::sweep_task(modules)));
    }
    {
        let (modules, kb) = (modules.clone(), kb.clone());
        jobs.push(Job::supervised("kb.scans", move || {
            steve_knowledge::scan_job(modules.clone(), kb.clone())
        }));
    }
    {
        let (modules, kb) = (modules.clone(), kb.clone());
        jobs.push(Job::task("kb.chat", move || steve_knowledge::sources::chat::task(modules, kb)));
    }
}}

install! { sync(reg, cx) [mut jobs, kb, modules] => [jobs, kb, modules] {
    let Env { bus, .. } = cx.clone();
    // memories and Bot memories are re-indexed when the store says they changed
    let initial = std::sync::Arc::new(std::sync::Mutex::new(Some(bus.tap(|event| {
        matches!(
            event.as_domain(),
            Some(steve_types::DomainEvent::Config(
                steve_types::ConfigEvent::Mm { .. } | steve_types::ConfigEvent::Bot { .. }
            ))
        )
    }))));
    let bus = bus.clone();
    let m = modules.clone();
    jobs.push(Job::supervised("kb.changes", move || {
        let mut changes = initial
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take()
            .unwrap_or_else(|| {
                bus.tap(|event| {
                    matches!(
                        event.as_domain(),
                        Some(steve_types::DomainEvent::Config(
                            steve_types::ConfigEvent::Mm { .. }
                                | steve_types::ConfigEvent::Bot { .. }
                        ))
                    )
                })
            });
        let m = m.clone();
        async move {
            while let Some(event) = changes.recv().await {
                let m = m.clone();
                steve_runtime::round("kb.changes", async move {
                    match tokio::task::spawn_blocking(move || {
                        steve_knowledge::sync::on_event(&m, &event.event)
                    })
                    .await
                    {
                        Ok(Ok(())) => {}
                        Ok(Err(e)) => tracing::warn!("kb sync: {e}"),
                        Err(e) => tracing::warn!("kb sync: {e}"),
                    }
                })
                .await;
            }
        }
    }));
}}
