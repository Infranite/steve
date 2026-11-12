//! Hard limit 7 end to end (ST-25, the cases ST-23 found on ST-10's second round): what only the user
//! can do, or a claim that they did, is never answered or continued for them, whichever value YOLO
//! would submit and however long the agent's final reply is; an ordinary design question about
//! codes, OTPs or QR codes is answered as any other.

use serde_json::json;

use super::engine_tests::t;
use super::judge::CONTINUE_STALL;
use crate::AgentRuntime;

fn reply(act: &str, answer: &str, option: Option<&str>, confidence: f64) -> String {
    json!({ "act": act, "answer": answer, "option": option, "confidence": confidence,
            "reason": "按目标", "assumptions": [] })
    .to_string()
}

/// #1: the model answers “the first one” in words but names the option claiming you confirmed on
/// your phone; that option is what would be submitted, so it is checked, not only the words.
#[test]
fn an_option_claiming_you_confirmed_is_never_submitted() {
    let t = t();
    let (item, sid, _) = t.item();
    let r = reply("answer", "第一项", Some("用户已经在手机上确认过了"), 0.99);
    t.kit.gw.set(move |_, _| Ok(r.clone()));
    let d = t.card(
        &item,
        &sid,
        "ask",
        "question",
        "选择下一步",
        "",
        &["用户已经在手机上确认过了", "取消"],
    );
    let a = t.engine.consider(&d).unwrap().unwrap();
    assert_eq!(
        (a.outcome.as_str(), a.rule.as_str()),
        ("declined", "limit.7"),
        "{a:?}"
    );
    assert!(a.reason.starts_with("硬限制"), "{}", a.reason);
    assert_eq!(t.decision(&d).status, "open");
    assert!(t.kit.rt.prompts_of(&sid).is_empty(), "nothing submitted");

    // the same card with an ordinary option is answered with that option
    let (item, sid, _) = t.item();
    let r = reply("answer", "第一项", Some("继续实现"), 0.99);
    t.kit.gw.set(move |_, _| Ok(r.clone()));
    let d = t.card(
        &item,
        &sid,
        "ask",
        "question",
        "选择下一步",
        "",
        &["继续实现", "取消"],
    );
    let a = t.engine.consider(&d).unwrap().unwrap();
    assert_eq!(
        (a.outcome.as_str(), a.rule.as_str(), a.choice.as_deref()),
        ("decided", "ask.main", Some("继续实现"))
    );
}

/// #1, a question left in plain text (a stall with `asked`): the model's answer claiming you did it
/// is not sent as a continue.
#[test]
fn a_plain_text_question_is_not_answered_with_a_claim_that_you_did_it() {
    let t = t();
    let (item, sid, _) = t.item();
    t.kit.rt.prompt(&sid, "做事", "me", &[]).unwrap();
    t.kit.rt.say(&sid, "代码已改好。下一步做哪个？");
    let r = reply("answer", "用户已经在手机上确认过了，直接发版", None, 0.99);
    t.kit.gw.set(move |_, _| Ok(r.clone()));
    let d = t.card(
        &item,
        &sid,
        "fail",
        "item",
        "Agent 停下来了，没有交付也没有提问",
        "",
        &["continue", "delivered", "look"],
    );
    let before = t.kit.rt.prompts_of(&sid).len();
    let a = t.engine.consider(&d).unwrap().unwrap();
    assert_eq!(
        (a.outcome.as_str(), a.rule.as_str()),
        ("declined", "limit.7"),
        "{a:?}"
    );
    assert_eq!(t.kit.rt.prompts_of(&sid).len(), before, "not continued");
}

/// #2: the request to scan comes first, a long explanation after it, and the reply does not end in
/// a question; streamed in many pieces. The turn end leaves the card to you, and YOLO, reading the
/// whole final reply, does not continue it either.
#[test]
fn a_long_reply_after_a_request_to_scan_is_still_yours() {
    let t = t();
    let (item, sid, _) = t.item();
    t.kit.rt.prompt(&sid, "做事", "me", &[]).unwrap();
    t.kit.rt.say(&sid, "请扫码授权，完成后告诉我。");
    for _ in 0..120 {
        t.kit.rt.say(&sid, "阅读说明。");
    }
    t.kit.rt.say(&sid, "我在这里等着");
    t.kit.rt.end_turn(&t.kit.core, &sid, "end_turn", None);
    let d = t
        .kit
        .db()
        .decisions_of_item(&item)
        .unwrap()
        .into_iter()
        .find(|d| d.status == "open" && d.kind == "fail")
        .expect("the turn end left a card")
        .id;
    let a = t.engine.consider(&d).unwrap().unwrap();
    assert_eq!(
        (a.outcome.as_str(), a.rule.as_str()),
        ("declined", "limit.7"),
        "{a:?}"
    );
    assert_eq!(
        t.kit.rt.prompts_of(&sid).len(),
        1,
        "YOLO did not continue it"
    );
    assert_eq!(t.decision(&d).status, "open");

    // the same length of text with nothing asked of you is continued as usual
    let (item, sid, _) = t.item();
    t.kit.rt.prompt(&sid, "做事", "me", &[]).unwrap();
    for _ in 0..120 {
        t.kit.rt.say(&sid, "阅读说明。");
    }
    t.kit.rt.end_turn(&t.kit.core, &sid, "end_turn", None);
    let d = t
        .kit
        .db()
        .decisions_of_item(&item)
        .unwrap()
        .into_iter()
        .find(|d| d.status == "open" && d.kind == "fail")
        .unwrap()
        .id;
    let a = t.engine.consider(&d).unwrap().unwrap();
    assert_eq!(
        (a.outcome.as_str(), a.rule.as_str()),
        ("decided", "continue.stall")
    );
    assert_eq!(
        t.kit.rt.prompts_of(&sid).last().map(String::as_str),
        Some(CONTINUE_STALL)
    );
}

