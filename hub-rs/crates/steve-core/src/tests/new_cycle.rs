//! A new cycle (ST-49): reopened, the item waits for your word only until work starts on it. A message,
//! the bot taking it up or its progress make it run again with a summary of now; only an open card keeps
//! it waiting on you. The plan of the cycle before is kept as an earlier round, not shown as this one's.

use crate::ItemAction;
use crate::patch;
use crate::repo::PlanStep;
use crate::testkit::Kit;

fn step(s: &str, done: bool) -> PlanStep {
    PlanStep {
        step: s.into(),
        done,
        left: false,
    }
}

/// A delivered item with a finished plan, accepted and closed, then reopened by you.
fn reopened(k: &Kit) -> (String, String) {
    let x = k.delivered(false);
    let s = k.db().session(&x.session).unwrap().unwrap();
    k.core
        .tool_update(&s, "交付了", &[step("a", true), step("b", true)])
        .unwrap();
    k.core.resolve(&x.decision, "pass", "", "me").unwrap();
    assert_eq!(k.phase(&x.id), "closed");
    let i = k.core.item_action(&x.id, ItemAction::Reopen, "me").unwrap();
    assert_eq!((i.phase.as_str(), i.cycle), ("ready", 2));
    (x.id, x.session)
}

fn open_cards(k: &Kit, item: &str) -> usize {
    k.db()
        .decisions_of_item(item)
        .unwrap()
        .iter()
        .filter(|d| d.status == "open")
        .count()
}

#[test]
fn reopened_it_waits_for_your_word_with_the_last_plan_kept_as_a_round() {
    let k = Kit::new();
    let (id, _) = reopened(&k);
    let (status, _, now) = k.status(&id);
    assert_eq!(
        (status.as_str(), now.as_str()),
        ("wait", "新周期，等你说要做什么")
    );
    let i = k.db().item(&id).unwrap().unwrap();
    assert!(i.plan.is_empty(), "the finished plan is not this cycle's");
    let rounds = k.db().plan_rounds(&id).unwrap();
    assert_eq!(
        rounds
            .iter()
            .map(|r| (r.end.as_str(), r.steps.len()))
            .collect::<Vec<_>>(),
        vec![("done", 2)]
    );
}

#[test]
fn a_message_to_the_reopened_item_makes_it_run_and_progress_keeps_it_so() {
    let k = Kit::new();
    let (id, sid) = reopened(&k);
    k.core
        .session_message(&sid, "这明明构建失败了，任务咋还完成了呢？", false, &[])
        .unwrap();
    let (status, stage, now) = k.status(&id);
    assert_eq!(k.phase(&id), "active");
    assert_eq!(
        (status.as_str(), stage.as_str(), now.as_str()),
        ("run", "执行", "新周期，收到新消息，开始处理"),
        "the bot took it up: running, no stale wait summary"
    );
    let s = k.db().session(&sid).unwrap().unwrap();
    k.core
        .tool_update(&s, "查构建日志", &[step("查日志", false)])
        .unwrap();
    let (status, _, now) = k.status(&id);
    assert_eq!((status.as_str(), now.as_str()), ("run", "查构建日志"));
    let i = k.db().item(&id).unwrap().unwrap();
    assert_eq!(i.plan.len(), 1, "the new cycle's own plan");
}

#[test]
fn a_supplement_to_the_reopened_item_makes_it_run() {
    let k = Kit::new();
    let (id, _) = reopened(&k);
    k.core
        .supplement(&id, "再看看", crate::SupplementMode::Auto, &[])
        .unwrap();
    assert_eq!(k.status(&id).0, "run");
    assert_eq!(k.phase(&id), "active");
}

#[test]
fn only_an_open_card_makes_the_new_cycle_wait_on_you_and_answering_it_runs_again() {
    let k = Kit::new();
    let (id, sid) = reopened(&k);
    k.core.session_message(&sid, "修一下", false, &[]).unwrap();
    let s = k.db().session(&sid).unwrap().unwrap();
    k.core
        .tool_request_approval(&s, "推送到 main", "git push")
        .unwrap();
    assert_eq!(k.status(&id).0, "wait", "an approval is a real wait");
    assert_eq!(open_cards(&k, &id), 1, "and you have its card");
    /* progress while the approval is open keeps it waiting */
    k.core.tool_update(&s, "等批准", &[]).unwrap();
    assert_eq!(k.status(&id).0, "wait");
    let d = k
        .db()
        .decisions_of_item(&id)
        .unwrap()
        .into_iter()
        .find(|d| d.status == "open")
        .unwrap();
    k.core.resolve(&d.id, "yes", "", "me").unwrap();
    assert_eq!(k.status(&id).0, "run");
    assert_eq!(open_cards(&k, &id), 0);
}

#[test]
fn a_wait_without_a_card_ends_when_the_bot_reports_progress() {
    let k = Kit::new();
    /* as ST-49 was left: the cycle started, the bot at work, the item still saying it waits on you */
    let id = k.item("wait", "定义", None);
    k.db()
        .exec(
            "UPDATE items SET phase = 'active', cycle = 2 WHERE id = ?",
            &[&id],
        )
        .unwrap();
    k.db()
        .item_patch(&id, patch![now: "新周期，等你说要做什么"])
        .unwrap();
    let sid = k.session(&id, "running");
    let s = k.db().session(&sid).unwrap().unwrap();
    k.core.tool_update(&s, "在查原因", &[]).unwrap();
    let (status, _, now) = k.status(&id);
    assert_eq!((status.as_str(), now.as_str()), ("run", "在查原因"));
}
