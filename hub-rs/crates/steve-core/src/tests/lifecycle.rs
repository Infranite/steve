//! The item lifecycle (cycle.rs): an item closes only when its result is accepted, everything it
//! requires is done and no work of it runs any more; a closed cycle stays as it closed — new input is
//! refused until an explicit new cycle.

use crate::git::tests::{branch_with, g, repo};
use crate::patch;
use crate::repo::{PlanStep, WS};
use crate::runtime::{AgentRuntime, Check, Output};
use crate::testkit::Kit;
use crate::{ItemAction, SupplementMode};
use serde_json::json;
use sha2::Digest;

fn design_report(k: &Kit, item: &str, body: &str) -> Output {
    let dir = k.home().join("workspaces").join(item).join("outputs");
    std::fs::create_dir_all(&dir).unwrap();
    let hash = sha2::Sha256::digest(body.as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect::<String>();
    let path = dir.join(format!("output-{hash}.md"));
    std::fs::write(&path, body).unwrap();
    Output {
        kind: "方案".into(),
        title: "设计方案".into(),
        location: Some(path.to_string_lossy().into_owned()),
        role: Some("result".into()),
    }
}

/// The owner switches the item's design gate on (Q1): only then does a delivery wait for the design
/// decision.
fn gate(k: &Kit, item: &str) {
    k.db()
        .store()
        .items()
        .set_design_gate(item, true)
        .unwrap()
        .unwrap();
}

fn last_json(k: &Kit, v: serde_json::Value) {
    std::fs::create_dir_all(k.home().join("releases")).unwrap();
    std::fs::write(k.home().join("releases/last.json"), v.to_string()).unwrap();
}

#[test]
fn accepted_without_a_release_is_done() {
    let k = Kit::new();
    let x = k.delivered(false);
    k.core.resolve(&x.decision, "pass", "", "me").unwrap();
    assert_eq!(k.item_label(&x.id), "done");
}

#[test]
fn release_texts_follow_the_kind() {
    for (r, want) in [
        (
            json!({"ok": true, "version": "v2", "kind": "hub"}),
            "已发布新版本 v2",
        ),
        (
            json!({"ok": true, "version": "v2", "kind": "web"}),
            "已发布前端新版本 v2（Hub 没有重启，刷新页面生效）",
        ),
        (
            json!({"ok": true, "version": "v2", "kind": "hub+web"}),
            "已发布Hub 和前端新版本 v2",
        ),
        (
            json!({"ok": false, "version": "v2", "kind": "web", "prev": "", "error": "资源 404"}),
            "发布前端 v2 失败，已回到 上一个版本：资源 404",
        ),
        (
            json!({"ok": false, "version": "v2", "kind": "hub+web", "hub_ok": true, "web_prev": "w1", "prev": "v1"}),
            "发布Hub 和前端 v2 失败，Hub 已是新版本，前端仍是 w1：健康检查没有通过",
        ),
        (
            json!({"ok": false, "version": "v2", "kind": "hub+web", "hub_ok": false, "prev": "v1"}),
            "发布Hub 和前端 v2 失败，已回到 v1：健康检查没有通过",
        ),
        (
            json!({"ok": false, "prev": "v1"}),
            "发布  失败，已回到 v1：健康检查没有通过",
        ),
    ] {
        let k = Kit::new();
        last_json(&k, r.clone());
        k.core.report_release().unwrap();
        let ns = steve_store::notify::list_recent(k.db().store(), WS, 100).unwrap();
        assert_eq!(ns[0].text, want, "{r}");
    }
}

#[test]
fn a_question_after_the_acceptance_is_answered_before_the_close_is_judged_again() {
    let k = Kit::new();
    let x = k.delivered(false);
    k.rt.prompt(&x.session, "in flight", "steve", &[]).unwrap();
    k.core.resolve(&x.decision, "pass", "", "me").unwrap();
    let accepted_at = k.db().item(&x.id).unwrap().unwrap().closing.unwrap().at;
    let q = k.decision(&x.id, Some(&x.session), "ask", "question", &[]);
    let question_at = k
        .db()
        .decision(&q)
        .unwrap()
        .unwrap()
        .created
        .max(accepted_at + 1);
    steve_store::core_fixture::decision(
        k.db().store(),
        &q,
        steve_store::core_fixture::DecisionState::Created(question_at),
    );
    k.rt.end_turn(&k.core, &x.session, "end_turn", None);
    let i = k.db().item(&x.id).unwrap().unwrap();
    let c = i.closing.clone().unwrap();
    assert!(
        k.core.tx().delivery_blockers(&i, &c).unwrap().decision,
        "the open question holds the close"
    );
    assert!(
        k.open_fails(&x.id).is_empty(),
        "nothing else is asked while it is open"
    );
    k.core
        .resolve(&q, "answer", "不发布，保持现状", "me")
        .unwrap();
    k.rt.end_turn(&k.core, &x.session, "end_turn", None);
    assert_eq!(k.item_label(&x.id), "done");
    assert!(k.open_fails(&x.id).is_empty(), "no card asks");
}

#[test]
fn a_new_message_on_a_closed_item_is_refused_until_a_new_cycle() {
    let k = Kit::new();
    let x = k.delivered(false);
    k.core.resolve(&x.decision, "pass", "", "me").unwrap();
    assert_eq!(k.item_label(&x.id), "done");
    let e = k
        .core
        .session_message(&x.session, "再改一下", false, &[], "me")
        .unwrap_err();
    assert!(e.to_string().contains("已经结束了"), "{e}");
    assert_eq!(k.item_label(&x.id), "done", "nothing reopened it");
    assert!(
        !k.timeline_texts(&x.id).contains(&"再改一下".to_string()),
        "nothing of it was written"
    );
    // an explicit new cycle: the first cycle stays as it closed
    let expected = k.db().item(&x.id).unwrap().unwrap().cas_revision;
    k.core
        .lifecycle_command(&x.id, "new_cycle", 1, expected, None, None, None, "me")
        .unwrap();
    k.core
        .session_message(&x.session, "再改一下", false, &[], "me")
        .unwrap();
    let i = k.db().item(&x.id).unwrap().unwrap();
    assert_eq!(
        (i.phase.as_str(), i.cycle, i.delivery_revision),
        ("active", 2, 0)
    );
    let cycles = k.db().store().items().cycles(&x.id).unwrap();
    assert_eq!(cycles.len(), 1);
    assert_eq!(
        (cycles[0].outcome.as_str(), cycles[0].acceptance.as_deref()),
        ("completed", Some("human"))
    );
}

#[test]
fn supplementing_a_closed_item_is_refused() {
    let k = Kit::new();
    let x = k.delivered(false);
    k.core.resolve(&x.decision, "pass", "", "me").unwrap();
    let before = k.rt.prompts_of(&x.session).len();
    assert!(
        k.core
            .supplement(&x.id, "还有一点", SupplementMode::Auto, &[], "me")
            .is_err()
    );
    assert!(k.core.revise_goal(&x.id, "新目标", "me").is_err());
    assert!(k.core.deliver_queued(&x.id).is_err());
    assert_eq!(
        k.rt.prompts_of(&x.session).len(),
        before,
        "nothing reached the agent"
    );
    assert_eq!(k.item_label(&x.id), "done");
}

/// Automatic acceptance closes clean deliveries (including reported unrelated baseline failures),
/// while holding deliveries with failed unit/external checks or unfinished steps for user confirmation.
#[test]
fn auto_accept_holds_failed_or_unfinished_delivery_and_closes_clean_or_baseline() {
    let k = Kit::new();
    let x = k.delivered(false);
    k.db()
        .decision_resolve(
            &x.decision,
            json!({"choice": "skip"}),
            steve_store::decision_repo::DecisionChange::Resolve,
            None,
        )
        .unwrap();
    k.db()
        .settings_set(
            WS,
            &json!({"accDefault": "auto"}).as_object().unwrap().clone(),
            "me",
        )
        .unwrap();
    let s = k.db().session(&x.session).unwrap().unwrap();
    k.core
        .tool_deliver(
            &s,
            "still failing",
            &[],
            &[Check {
                name: "tests".into(),
                passed: false,
                operated: false,
                scope: None,
            }],
            &["待恢复外部上游".into()],
        )
        .unwrap();
    assert!(!k.open_kinds(&x.id).is_empty());
    assert_eq!(k.open_kinds(&x.id), vec!["accept"]);
    assert!(
        k.timeline_texts(&x.id)
            .contains(&"自动验收转人工确认：有没通过的检查或没做完的步骤，请确认".to_string())
    );

    k.core
        .tool_deliver(
            &s,
            "all unit checks green",
            &[],
            &[
                Check {
                    name: "unit tests".into(),
                    passed: true,
                    operated: false,
                    scope: None,
                },
                Check {
                    name: "repo-wide fmt".into(),
                    passed: false,
                    operated: false,
                    scope: Some("baseline".into()),
                },
            ],
            &[],
        )
        .unwrap();
    assert!(
        k.open_kinds(&x.id).is_empty(),
        "clean delivery closes automatically"
    );
    assert_eq!(k.item_label(&x.id), "done");
    let i = k.db().item(&x.id).unwrap().unwrap();
    assert_eq!(i.acceptance.as_deref(), Some("automatic"));
}

#[test]
fn design_delivery_waits_for_the_users_implementation_decision() {
    let k = Kit::new();
    let item = k.item("run", "执行", None);
    k.db().add_goal(&item, "改进页面布局").unwrap();
    gate(&k, &item);
    let sid = k.session(&item, "idle");
    let session = k.db().session(&sid).unwrap().unwrap();
    let report = design_report(&k, &item, "# 完整设计\n\n正文");
    k.core
        .tool_deliver(&session, "# 完整设计\n\n正文", &[report], &[], &[])
        .unwrap();
    let decision = k
        .db()
        .decisions_of_item(&item)
        .unwrap()
        .into_iter()
        .find(|d| {
            d.status == steve_types::DecisionStatus::Open
                && d.subject_type == "design_implementation"
        })
        .unwrap();
    assert!(!k.open_kinds(&item).is_empty());
    k.core
        .resolve(&decision.id, "只保留设计", "", "me")
        .unwrap();
    assert_eq!(k.item_label(&item), "done");
}

/// Q1: the words of a goal no longer open the design gate. With the switch off (the default), a goal
/// that only asks for a design — once the very words that did — is delivered for acceptance like any
/// other, without a design file.
#[test]
fn design_goal_without_the_switch_is_an_ordinary_delivery() {
    for goal in [
        "交付一份可评审的页面设计方案",
        "先出设计，设计通过后再实现",
        "设计并实现页面，完成验证",
    ] {
        let k = Kit::new();
        let item = k.item("run", "执行", None);
        k.db().add_goal(&item, goal).unwrap();
        let sid = k.session(&item, "idle");
        let session = k.db().session(&sid).unwrap().unwrap();
        k.core
            .tool_deliver(&session, "页面设计记录", &[], &[], &[])
            .unwrap();
        assert!(
            k.db()
                .decisions_of_item(&item)
                .unwrap()
                .iter()
                .all(|d| d.subject_type != "design_implementation"),
            "{goal}"
        );
    }
}

/// The gate on, a delivery listing an external link beside a verifiable design file is confirmed on
/// that file instead of being refused.
#[test]
fn gated_delivery_passes_over_an_unverifiable_link_to_the_design_file() {
    let k = Kit::new();
    let item = k.item("run", "执行", None);
    k.db().add_goal(&item, "改进页面布局").unwrap();
    gate(&k, &item);
    let sid = k.session(&item, "idle");
    let session = k.db().session(&sid).unwrap().unwrap();
    let external = Output {
        kind: "方案".into(),
        title: "外部文档".into(),
        location: Some("https://docs.example/design".into()),
        role: Some("result".into()),
    };
    let report = design_report(&k, &item, "完整方案");
    let path = report.location.clone().unwrap();
    k.core
        .tool_deliver(&session, "完整方案", &[external, report], &[], &[])
        .unwrap();
    let decision = k
        .db()
        .decisions_of_item(&item)
        .unwrap()
        .into_iter()
        .find(|d| {
            d.status == steve_types::DecisionStatus::Open
                && d.subject_type == "design_implementation"
        })
        .unwrap();
    assert!(
        decision.subject_id.contains(&path),
        "{}",
        decision.subject_id
    );
}

#[test]
fn confirming_design_starts_implementation_without_reasking_the_design_question() {
    let k = Kit::new();
    let item = k.item("run", "执行", None);
    k.db().add_goal(&item, "改进页面布局").unwrap();
    gate(&k, &item);
    let sid = k.session(&item, "idle");
    let session = k.db().session(&sid).unwrap().unwrap();
    let report = design_report(&k, &item, "完整方案");
    k.core
        .tool_deliver(&session, "完整方案", &[report], &[], &[])
        .unwrap();
    let decision = k
        .db()
        .decisions_of_item(&item)
        .unwrap()
        .into_iter()
        .find(|d| {
            d.status == steve_types::DecisionStatus::Open
                && d.subject_type == "design_implementation"
        })
        .unwrap();
    k.core
        .resolve(&decision.id, "按方案实现", "优先做手机布局", "me")
        .unwrap();
    assert!(
        k.db()
            .goal(&item)
            .unwrap()
            .starts_with("按用户确认的设计报告实现")
    );
    assert!(
        k.db()
            .decisions_of_item(&item)
            .unwrap()
            .iter()
            .all(|d| d.status != steve_types::DecisionStatus::Open
                || d.subject_type != "design_implementation")
    );
    /* the switch still on, the implementation delivered next goes to ordinary acceptance: the design
     * is not asked about again */
    k.core
        .tool_deliver(&session, "已按方案实现并验证", &[], &[], &[])
        .unwrap();
    assert_eq!(k.open_kinds(&item), vec!["accept"]);
    assert_eq!(
        k.db()
            .decisions_of_item(&item)
            .unwrap()
            .iter()
            .filter(|d| d.subject_type == "design_implementation")
            .count(),
        1
    );
}

/// The approval covers the implementation goal it added; a goal set after it is a new scope whose
/// delivery the gate, still on, holds for a design decision again.
#[test]
fn a_new_goal_after_the_approval_is_gated_again() {
    let k = Kit::new();
    let item = k.item("run", "执行", None);
    k.db().add_goal(&item, "改进页面布局").unwrap();
    gate(&k, &item);
    let sid = k.session(&item, "idle");
    let session = k.db().session(&sid).unwrap().unwrap();
    let open_design = |k: &Kit| {
        k.db()
            .decisions_of_item(&item)
            .unwrap()
            .into_iter()
            .find(|d| {
                d.status == steve_types::DecisionStatus::Open
                    && d.subject_type == "design_implementation"
            })
    };
    let report = design_report(&k, &item, "完整方案");
    k.core
        .tool_deliver(&session, "完整方案", &[report], &[], &[])
        .unwrap();
    let decision = open_design(&k).unwrap();
    k.core
        .resolve(&decision.id, "按方案实现", "", "me")
        .unwrap();
    k.db().add_goal(&item, "另加深色模式").unwrap();
    let report = design_report(&k, &item, "深色模式方案");
    k.core
        .tool_deliver(&session, "深色模式方案", &[report], &[], &[])
        .unwrap();
    assert!(open_design(&k).is_some());
}

#[test]
fn changed_design_file_cannot_reuse_the_old_approval() {
    let k = Kit::new();
    let item = k.item("run", "执行", None);
    k.db().add_goal(&item, "改进页面布局").unwrap();
    gate(&k, &item);
    let sid = k.session(&item, "idle");
    let session = k.db().session(&sid).unwrap().unwrap();
    let report = design_report(&k, &item, "原设计");
    let path = report.location.clone().unwrap();
    k.core
        .tool_deliver(&session, "原设计", &[report], &[], &[])
        .unwrap();
    let decision = k
        .db()
        .decisions_of_item(&item)
        .unwrap()
        .into_iter()
        .find(|d| {
            d.status == steve_types::DecisionStatus::Open
                && d.subject_type == "design_implementation"
        })
        .unwrap();
    std::fs::write(path, "已改动的设计").unwrap();
    assert!(
        k.core
            .resolve(&decision.id, "按方案实现", "", "me")
            .is_err()
    );
    assert!(!k.open_kinds(&item).is_empty());
}

#[test]
fn delivery_url_design_approval_checks_the_exact_record_and_bytes() {
    for tamper in [false, true] {
        let k = Kit::new();
        let item = k.item("run", "执行", None);
        k.db().add_goal(&item, "改进页面布局").unwrap();
        gate(&k, &item);
        let sid = k.session(&item, "idle");
        let session = k.db().session(&sid).unwrap().unwrap();
        let body = b"immutable design";
        let hash = sha2::Sha256::digest(body)
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect::<String>();
        let objects = k.home().join("deliveries/objects");
        std::fs::create_dir_all(&objects).unwrap();
        let path = objects.join(&hash);
        std::fs::write(&path, body).unwrap();
        let url = "https://steve.example/deliveries/dlv_design";
        steve_store::core_fixture::thread::received_delivery(
            k.db().store(),
            steve_store::core_fixture::thread::ReceivedDelivery {
                id: "dlv_design",
                url: &url,
                workspace: &session.workspace_id,
                item: &item,
                session: &sid,
                name: "design.md",
                mime: "text/plain",
                size: body.len() as i64,
                entrypoint: None,
                sha256: &hash,
            },
        );
        let mut report = Output {
            kind: "方案".into(),
            title: "设计方案".into(),
            location: Some(url.replace("steve.example", "other.example")),
            role: Some("result".into()),
        };
        assert!(
            k.core
                .tool_deliver(&session, "design", &[report.clone()], &[], &[])
                .is_err(),
            "an unrecorded host is not a received delivery"
        );
        report.location = Some(url.into());
        k.core
            .tool_deliver(&session, "design", &[report], &[], &[])
            .unwrap();
        let decision = k
            .db()
            .decisions_of_item(&item)
            .unwrap()
            .into_iter()
            .find(|d| {
                d.status == steve_types::DecisionStatus::Open
                    && d.subject_type == "design_implementation"
            })
            .unwrap();
        if tamper {
            std::fs::write(&path, b"modified design").unwrap();
        }
        let result = k.core.resolve(&decision.id, "按方案实现", "", "me");
        assert_eq!(result.is_ok(), !tamper, "{result:?}");
    }
}

/// #205: a design delivered with steve_deliver_file and listed as an ordinary `file` output (no
/// design kind, no role) is the design file when it opens in the browser; an archive without an
/// entry, or a URL this item did not receive, is not.
#[test]
fn a_received_openable_file_delivery_counts_as_the_design_file() {
    for (name, mime, entry, accepted) in [
        ("design.html", "text/html", None, true),
        ("DESIGN.md", "text/markdown", None, true),
        (
            "prototype.zip",
            "application/zip",
            Some("pages/index.html"),
            true,
        ),
        ("bundle.zip", "application/zip", None, false),
    ] {
        let k = Kit::new();
        let item = k.item("run", "执行", None);
        k.db().add_goal(&item, "改进页面布局").unwrap();
        gate(&k, &item);
        let sid = k.session(&item, "idle");
        let session = k.db().session(&sid).unwrap().unwrap();
        let body = format!("design {name}");
        let hash = sha2::Sha256::digest(body.as_bytes())
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect::<String>();
        let objects = k.home().join("deliveries/objects");
        std::fs::create_dir_all(&objects).unwrap();
        std::fs::write(objects.join(&hash), &body).unwrap();
        let url = format!(
            "https://steve.example/deliveries/dlv_x?ws={}",
            session.workspace_id
        );
        steve_store::core_fixture::thread::received_delivery(
            k.db().store(),
            steve_store::core_fixture::thread::ReceivedDelivery {
                id: "dlv_x",
                url: &url,
                workspace: &session.workspace_id,
                item: &item,
                session: &sid,
                name: name,
                mime: mime,
                size: body.len() as i64,
                entrypoint: entry,
                sha256: &hash,
            },
        );
        let output = |location: &str| Output {
            kind: "file".into(),
            title: name.into(),
            location: Some(location.into()),
            role: None,
        };
        let foreign = output(&url.replace("steve.example", "other.example"));
        assert!(
            k.core
                .tool_deliver(&session, "design", &[foreign], &[], &[])
                .is_err(),
            "a URL this item did not receive is no design file"
        );
        let result = k
            .core
            .tool_deliver(&session, "design", &[output(&url)], &[], &[]);
        assert_eq!(result.is_ok(), accepted, "{name}: {result:?}");
        if accepted {
            assert!(
                k.db()
                    .decisions_of_item(&item)
                    .unwrap()
                    .iter()
                    .any(|d| d.status == steve_types::DecisionStatus::Open
                        && d.subject_type == "design_implementation"),
                "{name}"
            );
        }
    }
}

fn step(s: &str, done: bool) -> PlanStep {
    PlanStep {
        step: s.into(),
        done,
        left: false,
    }
}
fn plan_of(k: &Kit, id: &str) -> Vec<(String, bool, bool)> {
    k.db()
        .item(id)
        .unwrap()
        .unwrap()
        .plan
        .into_iter()
        .map(|p| (p.step, p.done, p.left))
        .collect()
}

/// 2026-11-09: the agent judges when it is done. Steps it did not tick off are delivered as left open,
/// shown on the card, not a refusal.
#[test]
fn a_delivery_with_open_steps_shows_them_left_open() {
    let k = Kit::new();
    k.manual_acceptance();
    let x = k.delivered(false);
    let plan = vec![step("a", false), step("b", false), step("c", true)];
    k.db().item_patch(&x.id, patch![plan: plan]).unwrap();
    let s = k.db().session(&x.session).unwrap().unwrap();
    k.core
        .tool_deliver(&s, "ok", &[], &[], &["b".into()])
        .unwrap();
    assert_eq!(
        plan_of(&k, &x.id),
        vec![
            ("a".into(), false, true),
            ("b".into(), false, true),
            ("c".into(), true, false)
        ]
    );
    let card = k
        .db()
        .decisions_of_item(&x.id)
        .unwrap()
        .into_iter()
        .find(|d| d.kind == "accept" && d.status == steve_types::DecisionStatus::Open)
        .unwrap();
    assert!(
        card.draft.contains("没做完的计划步骤：a；b"),
        "{}",
        card.draft
    );
}

#[test]
fn finishing_never_ticks_steps_off() {
    let k = Kit::new();
    let x = k.delivered(false);
    let plan = vec![step("a", true), step("b", false)];
    k.db().item_patch(&x.id, patch![plan: plan]).unwrap();
    k.core.resolve(&x.decision, "pass", "", "me").unwrap();
    assert_eq!(k.item_label(&x.id), "done");
    assert_eq!(
        plan_of(&k, &x.id),
        vec![("a".into(), true, false), ("b".into(), false, true)]
    );
    assert!(
        k.timeline_texts(&x.id)
            .iter()
            .any(|t| t == "完成时计划里还有 1 步没做：b")
    );
}

#[test]
fn a_new_plan_keeps_the_old_one_as_an_abandoned_or_finished_round() {
    let k = Kit::new();
    let x = k.delivered(false);
    let s = k.db().session(&x.session).unwrap().unwrap();
    k.core
        .tool_update(&s, "1", &[step("a", true), step("b", false)])
        .unwrap();
    /* ticking off and adding steps stays in the same round */
    k.core
        .tool_update(
            &s,
            "2",
            &[step("a", true), step("b", true), step("c", false)],
        )
        .unwrap();
    assert!(k.db().plan_rounds(&x.id).unwrap().is_empty());
    /* dropping an open step replaces the plan */
    k.core.tool_update(&s, "3", &[step("x", false)]).unwrap();
    k.core.tool_update(&s, "4", &[step("x", true)]).unwrap();
    /* a new plan after a finished one is the next round */
    k.core.tool_update(&s, "5", &[step("y", false)]).unwrap();
    let rounds = k.db().plan_rounds(&x.id).unwrap();
    let got: Vec<(i64, &str, usize)> = rounds
        .iter()
        .map(|r| (r.round, r.end.as_str(), r.steps.len()))
        .collect();
    assert_eq!(got, vec![(1, "replaced", 3), (2, "done", 1)]);
    let tl = k.timeline_texts(&x.id);
    assert!(
        tl.contains(&"换了一版计划，上一版没做完的 1 步已废弃".to_string()),
        "{tl:?}"
    );
    assert!(
        tl.contains(&"上一轮计划已完成，开始新一轮计划".to_string()),
        "{tl:?}"
    );
}

#[test]
fn a_plan_sent_after_the_close_is_noted_and_reopens_nothing() {
    let k = Kit::new();
    let x = k.delivered(false);
    k.core.resolve(&x.decision, "pass", "", "me").unwrap();
    assert_eq!(k.item_label(&x.id), "done");
    let s = k.db().session(&x.session).unwrap().unwrap();
    k.core
        .tool_update(&s, "补一步", &[step("a", true), step("b", false)])
        .unwrap();
    assert_eq!(
        k.item_label(&x.id),
        "done",
        "a closed cycle is not reopened by a plan"
    );
    assert_eq!(k.phase(&x.id), "closed");
    assert!(
        k.timeline_texts(&x.id)
            .iter()
            .any(|t| t.contains("这次计划更新只记录"))
    );
}

/// A plan reported while the result waits to close is recorded and voids nothing (#239): what the
/// result holds is the worktree's fingerprint's to tell, and new input's.
#[test]
fn open_steps_while_closing_are_recorded_and_keep_the_frozen_result() {
    let k = Kit::new();
    let x = k.delivered(false);
    let s = k.db().session(&x.session).unwrap().unwrap();
    k.core
        .tool_update(&s, "补一步", &[step("a", true), step("b", false)])
        .unwrap();
    let i = k.db().item(&x.id).unwrap().unwrap();
    assert_eq!(i.phase.as_str(), "closing", "the delivery stands");
    assert_eq!(i.plan.len(), 2, "the plan is recorded");
    assert_eq!(
        k.open_kinds(&x.id),
        vec!["accept"],
        "its acceptance card stays"
    );
    assert!(
        k.timeline_texts(&x.id)
            .contains(&"收尾中的计划更新只记录，不作废待收尾的结果".to_string())
    );
    k.core.resolve(&x.decision, "pass", "", "me").unwrap();
    assert_eq!(k.phase(&x.id), "closed");
}

#[test]
fn undoing_an_acceptance_before_the_close_takes_it_back() {
    let k = Kit::new();
    let x = k.delivered(false);
    let dep = k.item("block", "定义", None);
    k.core.add_dep(&dep, &x.id, "me").unwrap();
    // the delivering turn still runs: the accepted result waits for it
    k.rt.st().turning.insert(x.session.clone());
    k.core.resolve(&x.decision, "pass", "", "me").unwrap();
    assert_eq!(k.phase(&x.id), "closing");
    assert_eq!(
        k.item_label(&dep),
        "block",
        "nothing unblocked before the close"
    );
    k.core.undo_decision(&x.decision, "me").unwrap();
    assert_eq!(k.open_kinds(&x.id), vec!["accept"]);
    k.rt.end_turn(&k.core, &x.session, "end_turn", None);
    assert_eq!(
        k.phase(&x.id),
        "closing",
        "waiting for your acceptance again"
    );
    assert_eq!(k.item_label(&dep), "block");
}

#[test]
fn undoing_an_acceptance_of_a_closed_cycle_is_refused_and_a_started_dependent_runs_on() {
    let k = Kit::new();
    let x = k.delivered(false);
    let dep = k.item("block", "定义", None);
    k.core.add_dep(&dep, &x.id, "me").unwrap();
    k.core.resolve(&x.decision, "pass", "", "me").unwrap();
    /* auto start: a session began on the dependent */
    assert_eq!(k.item_label(&dep), "run");
    let e = k.core.undo_decision(&x.decision, "me").unwrap_err();
    assert!(e.to_string().contains("已经结束了"), "{e}");
    assert_eq!(k.item_label(&x.id), "done");
    assert_eq!(k.item_label(&dep), "run");
}

#[test]
fn a_dropped_item_gives_back_its_clean_worktree_and_a_new_message_brings_it_back() {
    let k = Kit::new();
    let r = repo(k.home());
    let gp = k
        .db()
        .project_create(
            WS,
            "G",
            "",
            &r.to_string_lossy(),
            "git",
            true,
            Some("dev"),
            None,
        )
        .unwrap();
    let i = k.item("run", "执行", Some(&gp.id));
    let (name, wt) = branch_with(&r, &k.core.config().worktrees, &i, "x.txt", "x\n");
    k.db()
        .item_patch(
            &i,
            patch![cwd: wt.to_string_lossy().to_string(), branch: name.as_str()],
        )
        .unwrap();
    k.core.item_action(&i, ItemAction::Drop, "me").unwrap();
    assert_eq!(k.phase(&i), "closed");
    assert!(k.pump_outbox().done >= 1);
    assert!(!wt.exists());
    g(&r, &["rev-parse", "--verify", &name]);
    // a message does not bring it back; an explicit new cycle with a goal does
    assert!(
        k.core
            .supplement(&i, "还是要做", SupplementMode::Auto, &[], "me")
            .is_err()
    );
    let expected = k.db().item(&i).unwrap().unwrap().cas_revision;
    k.core
        .lifecycle_command(
            &i,
            "new_cycle",
            1,
            expected,
            None,
            Some("还是要做"),
            None,
            "me",
        )
        .unwrap();
    assert_eq!(k.phase(&i), "active");
    assert!(
        wt.join("x.txt").exists(),
        "worktree re-created from the kept branch"
    );
}

#[test]
fn a_finished_merged_item_gives_back_worktree_and_branch() {
    let k = Kit::new();
    let r = repo(k.home());
    let gp = k
        .db()
        .project_create(
            WS,
            "G",
            "",
            &r.to_string_lossy(),
            "git",
            true,
            Some("dev"),
            None,
        )
        .unwrap();
    let x = k.delivered(false);
    let (name, wt) = branch_with(&r, &k.core.config().worktrees, &x.id, "x.txt", "x\n");
    g(&r, &["merge", "-q", "--ff-only", &name]);
    k.db().item_patch(&x.id, patch![project_id: gp.id.as_str(), cwd: wt.to_string_lossy().to_string(), branch: name.as_str()]).unwrap();
    k.core
        .resolve(&x.decision, "pass", "no-release", "me")
        .unwrap();
    assert!(wt.exists(), "the tidy is not part of finishing any more");
    assert!(
        k.pump_outbox().done >= 1,
        "work.tidy runs after the closing commit"
    );
    assert!(!wt.exists());
    assert!(g(&r, &["branch", "--list", &name]).is_empty());
    assert!(
        k.timeline_texts(&x.id)
            .last()
            .unwrap()
            .contains("删除了已合并的分支")
    );
}

/// A delivered item in a git project with a release action, on its own worktree branched from main;
/// `commit` puts one commit on the branch.
pub(super) fn delivered_in_git(k: &Kit, commit: bool) -> (super::Delivered, std::path::PathBuf) {
    let r = repo(k.home());
    let rel = crate::repo::Release {
        kind: "script".into(),
        cmd: k.home().join("release.sh").to_string_lossy().into(),
    };
    let gp = k
        .db()
        .project_create(
            WS,
            "Steve",
            "",
            &r.to_string_lossy(),
            "git",
            true,
            Some("dev"),
            Some(&rel),
        )
        .unwrap();
    let x = k.delivered(false);
    let wts = k.core.config().worktrees.clone();
    let (name, wt) = if commit {
        branch_with(&r, &wts, &x.id, "x.txt", "x\n")
    } else {
        let name = format!("steve/{}", x.id.to_lowercase());
        let wt = wts.join(&x.id);
        g(
            &r,
            &[
                "worktree",
                "add",
                "-q",
                "-b",
                &name,
                &wt.to_string_lossy(),
                "main",
            ],
        );
        (name, wt)
    };
    k.db()
        .item_patch(&x.id, patch![project_id: gp.id.as_str(), cwd: wt.to_string_lossy().to_string(), branch: name.as_str()])
        .unwrap();
    (x, wt)
}

pub(super) fn release_decisions(k: &Kit, item: &str) -> usize {
    k.db()
        .decisions_of_item(item)
        .unwrap()
        .iter()
        .filter(|d| d.subject_type == "release")
        .count()
}

#[test]
fn publish_with_nothing_before_acceptance_refuses_and_leaves_the_item() {
    let k = Kit::new();
    let (x, _) = delivered_in_git(&k, false);
    let s = k.db().session(&x.session).unwrap().unwrap();
    let branch = k.db().item(&x.id).unwrap().unwrap().branch.unwrap();
    let e = k.core.tool_publish(&s, &branch, "说明").unwrap_err();
    assert!(e.to_string().starts_with("没有需要发布的改动，不用发布"));
    assert_ne!(k.item_label(&x.id), "done");
    assert_eq!(release_decisions(&k, &x.id), 0);
    assert_eq!(k.open_kinds(&x.id), vec!["accept".to_string()]);
}

/// Items left by the old code (ST-27, ST-30, ST-35): accepted, a release card answered no with a
/// reason, the release still pending, the agent idle. The periodic check closes them by that no.

/* ── only a person's input voids a completion; your Finish wins over Steve's automation (#347) ── */

/// Puts a message in the session's queue, as a session that can't take it now holds it.
fn queue(k: &Kit, sid: &str, text: &str, via: &str) {
    k.db().store().queued().add(sid, text, via, &[]).unwrap();
}

#[test]
fn a_queued_steve_message_and_your_finish_complete_the_item() {
    let k = Kit::new();
    let x = k.delivered(false);
    // the session went into error and Steve's takeover message waits in its queue
    k.db()
        .session_patch(&x.session, patch![status: "error"])
        .unwrap();
    queue(
        &k,
        &x.session,
        "（Steve）你上一次的交付已经作废：冻结之后结果又变了：工作区不见了，需要重新交付。",
        crate::cycle::STEVE_VIA,
    );
    k.finish(&x.id).unwrap();
    assert_eq!(k.phase(&x.id), "closed", "{:?}", k.timeline_texts(&x.id));
    let i = k.db().item(&x.id).unwrap().unwrap();
    assert_eq!(i.outcome.as_deref(), Some("completed"));
    let t = k.timeline_texts(&x.id);
    assert!(!t.iter().any(|l| l.contains("收到新的消息")), "{t:?}");
    assert!(k.db().queued(&x.session).unwrap().is_empty());
}

#[test]
fn only_a_persons_queued_message_voids_the_accepted_result() {
    // Steve's own follow-up waiting in the queue: the acceptance closes, the follow-up goes with it
    let k = Kit::new();
    let x = k.delivered(false);
    queue(
        &k,
        &x.session,
        "（Steve）PR 还没开。",
        crate::cycle::STEVE_VIA,
    );
    k.core.resolve(&x.decision, "pass", "", "me").unwrap();
    assert_eq!(k.phase(&x.id), "closed");
    assert!(k.db().queued(&x.session).unwrap().is_empty());
    // a message of yours waiting there: new input, the closing is void
    let k = Kit::new();
    let x = k.delivered(false);
    queue(&k, &x.session, "再改一下标题", "me");
    k.core.resolve(&x.decision, "pass", "", "me").unwrap();
    assert_eq!(k.phase(&x.id), "active");
    assert!(
        k.timeline_texts(&x.id)
            .iter()
            .any(|l| l == "收尾作废：收到新的消息")
    );
    assert_eq!(k.db().queued(&x.session).unwrap().len(), 1, "yours stays");
}
