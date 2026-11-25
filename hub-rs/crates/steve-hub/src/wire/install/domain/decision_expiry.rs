use super::*;
use std::sync::atomic::{AtomicI64, Ordering};
use std::time::{Duration, Instant};
use steve_modules::vault::SecretUses;
use steve_runtime::due::{DueError, DueSource, FALLBACK, due_loop};
use steve_runtime::policy::{Backoff, Jitter};
use steve_store::{node_write, wait_repo};

install! { install(reg, _cx) [mut jobs, modules, tools] => [jobs, modules, tools] {
    let source = Source {
        modules: modules.clone(),
        tools: tools.clone(),
        channel_scan_at: AtomicI64::new(modules.clock.now()),
    };
    let clock = modules.clock.clone();
    let wake = modules.store.commit_wake(&[
        "runtime_permissions",
        "secret_uses",
        "mems",
        "mem_candidates",
        "decisions",
        "item_dependency_waits",
        "item_operations",
        "gh_effects",
        "mail_drafts",
    ]);
    let source = Arc::new(source);
    jobs.push(Job::supervised("decisions.expire", move || {
        due_loop(
            source.clone(),
            clock.clone(),
            wake.clone(),
            Backoff {
                base: Duration::from_secs(1),
                max: FALLBACK,
                factor: 2,
                jitter: Jitter::None,
                attempts: None,
                reset_after: None,
            },
        )
    }));
}}

struct Source {
    modules: Modules,
    tools: Tools,
    channel_scan_at: AtomicI64,
}

impl DueSource for Source {
    fn name(&self) -> &'static str {
        "decisions.expire"
    }

    async fn next_due(&self) -> Result<Option<i64>, DueError> {
        let channel_scan_at = self.channel_scan_at.load(Ordering::Relaxed);
        self.modules
            .blocking(move |m| {
                m.store.read(|r| {
                    Ok([
                        Some(channel_scan_at),
                        node_write::next_permission_expiry(r)?,
                        steve_knowledge::candidates::next_expiry(r)?,
                        SecretUses::next_expiry(r)?,
                        wait_repo::next_due(r)?,
                    ]
                    .into_iter()
                    .flatten()
                    .min())
                })
            })
            .await
            .map_err(|e| DueError(e.to_string()))
    }

    async fn run_due(&self, now: i64) -> Result<(), DueError> {
        let tools = self.tools.clone();
        let scan_channels = self.channel_scan_at.load(Ordering::Relaxed) <= now;
        let (mut error, channels) = tokio::task::spawn_blocking({
            let modules = self.modules.clone();
            move || {
                // Run specialized short-TTL expirers first so domain side effects (Node rejects,
                // secret-use notifications, candidate staleness, channel card_ttl_h) settle before
                // the unified wait-contract sweep.
                let mut results = vec![
                    crate::wire::node_runtime::NodeRuntime::expire_permissions(&modules.store, now)
                        .map_err(|e| DueError(e.to_string())),
                    steve_knowledge::candidates::expire(&modules, now)
                        .map(|_| ())
                        .map_err(|e| DueError(e.to_string())),
                    tools
                        .uses
                        .expire_due(now)
                        .map_err(|e| DueError(e.to_string())),
                ];
                let channels =
                    scan_channels.then(|| tools.outbox.expire_stale(&tools.channels, now));
                results.push(sweep_due_waits(&modules, now));
                (results.into_iter().find_map(Result::err), channels)
            }
        })
        .await
        .map_err(|e| DueError(e.to_string()))?;
        match channels {
            Some(Ok(false)) => self.channel_scan_at.store(
                now.saturating_add(FALLBACK.as_millis() as i64),
                Ordering::Relaxed,
            ),
            Some(Err(e)) => {
                error.get_or_insert_with(|| DueError(e.to_string()));
            }
            Some(Ok(true)) | None => {}
        }
        error.map_or(Ok(()), Err)
    }
}

