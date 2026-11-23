//! #400: persisted successful tools win over stale no-delivery judgments, with negative controls.

use crate::repo::{NewDecision, Session, WS};
use crate::testkit::Kit;
use crate::{AgentRuntime, Check, Core, CreateItem, Mark, Output, TurnEnd};

fn started(k: &Kit) -> (String, Session) {
    let i = k
        .core
        .create_item(CreateItem {
            ws: WS.into(),
            title: "Receipt ordering".into(),
            goal: "finish the work".into(),
            ..Default::default()
        })
        .unwrap();
    k.db()
        .item_patch(&i.id, crate::patch![accept_mode: "manual"])
        .unwrap();
    let s = k.db().sessions_of_item(&i.id).unwrap().pop().unwrap();
    (i.id, s)
}

fn receipt(core: &Core, s: &Session, attempt: Option<&str>) -> Option<Mark> {
    let item = core
        .store()
        .item(s.item_id.as_deref().unwrap())
        .unwrap()
        .unwrap();
    core.tx().turn_receipt(&item, s, attempt).unwrap()
}

#[test]
fn provider_failure_after_a_committed_delivery_or_question_does_not_resurrect_work() {
    provider_failure_after_receipt(crate::runtime::MODEL_STREAM_INTERRUPTED);
    provider_failure_after_receipt(crate::runtime::MODEL_SERVICE_UNAVAILABLE);
}

fn provider_failure_after_receipt(stop: &str) {
    for deliver in [true, false] {
        let k = Kit::new();
        let (id, s) = started(&k);
        if deliver {
            k.core.tool_deliver(&s, "Done", &[], &[], &[]).unwrap();
        } else {
            k.core.tool_ask(&s, "Missing input", &[]).unwrap();
        }
        let prompts = k.rt.prompts_of(&s.id).len();
        k.rt.end_turn(
            &k.core,
            &s.id,
            stop,
            Some("stream disconnected before completion: upstream stream interrupted"),
        );
        assert!(k.open_fails(&id).is_empty());
        assert_eq!(k.rt.prompts_of(&s.id).len(), prompts);
        assert_eq!(
            k.db()
                .decisions_of_item(&id)
                .unwrap()
                .iter()
                .filter(|d| d.status == "open" && d.kind == if deliver { "accept" } else { "ask" })
                .count(),
            1
        );
    }
}

fn detach(k: &Kit, s: &Session) -> String {
    let a = k.rt.running(&s.id).unwrap().attempt;
    k.rt.st().attempts.remove(&s.id);
    k.rt.st().turning.remove(&s.id);
    k.db()
        .session_patch(&s.id, crate::patch![status: "idle"])
        .unwrap();
    a
}

fn end(core: &Core, s: &Session, a: &str) {
    core.on_turn_end(TurnEnd {
        session_id: s.id.clone(),
        attempt: Some(a.into()),
        stop_reason: "end_turn".into(),
        error: None,
    })
    .unwrap();
}

fn logical(k: &Kit, s: &Session, turn: &str, a: &str, at: i64, ordinal: i64) {
    steve_store::core_fixture::runtime::finished_turn(
        k.db().store(),
        steve_store::core_fixture::runtime::FinishedTurn {
            session: &s.id,
            turn,
            attempt: a,
            node: "fixture",
            node_session: "ns",
            ordinal,
            operation: &format!("op-{a}"),
            at,
        },
    );
}

#[test]
fn a_persisted_question_survives_restart_before_turn_end_and_replay() {
    let k = Kit::new();
    let (id, s) = started(&k);
    k.core
        .tool_ask(&s, "Which input?", &["A".into(), "B".into()])
        .unwrap();
    let a = detach(&k, &s);
    let core = k.reopened();
    assert_eq!(receipt(&core, &s, Some(&a)), Some(Mark::Asked));
    for _ in 0..3 {
        end(&core, &s, &a);
    }
    assert!(k.open_fails(&id).is_empty());
    assert_eq!(
        k.db()
            .decisions_of_item(&id)
            .unwrap()
            .iter()
            .filter(|d| d.status == "open")
            .count(),
        1
    );
}

