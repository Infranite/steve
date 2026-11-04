//! YOLO Y5 with a fake release script (it writes `releases/last.json` and exits, like ops/release.sh
//! for a failure before the restart): the user's own Steve is released after the cool-down, a script
//! elsewhere stays with the user, nothing is approved while a release runs, a failed release goes back
//! to the bot once, B1 trips, and the spacing rule holds.

use serde_json::{Value, json};
use steve_store::yolo as repo;

use super::engine_tests::{T, t};
use super::judge::RELEASE_AGAIN;
use crate::patch;
use crate::repo::{Release, WS};
use crate::runtime::AgentRuntime;

/// A project whose release script (under `dir`) writes `last.json` from `$STEVE_HOME/result.json`
/// (none: it reports nothing) and exits.
fn project(t: &T, dir: &std::path::Path, name: &str) -> String {
    std::fs::create_dir_all(dir).unwrap();
    let script = dir.join("release.sh");
    std::fs::write(
        &script,
        "mkdir -p \"$STEVE_HOME/releases\"\n[ -f \"$STEVE_HOME/result.json\" ] && sed \"s/ITEM/$2/\" \"$STEVE_HOME/result.json\" > \"$STEVE_HOME/releases/last.json\"\nexit 0\n",
    )
    .unwrap();
    let rel = Release {
        kind: "script".into(),
        cmd: script.to_string_lossy().into(),
    };
    t.kit
        .db()
        .project_create(
            WS,
            name,
            "",
            &dir.to_string_lossy(),
            "git",
            true,
            Some("dev"),
            Some(&rel),
        )
        .unwrap()
        .id
}

fn result_file(t: &T, ok: bool, error: &str) {
    let r = json!({ "ok": ok, "item": "ITEM", "version": "v2", "prev": "v1", "kind": "hub",
                    "error": error, "log": "/tmp/release.log" });
    std::fs::write(t.kit.home().join("result.json"), r.to_string()).unwrap();
}

/// An accepted item of `project` on branch `b` with a release card; returns (item, session, card).
fn releasing_item(t: &T, project: &str) -> (String, String, String) {
    let item = t.kit.item("run", "收尾", Some(project));
    let branch = format!("steve/{}", item.to_lowercase());
    t.kit
        .db()
        .item_patch(&item, patch![branch: branch.as_str(), now: "等你批准发布"])
        .unwrap();
    let sid = t.kit.session(&item, "idle");
    let acc = t.card(
        &item,
        &sid,
        "accept",
        "item",
        "验收",
        "",
        &["pass", "rework"],
    );
    t.kit
        .db()
        .decision_resolve(&acc, json!({"choice": "pass"}), "resolved", None)
        .unwrap();
    let d = t.card(
        &item,
        &sid,
        "approve",
        "release",
        "发布？",
        "",
        &["yes", "no"],
    );
    t.kit
        .db()
        .exec(
            "UPDATE decisions SET subject_id = ? WHERE id = ?",
            &[&branch, &d],
        )
        .unwrap();
    (item, sid, d)
}

fn outcome(t: &T, d: &str) -> Value {
    t.decision(d).result["outcome"].clone()
}

fn open_fail(t: &T, item: &str) -> Option<String> {
    t.kit
        .db()
        .decisions_of_item(item)
        .unwrap()
        .into_iter()
        .find(|d| d.kind == "fail" && d.status == "open")
        .map(|d| d.id)
}

