use super::*;

install! { install(reg, cx) [flow, gh, mail, outbox, ov] => [dispatcher, flow, gh, mail, outbox] {
    let Env { store, core, .. } = cx.clone();
    // registered now, before boot resumes sessions or the API listens: no finish happens earlier
    let mut module_consumers: Vec<Arc<dyn steve_store::outbox::Consumer>> = vec![
        steve_mail::outbox::MailEventsOut::new(mail.clone()),
        gh.outbox_consumer(),
        Arc::new(steve_channels::notifications::Intake),
    ];
    // an alarm's notification (a todo's reminder, a snoozed todo back), for its owner
    module_consumers.extend(steve_channels::notifications::AlarmIntake::all());
    // a member's morning brief, made when its alarm fires
    module_consumers.push(Arc::new(steve_digest::brief::BriefIntake));
    let mirror = steve_knowledge::openviking::Mirror::ID;
    let mirrored_before = store
        .reader(|c| steve_store::outbox::is_registered(c, mirror))
        .map_err(|e| BackendError::msg(format!("outbox: {e}")))?;
    match &ov {
        Some(ov) => module_consumers.push(Arc::new(steve_knowledge::openviking::Mirror {
            ov: ov.clone(),
            store: store.clone(),
        })),
        // no longer mirrored: what it owed is dropped; switching back on mirrors everything again
        None if mirrored_before => {
            steve_store::outbox::unregister(store, mirror)
                .map_err(|e| BackendError::msg(format!("outbox: {e}")))?;
            tracing::info!("knowledge: no longer mirrored to OpenViking");
        }
        None => {}
    }
    let dispatcher = crate::wire::outbox::dispatcher(store, core, &flow, module_consumers)
        .map_err(|e| BackendError::msg(format!("outbox: {e}")))?;
    // the first time: everything indexed before is mirrored too
    if ov.is_some() && !mirrored_before {
        let n = steve_knowledge::openviking::backfill(store)
            .map_err(|e| BackendError::msg(format!("knowledge backfill: {e}")))?;
        tracing::info!(objects = n, "knowledge: mirroring the index to OpenViking");
    }
}}

install! { jobs(reg, cx) [dispatcher, flow, mut jobs] => [flow, jobs] {
    let Env { store, .. } = cx.clone();
    jobs.push(Job::new("outbox.dispatch", crate::wire::outbox::run(dispatcher, store.clone(), flow.clone())));
}}
