//! `steve_lane_status`: what the calling item's release request became, read from the lane's rows and
//! cards (or the item's release card), in each state an agent may find it in.

use steve_store::lanes::NewRequest;

use crate::lane::status::{LaneStatus, Next};
use crate::lane::store;
use crate::repo::{NewDecision, WS};
use crate::testkit::Kit;

const UP: &str = "github.com/x/y";

/// An item with a session; the session's status.
fn agent(k: &Kit) -> (String, steve_store::Session) {
    let i = k.item("run", "收尾", None);
    let sid = k.session(&i, "idle");
    (i.clone(), k.db().session(&sid).unwrap().unwrap())
}

/// `item`'s request into the lane of (UP, main).
fn enqueue(k: &Kit, item: &str) -> steve_store::lanes::LaneRequest {
    let branch = format!("steve/{item}");
    store::request(
        k.db().store(),
        UP,
        "main",
        &NewRequest {
            lane_id: "",
            workspace_id: Some(WS),
            item_id: Some(item),
            source: "item",
            dir: "/nonexistent",
            branch: &branch,
            head: "abc123",
            repo: None,
        },
    )
    .unwrap()
    .0
}

fn status(k: &Kit, s: &steve_store::Session) -> LaneStatus {
    k.core.lane_status(s).unwrap()
}

#[test]
fn nothing_requested_says_to_publish_when_accepted() {
    let k = Kit::new();
    let (i, s) = agent(&k);
    let st = status(&k, &s);
    assert_eq!(st.item_id.as_deref(), Some(i.as_str()));
    assert_eq!(
        (st.state, st.next, st.path),
        ("not_requested", Next::Publish, None)
    );
    assert!(st.batch.is_none() && st.request.is_none());
    let text = st.render();
    assert!(text.contains("没有调用过 steve_publish"), "{text}");
    assert!(text.contains("\"state\": \"not_requested\""), "{text}");
}

#[test]
fn a_request_in_a_collecting_batch_and_one_queued_behind_it() {
    let k = Kit::new();
    store::set_policy(k.db().store(), UP, "main", None, Some(2)).unwrap();
    let (a, sa) = agent(&k);
    let (b, sb) = agent(&k);
    let (c, sc) = agent(&k);
    let (d, sd) = agent(&k);
    let ra = enqueue(&k, &a);
    // A goes straight through; before its card is made Steve is checking the target's rules
    let st = status(&k, &sa);
    assert_eq!((st.state, st.next), ("checking", Next::Wait));
    store::approved(k.db().store(), &ra.lane_id, 1).unwrap();
    assert_eq!(status(&k, &sa).state, "merging");
    // B and C collect in MQ-2 (at most two), D waits behind it
    enqueue(&k, &b);
    enqueue(&k, &c);
    enqueue(&k, &d);
    let st = status(&k, &sc);
    assert_eq!((st.state, st.next), ("collecting", Next::Wait));
    let batch = st.batch.as_ref().unwrap();
    assert_eq!(batch.id, "MQ-2");
    assert_eq!(batch.branch, "steve/merge/main/MQ-2");
    assert_eq!(batch.target, format!("{UP} main"));
    assert_eq!((batch.position, batch.size), (Some(2), 2));
    assert_eq!(batch.others, [format!("{b}（steve/{b}）")]);
    assert!(batch.closes_at.is_some(), "the window's end");
    assert_eq!(status(&k, &sb).batch.unwrap().position, Some(1));
    let st = status(&k, &sd);
    assert_eq!((st.state, st.queue_position), ("queued", Some(1)));
    assert!(st.batch.is_none());
    assert!(st.summary.contains("排队"), "{}", st.summary);
    assert!(st.advice.contains("不要问用户"), "{}", st.advice);
}

#[test]
fn a_batch_waiting_on_its_card_names_the_card_and_who_answers() {
    let k = Kit::new();
    let (a, sa) = agent(&k);
    let r = enqueue(&k, &a);
    store::ask(k.db().store(), &r.lane_id, 1).unwrap();
    let st = status(&k, &sa);
    assert_eq!((st.state, st.next), ("waiting_for_approval", Next::Wait));
    let card = st.pending_card.as_ref().expect("the card");
    assert_eq!(card.question, "合并 MQ-1（1 项）");
    assert_eq!(
        (card.kind.as_str(), card.answer_by.as_str()),
        ("approve", "user")
    );
    assert_eq!(st.batch.as_ref().unwrap().id, "MQ-1");
    assert!(st.summary.contains("等用户批准合并"), "{}", st.summary);
}