#[test]
fn delivery_and_end_in_either_order_keep_only_the_acceptance_after_restart() {
    for late in [false, true] {
        let k = Kit::new();
        let (id, s) = started(&k);
        let a = k.rt.running(&s.id).unwrap().attempt;
        if late {
            k.rt.end_turn(&k.core, &s.id, "end_turn", None);
            assert_eq!(
                k.open_fails(&id).len(),
                1,
                "real fallback before receipt arrives"
            );
        }
        k.core.tool_deliver(&s, "Done", &[], &[], &[]).unwrap();
        if !late {
            detach(&k, &s);
        }
        let core = k.reopened();
        assert_eq!(receipt(&core, &s, Some(&a)), Some(Mark::Delivered));
        for _ in 0..3 {
            end(&core, &s, &a);
        }
        core.check_stalls().unwrap();
        assert!(k.open_fails(&id).is_empty());
        let item = core.store().item(&id).unwrap().unwrap();
        assert_eq!(
            (item.phase.as_str(), item.now.as_str()),
            ("closing", "交付了，等你验收")
        );
        assert_eq!(
            k.db()
                .decisions_of_item(&id)
                .unwrap()
                .iter()
                .filter(|d| d.kind == "accept" && d.status == "open")
                .count(),
            1
        );
    }
}

#[test]
fn a_question_arriving_after_end_replaces_the_fallback_and_survives_restart() {
    let k = Kit::new();
    let (id, s) = started(&k);
    let a = k.rt.running(&s.id).unwrap().attempt;
    k.rt.end_turn(&k.core, &s.id, "end_turn", None);
    assert_eq!(k.open_fails(&id).len(), 1);
    k.core.tool_ask(&s, "Missing input", &[]).unwrap();
    let core = k.reopened();
    assert_eq!(receipt(&core, &s, Some(&a)), Some(Mark::Asked));
    end(&core, &s, &a);
    assert!(k.open_fails(&id).is_empty());
}

#[test]
fn accepted_delivery_waiting_on_pr_closeout_is_not_idle_after_restart() {
    let k = Kit::new();
    let (id, s) = started(&k);
    k.core
        .tool_deliver(&s, "ready for PR", &[], &[], &[])
        .unwrap();
    let i = k.db().item(&id).unwrap().unwrap();
    // A deterministic pending closeout gate; no Git host or publishing action runs here.
    k.core
        .tx()
        .record_item_operation(
            &i,
            "fixture.pr",
            None,
            true,
            "pending",
            "等待 PR 检查",
            serde_json::json!({}),
        )
        .unwrap();
    let accept = k
        .db()
        .decisions_of_item(&id)
        .unwrap()
        .into_iter()
        .find(|d| d.kind == "accept" && d.status == "open")
        .unwrap();
    k.core.resolve(&accept.id, "pass", "", "me").unwrap();
    let a = detach(&k, &s);
    let core = k.reopened();
    end(&core, &s, &a);
    core.check_stalls().unwrap();
    let i = core.store().item(&id).unwrap().unwrap();
    assert_eq!(i.phase, "closing");
    assert_eq!(i.now, "已验收，正在结束");
    // The accepted-state line and the pending closeout gate are separate public fields.
    let view = crate::cycle::lifecycle_view(&core.tx(), &i).unwrap();
    assert_eq!(view["closing"]["accepted"], true);
    assert!(
        view["closing"]["waiting"]
            .as_array()
            .unwrap()
            .iter()
            .any(|w| w == "operation")
    );
    assert!(
        view["blockers"]
            .as_array()
            .unwrap()
            .iter()
            .any(|b| b["kind"] == "operation")
    );
    assert!(
        core.store()
            .store()
            .items()
            .operations(&id, i.cycle)
            .unwrap()
            .iter()
            .any(|o| o.kind == "fixture.pr" && o.status == "pending" && o.label == "等待 PR 检查")
    );
    assert!(core.tx().turn_has_wait(&i, false).unwrap());
    assert!(k.open_fails(&id).is_empty());
}

