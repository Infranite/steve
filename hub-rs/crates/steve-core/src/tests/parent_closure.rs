use crate::cycle::CloseKind;
use crate::repo::{NewItem, Parent, WS};
use crate::runtime::AgentRuntime;
use crate::testkit::Kit;
use crate::{ItemAction, patch};

fn child(k: &Kit, parent: &str) -> String {
    k.db()
        .item_create(
            WS,
            &NewItem {
                tags: k.task_tags(WS),
                title: "child".into(),
                goal: "unfinished".into(),
                bot_id: "dev".into(),
                parent: Some(Parent {
                    item: Some(Some(parent.into())),
                    ..Default::default()
                }),
                ..Default::default()
            },
        )
        .unwrap()
        .id
}

#[test]
fn completed_and_abandoned_parents_retire_children_without_claiming_their_success() {
    for finish in [true, false] {
        let k = Kit::new();
        let parent = k.item("run", "执行", None);
        let open = child(&k, &parent);
        let failed = child(&k, &parent);
        k.core
            .tx()
            .request_cycle_stop(&failed, CloseKind::Fail, "workflow", "review failed", None)
            .unwrap();
        let success = child(&k, &parent);
        k.finish(&success).unwrap();
        let cancelled = child(&k, &parent);
        k.core
            .tx()
            .request_cycle_stop(
                &cancelled,
                CloseKind::Cancel,
                "me",
                "independently cancelled",
                None,
            )
            .unwrap();
        let archived = child(&k, &parent);
        k.core
            .tx()
            .request_cycle_stop(&archived, CloseKind::Fail, "workflow", "old failure", None)
            .unwrap();
        k.db()
            .item_patch(&archived, patch![archived: true])
            .unwrap();
        let untouched = [success.clone(), cancelled.clone(), archived.clone()]
            .map(|id| k.db().item(&id).unwrap().unwrap());
        let sid = k.session(&open, "idle");
        k.decision(&open, Some(&sid), "ask", "item", &["yes"]);
        k.db().queue_add(&sid, "stale start", "steve", &[]).unwrap();
        if finish {
            k.finish(&parent).unwrap();
            k.pump_outbox();
        } else {
            k.core.item_action(&parent, ItemAction::Drop, "me").unwrap();
        }
        for id in [&open, &failed] {
            let i = k.db().item(id).unwrap().unwrap();
            assert_eq!(i.phase.as_str(), "closed");
            assert!(i.archived);
            assert!(
                k.timeline_texts(id)
                    .iter()
                    .any(|message| message.contains(&format!("随父事项 {parent} 结束")))
            );
            assert_ne!(i.outcome.as_deref(), Some("completed"));
            assert!(k.open_kinds(id).is_empty());
        }
        assert_eq!(
            k.db().item(&open).unwrap().unwrap().outcome.as_deref(),
            Some("cancelled")
        );
        // The historical review verdict is immutable; archiving records why it is no longer work.
        assert_eq!(
            k.db().item(&failed).unwrap().unwrap().outcome.as_deref(),
            Some("failed")
        );
        assert_eq!(k.db().session(&sid).unwrap().unwrap().status, "ended");
        assert!(k.db().queued(&sid).unwrap().is_empty());
        for i in untouched {
            let after = k.db().item(&i.id).unwrap().unwrap();
            assert_eq!(after.closed_reason, i.closed_reason);
            assert_eq!(after.archived, i.archived);
            assert_eq!(after.outcome, i.outcome);
        }
    }
}

#[test]
fn upgrade_reconciles_nested_retired_attempts_idempotently_even_with_resume_disabled() {
    let k = Kit::with(|c| {
        c.no_resume = true;
    });
    // Fixture represents a database from an older installation, with no in-memory graph.
    let parent = k.item("done", "收尾", None);
    let old = child(&k, &parent);
    k.core
        .tx()
        .request_cycle_stop(&old, CloseKind::Fail, "workflow", "old failure", None)
        .unwrap();
    let nested = child(&k, &old);
    let unrelated = k.item("run", "执行", None);
    let reopened = k.reopened();
    reopened.lifecycle_tick().unwrap();
    let before = k.timeline_texts(&nested);
    reopened.lifecycle_tick().unwrap();
    assert_eq!(k.timeline_texts(&nested), before);
    for id in [&old, &nested] {
        let i = k.db().item(id).unwrap().unwrap();
        assert!(i.archived);
        assert_eq!(i.phase.as_str(), "closed");
        assert!(
            k.timeline_texts(id)
                .iter()
                .any(|message| message.contains("随父事项"))
        );
    }
    assert_eq!(
        k.db().item(&unrelated).unwrap().unwrap().phase.as_str(),
        "active"
    );
    assert!(k.rt.st().prompts.is_empty());
}

