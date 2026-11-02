//! YOLO Y3: `steve_ask` answered for the user with the fake gateway — a confident answer reaches the
//! session with the fixed prefix, an explicit objection or a low-confidence answer without a
//! recommendation stays with the user, a question about pushing never goes to the model, a repeated
//! question stays, and “which project” moves the item into the project.

use serde_json::json;

use super::engine_tests::t;
use crate::gateway::Tier;
use crate::repo::WS;

const OPTS: [&str; 2] = ["A", "B"];

fn reply(act: &str, answer: &str, option: Option<&str>, confidence: f64, reason: &str) -> String {
    json!({ "act": act, "answer": answer, "option": option, "confidence": confidence,
            "reason": reason, "assumptions": [] })
    .to_string()
}

#[test]
fn a_confident_answer_reaches_the_session_with_the_prefix() {
    let t = t();
    let (item, sid, _) = t.item();
    let r = reply("answer", "用 A", Some("A"), 0.9, "目标里写了要简单");
    t.kit.gw.set(move |_, tier| {
        assert_eq!(tier, Tier::Smart);
        Ok(r.clone())
    });
    let d = t.card(
        &item,
        &sid,
        "ask",
        "question",
        "用 A 还是 B？",
        "推荐：B\n理由：更快",
        &OPTS,
    );
    let a = t.engine.consider(&d).unwrap().unwrap();
    assert_eq!(
        (a.outcome.as_str(), a.rule.as_str()),
        ("decided", "ask.main")
    );
    assert_eq!(a.choice.as_deref(), Some("A"));
    let sent = t.kit.rt.prompts_of(&sid);
    assert_eq!(
        sent.last().map(String::as_str),
        Some(
            "（YOLO：Steve 代你回答）用 A。依据：目标里写了要简单。如果这个假设后来证明不对，在交付摘要里写明。"
        )
    );
    let dd = t.decision(&d);
    assert_eq!(dd.result["answer"], "A");
    assert_eq!(dd.result["decided_by"], "yolo");
    assert!(
        t.kit
            .timeline_texts(&item)
            .contains(&"YOLO 代答：A".to_string())
    );
    // the prompt carried the question, the options and the agent's leaning
    let calls = t.kit.gw.calls.lock().unwrap().clone();
    assert_eq!(
        calls.len(),
        1,
        "asked once, also for the check right before answering"
    );
    assert!(calls[0].contains("问题：用 A 还是 B？"));
    assert!(calls[0].contains("Agent 推荐：B（理由：更快）"));
}

#[test]
fn an_objection_or_low_confidence_without_recommendation_stays_with_you() {
    let t = t();
    let (item, sid, _) = t.item();
    // the model says ask the user (not for lack of information): the recommendation is not taken
    let r = reply("ask_user", "", None, 0.9, "要看用户的偏好");
    t.kit.gw.set(move |_, _| Ok(r.clone()));
    let d = t.card(
        &item,
        &sid,
        "ask",
        "question",
        "用哪个配色？",
        "推荐：A\n理由：好看",
        &OPTS,
    );
    let a = t.engine.consider(&d).unwrap().unwrap();
    assert_eq!(
        (a.outcome.as_str(), a.rule.as_str()),
        ("declined", "ask.unsure")
    );
    assert_eq!(a.reason, "YOLO 没替你答：要看用户的偏好");
    assert_eq!(t.decision(&d).status, "open");

    // low confidence and no recommendation
    let (item, sid, _) = t.item();
    let r = reply("answer", "用 B", Some("B"), 0.4, "猜的");
    t.kit.gw.set(move |_, _| Ok(r.clone()));
    let d = t.card(&item, &sid, "ask", "question", "表格还是列表？", "", &OPTS);
    let a = t.engine.consider(&d).unwrap().unwrap();
    assert_eq!(a.outcome, "declined");
    assert_eq!(a.reason, "YOLO 没替你答：猜的");

    // lack of information only: the agent's recommendation is taken
    let (item, sid, _) = t.item();
    let r = reply("ask_user", "", None, 0.2, "信息不足：没写偏好");
    t.kit.gw.set(move |_, _| Ok(r.clone()));
    let d = t.card(
        &item,
        &sid,
        "ask",
        "question",
        "先做哪个？",
        "推荐：B\n理由：依赖少",
        &OPTS,
    );
    let a = t.engine.consider(&d).unwrap().unwrap();
    assert_eq!(
        (a.outcome.as_str(), a.rule.as_str(), a.choice.as_deref()),
        ("decided", "ask.recommended", Some("B"))
    );
    assert!(
        t.kit
            .rt
            .prompts_of(&sid)
            .last()
            .unwrap()
            .starts_with("（YOLO：Steve 代你回答）B。依据：依赖少。")
    );

    // no usable reply: ambiguous, stays with you even with a recommendation
    let (item, sid, _) = t.item();
    t.kit.gw.set(|_, _| Ok("我觉得都行".into()));
    let d = t.card(&item, &sid, "ask", "question", "改名吗？", "推荐：A", &OPTS);
    let a = t.engine.consider(&d).unwrap().unwrap();
    assert_eq!(a.outcome, "declined");
    assert_eq!(t.decision(&d).status, "open");
}

