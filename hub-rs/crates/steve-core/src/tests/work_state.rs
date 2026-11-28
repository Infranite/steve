//! One judgement of an item's work (`crate::turn_wait`) for the turn end, the stall watch and
//! Steve's continues: a stop card is never built from the session's last system line, user stops
//! survive restarts, and requests for user action are never blindly continued.

use serde_json::json;

use crate::stall::{STALL, STALL_AFTER_MS};
use crate::takeover::LINE;
use crate::testkit::Kit;
use crate::util::now;
use crate::{AgentRuntime, patch};

fn later() -> i64 {
    now() + STALL_AFTER_MS + 1_000
}

fn kit(continues: u32) -> Kit {
    Kit::with(|c| c.takeover_continues = continues)
}

fn steve_lines(k: &Kit, item: &str) -> Vec<String> {
    k.timeline_texts(item)
        .into_iter()
        .filter(|t| t.starts_with(LINE))
        .collect()
}

fn old_card(k: &Kit, item: &str, subject_type: &str, options: &[&str], question: &str) -> String {
    k.db()
        .decision_create(crate::repo::NewDecision {
            workspace_id: crate::repo::WS.into(),
            kind: "fail".into(),
            subject_type: subject_type.into(),
            subject_id: item.into(),
            item_id: Some(item.into()),
            question: question.into(),
            options: Some(options.iter().map(|s| s.to_string()).collect()),
            requested_by: "steve".into(),
            ..Default::default()
        })
        .unwrap()
        .id
}

fn restarted(k: &Kit) -> crate::Core {
    crate::Core::new(
        k.db().clone(),
        k.rt.clone(),
        k.gw.clone(),
        k.core.config().clone(),
    )
}

/// The runtime finished the session's turn but its end is not told yet (held up on the way).
fn over_untold(k: &Kit, sid: &str) {
    {
        let mut st = k.rt.st();
        st.turning.remove(sid);
        st.attempts.remove(sid);
    }
    k.db().session_patch(sid, patch![status: "idle"]).unwrap();
}

fn end_of(core: &crate::Core, sid: &str, attempt: &str, stop: &str) {
    core.on_turn_end(crate::TurnEnd {
        session_id: sid.into(),
        attempt: Some(attempt.into()),
        stop_reason: stop.into(),
        error: None,
    })
    .unwrap();
}

/// A card for an idle item says what the state is: an old release notice in the session is never
/// quoted as the stop reason.
#[test]
fn a_stop_card_never_quotes_an_old_system_line() {
    let k = kit(0);
    let i = k.item("run", "执行", None);
    let s = k.session(&i, "idle");
    k.db()
        .event_add(
            &s,
            "system",
            json!({"text": "已发布 v0.2.99：Release 构建通过"}),
        )
        .unwrap();
    assert_eq!(
        k.core.tx().check_stalls(later()).unwrap(),
        std::slice::from_ref(&i)
    );
    let q = k.open_fails(&i).pop().unwrap();
    assert_eq!(
        q,
        "它停下了：会话空闲：没有回合在运行、没有排队输入，也没有交付或提问"
    );
}

/// A registered prerequisite wait explains an idle item even before its status caught up.
#[test]
fn a_registered_dependency_wait_is_not_a_stall() {
    let k = kit(2);
    let item = k.item("run", "执行", None);
    let dep = k.item("run", "执行", None);
    let sid = k.session(&item, "idle");
    let s = k.db().session(&sid).unwrap().unwrap();
    k.core
        .tool_item_dependencies(
            &s,
            json!({"action":"wait","deps":[dep],"reason":"等前置验收","resume":"automatic"}),
        )
        .unwrap();
    assert!(k.core.tx().check_stalls(later()).unwrap().is_empty());
    assert!(k.rt.prompts_of(&sid).is_empty());
    assert!(k.open_fails(&item).is_empty());
}

/// 2026-11-12: the Node could not link for hours and input waited with only "queued" to show. What
/// waits on it says the Node is not linked and why, and stops saying so once it links.
#[test]
fn queued_input_is_left_for_its_session_when_the_node_is_offline() {
    let k = kit(2);
    let i = k.item("run", "执行", None);
    let s = k.session(&i, "idle");
    k.db().queue_add(&s, "继续", "me", &[]).unwrap();
    k.rt.st().node_offline = Some("offline".into());
    assert!(k.core.tx().check_stalls(later()).unwrap().is_empty());
    assert!(k.open_fails(&i).is_empty());
    assert_eq!(k.db().queued(&s).unwrap().len(), 1);
}