#[test]
fn gateway_retry_uses_the_same_durable_logical_turn() {
    let k = Kit::new();
    let (id, s) = started(&k);
    let a = k.rt.running(&s.id).unwrap().attempt;
    logical(&k, &s, "t1", &a, 1, 0);
    k.core.tool_ask(&s, "Choose", &[]).unwrap();
    logical(&k, &s, "t1", "retry", 2, 1);
    detach(&k, &s);
    let core = k.reopened();
    assert_eq!(receipt(&core, &s, Some("retry")), Some(Mark::Asked));
    end(&core, &s, "retry");
    assert!(k.open_fails(&id).is_empty());
}

#[test]
fn an_old_end_after_the_newer_turn_is_idle_never_consumes_its_question() {
    let k = Kit::new();
    let (id, s) = started(&k);
    logical(&k, &s, "old", "old-attempt", 1, 0);
    let a = k.rt.running(&s.id).unwrap().attempt;
    logical(&k, &s, "current", &a, 2, 0);
    k.core.tool_ask(&s, "Current question", &[]).unwrap();
    detach(&k, &s);
    let core = k.reopened();
    end(&core, &s, "old-attempt");
    assert!(
        core.store()
            .store()
            .sessions()
            .stop(&s.id)
            .unwrap()
            .is_none()
    );
    assert_eq!(receipt(&core, &s, Some(&a)), Some(Mark::Asked));
    end(&core, &s, &a);
    assert!(k.open_fails(&id).is_empty());
}

#[test]
fn old_turn_and_cycle_and_goal_receipts_are_not_current_success() {
    for change in ["input", "logical_turn", "cycle", "goal"] {
        let k = Kit::new();
        let (id, s) = started(&k);
        let a = k.rt.running(&s.id).unwrap().attempt;
        logical(&k, &s, "t1", &a, 1, 0);
        k.core.tool_ask(&s, "Old question", &[]).unwrap();
        match change {
            "input" => {
                k.db()
                    .event_add(&s.id, "user", serde_json::json!({"text": "new work"}))
                    .unwrap();
            }
            "logical_turn" => logical(&k, &s, "t2", "next", 2, 0),
            "cycle" => {
                steve_store::core_fixture::item(
                    k.db().store(),
                    &id,
                    steve_store::core_fixture::ItemState::BumpCycle,
                );
            }
            "goal" => {
                k.db().add_goal(&id, "changed goal").unwrap();
            }
            _ => unreachable!(),
        }
        detach(&k, &s);
        let core = k.reopened();
        assert_eq!(receipt(&core, &s, None), None, "{change}");
        crate::lifecycle::supersede(k.db(), &id, &["ask"], "old question retired").unwrap();
        end(
            &core,
            &s,
            if change == "logical_turn" { "next" } else { &a },
        );
        assert_eq!(k.open_fails(&id).len(), 1, "{change}: fallback must remain");
    }
}

#[test]
fn a_delivery_from_the_previous_input_never_hides_a_real_stop() {
    let k = Kit::new();
    let (id, s) = started(&k);
    k.core
        .tool_deliver(&s, "old result", &[], &[], &[])
        .unwrap();
    detach(&k, &s);
    k.core
        .session_message(&s.id, "Do another thing", false, &[])
        .unwrap();
    let core = k.reopened();
    assert_eq!(receipt(&core, &s, None), None);
    k.rt.say(&s.id, "已重新交付；这只是文字汇报，当前轮收据尚未到达");
    k.rt.end_turn(&core, &s.id, "end_turn", None);
    assert_eq!(k.open_fails(&id).len(), 1);
    assert!(k.open_fails(&id)[0].contains("已重新交付"));
    core.tool_deliver(&s, "current result now persisted", &[], &[], &[])
        .unwrap();
    assert!(k.open_fails(&id).is_empty());
    assert_eq!(receipt(&k.reopened(), &s, None), Some(Mark::Delivered));
}