#[test]
fn own_steve_is_released_after_the_cool_down_elsewhere_stays_with_you() {
    let t = t();
    let own = project(&t, &t.kit.home().join("current/ops"), "Steve");
    result_file(&t, true, "");
    let (item, _, d) = releasing_item(&t, &own);
    let a = t.engine.consider(&d).unwrap().unwrap();
    assert_eq!(
        (a.outcome.as_str(), a.rule.as_str()),
        ("scheduled", "release.self")
    );
    assert_eq!(a.due_at, Some(t.yolo.now() + 180_000));
    t.clock.advance(179_000);
    assert!(t.engine.tick().iter().all(|x| x.outcome != "decided"));
    t.clock.advance(1_000);
    assert!(t.engine.tick().iter().any(|x| x.outcome == "decided"));
    assert_eq!(t.decision(&d).result["approved"], true);
    assert_eq!(t.decision(&d).result["decided_by"], "yolo");
    assert!(t.kit.wait(|| outcome(&t, &d) == "released"));
    // a release is evidence for a closing, never a close by itself: no delivery is waiting here
    let i = t.kit.db().item(&item).unwrap().unwrap();
    assert_eq!((i.phase.as_str(), i.status.as_str()), ("active", "run"));
    let ops = t.kit.db().store().items().operations(&item, 1).unwrap();
    assert!(
        ops.iter()
            .any(|o| o.kind == "release" && !o.required && o.status == "succeeded"),
        "{ops:?}"
    );

    let other = tempfile::tempdir().unwrap();
    let theirs = project(&t, other.path(), "别人的");
    let (_, _, d) = releasing_item(&t, &theirs);
    let a = t.engine.consider(&d).unwrap().unwrap();
    assert_eq!(
        (a.outcome.as_str(), a.rule.as_str()),
        ("declined", "limit.5")
    );
}

#[test]
fn nothing_is_approved_while_a_release_runs() {
    let t = t();
    let own = project(&t, &t.kit.home().join("ops"), "Steve");
    let (_, _, d) = releasing_item(&t, &own);
    t.kit.rt.hold(true);
    assert!(
        t.engine.consider(&d).unwrap().is_none(),
        "waits, records nothing"
    );
    t.kit.rt.hold(false);
    assert_eq!(t.engine.consider(&d).unwrap().unwrap().outcome, "scheduled");

    // scheduled, then a release starts: still scheduled, later, never a refused-release card
    t.kit.rt.hold(true);
    t.clock.advance(181_000);
    t.engine.tick();
    let rows = t.actions(&d);
    assert_eq!(rows[0].outcome, "scheduled");
    assert!(rows[0].due_at.unwrap() > t.yolo.now());
    assert_eq!(t.decision(&d).status, "open");
    t.kit.rt.hold(false);

    // a second card waits for the first scheduled one
    let (_, _, d2) = releasing_item(&t, &own);
    assert!(t.engine.consider(&d2).unwrap().is_none());

    // a card refused because another release was running continues once that release is over
    let (item, sid, _) = t.item();
    let f = t.card(
        &item,
        &sid,
        "fail",
        "item",
        "另一个发布（X-1）正在进行；它结束后选“继续”，让它重新请求发布",
        "",
        &["continue", "delivered", "look"],
    );
    t.kit.rt.hold(true);
    assert!(t.engine.consider(&f).unwrap().is_none());
    t.kit.rt.hold(false);
    let a = t.engine.consider(&f).unwrap().unwrap();
    assert_eq!(
        (a.outcome.as_str(), a.text.as_deref()),
        ("decided", Some(RELEASE_AGAIN))
    );
}

#[test]
fn a_degraded_release_is_not_handed_back_to_the_bot() {
    let t = t();
    let own = project(&t, &t.kit.home().join("ops"), "Steve");
    let r = json!({ "ok": false, "degraded": true, "item": "ITEM", "version": "v2", "prev": "v1", "kind": "hub",
                    "error": "新版本健康检查没有通过；数据库已被新版本迁移，没有切回 v1，新版本保留（降级运行）",
                    "log": "/tmp/release.log" });
    std::fs::write(t.kit.home().join("result.json"), r.to_string()).unwrap();
    let (item, sid, d) = releasing_item(&t, &own);
    t.kit.core.resolve(&d, "yes", "", "me").unwrap();
    assert!(t.kit.wait(|| open_fail(&t, &item).is_some()));
    let f = open_fail(&t, &item).unwrap();
    let prompts = t.kit.rt.prompts_of(&sid).len();
    t.clock.advance(181_000);
    let a = t.engine.consider(&f).unwrap();
    assert!(a.as_ref().is_none_or(|a| a.outcome != "decided"), "{a:?}");
    assert_eq!(
        t.kit.rt.prompts_of(&sid).len(),
        prompts,
        "nothing sent to the bot"
    );
    assert_eq!(t.decision(&f).status, "open", "the card waits for you");
}

