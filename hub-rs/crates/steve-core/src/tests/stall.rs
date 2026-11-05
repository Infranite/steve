//! Items left in `run` with nothing going on get one “它停下了” card (crate::stall).

use crate::AgentRuntime;
use crate::repo::HubEvent;
use crate::stall::{STALL, STALL_AFTER_MS};
use crate::testkit::Kit;
use crate::util::now;
use serde_json::json;

fn later() -> i64 {
    now() + STALL_AFTER_MS + 1_000
}

fn stall_cards(k: &Kit, item: &str) -> Vec<crate::repo::Decision> {
    k.db()
        .decisions_of_item(item)
        .unwrap()
        .into_iter()
        .filter(|d| d.subject_type == STALL)
        .collect()
}

/// A cancelled turn leaves the item `run` with an idle session: after two minutes it gets the card, once.
#[test]
fn a_stopped_item_gets_one_card_with_the_last_system_line() {
    let k = Kit::new();
    let i = k.item("run", "执行", None);
    let s = k.session(&i, "idle");
    k.db()
        .event_add(&s, "system", json!({"text": "你中断了当前回合"}))
        .unwrap();
    assert!(
        k.core.tx().check_stalls(now()).unwrap().is_empty(),
        "not yet"
    );
    assert_eq!(
        k.core.tx().check_stalls(later()).unwrap(),
        std::slice::from_ref(&i)
    );
    let d = &stall_cards(&k, &i)[0];
    assert_eq!(
        (d.kind.as_str(), d.question.as_str(), d.status.as_str()),
        ("fail", "它停下了：你中断了当前回合", "open")
    );
    assert_eq!(d.options, ["continue", "look", "drop"]);
    assert_eq!(d.session_id.as_deref(), Some(s.as_str()));
    assert_eq!(k.status(&i).0, "wait", "the item waits on the user");
    assert!(
        k.core.tx().check_stalls(later()).unwrap().is_empty(),
        "one card per item"
    );
    assert_eq!(stall_cards(&k, &i).len(), 1);
}

#[test]
fn busy_queued_waiting_paused_or_releasing_items_are_not_stalled() {
    let k = Kit::new();
    // a turn running
    let busy = k.item("run", "执行", None);
    let bs = k.session(&busy, "idle");
    k.rt.prompt(&bs, "做事", "me", &[]).unwrap();
    // something queued (waits for a hold or a restart)
    let queued = k.item("run", "执行", None);
    let qs = k.session(&queued, "idle");
    k.db().queue_add(&qs, "排队的", "me", &[]).unwrap();
    // an open question
    let asked = k.item("run", "执行", None);
    let a = k.session(&asked, "idle");
    k.decision(&asked, Some(&a), "ask", "item", &[]);
    // paused, and an item without a live session
    let paused = k.item("pause", "执行", None);
    k.session(&paused, "idle");
    k.item("run", "执行", None);
    assert!(k.core.tx().check_stalls(later()).unwrap().is_empty());
    // while the Hub holds for a release nothing is flagged
    let idle = k.item("run", "执行", None);
    k.session(&idle, "idle");
    k.core.tx().set_hold(crate::occupancy::Hold::Release, true);
    assert!(k.core.tx().check_stalls(later()).unwrap().is_empty());
    k.core.tx().set_hold(crate::occupancy::Hold::Release, false);
    assert_eq!(k.core.tx().check_stalls(later()).unwrap(), [idle]);
}

#[test]
fn a_turn_starting_expires_the_card_and_continue_sends_a_turn() {
    let k = Kit::new();
    let i = k.item("run", "执行", None);
    let s = k.session(&i, "idle");
    k.core.tx().check_stalls(later()).unwrap();
    let d = stall_cards(&k, &i)[0].id.clone();
    k.events.lock().unwrap().clear();
    // 让它继续: the answer goes to the session
    k.core.resolve(&d, "continue", "", "me").unwrap();
    assert_eq!(
        k.rt.prompts_of(&s).last().unwrap(),
        "请继续。完成后用 steve_deliver 交付；需要决定时用 steve_ask。"
    );
    assert_eq!(k.status(&i).0, "run");
    // stalls again, then a turn starts some other way (the Hub calls expire_stall as the turn starts)
    k.rt.end_turn(&k.core, &s, "cancelled", None);
    k.core.tx().check_stalls(later()).unwrap();
    assert_eq!(
        stall_cards(&k, &i)
            .iter()
            .filter(|d| d.status == "open")
            .count(),
        1
    );
    k.core.expire_stall(&s).unwrap();
    assert!(stall_cards(&k, &i).iter().all(|d| d.status != "open"));
    assert_eq!(k.status(&i).0, "run");
    assert!(
        k.events
            .lock()
            .unwrap()
            .iter()
            .any(|e| matches!(e, HubEvent::Decision { .. })),
        "the web hears about it"
    );
}

#[test]
fn drop_gives_the_item_up() {
    let k = Kit::new();
    let i = k.item("run", "执行", None);
    let s = k.session(&i, "idle");
    k.core.tx().check_stalls(later()).unwrap();
    let d = stall_cards(&k, &i)[0].id.clone();
    k.core.resolve(&d, "drop", "", "me").unwrap();
    assert_eq!(k.status(&i).0, "drop");
    assert_eq!(k.db().session(&s).unwrap().unwrap().status, "ended");
}
