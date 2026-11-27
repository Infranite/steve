//! Items left in `run` with nothing going on get one “它停下了” card (crate::stall).

use crate::AgentRuntime;
use crate::stall::{STALL, STALL_AFTER_MS};
use crate::testkit::Kit;
use crate::util::now;

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

/// A cancelled turn leaves the item `run` with an idle session: after two minutes it gets the card, once,
/// saying how the turn ended.
#[test]
fn a_stopped_item_gets_one_card_with_how_its_turn_ended() {
    let k = Kit::new();
    let i = k.item("run", "执行", None);
    let s = k.session(&i, "idle");
    k.rt.prompt(&s, "做事", "me", &[]).unwrap();
    k.rt.end_turn(&k.core, &s, "cancelled", None);
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
        ("fail", "它停下了：你中断了这一回合", "open")
    );
    assert_eq!(d.options, ["continue", "look", "drop"]);
    assert_eq!(d.session_id.as_deref(), Some(s.as_str()));
    assert!(!k.open_kinds(&i).is_empty(), "the item waits on the user");
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
    assert_eq!(k.item_label(&i), "run");
    // stalls again, then a turn starts some other way (the Hub calls expire_stall as the turn starts)
    k.rt.end_turn(&k.core, &s, "cancelled", None);
    k.core.tx().check_stalls(later()).unwrap();
    assert_eq!(
        stall_cards(&k, &i)
            .iter()
            .filter(|d| d.status == steve_types::DecisionStatus::Open)
            .count(),
        1
    );
    k.core.expire_stall(&s).unwrap();
    assert!(
        stall_cards(&k, &i)
            .iter()
            .all(|d| d.status != steve_types::DecisionStatus::Open)
    );
    assert_eq!(k.item_label(&i), "run");
    assert!(
        k.events.lock().unwrap().iter().any(|e| matches!(
            e,
            steve_types::BusEvent::Domain(steve_types::DomainEvent::Work(
                steve_types::WorkEvent::Decision { .. }
            ))
        )),
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
    assert_eq!(k.item_label(&i), "drop");
    assert_eq!(k.db().session(&s).unwrap().unwrap().status, "ended");
}

#[test]
fn turn_stall_stop_reason_is_not_attributed_to_user_in_stall_watch_or_diagnostics() {
    use steve_types::orchestration::{
        STOP_REASON_STALL_NO_PROGRESS, STOP_REASON_STALL_REPEATED_FAILURE,
    };

    for (stop_reason, expected_zh, expected_en) in [
        (
            STOP_REASON_STALL_NO_PROGRESS,
            "可观察执行时间内无可核实进展，Steve 已请求停止",
            "No verified progress within the observable window; Steve requested a stop",
        ),
        (
            STOP_REASON_STALL_REPEATED_FAILURE,
            "同一工具操作连续累计失败五次，Steve 已请求停止",
            "The same tool action failed five times; Steve requested a stop",
        ),
    ] {
        let k = Kit::new();
        let i = k.item("run", "执行", None);
        let s = k.session(&i, "idle");
        k.rt.prompt(&s, "做事", "me", &[]).unwrap();
        let prompts_before = k.rt.prompts_of(&s).len();
        k.rt.end_turn(&k.core, &s, stop_reason, None);

        // Must never be attributed to the user, and must not trigger Steve takeover on turn end.
        assert_eq!(k.rt.prompts_of(&s).len(), prompts_before);
        let kept = k.db().store().sessions().stop(&s).unwrap().unwrap();
        assert!(!kept.yours, "{kept:?}");
        assert!(!crate::turn_wait::yours(&kept), "{kept:?}");
        assert!(!crate::turn_wait::held(&kept), "{kept:?}");
        assert!(crate::turn_wait::stall_reason(&kept).is_some(), "{kept:?}");

        // Diagnostic snapshot reports real stall reason and yours=false in both zh-CN and en.
        let snap_zh = k.core.diagnostic_snapshot(&i).unwrap();
        assert_eq!(snap_zh["last_stop"]["yours"], false);
        assert_eq!(snap_zh["last_stop"]["reason"], stop_reason);
        assert_eq!(snap_zh["last_stop"]["why"], expected_zh);
        let snap_en = steve_i18n::with("en", || k.core.diagnostic_snapshot(&i).unwrap());
        assert_eq!(snap_en["last_stop"]["yours"], false);
        assert_eq!(snap_en["last_stop"]["reason"], stop_reason);
        assert_eq!(snap_en["last_stop"]["why"], expected_en);

        // Idle stall watch opens a card with the real stall reason (never "你中断了这一回合")
        // and does not auto-continue via takeover.
        assert_eq!(
            k.core.tx().check_stalls(later()).unwrap(),
            std::slice::from_ref(&i)
        );
        assert_eq!(k.rt.prompts_of(&s).len(), prompts_before);
        let d = &stall_cards(&k, &i)[0];
        assert_eq!(d.question, format!("它停下了：{expected_zh}"));
        assert!(!d.question.contains("你中断了这一回合"));
    }
}