#[test]
fn prose_and_rejected_delivery_are_not_receipts_or_partial_deliveries() {
    for refused in [false, true] {
        let k = Kit::new();
        let (id, s) = started(&k);
        k.rt.say(&s.id, "I delivered and everything passed");
        if refused {
            let outputs = [Output {
                kind: "branch".into(),
                title: "unaccepted result".into(),
                location: Some("fixture/result".into()),
                role: None,
            }];
            let checks = [Check {
                name: "invalid scope".into(),
                passed: true,
                operated: false,
                scope: Some("invalid".into()),
            }];
            assert!(
                k.core
                    .tool_deliver(&s, "refused", &outputs, &checks, &[])
                    .is_err()
            );
            assert!(
                !k.timeline_texts(&id)
                    .iter()
                    .any(|t| t == crate::takeover::DELIVERED)
            );
            assert!(k.db().store().artifacts().list(&id).unwrap().is_empty());
        }
        assert_eq!(receipt(&k.core, &s, None), None);
        k.rt.end_turn(&k.core, &s.id, "end_turn", None);
        assert_eq!(k.open_fails(&id).len(), 1);
        assert_eq!(k.phase(&id), "active");
    }
}

#[test]
fn a_failed_receipt_write_rolls_back_business_state_and_memory_mark() {
    for ask in [false, true] {
        let k = Kit::new();
        let (id, s) = started(&k);
        steve_store::core_fixture::reject_tool_receipts(k.db().store());
        let result = if ask {
            k.core.tool_ask(&s, "not committed", &[])
        } else {
            k.core.tool_deliver(&s, "not committed", &[], &[], &[])
        };
        assert!(result.is_err());
        assert!(k.db().decisions_of_item(&id).unwrap().is_empty());
        assert!(
            !k.timeline_texts(&id)
                .iter()
                .any(|t| t == crate::takeover::DELIVERED)
        );
        assert_eq!(k.phase(&id), "active");
        k.rt.end_turn(&k.core, &s.id, "end_turn", None);
        assert_eq!(k.open_fails(&id).len(), 1);
    }
}

#[test]
fn restart_tidy_expires_only_no_receipt_cards_and_preserves_real_errors() {
    let k = Kit::new();
    let (id, s) = started(&k);
    k.core.tool_ask(&s, "Current input needed", &[]).unwrap();
    detach(&k, &s);
    for (question, options) in [
        (
            "Agent 停下来了，没有交付也没有提问",
            vec!["continue".into()],
        ),
        ("会话出错：real error", vec!["continue".into()]),
        ("发布状态未知", vec!["release_live".into()]),
    ] {
        k.db()
            .decision_create(NewDecision {
                workspace_id: WS.into(),
                kind: "fail".into(),
                subject_type: "item".into(),
                subject_id: id.clone(),
                item_id: Some(id.clone()),
                session_id: Some(s.id.clone()),
                question: question.into(),
                options: Some(options),
                requested_by: "steve".into(),
                ..Default::default()
            })
            .unwrap();
    }
    let core = k.reopened();
    for _ in 0..3 {
        core.check_stalls().unwrap();
    }
    assert_eq!(
        k.open_fails(&id),
        vec!["会话出错：real error", "发布状态未知"]
    );
    assert_eq!(
        k.db()
            .decisions_of_item(&id)
            .unwrap()
            .iter()
            .filter(|d| d.kind == "ask" && d.status == "open")
            .count(),
        1
    );
}

#[test]
fn failed_automatic_acceptance_does_not_dispatch_an_uncommitted_hook() {
    struct Hook;
    #[async_trait::async_trait]
    impl steve_modules::hooks::ItemHooks for Hook {
        fn name(&self) -> &'static str {
            "receipt rollback fixture"
        }
    }
    let k = Kit::new();
    let (id, s) = started(&k);
    k.db()
        .item_patch(&id, crate::patch![accept_mode: "auto"])
        .unwrap();
    detach(&k, &s);
    let mut registry = steve_modules::Registry::new();
    registry.item_hooks(Box::new(Hook));
    k.core.attach_modules(steve_modules::Modules::new(
        k.db().store().clone(),
        k.core.module_gateway(),
        registry,
    ));
    steve_store::core_fixture::reject_tool_receipts(k.db().store());
    let mut tx = k.core.tx();
    assert!(
        tx.tool_deliver(&s, "automatic result", &[], &[], &[])
            .is_err()
    );
    // Observe the dispatch boundary while locked, before Drop could send acceptance to a module.
    assert!(tx.st.hooks.is_empty());
    assert!(tx.st.cancels.is_empty());
    assert_eq!(tx.item(&id).unwrap().phase, "active");
}