fn decision_wait_kind_label(subject_type: &str, kind: &str) -> &'static str {
    match (subject_type, kind) {
        ("chan_message", "approve") => steve_types::WaitKind::ChannelDraft.as_str(),
        ("mail_draft", _) => steve_types::WaitKind::MailDraft.as_str(),
        ("flow_step", _) => steve_types::WaitKind::FlowWait.as_str(),
        _ => steve_types::WaitKind::Decision.as_str(),
    }
}

/// Unified bounded sweep of all durable waits governed by [`steve_types::WaitContract`].
pub(crate) fn sweep_due_waits(modules: &Modules, now: i64) -> Result<(), DueError> {
    let started = Instant::now();
    let mut first_error: Option<DueError> = None;

    let due_decisions = modules
        .store
        .read(|r| wait_repo::due_decisions(r, now, wait_repo::WAIT_SWEEP_BATCH))
        .map_err(|e| DueError(e.to_string()))?;
    for due in due_decisions {
        let kind_label = decision_wait_kind_label(&due.subject_type, &due.kind);
        let outcome = if steve_modules::decisions::has_handler(modules, &due.subject_type) {
            steve_modules::decisions::expire(modules, &due.id).map_err(|e| DueError(e.to_string()))
        } else {
            modules
                .store
                .write(|w| wait_repo::expire_core_decision(w, &due.id, now))
                .map_err(|e| DueError(e.to_string()))
        };
        match outcome {
            Ok(true) => {
                metrics::counter!(
                    "steve_wait_timeouts_total",
                    "kind" => kind_label,
                    "action" => steve_types::WaitTimeoutAction::Close.as_str()
                )
                .increment(1);
            }
            Ok(false) => {}
            Err(e) => {
                tracing::warn!(decision = %due.id, error = %e, "wait contract decision expiry failed");
                first_error.get_or_insert(e);
            }
        }
    }

    for (kind, action, run) in [
        (
            steve_types::WaitKind::Dependency,
            steve_types::WaitTimeoutAction::EscalateToOwner,
            modules
                .store
                .write(|w| wait_repo::expire_dependency_waits(w, now, wait_repo::WAIT_SWEEP_BATCH)),
        ),
        (
            steve_types::WaitKind::ItemOperation,
            steve_types::WaitTimeoutAction::EscalateToOwner,
            modules
                .store
                .write(|w| wait_repo::expire_item_operations(w, now, wait_repo::WAIT_SWEEP_BATCH)),
        ),
        (
            steve_types::WaitKind::ExternalEffect,
            steve_types::WaitTimeoutAction::EscalateToOwner,
            modules
                .store
                .write(|w| wait_repo::expire_gh_effects(w, now, wait_repo::WAIT_SWEEP_BATCH)),
        ),
        (
            steve_types::WaitKind::MailDraft,
            steve_types::WaitTimeoutAction::Close,
            modules
                .store
                .write(|w| wait_repo::expire_mail_drafts(w, now, wait_repo::WAIT_SWEEP_BATCH)),
        ),
    ] {
        match run {
            Ok(0) => {}
            Ok(n) => {
                metrics::counter!(
                    "steve_wait_timeouts_total",
                    "kind" => kind.as_str(),
                    "action" => action.as_str()
                )
                .increment(n as u64);
            }
            Err(e) => {
                tracing::warn!(kind = kind.as_str(), error = %e, "wait contract sweep failed");
                first_error.get_or_insert_with(|| DueError(e.to_string()));
            }
        }
    }

    metrics::histogram!("steve_wait_sweep_duration_seconds").record(started.elapsed());
    first_error.map_or(Ok(()), Err)
}

#[cfg(test)]
mod tests {
    use super::*;
    use steve_modules::testkit::{TestEnv, WS};
    use steve_store::{DecisionRows, NewDecision, mail};
    use steve_types::{DecisionStatus, Millis, WaitContract, WaitOwner, WaitTimeoutAction};