/// ST-1/2/3 + HANDOFF.md §2:
/// - A 429 whose reset is within the workspace wait threshold (default 10m) waits automatically
///   without opening a card or spending continues, and resumes once due.
/// - A 429 whose reset is beyond the threshold when all of the Bot's execution surfaces are
///   unavailable opens a coordinator card; choosing `"wait"` arms `quota_wait` and resumes when due.
/// - A bare 429 with no reset time and no fallbacks stays an ordinary error continued within budget.
#[test]
fn a_quota_that_is_back_later_is_waited_for_not_continued() {
    let k = kit(2);
    let i = k.item("run", "执行", None);
    let s = k.session(&i, "idle");
    k.rt.prompt(&s, "做吧", "me", &[]).unwrap();

    // 1. Short recovery (5m <= 10m threshold): waits automatically without card.
    k.rt.end_turn(
        &k.core,
        &s,
        "error",
        Some("API Error: 429 · upstream account quota exhausted; try again in 5m"),
    );
    assert_eq!(k.rt.prompts_of(&s).len(), 1, "not continued at once");
    assert!(steve_lines(&k, &i).is_empty(), "no continue spent");
    assert!(
        k.open_fails(&i).is_empty(),
        "short reset waits without card"
    );

    // the watch leaves it while the quota is out, then wakes once due
    assert!(k.core.tx().check_stalls(now() + 60_000).unwrap().is_empty());
    assert_eq!(k.rt.prompts_of(&s).len(), 1);
    let back_short = now() + 5 * 60_000 + 5_000;
    assert!(k.core.tx().check_stalls(back_short).unwrap().is_empty());
    let prompts = k.rt.prompts_of(&s);
    assert_eq!(prompts.len(), 2);
    assert!(prompts[1].contains("配额已恢复"), "{}", prompts[1]);
    assert!(steve_lines(&k, &i).is_empty());

    // 2. Long recovery (2h > 10m threshold) with no available fallback: opens coordinator card;
    // owner chooses "wait" -> arms quota_wait and wakes when due.
    k.rt.end_turn(
        &k.core,
        &s,
        "error",
        Some("API Error: 429 · upstream account quota exhausted; try again in 2h"),
    );
    let card = k
        .db()
        .decisions_of_item(&i)
        .unwrap()
        .into_iter()
        .find(|d| {
            d.status == steve_types::DecisionStatus::Open
                && d.subject_type == crate::HANDOFF_UNAVAILABLE_SUBJECT
        })
        .expect("coordinator card for long quota wait");
    assert!(
        card.options.contains(&"wait".to_string()),
        "{:?}",
        card.options
    );
    k.core.resolve(&card.id, "wait", "", "me").unwrap();
    assert!(k.open_fails(&i).is_empty());
    assert!(k.core.tx().check_stalls(later()).unwrap().is_empty());
    assert_eq!(k.rt.prompts_of(&s).len(), 2);
    let back_long = now() + 2 * 3_600_000 + 60_000;
    assert!(k.core.tx().check_stalls(back_long).unwrap().is_empty());
    let prompts = k.rt.prompts_of(&s);
    assert_eq!(prompts.len(), 3);
    assert!(prompts[2].contains("配额已恢复"), "{}", prompts[2]);
    assert!(steve_lines(&k, &i).is_empty());

    // 3. A bare 429 with no reset time and no fallbacks stays an ordinary error: continued within budget.
    k.rt.end_turn(
        &k.core,
        &s,
        "error",
        Some("API Error: 429 Too Many Requests"),
    );
    assert_eq!(steve_lines(&k, &i).len(), 1);
}

/// The look before a continue is spent does not take the item's own stop card for a wait: a voided
/// delivery still reaches an agent whose earlier stop left a card open.
#[test]
fn an_open_stop_card_does_not_hold_back_a_continue() {
    let k = kit(2);
    let i = k.item("run", "执行", None);
    let s = k.session(&i, "idle");
    old_card(
        &k,
        &i,
        STALL,
        &["continue", "look", "drop"],
        "它停下了：会话空闲",
    );
    k.core.tx().takeover_invalidated(&i, "计划已更新").unwrap();
    let p = k.rt.prompts_of(&s);
    assert_eq!(p.len(), 1, "continued");
    assert!(p[0].contains("交付已经作废"), "{p:?}");
    assert_eq!(steve_lines(&k, &i).len(), 1);
}

