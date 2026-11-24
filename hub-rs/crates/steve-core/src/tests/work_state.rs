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

/// An agent that stops waiting for something only you can do is yours: Steve does not continue it
/// (it would only take the step as done), and YOLO meets hard limit 7 on the card.
#[test]
fn an_agent_waiting_for_your_own_action_is_not_continued() {
    let k = kit(2);
    let i = k.item("run", "执行", None);
    let s = k.session(&i, "idle");
    k.rt.prompt(&s, "做事", "me", &[]).unwrap();
    k.rt.say(&s, "代码已改好。需要你本人在手机上确认登录，完成后告诉我。");
    k.rt.end_turn(&k.core, &s, "end_turn", None);
    assert_eq!(k.rt.prompts_of(&s).len(), 1, "not continued");
    assert!(steve_lines(&k, &i).is_empty());
    assert_eq!(k.open_fails(&i).len(), 1, "the card is yours");
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

/// ST-1/2/3: a 429 that says the quota is back in hours. Continuing at once fails within a second
/// and spends the continues; the item waits until then instead, and the agent goes on after.
#[test]
fn a_quota_that_is_back_later_is_waited_for_not_continued() {
    let k = kit(2);
    let i = k.item("run", "执行", None);
    let s = k.session(&i, "idle");
    k.rt.prompt(&s, "做吧", "me", &[]).unwrap();
    k.rt.end_turn(
        &k.core,
        &s,
        "error",
        Some("API Error: 429 · upstream account quota exhausted; try again in 2h"),
    );
    assert_eq!(k.rt.prompts_of(&s).len(), 1, "not continued at once");
    assert!(steve_lines(&k, &i).is_empty(), "no continue spent");
    assert!(k.open_fails(&i).is_empty(), "no card");

    // the watch leaves it while the quota is out
    assert!(k.core.tx().check_stalls(later()).unwrap().is_empty());
    assert_eq!(k.rt.prompts_of(&s).len(), 1);
    // the time has come: continued once, still nothing spent
    let back = now() + 2 * 3_600_000 + 60_000;
    assert!(k.core.tx().check_stalls(back).unwrap().is_empty());
    let prompts = k.rt.prompts_of(&s);
    assert_eq!(prompts.len(), 2);
    assert!(prompts[1].contains("配额已恢复"), "{}", prompts[1]);
    assert!(steve_lines(&k, &i).is_empty());

    // a quota with no time stays an ordinary error: continued within the budget
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

/// ST-25 #2 at the turn end: a request to scan at the start of a long final reply, streamed in many
/// pieces and not ending in a question, is still a wait on you. Steve does not continue it.
#[test]
fn a_long_final_reply_asking_you_to_scan_is_not_continued() {
    let k = kit(2);
    let i = k.item("run", "执行", None);
    let s = k.session(&i, "idle");
    k.rt.prompt(&s, "做事", "me", &[]).unwrap();
    k.rt.say(&s, "请扫码授权，完成后告诉我。");
    for _ in 0..120 {
        k.rt.say(&s, "阅读说明。");
    }
    k.rt.say(&s, "我在这里等着");
    k.rt.end_turn(&k.core, &s, "end_turn", None);
    assert_eq!(k.rt.prompts_of(&s).len(), 1, "not continued");
    assert!(steve_lines(&k, &i).is_empty());
    assert_eq!(k.open_fails(&i).len(), 1, "the card is yours");
    // and the stall watch later on does not continue it either
    assert!(k.core.tx().check_stalls(later()).unwrap().is_empty());
    assert_eq!(k.rt.prompts_of(&s).len(), 1);

    // what it said before its last tool call is not what it waits for now
    let i = k.item("run", "执行", None);
    let s = k.session(&i, "idle");
    k.rt.prompt(&s, "做事", "me", &[]).unwrap();
    k.rt.say(&s, "看到文档里写着请扫码授权。");
    k.db()
        .event_add(&s, steve_transcript::TOOL, json!({"tool": {"id": "t1"}}))
        .unwrap();
    k.rt.say(&s, "改好了一半，接着做。");
    k.rt.end_turn(&k.core, &s, "end_turn", None);
    assert_eq!(k.rt.prompts_of(&s).len(), 2, "continued by Steve");
}

/// ST-25 #8 at the turn end: an English request to sign in and say when done, with no question mark.
#[test]
fn an_english_request_to_log_in_is_not_continued() {
    let k = kit(2);
    for said in [
        "Please log in to your account, then reply Done.",
        "Please log in to GitHub and tell me when done.",
    ] {
        let i = k.item("run", "执行", None);
        let s = k.session(&i, "idle");
        k.rt.prompt(&s, "do it", "me", &[]).unwrap();
        k.rt.say(&s, said);
        k.rt.end_turn(&k.core, &s, "end_turn", None);
        assert_eq!(k.rt.prompts_of(&s).len(), 1, "{said}: not continued");
        assert_eq!(k.open_fails(&i).len(), 1, "{said}: the card is yours");
    }
}

/// ST-25 #9 at the turn end: words about codes in a report are no wait on you; Steve continues it.
#[test]
fn a_report_about_codes_is_continued() {
    let k = kit(2);
    let i = k.item("run", "执行", None);
    let s = k.session(&i, "idle");
    k.rt.prompt(&s, "做事", "me", &[]).unwrap();
    k.rt.say(
        &s,
        "验证码有效期改成 120 秒了，OTP 位数和二维码组件也换好了，测试通过。",
    );
    k.rt.end_turn(&k.core, &s, "end_turn", None);
    assert_eq!(k.rt.prompts_of(&s).len(), 2, "continued by Steve");
    assert!(k.open_fails(&i).is_empty());
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
    core.session_message(&s, "接着做", false, &[]).unwrap();
    assert_eq!(k.rt.prompts_of(&s).len(), 2);
    k.rt.say(&s, "改了一半");
    k.rt.end_turn(&core, &s, "end_turn", None);
    let kept = k.db().store().sessions().stop(&s).unwrap().unwrap();
    assert!(!crate::turn_wait::yours(&kept), "{kept:?}");
    assert_eq!(k.rt.prompts_of(&s).len(), 3, "continued by Steve");
    assert_eq!(steve_lines(&k, &i).len(), 1);
}

/// ST-37 #1 at the turn end and Steve's continue (takeover): asked to type a code before going on,
/// or to use a passkey, the agent waits on you. Steve spends no continue on it and the stall watch
/// does not either; a report of how the code step was built is continued as usual.
#[test]
fn a_code_to_type_or_a_passkey_is_not_continued() {
    let k = kit(2);
    for said in [
        "输入验证码后继续",
        "Use your passkey",
        "登录页已打开。Use your passkey to sign in, then reply.",
    ] {
        let i = k.item("run", "执行", None);
        let s = k.session(&i, "idle");
        k.rt.prompt(&s, "做事", "me", &[]).unwrap();
        k.rt.say(&s, said);
        k.rt.end_turn(&k.core, &s, "end_turn", None);
        assert_eq!(k.rt.prompts_of(&s).len(), 1, "{said}: not continued");
        assert!(steve_lines(&k, &i).is_empty(), "{said}");
        assert_eq!(k.open_fails(&i).len(), 1, "{said}: the card is yours");
        assert!(k.core.tx().check_stalls(later()).unwrap().is_empty());
        assert_eq!(k.rt.prompts_of(&s).len(), 1, "{said}");
    }
    let i = k.item("run", "执行", None);
    let s = k.session(&i, "idle");
    k.rt.prompt(&s, "做事", "me", &[]).unwrap();
    k.rt.say(
        &s,
        "已实现：用户输入验证码后继续登录，passkey 也支持了，测试通过。",
    );
    k.rt.end_turn(&k.core, &s, "end_turn", None);
    assert_eq!(k.rt.prompts_of(&s).len(), 2, "continued by Steve");
    assert!(k.open_fails(&i).is_empty());
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

    core.session_message(&s, "接着做", false, &[]).unwrap();
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
    k.core.session_message(&s, "换个做法", false, &[]).unwrap();
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
    k.core.session_message(&s, "接着做", false, &[]).unwrap();
    k.rt.say(&s, "改了一半");
    k.rt.end_turn(&k.core, &s, "end_turn", None);
    let kept = k.db().store().sessions().stop(&s).unwrap().unwrap();
    assert!(!crate::turn_wait::held(&kept), "{kept:?}");
    assert_eq!(k.rt.prompts_of(&s).len(), 3, "continued by Steve");
}

/// ST-37 #6: the agent asked you to scan, then a tool call made that moot (a local substitute, no
/// authorization needed), and the turn ended with nothing said after the call. Its final reply is
/// empty: the obsolete request is not read as a wait on you, Steve continues it. A long request
/// streamed after the last tool call is read whole and stays yours.
#[test]
fn no_final_text_after_tool_does_not_revive_obsolete_request() {
    let k = kit(2);
    let i = k.item("run", "执行", None);
    let s = k.session(&i, "idle");
    k.rt.prompt(&s, "做事", "me", &[]).unwrap();
    k.rt.say(&s, "请扫码授权，完成后告诉我。");
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

    // after the last tool call: a long request, streamed in pieces, read whole and yours
    let i = k.item("run", "执行", None);
    let s = k.session(&i, "idle");
    k.rt.prompt(&s, "做事", "me", &[]).unwrap();
    k.db()
        .event_add(&s, steve_transcript::TOOL, json!({"tool": {"id": "t1"}}))
        .unwrap();
    k.rt.say(&s, "请扫码授权，");
    k.rt.say(&s, "完成后告诉我。");
    for _ in 0..120 {
        k.rt.say(&s, "阅读说明。");
    }
    k.rt.say(&s, "我在这里等着");
    let reply = k.db().final_reply(&s).unwrap();
    assert!(
        reply.starts_with("请扫码授权，完成后告诉我。") && reply.ends_with("我在这里等着"),
        "{}",
        reply.len()
    );
    k.rt.end_turn(&k.core, &s, "end_turn", None);
    assert_eq!(k.rt.prompts_of(&s).len(), 1, "not continued");
    assert_eq!(k.open_fails(&i).len(), 1, "the card is yours");

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
    k.rt.say(&s, "请扫码授权，完成后告诉我。");
    k.db()
        .event_add(
            &s,
            steve_transcript::TOOL,
            json!({"tool": {"id": "t9", "status": "completed"}}),
        )
        .unwrap();
    assert_eq!(
        k.db().final_reply(&s).unwrap(),
        "请扫码授权，完成后告诉我。"
    );
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