#[test]
fn a_failed_release_goes_back_once_and_two_yolo_failures_trip_b1() {
    let t = t();
    let own = project(&t, &t.kit.home().join("ops"), "Steve");
    result_file(&t, false, "健康检查没有通过");
    // the user approves the first one: it fails, YOLO hands it back to the bot with the reason
    let (item, sid, d) = releasing_item(&t, &own);
    t.kit.core.resolve(&d, "yes", "", "me").unwrap();
    assert!(t.kit.wait(|| open_fail(&t, &item).is_some()));
    let f = open_fail(&t, &item).unwrap();
    // you just answered on this item: YOLO waits for you first
    assert!(t.engine.consider(&f).unwrap().is_none());
    t.clock.advance(181_000);
    let a = t.engine.consider(&f).unwrap().unwrap();
    assert_eq!(
        (a.outcome.as_str(), a.rule.as_str()),
        ("decided", "continue.release")
    );
    assert!(
        t.kit
            .rt
            .prompts_of(&sid)
            .last()
            .unwrap()
            .starts_with("发布失败并已自动回到 v1：健康检查没有通过。日志 "),
        "{:?}",
        t.kit.rt.prompts_of(&sid).last()
    );
    // the second failure of the same item stays with you
    let d = t.card(
        &item,
        &sid,
        "approve",
        "release",
        "发布？",
        "",
        &["yes", "no"],
    );
    let branch = format!("steve/{}", item.to_lowercase());
    t.kit
        .db()
        .exec(
            "UPDATE decisions SET subject_id = ? WHERE id = ?",
            &[&branch, &d],
        )
        .unwrap();
    t.kit.core.resolve(&d, "yes", "", "me").unwrap();
    assert!(t.kit.wait(|| open_fail(&t, &item).is_some_and(|x| x != f)));
    let f2 = open_fail(&t, &item).unwrap();
    t.clock.advance(181_000);
    let a = t.engine.consider(&f2).unwrap().unwrap();
    assert_eq!(
        (a.outcome.as_str(), a.rule.as_str()),
        ("declined", "continue.release")
    );
    assert!(t.paused().is_none(), "the user approved those: no B1");

    // two failed releases YOLO approved in a row trip B1
    for n in 0..2 {
        let (item, _, d) = releasing_item(&t, &own);
        t.clock.advance(16 * 60_000);
        assert_eq!(t.engine.consider(&d).unwrap().unwrap().outcome, "scheduled");
        t.clock.advance(181_000);
        t.engine.tick();
        assert_eq!(t.decision(&d).result["decided_by"], "yolo", "release {n}");
        // the failure card may already be taken up by the same tick that started the release
        let any_fail = || {
            t.kit
                .db()
                .decisions_of_item(&item)
                .unwrap()
                .into_iter()
                .any(|d| d.kind == "fail")
        };
        assert!(t.kit.wait(any_fail));
        if let Some(f) = open_fail(&t, &item) {
            t.engine.consider(&f).unwrap();
        }
    }
    let p = t.paused().expect("B1 tripped");
    assert_eq!(p["code"], "B1");
}

