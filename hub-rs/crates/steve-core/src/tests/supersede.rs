//! hub/test/supersede.test.ts: a question or failure card expires once the conversation goes on without it.

use crate::ItemAction;
use crate::lifecycle::supersede;
use crate::testkit::Kit;
use serde_json::json;

fn ask(k: &Kit, item: &str, kind: &str) -> String {
    k.decision(item, None, kind, "question", &[])
}

#[test]
fn superseding_expires_questions_and_failure_cards_and_runs_again() {
    let k = Kit::new();
    let i = k.item("run", "定义", None);
    let a = ask(&k, &i, "ask");
    let f = ask(&k, &i, "fail");
    assert_eq!(
        supersede(k.db(), &i, &["ask", "fail"], "你直接补充了信息").unwrap(),
        2
    );
    assert_eq!(
        k.db().decision(&a).unwrap().unwrap().status,
        steve_types::DecisionStatus::Expired
    );
    assert_eq!(
        k.db().decision(&f).unwrap().unwrap().result,
        json!({"superseded": "你直接补充了信息"})
    );
    assert_eq!(k.item_label(&i), "run");
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
    supersede(k.db(), &i, &["ask", "fail"], "x").unwrap();
    assert_eq!(
        k.db().decision(&acc).unwrap().unwrap().status,
        steve_types::DecisionStatus::Open
    );
}

#[test]
fn nothing_to_supersede_changes_nothing() {
    let k = Kit::new();
    let i = k.item("run", "定义", None);
    k.core.item_action(&i, ItemAction::Pause, "me").unwrap();
    assert_eq!(supersede(k.db(), &i, &["ask"], "x").unwrap(), 0);
    assert_eq!(k.item_label(&i), "pause");
}

#[test]
fn bot_tools_supersede_what_they_move_past() {
    let k = Kit::new();
    k.manual_acceptance();
    let i = k.item("run", "执行", None);
    let sid = k.session(&i, "running");
    let s = k.db().session(&sid).unwrap().unwrap();
    let f = ask(&k, &i, "fail");
    k.core.tool_update(&s, "在做", &[]).unwrap();
    assert_eq!(
        k.db().decision(&f).unwrap().unwrap().status,
        steve_types::DecisionStatus::Expired
    );
    k.core.tool_ask(&s, "q1", &[]).unwrap();
    k.core.tool_ask(&s, "q2", &[]).unwrap();
    assert_eq!(k.open_kinds(&i), vec!["ask"]);
    k.core.tool_request_approval(&s, "部署", "细节").unwrap();
    assert_eq!(k.open_kinds(&i), vec!["approve"]);
    k.core.tool_deliver(&s, "好了", &[], &[], &[]).unwrap();
    k.core.tool_deliver(&s, "又好了", &[], &[], &[]).unwrap();
    assert_eq!(k.open_kinds(&i), vec!["approve", "accept"]);
}