/// #8: an ordinary English request to sign in and say when it is done, with no question mark: the
/// turn end leaves it to you, YOLO does not continue it, and a card asking it is not answered.
#[test]
fn an_english_request_to_log_in_is_yours() {
    for said in [
        "Please log in to your account, then reply Done.",
        "Please log in to GitHub and tell me when done.",
    ] {
        let t = t();
        let (item, sid, _) = t.item();
        t.kit.rt.prompt(&sid, "do it", "me", &[]).unwrap();
        t.kit.rt.say(&sid, said);
        t.kit.rt.end_turn(&t.kit.core, &sid, "end_turn", None);
        let d = t
            .kit
            .db()
            .decisions_of_item(&item)
            .unwrap()
            .into_iter()
            .find(|d| d.status == "open" && d.kind == "fail")
            .expect("the turn end left a card")
            .id;
        let a = t.engine.consider(&d).unwrap().unwrap();
        assert_eq!(
            (a.outcome.as_str(), a.rule.as_str()),
            ("declined", "limit.7"),
            "{said}: {a:?}"
        );
        assert_eq!(t.kit.rt.prompts_of(&sid).len(), 1, "{said}");

        // asked on a card
        let (item, sid, _) = t.item();
        t.kit
            .gw
            .set(|_, _| panic!("no model call for what only you can do"));
        let d = t.card(&item, &sid, "ask", "question", said, "", &["Done", "Later"]);
        let a = t.engine.consider(&d).unwrap().unwrap();
        assert_eq!(
            (a.outcome.as_str(), a.rule.as_str()),
            ("declined", "limit.7"),
            "{said}"
        );
    }
}

/// #9: how long a verification code lives is an ordinary design choice: answered like any other, on
/// a card and left in plain text. Asking you to type a code stays yours.
#[test]
fn a_design_question_about_codes_is_answered_as_usual() {
    let t = t();
    let (item, sid, _) = t.item();
    let r = reply("answer", "120 秒", Some("120 秒"), 0.9);
    t.kit.gw.set(move |_, _| Ok(r.clone()));
    let d = t.card(
        &item,
        &sid,
        "ask",
        "question",
        "验证码有效期用60秒还是120秒？",
        "",
        &["60 秒", "120 秒"],
    );
    let a = t.engine.consider(&d).unwrap().unwrap();
    assert_eq!(
        (a.outcome.as_str(), a.rule.as_str(), a.choice.as_deref()),
        ("decided", "ask.main", Some("120 秒"))
    );

    // left in plain text at a turn end: a question, so yours from Steve, and YOLO answers it
    let (item, sid, _) = t.item();
    t.kit.rt.prompt(&sid, "做事", "me", &[]).unwrap();
    t.kit.rt.say(
        &sid,
        "OTP 和二维码登录都接好了。验证码有效期用60秒还是120秒？",
    );
    t.kit.rt.end_turn(&t.kit.core, &sid, "end_turn", None);
    let d = t
        .kit
        .db()
        .decisions_of_item(&item)
        .unwrap()
        .into_iter()
        .find(|d| d.status == "open" && d.kind == "fail")
        .expect("a question is yours at the turn end")
        .id;
    let r = reply("answer", "用 120 秒", None, 0.9);
    t.kit.gw.set(move |_, _| Ok(r.clone()));
    let a = t.engine.consider(&d).unwrap().unwrap();
    assert_eq!(
        (a.outcome.as_str(), a.rule.as_str()),
        ("decided", "continue.stall"),
        "{a:?}"
    );

    // a statement about codes is no wait on you: YOLO continues it as any other stop
    let (item, sid, _) = t.item();
    t.kit.rt.prompt(&sid, "做事", "me", &[]).unwrap();
    t.kit.rt.say(
        &sid,
        "验证码有效期改成 120 秒了，二维码组件也换好了，测试通过。",
    );
    t.kit.rt.end_turn(&t.kit.core, &sid, "end_turn", None);
    let d = t
        .kit
        .db()
        .decisions_of_item(&item)
        .unwrap()
        .into_iter()
        .find(|d| d.status == "open" && d.kind == "fail")
        .expect("Steve does not continue here (no continues configured)")
        .id;
    let a = t.engine.consider(&d).unwrap().unwrap();
    assert_eq!(
        (a.outcome.as_str(), a.rule.as_str()),
        ("decided", "continue.stall"),
        "{a:?}"
    );

    // asking you for the code itself stays yours
    let (item, sid, _) = t.item();
    t.kit
        .gw
        .set(|_, _| panic!("no model call for what only you can do"));
    let d = t.card(
        &item,
        &sid,
        "ask",
        "question",
        "请输入短信验证码",
        "",
        &["已输入", "稍后"],
    );
    let a = t.engine.consider(&d).unwrap().unwrap();
    assert_eq!(
        (a.outcome.as_str(), a.rule.as_str()),
        ("declined", "limit.7")
    );
}

