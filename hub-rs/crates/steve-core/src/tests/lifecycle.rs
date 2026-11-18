//! The item lifecycle (cycle.rs): an item closes only when its result is accepted, everything it
//! requires is done and no work of it runs any more; a closed cycle stays as it closed — new input is
//! refused until an explicit new cycle.

use crate::git::tests::{branch_with, g, repo};
use crate::patch;
use crate::repo::{PlanStep, WS};
use crate::runtime::{Check, Output};
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
    assert_eq!(k.status(&x.id).0, "done");
}

#[test]
fn accepted_with_a_release_stays_in_progress_until_it_lands() {
    let k = Kit::new();
    let x = k.delivered(true);
    k.core.resolve(&x.decision, "pass", "", "me").unwrap();
    let (status, stage, now) = k.status(&x.id);
    assert_ne!(status, "done");
    assert_eq!(stage, "收尾");
    assert_eq!(now, "等待发布");
    /* the agent is told to publish */
    assert!(
        k.rt.prompts_of(&x.session)
            .last()
            .unwrap()
            .starts_with("验收通过。确认改动都已提交到 steve/x 后")
    );
    last_json(&k, json!({"ok": true, "version": "v1", "item": x.id}));
    k.core.report_release().unwrap();
    // the release is evidence; the close also waits for the turn that was asked to publish
    assert_eq!(k.phase(&x.id), "closing");
    k.rt.end_turn(&k.core, &x.session, "end_turn", None);
    assert_eq!(k.status(&x.id).0, "done");
    let r: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(k.home().join("releases/last.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(r["reported"], true);
    let ns = k.core.notifications(WS).unwrap();
    assert_eq!(
        (
            ns[0].kind.as_str(),
            ns[0].level.as_str(),
            ns[0].text.as_str()
        ),
        ("release", "success", "已发布新版本 v1")
    );
    assert_eq!(ns[0].item_id.as_deref(), Some(x.id.as_str()));
    assert!(
        k.db().messages(WS).unwrap().is_empty(),
        "a success stays out of the conversation"
    );
    /* reported once */
    k.core.report_release().unwrap();
    assert_eq!(k.core.notifications(WS).unwrap().len(), 1);
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
        let ns = k.core.notifications(WS).unwrap();
        assert_eq!(ns[0].text, want, "{r}");
    }
}

#[test]
fn release_declined_without_a_reason_finishes() {
    let k = Kit::new();
    let x = k.delivered(true);
    k.core.resolve(&x.decision, "pass", "", "me").unwrap();
    let r = k.decision(
        &x.id,
        Some(&x.session),
        "approve",
        "release",
        &["yes", "no"],
    );
    k.core.resolve(&r, "no", "", "me").unwrap();
    k.rt.end_turn(&k.core, &x.session, "end_turn", None);
    let (status, _, now) = k.status(&x.id);
    assert_eq!(status, "done");
    assert_eq!(now, "已验收");
    // the requirement it needed is recorded as waived by you, not as released
    let ops = k.db().store().items().operations(&x.id, 1).unwrap();
    assert!(
        ops.iter()
            .any(|o| o.kind == "release" && o.status == "waived")
    );
}

#[test]
fn release_declined_with_a_reason_goes_back_to_the_agent() {
    let k = Kit::new();
    let x = k.delivered(true);
    k.core.resolve(&x.decision, "pass", "", "me").unwrap();
    /* the agent asks for the release in its turn, then ends it */
    let r = k.decision(
        &x.id,
        Some(&x.session),
        "approve",
        "release",
        &["yes", "no"],
    );
    k.rt.end_turn(&k.core, &x.session, "end_turn", None);
    k.core
        .resolve(&r, "no", "走 PR 合进去就行，不用发布", "me")
        .unwrap();
    assert!(
        k.rt.prompts_of(&x.session)
            .last()
            .unwrap()
            .starts_with("用户没有批准发布：走 PR 合进去就行，不用发布。这个周期不再发布")
    );
    /* the conversation shows the choice with the note the agent got (ST-32) */
    assert!(
        k.timeline_texts(&x.id)
            .contains(&format!("不发布 {}：走 PR 合进去就行，不用发布", x.session))
    );
    // the agent does what you said instead first: the close waits for that turn
    assert_ne!(k.status(&x.id).0, "done");
    k.rt.end_turn(&k.core, &x.session, "end_turn", None);
    let i = k.db().item(&x.id).unwrap().unwrap();
    assert_eq!((i.status.as_str(), i.phase.as_str()), ("done", "closed"));
    let ops = k.db().store().items().operations(&x.id, 1).unwrap();
    let rel = ops.iter().find(|o| o.kind == "release").unwrap();
    assert_eq!(rel.status, "waived");
    assert_eq!(rel.evidence["reason"], "走 PR 合进去就行，不用发布");
    assert!(
        k.timeline_texts(&x.id)
            .contains(&"你选择了不发布，发布这一步不再等".to_string())
    );
}

/// The user said no to the release; the agent's follow-up work (a PR merged) made a new result, and
/// accepting it again — or the user marking it done — must not wait for a release again (ST-27).
#[test]
fn a_no_to_the_release_holds_for_every_later_result_of_the_cycle() {
    for by_user in [false, true] {
        let k = Kit::new();
        let x = k.delivered(true);
        k.core.resolve(&x.decision, "pass", "", "me").unwrap();
        let r = k.decision(
            &x.id,
            Some(&x.session),
            "approve",
            "release",
            &["yes", "no"],
        );
        k.core
            .resolve(&r, "no", "走 PR 合并，不用发布", "me")
            .unwrap();
        // the agent's follow-up (a new message to it) voids the frozen result
        k.core
            .tx()
            .lc_invalidate(&x.id, crate::cycle::void::MESSAGE, "收到新的消息")
            .unwrap();
        k.rt.end_turn(&k.core, &x.session, "end_turn", None);
        assert_eq!(k.phase(&x.id), "active");
        let before = k.rt.prompts_of(&x.session).len();
        if by_user {
            k.finish(&x.id).unwrap();
            // marking done stops the turn still at work; its end commits the close
            k.rt.end_turn(&k.core, &x.session, "cancelled", None);
        } else {
            let s = k.db().session(&x.session).unwrap().unwrap();
            k.db()
                .item_patch(&x.id, patch![accept_mode: "auto"])
                .unwrap();
            k.core
                .tool_deliver(
                    &s,
                    "PR 已合并",
                    &[],
                    &[Check {
                        name: "tests".into(),
                        passed: true,
                        operated: false,
                        scope: None,
                    }],
                    &[],
                )
                .unwrap();
            k.rt.end_turn(&k.core, &x.session, "end_turn", None);
        }
        let i = k.db().item(&x.id).unwrap().unwrap();
        assert_eq!(
            (i.status.as_str(), i.phase.as_str(), i.revision),
            ("done", "closed", 2),
            "by_user={by_user}"
        );
        assert!(
            k.rt.prompts_of(&x.session)[before..]
                .iter()
                .all(|p| !p.contains("steve_publish")),
            "nobody is asked to publish again"
        );
        let ops = k.db().store().items().operations(&x.id, 1).unwrap();
        let rel = ops
            .iter()
            .find(|o| o.kind == "release" && o.revision == 2)
            .unwrap();
        assert_eq!(rel.status, "waived");
        assert_eq!(rel.evidence["reason"], "走 PR 合并，不用发布");
    }
}

#[test]
fn an_approval_after_the_no_takes_it_back() {
    let k = Kit::new();
    let x = k.delivered(true);
    k.core.resolve(&x.decision, "pass", "", "me").unwrap();
    /* the agent asks for the release in its turn, then ends it */
    let no = k.decision(
        &x.id,
        Some(&x.session),
        "approve",
        "release",
        &["yes", "no"],
    );
    k.rt.end_turn(&k.core, &x.session, "end_turn", None);
    k.db()
        .decision_resolve(
            &no,
            json!({"approved": false, "text": "先别发"}),
            "resolved",
            None,
        )
        .unwrap();
    std::thread::sleep(std::time::Duration::from_millis(2));
    let yes = k.decision(
        &x.id,
        Some(&x.session),
        "approve",
        "release",
        &["yes", "no"],
    );
    k.db()
        .decision_resolve(
            &yes,
            json!({"approved": true, "text": ""}),
            "resolved",
            None,
        )
        .unwrap();
    let i = k.db().item(&x.id).unwrap().unwrap();
    assert_eq!(k.core.tx().release_declined(&i).unwrap(), None);
    k.core.tx().lc_try_close(&x.id).unwrap();
    assert_eq!(k.phase(&x.id), "closing", "it still waits for the release");
}

/// The agent asked about the release after the acceptance (instead of publishing); the answer is handled
/// first, then the close is judged again: only the release is left and nobody requested it, so the item
/// completes without one (2026-11-09: the agent's judgment stands, no card).
#[test]
fn a_question_after_the_acceptance_is_answered_before_the_close_is_judged_again() {
    let k = Kit::new();
    let x = k.delivered(true);
    k.core.resolve(&x.decision, "pass", "", "me").unwrap();
    std::thread::sleep(std::time::Duration::from_millis(2));
    let q = k.decision(&x.id, Some(&x.session), "ask", "question", &[]);
    k.rt.end_turn(&k.core, &x.session, "end_turn", None);
    let i = k.db().item(&x.id).unwrap().unwrap();
    let c = crate::cycle::closing_of(&i).unwrap();
    assert!(
        k.core.tx().lc_gate(&i, &c).unwrap().decision,
        "the open question holds the close"
    );
    assert!(
        k.open_fails(&x.id).is_empty(),
        "nothing else is asked while it is open"
    );
    k.core
        .resolve(&q, "answer", "不发布，保持现状", "me")
        .unwrap();
    assert_eq!(k.status(&x.id).2, "收到回答，继续");
    k.rt.end_turn(&k.core, &x.session, "end_turn", None);
    assert_eq!(k.status(&x.id).0, "done");
    assert!(k.open_fails(&x.id).is_empty(), "no card asks");
}

#[test]
fn only_the_release_left_and_not_requested_completes_without_it() {
    let k = Kit::new();
    let x = k.delivered(true);
    k.core.resolve(&x.decision, "pass", "", "me").unwrap();
    k.rt.end_turn(&k.core, &x.session, "end_turn", None);
    assert_eq!(k.status(&x.id).0, "done");
    assert!(k.open_fails(&x.id).is_empty());
    assert!(
        k.db()
            .store()
            .items()
            .operations(&x.id, k.db().item(&x.id).unwrap().unwrap().cycle)
            .unwrap()
            .iter()
            .any(|o| o.kind == "release" && o.status == "waived")
    );
    assert!(
        k.timeline_texts(&x.id)
            .contains(&"验收已经通过，Agent 没有请求发布：不发布，按验收完成".to_string())
    );
}

#[test]
fn a_new_message_on_a_closed_item_is_refused_until_a_new_cycle() {
    let k = Kit::new();
    let x = k.delivered(false);
    k.core.resolve(&x.decision, "pass", "", "me").unwrap();
    assert_eq!(k.status(&x.id).0, "done");
    let e = k
        .core
        .session_message(&x.session, "再改一下", false, &[])
        .unwrap_err();
    assert!(e.to_string().contains("已经结束了"), "{e}");
    assert_eq!(k.status(&x.id).0, "done", "nothing reopened it");
    assert!(
        !k.timeline_texts(&x.id).contains(&"再改一下".to_string()),
        "nothing of it was written"
    );
    // an explicit new cycle: the first cycle stays as it closed
    k.core
        .lifecycle_command(&x.id, "new_cycle", 1, 1, None, None, "me")
        .unwrap();
    k.core
        .session_message(&x.session, "再改一下", false, &[])
        .unwrap();
    let i = k.db().item(&x.id).unwrap().unwrap();
    assert_eq!((i.phase.as_str(), i.cycle, i.revision), ("active", 2, 0));
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
            .supplement(&x.id, "还有一点", SupplementMode::Auto, &[])
            .is_err()
    );
    assert!(k.core.revise_goal(&x.id, "新目标").is_err());
    assert!(k.core.deliver_queued(&x.id).is_err());
    assert_eq!(
        k.rt.prompts_of(&x.session).len(),
        before,
        "nothing reached the agent"
    );
    assert_eq!(k.status(&x.id).0, "done");
}

