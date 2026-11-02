//! hub/test/lifecycle.test.ts: an item is done only when it is fully finished, and any new message puts it
//! back in progress.

use crate::git::tests::{branch_with, g, repo};
use crate::orchestrator::RESUME;
use crate::patch;
use crate::repo::{PlanStep, WS};
use crate::runtime::Check;
use crate::testkit::Kit;
use crate::{ItemAction, SupplementMode};
use serde_json::json;

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
    let (status, _, now) = k.status(&x.id);
    assert_eq!(status, "done");
    assert_eq!(now, "已验收，未发布");
}

#[test]
fn release_declined_with_a_reason_goes_back_to_the_agent() {
    let k = Kit::new();
    let x = k.delivered(true);
    k.core.resolve(&x.decision, "pass", "", "me").unwrap();
    k.rt.end_turn(&k.core, &x.session, "end_turn", None);
    let r = k.decision(
        &x.id,
        Some(&x.session),
        "approve",
        "release",
        &["yes", "no"],
    );
    k.core.resolve(&r, "no", "先别发", "me").unwrap();
    assert_eq!(
        k.rt.prompts_of(&x.session).last().unwrap(),
        "用户没有批准发布：先别发。"
    );
    assert_ne!(k.status(&x.id).0, "done");
}

#[test]
fn a_new_message_on_a_finished_item_puts_it_back_in_progress() {
    let k = Kit::new();
    let x = k.delivered(false);
    k.core.resolve(&x.decision, "pass", "", "me").unwrap();
    assert_eq!(k.status(&x.id).0, "done");
    k.core
        .session_message(&x.session, "再改一下", false, &[])
        .unwrap();
    let (status, stage, _) = k.status(&x.id);
    assert_eq!((status.as_str(), stage.as_str()), ("run", "执行"));
}

#[test]
fn supplementing_a_finished_item_puts_it_back_in_progress() {
    let k = Kit::new();
    let x = k.delivered(false);
    k.core.resolve(&x.decision, "pass", "", "me").unwrap();
    k.core
        .supplement(&x.id, "还有一点", SupplementMode::Auto, &[])
        .unwrap();
    assert_eq!(k.status(&x.id).0, "run");
    assert_eq!(
        k.rt.prompts_of(&x.session).last().unwrap(),
        "补充信息：还有一点"
    );
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
            }],
            &[],
        )
        .unwrap();
    let (status, _, now) = k.status(&x.id);
    assert_ne!(status, "done");
    assert_eq!(now, "等待发布");
}

#[test]
fn auto_accept_without_release_finishes_and_failed_checks_ask() {
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
            "ok",
            &[],
            &[Check {
                name: "tests".into(),
                passed: false,
            }],
            &[],
        )
        .unwrap();
    assert_eq!(k.open_kinds(&x.id), vec!["accept"]);
    let d = k
        .db()
        .decisions_of_item(&x.id)
        .unwrap()
        .into_iter()
        .find(|d| d.status == "open")
        .unwrap();
    assert_eq!(d.draft, "ok\n\n检查：✗ tests");
    assert_eq!(
        k.status(&x.id),
        (
            "wait".into(),
            "交付与验收".into(),
            "交付了，等你验收".into()
        )
    );
    k.core.tool_deliver(&s, "ok2", &[], &[], &[]).unwrap();
    assert_eq!(k.status(&x.id).0, "done");
    assert!(
        k.timeline_texts(&x.id)
            .contains(&"自检全部通过，自动验收".to_string())
    );
    assert!(
        k.open_kinds(&x.id).is_empty(),
        "the older delivery was superseded"
    );
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