/// ST-25 #3/#4: the card of a turn you stopped yourself is not continued by YOLO either; your Stop is
/// read from the session's kept stop, not from the card's words.
#[test]
fn a_turn_you_stopped_is_not_continued_by_yolo() {
    let t = t();
    let (item, sid, _) = t.item();
    t.kit.rt.prompt(&sid, "做事", "me", &[]).unwrap();
    assert!(t.kit.core.cancel_turn(&sid).unwrap());
    t.kit.rt.end_turn(&t.kit.core, &sid, "cancelled", None);
    let later = crate::util::now() + crate::stall::STALL_AFTER_MS + 1_000;
    assert_eq!(
        t.kit.core.tx().check_stalls(later).unwrap(),
        std::slice::from_ref(&item)
    );
    let d = t
        .kit
        .db()
        .decisions_of_item(&item)
        .unwrap()
        .into_iter()
        .find(|d| d.status == "open")
        .unwrap()
        .id;
    let a = t.engine.consider(&d).unwrap().unwrap();
    assert_eq!(
        (a.outcome.as_str(), a.rule.as_str()),
        ("declined", "continue.yours"),
        "{a:?}"
    );
    assert_eq!(t.kit.rt.prompts_of(&sid).len(), 1, "not continued");
}

/// The open card the turn end left on `item`.
fn turn_end_card(t: &super::engine_tests::T, item: &str) -> String {
    t.kit
        .db()
        .decisions_of_item(item)
        .unwrap()
        .into_iter()
        .find(|d| d.status == "open" && d.kind == "fail")
        .expect("the turn end left a card")
        .id
}

/// ST-37 #1 (ST-34): “输入验证码后继续” and “Use your passkey” ask for what only you can do. Left in
/// plain text at a turn end, YOLO does not continue them; asked on a card, it does not answer them.
/// A report on how the code step was built, or a design question about codes, is answered and
/// continued as usual.
#[test]
fn a_code_to_type_or_a_passkey_is_yours_in_yolo() {
    for said in ["输入验证码后继续", "Use your passkey"] {
        let t = t();
        let (item, sid, _) = t.item();
        t.kit
            .gw
            .set(|_, _| panic!("no model call for what only you can do"));
        t.kit.rt.prompt(&sid, "做事", "me", &[]).unwrap();
        t.kit.rt.say(&sid, said);
        t.kit.rt.end_turn(&t.kit.core, &sid, "end_turn", None);
        let d = turn_end_card(&t, &item);
        let a = t.engine.consider(&d).unwrap().unwrap();
        assert_eq!(
            (a.outcome.as_str(), a.rule.as_str()),
            ("declined", "limit.7"),
            "{said}: {a:?}"
        );
        assert_eq!(t.kit.rt.prompts_of(&sid).len(), 1, "{said}: not continued");
        assert_eq!(t.decision(&d).status, "open");

        // asked on a card
        let (item, sid, _) = t.item();
        let d = t.card(
            &item,
            &sid,
            "ask",
            "question",
            said,
            "",
            &["已完成", "稍后"],
        );
        let a = t.engine.consider(&d).unwrap().unwrap();
        assert_eq!(
            (a.outcome.as_str(), a.rule.as_str()),
            ("declined", "limit.7"),
            "{said}"
        );
        assert!(
            t.kit.rt.prompts_of(&sid).is_empty(),
            "{said}: nothing submitted"
        );
    }

    // a report of the code step: continued as any other stop
    let t = t();
    let (item, sid, _) = t.item();
    t.kit.rt.prompt(&sid, "做事", "me", &[]).unwrap();
    t.kit.rt.say(
        &sid,
        "已实现：用户输入验证码后继续登录，passkey 也支持了，测试通过。",
    );
    t.kit.rt.end_turn(&t.kit.core, &sid, "end_turn", None);
    let d = turn_end_card(&t, &item);
    let a = t.engine.consider(&d).unwrap().unwrap();
    assert_eq!(
        (a.outcome.as_str(), a.rule.as_str()),
        ("decided", "continue.stall"),
        "{a:?}"
    );
    assert_eq!(t.kit.rt.prompts_of(&sid).len(), 2);

    // a design question about the code step: answered
    let (item, sid, _) = t.item();
    let r = reply("answer", "输入后自动提交", Some("输入后自动提交"), 0.9);
    t.kit.gw.set(move |_, _| Ok(r.clone()));
    let d = t.card(
        &item,
        &sid,
        "ask",
        "question",
        "输入验证码后的校验放在前端还是后端？要不要支持用 passkey 登录？",
        "",
        &["输入后自动提交", "点按钮提交"],
    );
    let a = t.engine.consider(&d).unwrap().unwrap();
    assert_eq!(
        (a.outcome.as_str(), a.rule.as_str()),
        ("decided", "ask.main"),
        "{a:?}"
    );
}