#[test]
fn a_failed_release_reopens_with_a_fail_card() {
    let k = Kit::new();
    let x = k.delivered(true);
    k.core.resolve(&x.decision, "pass", "", "me").unwrap();
    assert_eq!(k.status(&x.id).2, "等待发布");
    last_json(
        &k,
        json!({"ok": false, "version": "v2", "prev": "v1", "item": x.id, "error": "构建失败"}),
    );
    k.core.report_release().unwrap();
    let (status, stage, _) = k.status(&x.id);
    assert_ne!(status, "done");
    assert_eq!(stage, "收尾");
    assert_eq!(
        k.open_fails(&x.id),
        vec!["发布 v2 失败，已回到 v1：构建失败"]
    );
    /* a failure is a notification; the fail card is what the user acts on, no copy in the conversation */
    let n = &k.core.notifications(WS).unwrap()[0];
    assert_eq!(n.level, "fail");
    assert!(
        !k.db()
            .messages(WS)
            .unwrap()
            .iter()
            .any(|m| m.text == n.text),
        "not repeated in the conversation"
    );
}

#[test]
fn auto_accepted_with_a_release_pending_is_not_done_either() {
    let k = Kit::new();
    let x = k.delivered(true);
    k.db()
        .decision_resolve(&x.decision, json!({"choice": "skip"}), "resolved", None)
        .unwrap();
    k.db()
        .item_patch(&x.id, patch![accept_mode: "auto"])
        .unwrap();
    let s = k.db().session(&x.session).unwrap().unwrap();
    k.core
        .tool_deliver(
            &s,
            "ok",
            &[],
            &[Check {
                name: "tests".into(),
                passed: true,
                operated: false,
                scope: None,
            }],
            &[],
        )
        .unwrap();
    let (status, _, now) = k.status(&x.id);
    assert_ne!(status, "done");
    assert_eq!(now, "等待发布");
}