#[test]
fn yolo_metadata_keeps_a_failure_written_after_the_answer_snapshot() {
    let t = t();
    let own = project(&t, &t.kit.home().join("ops"), "Steve");
    let (item, _, d) = releasing_item(&t, &own);
    let action = t.engine.consider(&d).unwrap().unwrap();
    // Reproduce the exact interleaving without sleeps: resolve returns a releasing snapshot,
    // then the script reports failure, then YOLO records its approval metadata.
    let after = t.kit.core.resolve(&d, "yes", "", "yolo").unwrap();
    assert_eq!(after.result["outcome"], "releasing");
    assert!(t.kit.wait(|| open_fail(&t, &item).is_some()));
    let failed = t.decision(&d).result;
    assert_eq!(failed["outcome"], "failed");
    assert_eq!(failed["reason"], crate::release::NO_RESULT);
    t.engine.record_answer(&after, &action).unwrap();
    t.yolo.finish(&action.id, "decided", None).unwrap();
    let annotated = t.decision(&d).result;
    assert_eq!(annotated["outcome"], failed["outcome"]);
    assert_eq!(annotated["reason"], failed["reason"]);
    assert_eq!(annotated["approved"], true);
    assert_eq!(annotated["decided_by"], "yolo");
    assert_eq!(annotated["yolo_action"], action.id);
    assert_eq!(annotated["yolo_rule"], action.rule);
    assert!(
        t.engine
            .consider(&open_fail(&t, &item).unwrap())
            .unwrap()
            .is_none()
    );
    assert_eq!(
        t.paused().expect("B1 must see the preserved failure")["code"],
        "B1"
    );
}

#[test]
fn a_release_without_a_result_trips_b1_at_once() {
    let t = t();
    let own = project(&t, &t.kit.home().join("ops"), "Steve");
    // no result.json: the script exits without reporting
    let (item, _, d) = releasing_item(&t, &own);
    t.engine.consider(&d).unwrap();
    t.clock.advance(181_000);
    t.engine.tick();
    assert_eq!(t.decision(&d).result["decided_by"], "yolo");
    assert!(t.kit.wait(|| open_fail(&t, &item).is_some()));
    assert!(
        t.engine
            .consider(&open_fail(&t, &item).unwrap())
            .unwrap()
            .is_none()
    );
    assert_eq!(t.paused().expect("B1")["code"], "B1");
}

#[test]
fn spacing_and_rebase_returns_leave_the_card_with_you() {
    let t = t();
    let own = project(&t, &t.kit.home().join("ops"), "Steve");
    // a YOLO release approved five minutes ago
    let now = t.yolo.now();
    let w = t
        .yolo
        .store()
        .reader(|c| repo::window_current(c, WS))
        .unwrap()
        .unwrap();
    let a = t
        .yolo
        .record(&repo::NewAction {
            workspace_id: WS.into(),
            window_id: Some(w.id.clone()),
            kind: "approve".into(),
            subject_type: "release".into(),
            rule: "release.self".into(),
            outcome: "scheduled".into(),
            due_at: Some(now - 300_000),
            ..Default::default()
        })
        .unwrap();
    t.clock.advance(-300_000);
    t.yolo.finish(&a.id, "decided", None).unwrap();
    t.clock.advance(300_000);
    let (_, _, d) = releasing_item(&t, &own);
    let x = t.engine.consider(&d).unwrap().unwrap();
    assert_eq!(
        (x.outcome.as_str(), x.rule.as_str()),
        ("declined", "release.spacing")
    );

    // three rebase conflicts handed back: the fourth release card stays with you, the item leaves YOLO
    t.clock.advance(20 * 60_000);
    let (item, _, d) = releasing_item(&t, &own);
    for _ in 0..3 {
        t.kit
            .db()
            .timeline_add(
                &item,
                "steve",
                "sys",
                "rebase 到 main 有冲突，已撤销，交回给它处理：x",
                None,
            )
            .unwrap();
    }
    let x = t.engine.consider(&d).unwrap().unwrap();
    assert_eq!(
        (x.outcome.as_str(), x.rule.as_str()),
        ("declined", "release.rebase")
    );
    assert!(!t.yolo.effective(WS, Some(&item)).unwrap().on);
}