#[test]
fn a_request_sent_back_says_why_and_to_fix_it() {
    let k = Kit::new();
    let (a, sa) = agent(&k);
    let r = enqueue(&k, &a);
    store::left(k.db().store(), &r.lane_id, 1, &r.id, "冲突：src/x.rs").unwrap();
    let st = status(&k, &sa);
    assert_eq!((st.state, st.next), ("left", Next::Fix));
    assert_eq!(st.failure.as_ref().unwrap().reason, "冲突：src/x.rs");
    assert!(st.advice.contains("steve_deliver"), "{}", st.advice);
}

#[test]
fn a_failed_release_waits_on_its_card_then_a_release_says_the_version() {
    let k = Kit::new();
    let (a, sa) = agent(&k);
    let r = enqueue(&k, &a);
    let s = k.db().store();
    store::approved(s, &r.lane_id, 1).unwrap();
    store::merged(s, &r.lane_id, 1).unwrap();
    let st = status(&k, &sa);
    assert_eq!(st.state, "releasing");
    assert_eq!(st.merged.as_ref().unwrap().into, "main");
    store::release_failed(s, &r.lane_id, 1, "CI 没通过：gh run rerun 7 --failed").unwrap();
    k.db()
        .decision_create(NewDecision {
            workspace_id: WS.into(),
            kind: "fail".into(),
            subject_type: "delivery_batch".into(),
            subject_id: format!("lane:{}:1", r.lane_id),
            question: "MQ-1 已合并，但发布失败".into(),
            options: Some(vec!["retry".into(), "drop".into()]),
            requested_by: "steve".into(),
            ..Default::default()
        })
        .unwrap();
    let st = status(&k, &sa);
    assert_eq!((st.state, st.next), ("release_failed", Next::Wait));
    assert_eq!(
        st.failure.as_ref().unwrap().reason,
        "CI 没通过：gh run rerun 7 --failed"
    );
    let card = st.pending_card.as_ref().unwrap();
    assert_eq!((card.kind.as_str(), card.options.len()), ("fail", 2));
    assert_eq!(st.batch.as_ref().unwrap().state, "release_failed");
    // retried and released
    store::released(s, &r.lane_id, 1, "ok", Some("0.9.1")).unwrap();
    let st = status(&k, &sa);
    assert_eq!((st.state, st.next), ("released", Next::Nothing));
    let m = st.merged.as_ref().unwrap();
    assert_eq!(
        (m.into.as_str(), m.commit.as_deref(), m.version.as_deref()),
        ("main", Some("abc123"), Some("0.9.1"))
    );
    assert!(st.summary.contains("v0.9.1"), "{}", st.summary);
}

#[test]
fn a_release_card_of_a_project_with_its_own_script() {
    let k = Kit::new();
    let (i, s) = agent(&k);
    let d = k
        .db()
        .decision_create(NewDecision {
            workspace_id: WS.into(),
            kind: "approve".into(),
            subject_type: "release".into(),
            subject_id: "steve/st-1".into(),
            item_id: Some(i.clone()),
            session_id: Some(s.id.clone()),
            question: "发布新版本（P · 分支 steve/st-1）".into(),
            options: Some(vec!["yes".into(), "no".into()]),
            requested_by: "dev".into(),
            ..Default::default()
        })
        .unwrap();
    let st = status(&k, &s);
    assert_eq!(
        (st.path, st.state, st.next),
        (Some("release"), "waiting_for_approval", Next::Wait)
    );
    assert_eq!(st.pending_card.as_ref().unwrap().decision_id, d.id);
    k.db()
        .decision_resolve(
            &d.id,
            serde_json::json!({"approved": true, "outcome": "failed", "error": "健康检查没通过", "log": "/tmp/release-1.log"}),
            "resolved",
            None,
        )
        .unwrap();
    let st = status(&k, &s);
    assert_eq!((st.state, st.next), ("failed", Next::Fix));
    let f = st.failure.as_ref().unwrap();
    assert_eq!(
        (f.reason.as_str(), f.log.as_deref()),
        ("健康检查没通过", Some("/tmp/release-1.log"))
    );
}