/// ST-25 #4: you stop an ordinary item's turn and the Hub restarts before the stall watch opens a
/// card. Your Stop is kept with the session, not in memory: after the restart the card is yours and
/// nothing continues it.
#[test]
fn your_stop_is_yours_after_a_restart() {
    let k = kit(2);
    let i = k.item("run", "执行", None);
    let s = k.session(&i, "idle");
    k.rt.prompt(&s, "做事", "me", &[]).unwrap();
    k.rt.say(&s, "在改代码");
    assert!(k.core.cancel_turn(&s).unwrap());
    k.rt.end_turn(&k.core, &s, "cancelled", None);
    assert!(k.open_fails(&i).is_empty(), "no card yet");

    let core = restarted(&k);
    core.recover().unwrap();
    assert_eq!(
        core.tx().check_stalls(later()).unwrap(),
        std::slice::from_ref(&i)
    );
    assert_eq!(k.rt.prompts_of(&s).len(), 1, "not continued");
    assert!(steve_lines(&k, &i).is_empty());
    let cards = k.open_fails(&i);
    assert_eq!(cards.len(), 1);
    assert!(cards[0].contains("你中断了这一回合"), "{cards:?}");
}

/// ST-25 #4: the end of an older turn that comes late does not take your Stop from you; a message
/// of yours starts another turn, and once that one ends without delivering Steve may go on again.
#[test]
fn a_late_old_turn_end_does_not_undo_your_stop_but_your_message_does() {
    let k = kit(2);
    let i = k.item("run", "执行", None);
    let s = k.session(&i, "idle");
    k.rt.prompt(&s, "做事", "me", &[]).unwrap();
    assert!(k.core.cancel_turn(&s).unwrap());
    k.rt.end_turn(&k.core, &s, "cancelled", None);
    // an earlier attempt's end, late
    k.core
        .on_turn_end(crate::TurnEnd {
            session_id: s.clone(),
            attempt: Some("a-old".into()),
            stop_reason: "end_turn".into(),
            error: None,
        })
        .unwrap();
    let kept = k.db().store().sessions().stop(&s).unwrap().unwrap();
    assert!(crate::turn_wait::yours(&kept), "{kept:?}");
    assert_eq!(k.rt.prompts_of(&s).len(), 1, "not continued");
    let core = restarted(&k);
    assert_eq!(
        core.tx().check_stalls(later()).unwrap(),
        std::slice::from_ref(&i)
    );
    assert_eq!(k.rt.prompts_of(&s).len(), 1, "not continued");
    let card = k
        .db()
        .decisions_of_item(&i)
        .unwrap()
        .into_iter()
        .find(|d| d.status == steve_types::DecisionStatus::Open)
        .unwrap();
    assert!(card.question.contains("你中断了这一回合"));

    // your own message: a new turn; it ends without delivering, and Steve continues it
    core.session_message(&s, "接着做", false, &[], "me")
        .unwrap();
    assert_eq!(k.rt.prompts_of(&s).len(), 2);
    k.rt.say(&s, "改了一半");
    k.rt.end_turn(&core, &s, "end_turn", None);
    let kept = k.db().store().sessions().stop(&s).unwrap().unwrap();
    assert!(!crate::turn_wait::yours(&kept), "{kept:?}");
    assert_eq!(k.rt.prompts_of(&s).len(), 3, "continued by Steve");
    assert_eq!(steve_lines(&k, &i).len(), 1);
}

/// ST-37 #2 at the Core: the same end of the same attempt told again after a restart (the same
/// SQLite file opened afresh) is the same end: your Stop stays, the stall watch gives you the card
/// and Steve continues nothing. Your new message lifts it.
#[test]
fn a_replayed_end_after_a_restart_keeps_your_stop() {
    let k = kit(2);
    let i = k.item("run", "执行", None);
    let s = k.session(&i, "idle");
    k.rt.prompt(&s, "做事", "me", &[]).unwrap();
    let a = k.rt.running(&s).unwrap().attempt;
    assert!(k.core.cancel_turn(&s).unwrap());
    // your Stop and the turn's completion race: it ends end_turn
    k.rt.end_turn(&k.core, &s, "end_turn", None);
    assert!(k.db().store().sessions().stop(&s).unwrap().unwrap().yours);

    let core = k.reopened();
    core.recover().unwrap();
    end_of(&core, &s, &a, "end_turn");
    let kept = core.store().store().sessions().stop(&s).unwrap().unwrap();
    assert!(kept.yours, "{kept:?}");
    assert_eq!(k.rt.prompts_of(&s).len(), 1, "not continued");
    core.tx().check_stalls(later()).unwrap();
    assert_eq!(k.rt.prompts_of(&s).len(), 1, "not continued");
    assert!(steve_lines(&k, &i).is_empty());

    core.session_message(&s, "接着做", false, &[], "me")
        .unwrap();
    k.rt.say(&s, "改了一半");
    k.rt.end_turn(&core, &s, "end_turn", None);
    assert!(!crate::turn_wait::held(
        &core.store().store().sessions().stop(&s).unwrap().unwrap()
    ));
    assert_eq!(k.rt.prompts_of(&s).len(), 3, "continued by Steve");
}

