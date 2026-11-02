//! Thread routing storage (migration 016, R0): route_log once per message, corrections counted once,
//! research runs and their interruption at boot, conversation items and the one-time backfill.

mod common;

use common::*;
use serde_json::json;
use steve_store::*;

fn with_steve() -> T {
    let t = fresh();
    t.store
        .bots()
        .seed(
            "steve",
            true,
            &NewBot {
                name: "Steve".into(),
                harness: "claude-code".into(),
                ..Default::default()
            },
            WS,
        )
        .unwrap();
    t
}

fn item(s: &Store, title: &str, status: &str, source: (&str, &str)) -> String {
    s.items()
        .create(
            WS,
            &NewItem {
                title: title.into(),
                goal: title.into(),
                bot_id: "steve".into(),
                status: Some(status.into()),
                source: Some((source.0.into(), source.1.into())),
                ..Default::default()
            },
        )
        .unwrap()
        .unwrap()
        .id
}

fn say(s: &Store, author: &str, text: &str, item: Option<&str>) -> String {
    let id = s
        .messages()
        .add(WS, author, text, item, &serde_json::Value::Null, "steve")
        .unwrap();
    std::thread::sleep(std::time::Duration::from_millis(2));
    id
}

#[test]
fn a_message_is_routed_once_and_corrected_once() {
    let t = with_steve();
    let th = t.store.thread();
    let n = NewRouteLog {
        workspace_id: WS.into(),
        message_id: "msg_1".into(),
        model: Some("claude-haiku-5-5".into()),
        model_kind: "item".into(),
        kind: "research".into(),
        confidence: Some(0.6),
        signals: vec!["question".into()],
        guard: Some("4".into()),
        ms: 1200,
        ..Default::default()
    };
    let a = th.route_add(&n).unwrap();
    assert!(a.id.starts_with("rt_"));
    assert_eq!(a.signals, vec!["question"]);
    let b = th
        .route_add(&NewRouteLog {
            kind: "item".into(),
            ..n.clone()
        })
        .unwrap();
    assert_eq!(a, b, "the first routing stays");
    th.route_reply("msg_1", "msg_2", None).unwrap();
    assert!(th.route_override("msg_1", "to_item", Some("ST-1")).unwrap());
    assert!(!th.route_override("msg_1", "to_inline", None).unwrap());
    th.route_outcome("msg_1", "offer_taken", None).unwrap();
    let r = th.route_of("msg_1").unwrap().unwrap();
    assert_eq!(r.reply_id.as_deref(), Some("msg_2"));
    assert_eq!(r.override_.as_deref(), Some("to_item"));
    assert!(r.override_at.is_some());
    assert_eq!(r.item_id.as_deref(), Some("ST-1"));
    assert_eq!(r.outcome.as_deref(), Some("offer_taken"));
    let j = serde_json::to_value(&r).unwrap();
    assert_eq!(j["override"], "to_item");
    assert_eq!(th.routes(WS, 0, i64::MAX).unwrap().len(), 1);
    assert!(th.routes(WS, 0, 1).unwrap().is_empty());
}

#[test]
fn runs_move_through_their_states_and_are_interrupted_at_boot() {
    let t = with_steve();
    let th = t.store.thread();
    let r = th
        .run_add(&NewThreadRun {
            workspace_id: WS.into(),
            message_id: "msg_1".into(),
            ..Default::default()
        })
        .unwrap();
    assert!(r.id.starts_with("tr_"));
    assert_eq!(r.status, "queued");
    let r = th
        .run_patch(
            &r.id,
            &RunPatch {
                status: Some("running".into()),
                token_hash: Some(Some("h".into())),
                started: Some(5),
                ..Default::default()
            },
        )
        .unwrap()
        .unwrap();
    assert_eq!(
        (r.status.as_str(), r.token_hash.as_deref()),
        ("running", Some("h"))
    );
    let done = th
        .run_add(&NewThreadRun {
            workspace_id: WS.into(),
            message_id: "msg_2".into(),
            ..Default::default()
        })
        .unwrap();
    th.run_patch(
        &done.id,
        &RunPatch {
            status: Some("done".into()),
            tools: Some(4),
            token_hash: Some(None),
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(th.runs_active(WS).unwrap().len(), 1);
    let gone = th.runs_interrupt(true).unwrap();
    assert_eq!(gone.len(), 1);
    assert_eq!(gone[0].status, "interrupted");
    assert_eq!(gone[0].token_hash, None);
    assert!(th.runs_active(WS).unwrap().is_empty());
    assert_eq!(th.run(&done.id).unwrap().unwrap().tools, 4);
    assert_eq!(th.runs_of_message("msg_2").unwrap().len(), 1);
}

#[test]
fn conversation_items_keep_their_origin_and_go_with_the_item() {
    let t = with_steve();
    let s = &t.store;
    let i = item(s, "改按钮", "run", ("对话", "chat"));
    let ti = s.thread().item_add(&i, WS, "msg_a", None, true).unwrap();
    assert!(ti.follow && ti.from_question);
    s.thread().item_add(&i, WS, "msg_b", None, false).unwrap();
    s.thread().item_reply(&i, "msg_r").unwrap();
    assert!(s.thread().item_follow(&i, false).unwrap());
    s.thread().item_posted(&i, 9).unwrap();
    let ti = s.thread().item(&i).unwrap().unwrap();
    assert_eq!(
        (
            ti.origin_msg.as_str(),
            ti.reply_msg.as_deref(),
            ti.follow,
            ti.last_post
        ),
        ("msg_a", Some("msg_r"), false, Some(9))
    );
    assert_eq!(s.thread().items(WS).unwrap().len(), 1);
    s.items().remove(&i).unwrap();
    assert!(s.thread().item(&i).unwrap().is_none());
}

#[test]
fn backfill_links_unfinished_conversation_items_once() {
    let t = with_steve();
    let s = &t.store;
    let q1 = say(s, "me", "事项页面太散了", None);
    let open = item(s, "事项页布局", "run", ("对话", "chat"));
    let r1 = say(s, "steve", "好，交给它了", Some(&open));
    say(s, "me", "删掉菜单", None);
    let done = item(s, "删菜单", "done", ("对话", "chat"));
    say(s, "steve", "好", Some(&done));
    let manual = item(s, "手建", "run", ("手动", "manual"));
    let q3 = say(s, "me", "讲一下知识系统", None);
    let waiting = item(s, "知识系统", "wait", ("对话", "chat"));
    let r3 = say(s, "steve", "整理成事项", Some(&waiting));
    say(s, "steve", "已发布", Some(&waiting));

    let mut linked = s.thread().backfill(WS).unwrap();
    linked.sort();
    let mut want = vec![open.clone(), waiting.clone()];
    want.sort();
    assert_eq!(linked, want);
    let a = s.thread().item(&open).unwrap().unwrap();
    assert_eq!((a.origin_msg, a.reply_msg), (q1, Some(r1)));
    let b = s.thread().item(&waiting).unwrap().unwrap();
    assert_eq!((b.origin_msg, b.reply_msg), (q3, Some(r3)));
    assert!(s.thread().item(&done).unwrap().is_none());
    assert!(s.thread().item(&manual).unwrap().is_none());
    assert_eq!(
        s.settings().get(WS, BACKFILL_KEY, json!(null)).unwrap(),
        json!("v1")
    );

    let before = s.thread().items(WS).unwrap();
    assert!(s.thread().backfill(WS).unwrap().is_empty());
    assert_eq!(
        s.thread().items(WS).unwrap(),
        before,
        "running it again changes nothing"
    );
}