#[test]
fn a_question_about_pushing_never_reaches_the_model() {
    let t = t();
    let (item, sid, _) = t.item();
    t.kit
        .gw
        .set(|_, _| panic!("no model call for a hard-limited question"));
    let d = t.card(
        &item,
        &sid,
        "ask",
        "question",
        "要推送到远端吗？",
        "推荐：A",
        &["推送", "先不推"],
    );
    let a = t.engine.consider(&d).unwrap().unwrap();
    assert_eq!(a.outcome, "declined");
    assert!(a.rule.starts_with("limit."), "{}", a.rule);
    assert!(a.reason.starts_with("硬限制"));
    // an answer that walks outside is not sent either
    let (item, sid, _) = t.item();
    let r = reply("answer", "直接 git push 到 main", None, 0.95, "快");
    t.kit.gw.set(move |_, _| Ok(r.clone()));
    let d = t.card(&item, &sid, "ask", "question", "接下来怎么办？", "", &[]);
    let a = t.engine.consider(&d).unwrap().unwrap();
    assert_eq!(a.outcome, "declined");
    assert_eq!(t.decision(&d).status, "open");
}

#[test]
fn the_same_question_twice_stays_with_you() {
    let t = t();
    let (item, sid, _) = t.item();
    let r = reply("answer", "用 A", Some("A"), 0.9, "目标");
    t.kit.gw.set(move |_, _| Ok(r.clone()));
    let d1 = t.card(&item, &sid, "ask", "question", "用 A 还是 B？", "", &OPTS);
    assert_eq!(t.engine.consider(&d1).unwrap().unwrap().outcome, "decided");
    let d2 = t.card(&item, &sid, "ask", "question", "用  A 还是 B?", "", &OPTS);
    let a = t.engine.consider(&d2).unwrap().unwrap();
    assert_eq!(
        (a.outcome.as_str(), a.rule.as_str()),
        ("declined", "ask.repeated")
    );
    assert_eq!(t.kit.gw.calls.lock().unwrap().len(), 1);
}

#[test]
fn which_project_moves_the_item_and_starts_a_new_session() {
    let t = t();
    let dir = t.kit.home().join("front");
    std::fs::create_dir_all(&dir).unwrap();
    let p = t
        .kit
        .db()
        .project_create(
            WS,
            "前端",
            "网页",
            &dir.to_string_lossy(),
            "dir",
            false,
            Some("dev"),
            None,
        )
        .unwrap()
        .id;
    let (item, sid, _) = t.item();
    let r = reply("answer", "前端", Some("前端"), 0.9, "目标说的是网页");
    t.kit.gw.set(move |_, _| Ok(r.clone()));
    let d = t.card(
        &item,
        &sid,
        "ask",
        "question",
        "这件事属于哪个项目？",
        "",
        &["前端", "不需要"],
    );
    let a = t.engine.consider(&d).unwrap().unwrap();
    assert_eq!(a.outcome, "decided");
    let i = t.kit.db().item(&item).unwrap().unwrap();
    assert_eq!(i.project_id.as_deref(), Some(p.as_str()));
    assert!(
        t.kit
            .timeline_texts(&item)
            .iter()
            .any(|x| x == "关联到项目 前端，开新会话")
    );
    let sessions = t.kit.db().sessions_of_item(&item).unwrap();
    assert!(
        sessions.iter().any(|s| s.id != sid && s.status != "ended"),
        "a new session"
    );
}

#[test]
fn steve_ask_writes_the_recommendation_on_the_card() {
    let t = t();
    let (item, sid, _) = t.item();
    let s = t.kit.db().session(&sid).unwrap().unwrap();
    t.kit
        .core
        .tool_ask_with(
            &s,
            "用 A 还是 B？",
            &["A".into(), "B".into()],
            Some("A"),
            Some("简单"),
        )
        .unwrap();
    let d = t
        .kit
        .db()
        .decisions_of_item(&item)
        .unwrap()
        .into_iter()
        .find(|d| d.kind == "ask")
        .unwrap();
    assert_eq!(d.draft, "推荐：A\n理由：简单");
}