/// Automatic acceptance closes clean deliveries (including reported unrelated baseline failures),
/// while holding deliveries with failed unit/external checks or unfinished steps for user confirmation.
#[test]
fn auto_accept_holds_failed_or_unfinished_delivery_and_closes_clean_or_baseline() {
    let k = Kit::new();
    let x = k.delivered(false);
    k.db()
        .decision_resolve(&x.decision, json!({"choice": "skip"}), "resolved", None)
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
    assert_eq!(k.status(&x.id).0, "wait");
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
    assert_eq!(k.status(&x.id).0, "done");
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
        .find(|d| d.status == "open" && d.subject_type == "design_implementation")
        .unwrap();
    assert_eq!(k.status(&item).0, "wait");
    assert!(
        k.core
            .resolve(
                &decision.id,
                "只保留设计",
                "",
                steve_modules::outbound::YOLO_ACTOR
            )
            .is_err()
    );
    k.core
        .resolve(&decision.id, "只保留设计", "", "me")
        .unwrap();
    assert_eq!(k.status(&item).0, "done");
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
        assert_ne!(k.status(&item).1, "设计确认", "{goal}");
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
        .find(|d| d.status == "open" && d.subject_type == "design_implementation")
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
        .find(|d| d.status == "open" && d.subject_type == "design_implementation")
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
    assert_eq!(k.status(&item).1, "执行");
    assert!(
        k.db()
            .decisions_of_item(&item)
            .unwrap()
            .iter()
            .all(|d| d.status != "open" || d.subject_type != "design_implementation")
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
    assert_ne!(k.status(&item).1, "设计确认");
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
            .find(|d| d.status == "open" && d.subject_type == "design_implementation")
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
    assert_eq!(k.status(&item).1, "设计确认");
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
        .find(|d| d.status == "open" && d.subject_type == "design_implementation")
        .unwrap();
    std::fs::write(path, "已改动的设计").unwrap();
    assert!(
        k.core
            .resolve(&decision.id, "按方案实现", "", "me")
            .is_err()
    );
    assert_eq!(k.status(&item).0, "wait");
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
        k.db().store().transaction(|t|{
            t.execute("INSERT INTO deliveries(id,url,workspace_id,item_id,session_id,title,name,mime,size,created,sha256,manifest,preview) VALUES('dlv_design',?,?,?,?, 'Design','design.md','text/plain',?,1,?,'{}','{}')",rusqlite::params![url,session.workspace_id,item,sid,body.len() as i64,hash])?;
            Ok::<_,steve_store::StoreError>(())
        }).unwrap();
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
            .find(|d| d.status == "open" && d.subject_type == "design_implementation")
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
        k.db().store().transaction(|t|{
            t.execute("INSERT INTO deliveries(id,url,workspace_id,item_id,session_id,title,name,mime,size,created,entrypoint,sha256,manifest,preview) VALUES('dlv_x',?,?,?,?,'Design',?,?,?,1,?,?,'{}','{}')",rusqlite::params![url,session.workspace_id,item,sid,name,mime,body.len() as i64,entry,hash])?;
            Ok::<_,steve_store::StoreError>(())
        }).unwrap();
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
                    .any(|d| d.status == "open" && d.subject_type == "design_implementation"),
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
        .find(|d| d.kind == "accept" && d.status == "open")
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
    assert_eq!(k.status(&x.id).0, "done");
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
    assert_eq!(k.status(&x.id).0, "done");
    let s = k.db().session(&x.session).unwrap().unwrap();
    k.core
        .tool_update(&s, "补一步", &[step("a", true), step("b", false)])
        .unwrap();
    assert_eq!(
        k.status(&x.id).0,
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
    assert_eq!(i.phase, "closing", "the delivery stands");
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
    k.core.add_dep(&dep, &x.id).unwrap();
    // the delivering turn still runs: the accepted result waits for it
    k.rt.st().turning.insert(x.session.clone());
    k.core.resolve(&x.decision, "pass", "", "me").unwrap();
    assert_eq!(k.phase(&x.id), "closing");
    assert_eq!(
        k.status(&dep).0,
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
    assert_eq!(k.status(&dep).0, "block");
}

#[test]
fn undoing_an_acceptance_of_a_closed_cycle_is_refused_and_a_started_dependent_runs_on() {
    let k = Kit::new();
    let x = k.delivered(false);
    let dep = k.item("block", "定义", None);
    k.core.add_dep(&dep, &x.id).unwrap();
    k.core.resolve(&x.decision, "pass", "", "me").unwrap();
    /* auto start: a session began on the dependent */
    assert_eq!(k.status(&dep).0, "run");
    let e = k.core.undo_decision(&x.decision, "me").unwrap_err();
    assert!(e.to_string().contains("已经结束了"), "{e}");
    assert_eq!(k.status(&x.id).0, "done");
    assert_eq!(k.status(&dep).0, "run");
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
    assert!(!wt.exists());
    g(&r, &["rev-parse", "--verify", &name]);
    // a message does not bring it back; an explicit new cycle with a goal does
    assert!(
        k.core
            .supplement(&i, "还是要做", SupplementMode::Auto, &[])
            .is_err()
    );
    k.core
        .lifecycle_command(&i, "new_cycle", 1, 0, None, Some("还是要做"), "me")
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
    assert_eq!(k.pump_outbox().done, 1, "work.tidy, after the commit");
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
        done_when: None,
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
fn accepted_with_nothing_on_the_branch_asks_whether_it_is_live() {
    let k = Kit::new();
    let (x, wt) = delivered_in_git(&k, false);
    k.core.resolve(&x.decision, "pass", "", "me").unwrap();
    // nothing on the branch is no proof of a deployment: you are asked, it does not close by itself
    assert_eq!(k.phase(&x.id), "closing");
    assert_eq!(k.status(&x.id).2, "等你确认是否已上线");
    assert!(
        k.rt.prompts_of(&x.session)
            .iter()
            .all(|p| !p.contains("steve_publish")),
        "the agent is not told to publish"
    );
    assert_eq!(release_decisions(&k, &x.id), 0);
    let card = k
        .db()
        .decisions_of_item(&x.id)
        .unwrap()
        .into_iter()
        .find(|d| d.status == "open" && d.kind == "fail")
        .expect("a card asks");
    k.core.resolve(&card.id, "release_waive", "", "me").unwrap();
    let i = k.db().item(&x.id).unwrap().unwrap();
    assert_eq!(
        (i.status.as_str(), i.acceptance.as_deref()),
        ("done", Some("human"))
    );
    let ops = k.db().store().items().operations(&x.id, 1).unwrap();
    assert!(
        ops.iter()
            .any(|o| o.kind == "release" && o.status == "waived")
    );
    k.pump_outbox();
    assert!(!wt.exists(), "the empty worktree is given back");
}

#[test]
fn accepted_with_a_commit_on_the_branch_waits_for_the_release() {
    let k = Kit::new();
    let (x, _) = delivered_in_git(&k, true);
    k.core.resolve(&x.decision, "pass", "", "me").unwrap();
    let (status, stage, now) = k.status(&x.id);
    assert_ne!(status, "done");
    assert_eq!((stage.as_str(), now.as_str()), ("收尾", "等待发布"));
    assert!(
        k.rt.prompts_of(&x.session)
            .last()
            .unwrap()
            .starts_with("验收通过。确认改动都已提交到 steve/")
    );
}

#[test]
fn accepted_with_uncommitted_files_waits_for_the_release_and_publishing_nothing_asks_you() {
    let k = Kit::new();
    let (x, wt) = delivered_in_git(&k, false);
    std::fs::write(wt.join("note.md"), "x").unwrap();
    k.core.resolve(&x.decision, "pass", "", "me").unwrap();
    assert_eq!(k.status(&x.id).2, "等待发布");
    /* the agent finds nothing worth keeping and publishes anyway */
    std::fs::remove_file(wt.join("note.md")).unwrap();
    let s = k.db().session(&x.session).unwrap().unwrap();
    let branch = k.db().item(&x.id).unwrap().unwrap().branch.unwrap();
    let e = k.core.tool_publish(&s, &branch, "说明").unwrap_err();
    assert!(
        e.to_string()
            .starts_with("没有需要发布的改动，不用发布；已请用户确认"),
        "{e}"
    );
    assert_ne!(
        k.status(&x.id).0,
        "done",
        "publishing nothing finishes nothing"
    );
    assert_eq!(release_decisions(&k, &x.id), 0);
    // you checked: it is live
    let card = k
        .db()
        .decisions_of_item(&x.id)
        .unwrap()
        .into_iter()
        .find(|d| d.status == "open" && d.kind == "fail")
        .expect("a card asks");
    k.core.resolve(&card.id, "release_live", "", "me").unwrap();
    k.rt.end_turn(&k.core, &x.session, "end_turn", None);
    assert_eq!(k.status(&x.id).0, "done");
}

#[test]
fn publish_with_nothing_before_acceptance_refuses_and_leaves_the_item() {
    let k = Kit::new();
    let (x, _) = delivered_in_git(&k, false);
    let s = k.db().session(&x.session).unwrap().unwrap();
    let branch = k.db().item(&x.id).unwrap().unwrap().branch.unwrap();
    let e = k.core.tool_publish(&s, &branch, "说明").unwrap_err();
    assert!(e.to_string().starts_with("没有需要发布的改动，不用发布"));
    assert_ne!(k.status(&x.id).0, "done");
    assert_eq!(release_decisions(&k, &x.id), 0);
    assert_eq!(k.open_kinds(&x.id), vec!["accept".to_string()]);
}

/// Items left by the old code (ST-27, ST-30, ST-35): accepted, a release card answered no with a
/// reason, the release still pending, the agent idle. The periodic check closes them by that no.
#[test]
fn a_closing_left_waiting_despite_a_recorded_no_closes_on_the_next_check() {
    let k = Kit::new();
    let x = k.delivered(true);
    k.core.resolve(&x.decision, "pass", "", "me").unwrap();
    let r = k.decision(
        &x.id,
        Some(&x.session),
        "approve",
        "release",
        &["yes", "no"],
    );
    // recorded the way the old code left it: the answer, nothing waived
    k.db()
        .decision_resolve(
            &r,
            json!({"approved": false, "text": "合pr到master"}),
            "resolved",
            None,
        )
        .unwrap();
    k.rt.st().turning.clear();
    k.db()
        .session_patch(&x.session, patch![status: "idle"])
        .unwrap();
    assert_eq!(k.phase(&x.id), "closing");
    assert_eq!(k.core.tx().lc_tick().unwrap(), 1);
    let i = k.db().item(&x.id).unwrap().unwrap();
    assert_eq!((i.status.as_str(), i.phase.as_str()), ("done", "closed"));
}

/// [`delivered_in_git`] delivered again with the worktree as it is, so the closing holds its frozen
/// fingerprint (as `steve_deliver` leaves it); a release script that waits. Returns the acceptance.
fn frozen_in_git(k: &Kit) -> (super::Delivered, std::path::PathBuf, String) {
    let (x, wt) = delivered_in_git(k, true);
    std::fs::write(
        k.home().join("release.sh"),
        super::fake_release_script("exit 0\n"),
    )
    .unwrap();
    k.core
        .tx()
        .lc_invalidate(&x.id, crate::cycle::void::REDELIVER, "有新的交付")
        .unwrap();
    let d = k.decision(
        &x.id,
        Some(&x.session),
        "accept",
        "item",
        &["pass", "rework"],
    );
    k.core
        .tx()
        .lc_request_complete(
            &x.id,
            "dev",
            crate::cycle::Policy::Human,
            false,
            "交付",
            Some(d.clone()),
            vec![],
        )
        .unwrap();
    (x, wt, d)
}

fn release_cards(k: &Kit, item: &str) -> Vec<crate::repo::Decision> {
    k.db()
        .decisions_of_item(item)
        .unwrap()
        .into_iter()
        .filter(|d| d.kind == "approve" && d.subject_type == "release")
        .collect()
}

#[test]
fn accept_and_release_starts_the_release_without_a_round_of_the_agent() {
    let k = Kit::new();
    let (x, _, d) = frozen_in_git(&k);
    let prompts = k.rt.prompts_of(&x.session).len();
    k.core
        .resolve(&d, "pass", crate::orchestrator::ACCEPT_AND_RELEASE, "me")
        .unwrap();
    let cards = release_cards(&k, &x.id);
    assert_eq!(cards.len(), 1);
    let r = &cards[0];
    assert_eq!(r.status, "resolved");
    assert_eq!(r.result["approved"], true);
    assert_eq!(r.result["with_accept"], true);
    assert_eq!(r.result["outcome"], "releasing");
    assert_eq!(
        k.db().decision(&d).unwrap().unwrap().result["release"],
        "started"
    );
    let frozen = crate::cycle::closing_of(&k.db().item(&x.id).unwrap().unwrap())
        .unwrap()
        .fingerprint;
    assert_eq!(r.result["fingerprint"].as_str(), frozen.as_deref());
    assert!(
        k.core
            .tx()
            .st
            .releasing
            .as_ref()
            .is_some_and(|r| r.item == x.id)
    );
    // the agent is not asked to publish: no new prompt, a note waits for its next message
    assert_eq!(k.rt.prompts_of(&x.session).len(), prompts);
    assert!(
        k.core
            .with_steve_note(&x.session, "")
            .contains("已经开始发布")
    );
    assert_eq!(k.status(&x.id).2, "正在发布");
    assert!(
        k.timeline_texts(&x.id)
            .iter()
            .any(|t| t.starts_with("验收并发布 "))
    );
    k.core.tx().st.releasing = None;
}

#[test]
fn accept_and_release_falls_back_to_the_agent_when_the_branch_moved_or_a_release_runs() {
    // the worktree changed after the delivery was frozen: the agent's own request (a card) decides
    let k = Kit::new();
    let (x, wt, d) = frozen_in_git(&k);
    std::fs::write(wt.join("x.txt"), "changed\n").unwrap();
    k.core
        .resolve(&d, "pass", crate::orchestrator::ACCEPT_AND_RELEASE, "me")
        .unwrap();
    assert!(release_cards(&k, &x.id).is_empty());
    assert!(k.core.tx().st.releasing.is_none());

    // another release is running
    let k = Kit::new();
    let (x, _, d) = frozen_in_git(&k);
    k.core.tx().st.releasing = Some(crate::Releasing {
        token: "t".into(),
        item: "ST-99".into(),
        generation: 1,
        hold_since: None,
    });
    k.core
        .resolve(&d, "pass", crate::orchestrator::ACCEPT_AND_RELEASE, "me")
        .unwrap();
    assert!(release_cards(&k, &x.id).is_empty());
    assert!(
        k.rt.prompts_of(&x.session)
            .last()
            .unwrap()
            .starts_with("验收通过。确认改动都已提交到 steve/"),
        "the agent is asked to request it"
    );
    k.core.tx().st.releasing = None;
}

#[test]
fn accept_without_release_waives_it() {
    let k = Kit::new();
    let (x, _, d) = frozen_in_git(&k);
    k.core.resolve(&d, "pass", "no-release", "me").unwrap();
    assert!(release_cards(&k, &x.id).is_empty());
    assert!(
        k.db()
            .store()
            .items()
            .operations(&x.id, k.db().item(&x.id).unwrap().unwrap().cycle)
            .unwrap()
            .iter()
            .any(|o| o.kind == "release" && o.status == "waived")
    );
}

#[test]
fn accept_and_release_with_a_rebase_conflict_sends_the_agent_to_steve_publish_only() {
    let k = Kit::new();
    let (x, _, d) = frozen_in_git(&k);
    // main moved under the branch's own file: the rebase before the release conflicts
    let r = k.home().join("repo");
    std::fs::write(r.join("x.txt"), "theirs\n").unwrap();
    g(&r, &["add", "."]);
    g(&r, &["commit", "-q", "-m", "main x"]);
    k.core
        .resolve(&d, "pass", crate::orchestrator::ACCEPT_AND_RELEASE, "me")
        .unwrap();
    assert!(k.core.tx().st.releasing.is_none());
    let last = k.rt.prompts_of(&x.session).last().cloned().unwrap();
    assert!(
        last.contains("有冲突") && last.contains("再用 steve_publish 请求发布"),
        "{last}"
    );
    assert!(!last.contains("已经开始发布"), "{last}");
    // and nothing waits to tell it later that the release started
    assert_eq!(k.core.with_steve_note(&x.session, "next"), "next");
    assert_eq!(
        release_cards(&k, &x.id)[0].result["outcome"],
        "not_released"
    );
}

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
    assert!(
        t.iter()
            .any(|l| l == "你标记了完成，Steve 排队给 Bot 的 1 条消息不再发送"),
        "{t:?}"
    );
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