/// ST-37 #3: turn A's end is held up on the way; your message starts B and you stop B, which
/// is already idle when A's end comes, then B's. A's late end does not use up the Stop you asked
/// for B, nor becomes the session's last end; B's own end does, and it is yours.
#[test]
fn old_end_must_not_consume_new_stop_request() {
    let k = kit(2);
    let i = k.item("run", "执行", None);
    let s = k.session(&i, "idle");
    k.rt.prompt(&s, "做事", "me", &[]).unwrap();
    let a = k.rt.running(&s).unwrap().attempt;
    over_untold(&k, &s);
    k.core
        .session_message(&s, "换个做法", false, &[], "me")
        .unwrap();
    let b = k.rt.running(&s).expect("B runs").attempt;
    assert_ne!(a, b);
    assert!(k.core.cancel_turn(&s).unwrap());
    // B is idle by now (busy → idle), its end not told yet either
    over_untold(&k, &s);
    let prompts = k.rt.prompts_of(&s).len();

    end_of(&k.core, &s, &a, "end_turn");
    let mid = k.db().store().sessions().stop(&s).unwrap().unwrap();
    assert_eq!(mid.stop_asked.as_deref(), Some(b.as_str()), "{mid:?}");
    assert_ne!(mid.attempt.as_deref(), Some(a.as_str()), "{mid:?}");
    assert_eq!(
        k.rt.prompts_of(&s).len(),
        prompts,
        "A's end continues nothing"
    );
    assert!(k.open_fails(&i).is_empty(), "nor opens a card");

    end_of(&k.core, &s, &b, "end_turn");
    let after = k.db().store().sessions().stop(&s).unwrap().unwrap();
    assert_eq!(
        (
            after.attempt.as_deref(),
            after.stop_reason.as_deref(),
            after.yours,
            after.stop_asked.as_deref()
        ),
        (Some(b.as_str()), Some("end_turn"), true, None),
        "{after:?}"
    );
    assert_eq!(
        k.rt.prompts_of(&s).len(),
        prompts,
        "your Stop: not continued"
    );
    assert!(steve_lines(&k, &i).is_empty());

    // the order reversed after a restart changes nothing: A's end, told again, is the same end
    let core = k.reopened();
    end_of(&core, &s, &a, "end_turn");
    let again = core.store().store().sessions().stop(&s).unwrap().unwrap();
    assert_eq!(again.attempt.as_deref(), Some(b.as_str()));
    assert!(again.yours);
}

/// ST-37 #3 contrast: a Stop asked for one turn, then your message starts another before any end
/// came. That newer turn's end is not yours: it uses the old request up and is judged as usual.
#[test]
fn a_newer_turns_end_after_your_message_is_not_your_stop() {
    let k = kit(2);
    let i = k.item("run", "执行", None);
    let s = k.session(&i, "idle");
    k.rt.prompt(&s, "做事", "me", &[]).unwrap();
    assert!(k.core.cancel_turn(&s).unwrap());
    over_untold(&k, &s);
    k.core
        .session_message(&s, "接着做", false, &[], "me")
        .unwrap();
    k.rt.say(&s, "改了一半");
    k.rt.end_turn(&k.core, &s, "end_turn", None);
    let kept = k.db().store().sessions().stop(&s).unwrap().unwrap();
    assert!(!crate::turn_wait::held(&kept), "{kept:?}");
    assert_eq!(k.rt.prompts_of(&s).len(), 3, "continued by Steve");
}