#[test]
fn a_child_turn_or_unknown_effect_must_settle_before_parent_cleanup_hides_it() {
    let k = Kit::new();
    let parent = k.item("run", "执行", None);
    let running = child(&k, &parent);
    let sid = k.session(&running, "idle");
    k.rt.prompt(&sid, "work", "steve", &[]).unwrap();
    let stop_gate = k.rt.stop_gate.write().unwrap();
    k.core.item_action(&parent, ItemAction::Drop, "me").unwrap();
    let i = k.db().item(&running).unwrap().unwrap();
    assert_eq!(i.phase.as_str(), "closing");
    assert!(!i.archived);
    drop(stop_gate);
    k.rt.end_turn(&k.core, &sid, "cancelled", None);
    k.core.lifecycle_tick().unwrap();
    assert!(k.db().item(&running).unwrap().unwrap().archived);

    let other = child(&k, &parent);
    let i = k.db().item(&other).unwrap().unwrap();
    let (op, _) = k
        .core
        .tx()
        .record_item_operation(
            &i,
            "external",
            None,
            true,
            "unknown",
            "external effect",
            serde_json::json!({}),
        )
        .unwrap();
    k.core.lifecycle_tick().unwrap();
    let i = k.db().item(&other).unwrap().unwrap();
    assert_eq!(i.phase.as_str(), "closing");
    assert!(!i.archived);
    k.core
        .tx()
        .settle_item_operation(&op.id, "succeeded", serde_json::json!({}))
        .unwrap();
    k.core.lifecycle_tick().unwrap();
    assert!(k.db().item(&other).unwrap().unwrap().archived);
}

#[test]
fn startup_retires_open_legacy_workflow_children_and_preserves_closed_history() {
    let k = Kit::new();
    let legacy = |phase: &str| {
        let id = k.item(phase, "执行", None);
        steve_store::core_fixture::item(
            k.db().store(),
            &id,
            steve_store::core_fixture::ItemState::Source(r#"["协作工作","workflow"]"#),
        );
        id
    };
    let active = legacy("run");
    let paused = legacy("pause");
    let running = legacy("run");
    let sid = k.session(&running, "idle");
    k.rt.prompt(&sid, "legacy work", "steve", &[]).unwrap();
    let delivered = legacy("run");
    let delivered_sid = k.session(&delivered, "idle");
    let accept = k.decision(
        &delivered,
        Some(&delivered_sid),
        "accept",
        "item",
        &["pass"],
    );
    k.core
        .tx()
        .request_delivery_close(
            &delivered,
            "dev",
            crate::cycle::ClosePolicy::Human,
            false,
            "legacy delivery",
            Some(accept),
            vec![],
        )
        .unwrap();
    let closed_done = legacy("done");
    k.db()
        .timeline_add(&closed_done, "dev", "deliver", "kept artifact", None)
        .unwrap();
    let before_closed = k.db().item(&closed_done).unwrap().unwrap();
    let ordinary = k.item("run", "执行", None);

    assert_eq!(k.core.retire_legacy_workflow_children().unwrap(), 4);
    assert_eq!(k.core.retire_legacy_workflow_children().unwrap(), 0);

    for id in [&active, &paused, &delivered] {
        let i = k.db().item(id).unwrap().unwrap();
        assert_eq!(
            (
                i.phase.as_str(),
                i.outcome.as_deref(),
                i.closed_reason.as_deref()
            ),
            ("closed", Some("cancelled"), Some("legacy_workflow_retired")),
            "{id}"
        );
    }
    let r = k.db().item(&running).unwrap().unwrap();
    assert_eq!(r.phase.as_str(), "closing");
    k.rt.end_turn(&k.core, &sid, "cancelled", None);
    let r = k.db().item(&running).unwrap().unwrap();
    assert_eq!(
        (r.phase.as_str(), r.outcome.as_deref()),
        ("closed", Some("cancelled"))
    );

    let after_closed = k.db().item(&closed_done).unwrap().unwrap();
    assert_eq!(
        (
            after_closed.phase,
            after_closed.outcome,
            after_closed.closed_reason
        ),
        (
            before_closed.phase,
            before_closed.outcome,
            before_closed.closed_reason
        )
    );
    assert!(
        k.timeline_texts(&closed_done)
            .iter()
            .any(|t| t == "kept artifact")
    );
    assert_eq!(
        k.db().item(&ordinary).unwrap().unwrap().phase.as_str(),
        "active"
    );
}
