//! hub/test/supersede.test.ts: a question or failure card expires once the conversation goes on without it.

use crate::lifecycle::{RESUME_WINDOW_MS, may_resume, settle, supersede, tidy};
use crate::patch;
use crate::testkit::Kit;
use crate::util::now;
use serde_json::json;

fn ask(k: &Kit, item: &str, kind: &str) -> String {
    k.decision(item, None, kind, "question", &[])
}

#[test]
fn an_open_question_keeps_the_item_waiting() {
    let k = Kit::new();
    let i = k.item("run", "定义", None);
    ask(&k, &i, "ask");
    settle(k.db(), &i).unwrap();
    assert_eq!(k.status(&i).0, "wait");
}

#[test]
fn superseding_expires_questions_and_failure_cards_and_runs_again() {
    let k = Kit::new();
    let i = k.item("run", "定义", None);
    let a = ask(&k, &i, "ask");
    let f = ask(&k, &i, "fail");
    settle(k.db(), &i).unwrap();
    assert_eq!(
        supersede(k.db(), &i, &["ask", "fail"], "你直接补充了信息").unwrap(),
        2
    );
    assert_eq!(k.db().decision(&a).unwrap().unwrap().status, "expired");
    assert_eq!(
        k.db().decision(&f).unwrap().unwrap().result,
        json!({"superseded": "你直接补充了信息"})
    );
    assert_eq!(k.status(&i).0, "run");
    assert!(k.open_kinds(&i).is_empty());
    assert_eq!(
        k.timeline_texts(&i).last().unwrap(),
        "2 件待处理已失效：你直接补充了信息"
    );
}

#[test]
fn other_kinds_stay_open_and_the_item_keeps_waiting() {
    let k = Kit::new();
    let i = k.item("run", "定义", None);
    ask(&k, &i, "ask");
    let acc = ask(&k, &i, "accept");
    settle(k.db(), &i).unwrap();
    supersede(k.db(), &i, &["ask", "fail"], "x").unwrap();
    assert_eq!(k.db().decision(&acc).unwrap().unwrap().status, "open");
    assert_eq!(k.status(&i).0, "wait");
}

#[test]
fn nothing_to_supersede_changes_nothing() {
    let k = Kit::new();
    let i = k.item("run", "定义", None);
    k.db().item_patch(&i, patch![status: "pause"]).unwrap();
    assert_eq!(supersede(k.db(), &i, &["ask"], "x").unwrap(), 0);
    assert_eq!(k.status(&i).0, "pause");
}

#[test]
fn a_restart_interrupted_turn_resumes_once_per_window() {
    let k = Kit::new();
    let i = k.item("run", "定义", None);
    let s = k.session(&i, "idle");
    assert!(may_resume(k.db(), &s, now()).unwrap());
    k.db()
        .event_add(
            &s,
            "system",
            json!({"text": "Hub 重启，这一回合被中断，自动继续。", "resumed": true}),
        )
        .unwrap();
    assert!(!may_resume(k.db(), &s, now()).unwrap());
    assert!(may_resume(k.db(), &s, now() + RESUME_WINDOW_MS + 1000).unwrap());
}

#[test]
fn a_turn_a_deploy_cut_resumes_even_right_after_another_resume() {
    let k = Kit::new();
    let i = k.item("run", "定义", None);
    let s = k.session(&i, "idle");
    let note = |k: &Kit, v: serde_json::Value| {
        k.db().event_add(&s, "system", v).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(5));
    };
    note(&k, json!({"text": "Hub 重启，自动继续。", "resumed": true}));
    assert!(
        !may_resume(k.db(), &s, now()).unwrap(),
        "a crash right after: not again"
    );
    // a deploy cut it on purpose (it was waiting for a permission): not a crash loop
    note(
        &k,
        json!({"text": "为了升级暂停了这一回合", "drain_cut": true}),
    );
    assert!(may_resume(k.db(), &s, now()).unwrap());
    // resumed after that cut: an interruption now (no new cut) is a crash again
    note(&k, json!({"text": "Hub 重启，自动继续。", "resumed": true}));
    assert!(!may_resume(k.db(), &s, now()).unwrap());
}

#[test]
fn tidy_expires_questions_older_than_a_delivery_and_keeps_the_delivery() {
    let k = Kit::new();
    let i = k.item("run", "定义", None);
    let a = ask(&k, &i, "ask");
    std::thread::sleep(std::time::Duration::from_millis(5));
    let acc = ask(&k, &i, "accept");
    settle(k.db(), &i).unwrap();
    assert_eq!(tidy(k.db(), &i).unwrap(), 1);
    assert_eq!(k.db().decision(&a).unwrap().unwrap().status, "expired");
    assert_eq!(k.db().decision(&acc).unwrap().unwrap().status, "open");
    assert_eq!(k.status(&i).0, "wait");
}

#[test]
fn bot_tools_supersede_what_they_move_past() {
    let k = Kit::new();
    let i = k.item("run", "执行", None);
    let sid = k.session(&i, "running");
    let s = k.db().session(&sid).unwrap().unwrap();
    let f = ask(&k, &i, "fail");
    k.core.tool_update(&s, "在做", &[]).unwrap();
    assert_eq!(k.db().decision(&f).unwrap().unwrap().status, "expired");
    k.core.tool_ask(&s, "q1", &[]).unwrap();
    k.core.tool_ask(&s, "q2", &[]).unwrap();
    assert_eq!(k.open_kinds(&i), vec!["ask"]);
    k.core.tool_request_approval(&s, "部署", "细节").unwrap();
    assert_eq!(k.open_kinds(&i), vec!["approve"]);
    k.core.tool_deliver(&s, "好了", &[], &[], &[]).unwrap();
    k.core.tool_deliver(&s, "又好了", &[], &[], &[]).unwrap();
    assert_eq!(k.open_kinds(&i), vec!["approve", "accept"]);
}