    #[test]
    fn sweep_due_waits_routes_module_decisions_through_handlers_and_closes_core_decisions() {
        let link = steve_mail::DraftLink::new();
        let env = TestEnv::new(|r| {
            r.decision(steve_mail::drafts::SUBJECT_TYPE, link.handler());
        });
        let base = 1_796_000_000_000_i64;
        let deadline = base + 60_000;
        env.clock.set(base);

        // Seed mail connection + pending draft + open approval decision with WaitContract.
        env.store()
            .write(|w| {
                let conn = steve_store::mail::DraftRow {
                    id: "md_due_1".into(),
                    workspace_id: WS.into(),
                    connection_id: "con_due".into(),
                    thread_id: None,
                    reply_to_message: None,
                    mode: "new".into(),
                    to: vec![],
                    cc: vec![],
                    bcc: vec![],
                    subject: "Due draft".into(),
                    body: "Hello".into(),
                    files: vec![],
                    author: "steve".into(),
                    item_id: None,
                    session_id: None,
                    run_id: None,
                    decision_id: None,
                    basis: None,
                    content_hash: "h_due".into(),
                    status: "pending".into(),
                    send_at: None,
                    sent_message: None,
                    error: None,
                    created: base,
                    updated: base,
                    sent: None,
                    delivery_version: None,
                    wait: Some(
                        WaitContract::new(
                            WaitOwner::Owner,
                            Millis(base),
                            Millis(deadline + 60_000),
                            WaitTimeoutAction::Close,
                        )
                        .unwrap(),
                    ),
                };
                mail::insert_draft(w, &conn)?;
                let d = DecisionRows(w).open(&NewDecision {
                    workspace_id: WS.into(),
                    kind: "approve".into(),
                    subject_type: steve_mail::drafts::SUBJECT_TYPE.into(),
                    subject_id: "md_due_1".into(),
                    question: "Send mail?".into(),
                    options: Some(vec!["send".into(), "no".into()]),
                    requested_by: "steve".into(),
                    wait: Some(
                        WaitContract::new(
                            WaitOwner::Owner,
                            Millis(base),
                            Millis(deadline),
                            WaitTimeoutAction::Close,
                        )
                        .unwrap(),
                    ),
                    ..Default::default()
                })?;
                mail::set_draft_decision(w, "md_due_1", Some(&d.id), "h_due")?;
                // Also open a core decision with the same deadline.
                DecisionRows(w).open(&NewDecision {
                    workspace_id: WS.into(),
                    kind: "ask".into(),
                    subject_type: "item".into(),
                    subject_id: "ST-1".into(),
                    question: "Core question".into(),
                    requested_by: "steve".into(),
                    wait: Some(
                        WaitContract::new(
                            WaitOwner::Owner,
                            Millis(base),
                            Millis(deadline),
                            WaitTimeoutAction::Close,
                        )
                        .unwrap(),
                    ),
                    ..Default::default()
                })?;
                Ok::<_, steve_store::StoreError>(())
            })
            .unwrap();

        // Sweep before deadline changes nothing.
        sweep_due_waits(&env.modules, deadline - 1).unwrap();
        assert_eq!(
            env.store()
                .read(|r| mail::draft(r, "md_due_1"))
                .unwrap()
                .unwrap()
                .status,
            "pending"
        );
        assert_eq!(env.store().decisions().open(WS).unwrap().len(), 2);

        // Sweep at deadline expires the mail approval via MailDraftHandler (discarding draft)
        // AND closes the core decision.
        sweep_due_waits(&env.modules, deadline).unwrap();
        let draft = env
            .store()
            .read(|r| mail::draft(r, "md_due_1"))
            .unwrap()
            .unwrap();
        assert_eq!(draft.status, "discarded");
        assert!(env.store().decisions().open(WS).unwrap().is_empty());
        let dec = env
            .store()
            .decisions()
            .get(draft.decision_id.as_deref().unwrap())
            .unwrap()
            .unwrap();
        assert_eq!(dec.status, DecisionStatus::Expired);
    }
}