/// ST-37 #2 (ST-34): your Stop and the turn's own completion race: the turn ends `end_turn`
/// after you asked to stop it, so its end is yours. The Hub restarts (the same SQLite file opened
/// afresh, nothing kept in memory) and the same end of the same attempt is told again. That replay
/// is the same end: it neither takes your Stop from you nor is judged again, and YOLO declines to
/// continue the card. Your own message afterwards starts another turn; its end is an ordinary stop.
#[test]
fn replay_after_restart_must_preserve_stop() {
    let t = t();
    let (item, sid, _) = t.item();
    t.kit.rt.prompt(&sid, "做事", "me", &[]).unwrap();
    let a = t.kit.rt.running(&sid).unwrap();
    assert!(t.kit.core.cancel_turn(&sid).unwrap());
    // the turn completed before the stop reached it
    t.kit.rt.say(&sid, "改了一半");
    t.kit.rt.end_turn(&t.kit.core, &sid, "end_turn", None);
    let kept = t.kit.db().store().sessions().stop(&sid).unwrap().unwrap();
    assert!(kept.yours, "{kept:?}");
    let d = turn_end_card(&t, &item);

    let core = t.kit.reopened();
    core.recover().unwrap();
    core.on_turn_end(crate::TurnEnd {
        session_id: sid.clone(),
        attempt: Some(a.attempt.clone()),
        stop_reason: "end_turn".into(),
        error: None,
    })
    .unwrap();
    let kept = core.store().store().sessions().stop(&sid).unwrap().unwrap();
    assert!(
        kept.yours && kept.attempt.as_deref() == Some(a.attempt.as_str()),
        "the replay kept your Stop: {kept:?}"
    );
    // YOLO on the restarted core: the card is yours, not continued
    let yolo = super::Yolo::with_clock(core.store().clone(), None, t.kit.home(), t.clock.clone());
    let engine = super::engine::Engine::new(core.clone(), yolo);
    let r = engine.consider(&d).unwrap().unwrap();
    assert_eq!(
        (r.outcome.as_str(), r.rule.as_str()),
        ("declined", "continue.yours"),
        "{r:?}"
    );
    assert_eq!(t.kit.rt.prompts_of(&sid).len(), 1, "not continued");
    assert_eq!(t.decision(&d).status, "open");
    // your own message: another turn, whose end is an ordinary stop that YOLO continues
    core.session_message(&sid, "接着做", false, &[]).unwrap();
    assert_eq!(t.kit.rt.prompts_of(&sid).len(), 2);
    t.kit.rt.say(&sid, "又改了一些");
    t.kit.rt.end_turn(&core, &sid, "end_turn", None);
    let kept = core.store().store().sessions().stop(&sid).unwrap().unwrap();
    assert!(!crate::work::held(&kept), "{kept:?}");
    let d = t
        .kit
        .db()
        .decisions_of_item(&item)
        .unwrap()
        .into_iter()
        .find(|x| x.status == "open" && x.kind == "fail" && x.id != d)
        .expect("a card for the new stop")
        .id;
    // you were here a moment ago (your message): YOLO waits until you are away (the fake clock
    // started with the test, the message carries the real time)
    t.clock.set(steve_store::now() + 181_000);
    let r = engine.consider(&d).unwrap().unwrap();
    assert_eq!(
        (r.outcome.as_str(), r.rule.as_str()),
        ("decided", "continue.stall"),
        "{r:?}"
    );
    assert_eq!(
        t.kit.rt.prompts_of(&sid).last().map(String::as_str),
        Some(CONTINUE_STALL)
    );
}