/// ST-37 #6: an agent's final reply ends at tool calls (unless an update targets an earlier call)
/// and streams cleanly across pieces without reviving text from before the last tool call.
#[test]
fn no_final_text_after_tool_does_not_revive_obsolete_text() {
    let k = kit(2);
    let i = k.item("run", "执行", None);
    let s = k.session(&i, "idle");
    k.rt.prompt(&s, "做事", "me", &[]).unwrap();
    k.rt.say(&s, "准备先检查旧状态。");
    k.db()
        .event_add(
            &s,
            steve_transcript::TOOL,
            json!({"tool": {"id": "t1", "status": "completed", "title": "改用本地替代，无需授权"}}),
        )
        .unwrap();
    assert_eq!(k.db().final_reply(&s).unwrap(), "");
    k.rt.end_turn(&k.core, &s, "end_turn", None);
    assert_eq!(k.rt.prompts_of(&s).len(), 2, "continued by Steve");
    assert!(k.open_fails(&i).is_empty());

    // after the last tool call: a long reply streamed in pieces is read whole
    let i = k.item("run", "执行", None);
    let s = k.session(&i, "idle");
    k.rt.prompt(&s, "做事", "me", &[]).unwrap();
    k.db()
        .event_add(&s, steve_transcript::TOOL, json!({"tool": {"id": "t1"}}))
        .unwrap();
    k.rt.say(&s, "第一段说明，");
    k.rt.say(&s, "第二段结论。");
    for _ in 0..120 {
        k.rt.say(&s, "阅读说明。");
    }
    k.rt.say(&s, "末尾摘要");
    let reply = k.db().final_reply(&s).unwrap();
    assert!(
        reply.starts_with("第一段说明，第二段结论。") && reply.ends_with("末尾摘要"),
        "{}",
        reply.len()
    );

    // an update of a call begun before the reply, coming after it, does not hide the reply
    let i = k.item("run", "执行", None);
    let s = k.session(&i, "idle");
    k.rt.prompt(&s, "做事", "me", &[]).unwrap();
    k.db()
        .event_add(
            &s,
            steve_transcript::TOOL,
            json!({"tool": {"id": "t9", "status": "in_progress"}}),
        )
        .unwrap();
    k.rt.say(&s, "请确认是否继续。");
    k.db()
        .event_add(
            &s,
            steve_transcript::TOOL,
            json!({"tool": {"id": "t9", "status": "completed"}}),
        )
        .unwrap();
    assert_eq!(k.db().final_reply(&s).unwrap(), "请确认是否继续。");
}

/// Your Stop whose outcome the backend could not tell (its answer an error): the Stop stays kept,
/// never weakened into an ordinary stop Steve continues. The idle session is yours at the stall
/// watch, and the turn's end when it comes is yours too. A Stop the backend says reached nothing
/// (the turn was over) is withdrawn: the next end is judged as usual.
#[test]
fn a_stop_with_an_unknown_outcome_stays_yours() {
    let k = kit(2);
    let i = k.item("run", "执行", None);
    let s = k.session(&i, "idle");
    k.rt.prompt(&s, "做事", "me", &[]).unwrap();
    let a = k.rt.running(&s).unwrap().attempt;
    k.rt.st().user_stop_error = true;
    assert!(k.core.cancel_turn(&s).is_err());
    let kept = k.db().store().sessions().stop(&s).unwrap().unwrap();
    assert_eq!(kept.stop_asked.as_deref(), Some(a.as_str()), "kept");
    // the turn is gone without an end told: the stall watch gives you the card
    over_untold(&k, &s);
    assert_eq!(
        k.core.tx().check_stalls(later()).unwrap(),
        std::slice::from_ref(&i)
    );
    assert_eq!(k.rt.prompts_of(&s).len(), 1, "not continued");
    assert!(steve_lines(&k, &i).is_empty());
    let cards = k.open_fails(&i);
    assert!(cards[0].contains("你要求停止这一回合"), "{cards:?}");
    // its end comes after all: yours
    end_of(&k.core, &s, &a, "end_turn");
    assert!(k.db().store().sessions().stop(&s).unwrap().unwrap().yours);
    assert_eq!(k.rt.prompts_of(&s).len(), 1, "not continued");

    // a Stop that reached nothing is withdrawn
    let i = k.item("run", "执行", None);
    let s = k.session(&i, "idle");
    k.rt.st().user_stop_error = false;
    assert!(!k.core.cancel_turn(&s).unwrap());
    assert_eq!(
        k.db()
            .store()
            .sessions()
            .stop(&s)
            .unwrap()
            .unwrap()
            .stop_asked,
        None
    );
    k.rt.prompt(&s, "做事", "me", &[]).unwrap();
    k.rt.say(&s, "改了一半");
    k.rt.end_turn(&k.core, &s, "end_turn", None);
    assert_eq!(k.rt.prompts_of(&s).len(), 2, "continued by Steve");
}
