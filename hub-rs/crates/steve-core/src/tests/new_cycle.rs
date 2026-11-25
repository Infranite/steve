//! A new cycle (ST-49): reopened, the item waits for your word only until work starts on it. A message,
//! the bot taking it up or its progress make it run again with a summary of now; only an open card keeps
//! it waiting on you. The plan of the cycle before is kept as an earlier round, not shown as this one's.

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
    let i = k.new_cycle(&x.id).unwrap();
    assert_eq!((i.phase.as_str(), i.cycle), ("ready", 2));
    (x.id, x.session)
}

fn open_cards(k: &Kit, item: &str) -> usize {
    k.db()
        .decisions_of_item(item)
        .unwrap()
        .iter()
        .filter(|d| d.status == steve_types::DecisionStatus::Open)
        .count()
}

#[test]
fn reopened_it_waits_for_your_word_with_the_last_plan_kept_as_a_round() {
    let k = Kit::new();
    let (id, _) = reopened(&k);
    assert_eq!(k.phase(&id), "ready");
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
        .session_message(
            &sid,
            "这明明构建失败了，任务咋还完成了呢？",
            false,
            &[],
            "me",
        )
        .unwrap();
    let status = k.item_label(&id);
    assert_eq!(k.phase(&id), "active");
    assert_eq!(status, "run");
    let s = k.db().session(&sid).unwrap().unwrap();
    k.core
        .tool_update(&s, "查构建日志", &[step("查日志", false)])
        .unwrap();
    let status = k.item_label(&id);
    assert_eq!(status, "run");
    let i = k.db().item(&id).unwrap().unwrap();
    assert_eq!(i.plan.len(), 1, "the new cycle's own plan");
}

#[test]
fn a_supplement_to_the_reopened_item_makes_it_run() {
    let k = Kit::new();
    let (id, _) = reopened(&k);
    k.core
        .supplement(&id, "再看看", crate::SupplementMode::Auto, &[], "me")
        .unwrap();
    assert_eq!(k.item_label(&id), "run");
    assert_eq!(k.phase(&id), "active");
}

#[test]
fn only_an_open_card_makes_the_new_cycle_wait_on_you_and_answering_it_runs_again() {
    let k = Kit::new();
    let (id, sid) = reopened(&k);
    k.core
        .session_message(&sid, "修一下", false, &[], "me")
        .unwrap();
    let s = k.db().session(&sid).unwrap().unwrap();
    k.core
        .tool_request_approval(&s, "推送到 main", "git push")
        .unwrap();
    assert!(!k.open_kinds(&id).is_empty(), "an approval is a real wait");
    assert_eq!(open_cards(&k, &id), 1, "and you have its card");
    /* progress while the approval is open keeps it waiting */
    k.core.tool_update(&s, "等批准", &[]).unwrap();
    assert!(!k.open_kinds(&id).is_empty());
    let d = k
        .db()
        .decisions_of_item(&id)
        .unwrap()
        .into_iter()
        .find(|d| d.status == steve_types::DecisionStatus::Open)
        .unwrap();
    k.core.resolve(&d.id, "yes", "", "me").unwrap();
    assert_eq!(k.item_label(&id), "run");
    assert_eq!(open_cards(&k, &id), 0);
}