#[test]
fn a_delivery_with_open_steps_is_refused_until_they_are_done_or_declared_left_open() {
    let k = Kit::new();
    let x = k.delivered(false);
    let plan = vec![step("a", false), step("b", false), step("c", true)];
    k.db().item_patch(&x.id, patch![plan: plan]).unwrap();
    let s = k.db().session(&x.session).unwrap().unwrap();
    let err = k
        .core
        .tool_deliver(&s, "ok", &[], &[], &["b".into()])
        .unwrap_err()
        .to_string();
    assert!(
        err.contains("Not delivered") && err.contains("a") && !err.contains("b;"),
        "{err}"
    );
    assert_eq!(
        k.open_kinds(&x.id),
        vec!["accept"],
        "the earlier card stays"
    );
    k.core
        .tool_update(
            &s,
            "做完 a",
            &[step("a", true), step("b", false), step("c", true)],
        )
        .unwrap();
    k.core
        .tool_deliver(&s, "ok", &[], &[], &["b".into()])
        .unwrap();
    assert_eq!(
        plan_of(&k, &x.id),
        vec![
            ("a".into(), true, false),
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
    assert!(card.draft.contains("没做完的计划步骤：b"), "{}", card.draft);
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
fn open_steps_on_a_finished_item_bring_it_back_in_progress() {
    let k = Kit::new();
    let x = k.delivered(false);
    k.core.resolve(&x.decision, "pass", "", "me").unwrap();
    assert_eq!(k.status(&x.id).0, "done");
    let s = k.db().session(&x.session).unwrap().unwrap();
    k.core
        .tool_update(&s, "全部完成", &[step("a", true)])
        .unwrap();
    assert_eq!(k.status(&x.id).0, "done");
    k.core
        .tool_update(&s, "补一步", &[step("a", true), step("b", false)])
        .unwrap();
    assert_eq!(k.status(&x.id).0, "run");
}

#[test]
fn after_a_restart_resumed_once_a_repeat_shows_the_card_a_waiting_item_is_not_poked() {
    let k = Kit::new();
    let fresh_i = k.item("run", "执行", None);
    let fresh = k.session(&fresh_i, "running");
    let again_i = k.item("run", "执行", None);
    let again = k.session(&again_i, "idle");
    k.db()
        .event_add(
            &again,
            "system",
            json!({"text": "Hub 重启，这一回合被中断，自动继续。", "resumed": true}),
        )
        .unwrap();
    k.db()
        .session_patch(&again, patch![status: "running"])
        .unwrap();
    let wait_i = k.item("run", "执行", None);
    let waiting = k.session(&wait_i, "running");
    k.decision(&wait_i, Some(&waiting), "ask", "question", &["a", "b"]);

    k.core.recover().unwrap();
    assert!(
        k.open_fails(&fresh_i).is_empty(),
        "no card while the resume is pending"
    );
    assert!(k.open_fails(&again_i)[0].contains("已经自动继续过一次"));
    k.core.fire_resume();
    assert_eq!(k.user_texts(&fresh), vec![RESUME]);
    /* the adapter can't start: the resume fails and shows the card */
    k.rt.end_turn(&k.core, &fresh, "error", Some("spawn failed"));
    assert!(k.open_fails(&fresh_i)[0].contains("自动继续没有成功：spawn failed"));
    assert!(k.user_texts(&again).is_empty());
    assert!(k.user_texts(&waiting).is_empty());
    assert!(k.open_fails(&wait_i).is_empty());
    assert_eq!(k.db().session(&waiting).unwrap().unwrap().status, "idle");
}

#[test]
fn a_successful_resume_ends_like_any_turn() {
    let k = Kit::new();
    let i = k.item("run", "执行", None);
    let s = k.session(&i, "running");
    k.core.recover().unwrap();
    k.core.fire_resume();
    assert!(crate::AgentRuntime::is_busy(&*k.rt, &s));
    let session = k.db().session(&s).unwrap().unwrap();
    k.core
        .tool_ask(&session, "继续吗？", &["是".into()])
        .unwrap();
    k.rt.end_turn(&k.core, &s, "end_turn", None);
    assert_eq!(k.open_kinds(&i), vec!["ask"]);
    assert_eq!(k.status(&i).0, "wait");
}

#[test]
fn undoing_an_acceptance_restores_the_plan_and_reblocks_a_dependent_that_had_not_started() {
    let k = Kit::new();
    let x = k.delivered(false);
    let plan = vec![
        PlanStep {
            step: "a".into(),
            done: true,
            left: false,
        },
        PlanStep {
            step: "b".into(),
            done: false,
            left: false,
        },
    ];
    k.db().item_patch(&x.id, patch![plan: plan]).unwrap();
    let dep = k.item("block", "定义", None);
    k.core.add_dep(&dep, &x.id).unwrap();
    k.db()
        .settings_set(
            WS,
            &json!({"depsAfter": "等我开始"})
                .as_object()
                .unwrap()
                .clone(),
            "me",
        )
        .unwrap();
    k.core.resolve(&x.decision, "pass", "", "me").unwrap();
    assert_eq!(k.status(&dep).0, "wait");
    k.core.undo_decision(&x.decision, "me").unwrap();
    let done: Vec<bool> = k
        .db()
        .item(&x.id)
        .unwrap()
        .unwrap()
        .plan
        .iter()
        .map(|p| p.done)
        .collect();
    assert_eq!(done, vec![true, false]);
    assert_eq!(k.status(&dep).0, "block");
    assert!(k.open_kinds(&dep).is_empty());
}

#[test]
fn undoing_an_acceptance_leaves_a_dependent_that_already_ran() {
    let k = Kit::new();
    let x = k.delivered(false);
    let dep = k.item("block", "定义", None);
    k.core.add_dep(&dep, &x.id).unwrap();
    k.core.resolve(&x.decision, "pass", "", "me").unwrap();
    /* auto start: a session began on the dependent */
    assert_eq!(k.status(&dep).0, "run");
    k.core.undo_decision(&x.decision, "me").unwrap();
    assert_eq!(k.status(&dep).0, "run");
    assert!(
        k.timeline_texts(&dep)
            .last()
            .unwrap()
            .contains("这个事项已经开始，没有停下")
    );
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
    assert!(!wt.exists());
    g(&r, &["rev-parse", "--verify", &name]);
    k.core
        .supplement(&i, "还是要做", SupplementMode::Auto, &[])
        .unwrap();
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
fn delivered_in_git(k: &Kit, commit: bool) -> (super::Delivered, std::path::PathBuf) {
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

fn release_decisions(k: &Kit, item: &str) -> usize {
    k.db()
        .decisions_of_item(item)
        .unwrap()
        .iter()
        .filter(|d| d.subject_type == "release")
        .count()
}

#[test]
fn accepted_with_nothing_on_the_branch_is_done_without_a_release() {
    let k = Kit::new();
    let (x, wt) = delivered_in_git(&k, false);
    k.core.resolve(&x.decision, "pass", "", "me").unwrap();
    let (status, _, now) = k.status(&x.id);
    assert_eq!(
        (status.as_str(), now.as_str()),
        ("done", "没有需要发布的改动")
    );
    assert!(
        k.rt.prompts_of(&x.session)
            .iter()
            .all(|p| !p.contains("steve_publish")),
        "the agent is not told to publish"
    );
    assert_eq!(release_decisions(&k, &x.id), 0);
    assert!(
        k.timeline_texts(&x.id)
            .iter()
            .any(|t| t.contains("没有需要发布的改动"))
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
fn accepted_with_uncommitted_files_waits_for_the_release_and_publish_without_them_finishes() {
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
    assert!(e.to_string().starts_with("没有需要发布的改动，事项已完成"));
    let (status, _, now) = k.status(&x.id);
    assert_eq!(
        (status.as_str(), now.as_str()),
        ("done", "没有需要发布的改动")
    );
    assert_eq!(release_decisions(&k, &x.id), 0);
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
