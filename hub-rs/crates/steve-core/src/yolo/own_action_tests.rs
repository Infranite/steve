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
    assert_eq!(t.kit.rt.prompts_of(&sid).len(), 1, "YOLO did not continue it");
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
    t.kit
        .rt
        .say(&sid, "OTP 和二维码登录都接好了。验证码有效期用60秒还是120秒？");
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
    t.kit
        .rt
        .say(&sid, "验证码有效期改成 120 秒了，二维码组件也换好了，测试通过。");
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
