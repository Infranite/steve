//! YOLO Y6: the digest (windows → items, counts, cost), undo by kind (decision with B3, reanswer,
//! pause, rollback only for the newest release), the cool-down buttons, the `/汇报` line and the
//! `yolo` draft target (“开 YOLO 到明早 8 点，前端项目除外” → two operations, applied and undone).

use serde_json::{Value, json};
use steve_modules::changes::{self, Origin};
use steve_modules::testkit::{ME, caller};
use steve_modules::{DigestNotes, Modules, Registry};

use super::digest::{Notes, Query};
use super::engine_tests::{T, t};
use super::undo::CANNOT;
use crate::patch;
use crate::repo::WS;

/// An accepted delivery decided by YOLO; returns (item, session, decision, action).
fn accepted(t: &T) -> (String, String, String, String) {
    let (item, sid, _) = t.item();
    let d = t.card(
        &item,
        &sid,
        "accept",
        "item",
        "验收",
        "做完了\n\n检查：✓ cargo test",
        &["pass", "rework"],
    );
    let a = t.engine.consider(&d).unwrap().unwrap();
    assert_eq!(
        (a.outcome.as_str(), a.rule.as_str()),
        ("decided", "accept.checks")
    );
    (item, sid, d, a.id)
}

#[test]
fn the_digest_groups_by_window_and_item_with_counts_and_cost() {
    let t = t();
    let (item, sid, _, _) = accepted(&t);
    t.kit.db().session_patch(&sid, patch![cost: 1.5]).unwrap();
    let (item2, sid2, _) = t.item();
    let d = t.card(
        &item2,
        &sid2,
        "ask",
        "question",
        "要推送到远端吗？",
        "",
        &["推", "不推"],
    );
    t.engine.consider(&d).unwrap();
    let v = t.engine.digest(WS, &Query::default()).unwrap();
    let w = &v["windows"][0];
    assert_eq!(w["counts"], json!({"decided": 1, "declined": 1}));
    assert_eq!(w["summary"], "验收 1");
    assert_eq!(w["cost"], 1.5);
    let items = w["items"].as_array().unwrap();
    assert_eq!(items.len(), 2);
    assert_eq!(items[0]["item_id"], item.as_str());
    assert_eq!(items[0]["actions"][0]["can_undo"], "decision");
    assert_eq!(items[0]["actions"][0]["question"], "验收");
    assert_eq!(items[1]["actions"][0]["can_undo"], Value::Null);
    // filters
    let only = t
        .engine
        .digest(
            WS,
            &Query {
                window: Some("current".into()),
                outcome: Some("declined".into()),
                ..Default::default()
            },
        )
        .unwrap();
    assert_eq!(only["windows"][0]["items"].as_array().unwrap().len(), 1);
}

#[test]
fn undoing_a_decision_reopens_it_and_counts_for_b3() {
    let t = t();
    let (item, _, d, a) = accepted(&t);
    let row = t.engine.undo(WS, ME, &a, None).unwrap();
    assert!(row.undone.is_some());
    assert_eq!(t.decision(&d).status, "open");
    assert!(
        t.kit
            .timeline_texts(&item)
            .contains(&"撤销了决定".to_string())
    );
    // the reopened card stays with the user
    assert!(t.engine.consider(&d).unwrap().is_none());
    assert_eq!(
        t.engine.undo(WS, ME, &a, None).unwrap_err().to_string(),
        CANNOT
    );
    // a second undo within the hour trips B3
    let (_, _, _, a2) = accepted(&t);
    t.engine.undo(WS, ME, &a2, None).unwrap();
    assert_eq!(t.paused().expect("B3")["code"], "B3");
    let audits: i64 = t
        .yolo
        .store()
        .reader(|c| {
            c.query_row(
                "SELECT count(*) FROM audit WHERE action = 'yolo.undo'",
                [],
                |r| r.get(0),
            )
            .map_err(steve_modules::ApiError::from)
        })
        .unwrap();
    assert_eq!(audits, 2);
}

#[test]
fn reanswer_sends_a_correction_and_pause_stops_the_item() {
    let t = t();
    let (item, sid, _) = t.item();
    let r = json!({"act": "answer", "answer": "用 A", "option": "A", "confidence": 0.9, "reason": "目标"}).to_string();
    t.kit.gw.set(move |_, _| Ok(r.clone()));
    let d = t.card(
        &item,
        &sid,
        "ask",
        "question",
        "A 还是 B？",
        "",
        &["A", "B"],
    );
    let a = t.engine.consider(&d).unwrap().unwrap();
    assert_eq!(t.engine.undo_kind(&a).as_deref(), Some("reanswer"));
    assert!(
        t.engine
            .undo(WS, ME, &a.id, None)
            .unwrap_err()
            .to_string()
            .contains("text")
    );
    t.kit.rt.end_turn(&t.kit.core, &sid, "end_turn", None);
    t.engine.undo(WS, ME, &a.id, Some("用 B")).unwrap();
    let last = t.kit.rt.prompts_of(&sid).last().unwrap().clone();
    assert!(
        last.starts_with("（更正）用户改了答案：用 B。之前 Steve 替用户回答的是：A。"),
        "{last}"
    );

    // a stopped agent continued by YOLO: “暂停这件”
    let (item, sid, _) = t.item();
    let f = t.card(
        &item,
        &sid,
        "fail",
        "item",
        "Agent 停下来了，没有交付也没有提问",
        "",
        &["continue", "delivered", "look"],
    );
    let a = t.engine.consider(&f).unwrap().unwrap();
    assert_eq!(t.engine.undo_kind(&a).as_deref(), Some("pause"));
    t.engine.undo(WS, ME, &a.id, None).unwrap();
    assert_eq!(t.kit.db().item(&item).unwrap().unwrap().status, "pause");
    assert!(!t.yolo.effective(WS, Some(&item)).unwrap().on);
}

#[test]
fn rollback_is_offered_only_for_the_newest_release() {
    let t = t();
    let release = |item: &str, sid: &str| {
        let d = t.card(
            item,
            sid,
            "approve",
            "release",
            "发布？",
            "",
            &["yes", "no"],
        );
        t.kit
            .db()
            .decision_resolve(&d, json!({"approved": true, "outcome": "released", "prev": "v1", "version": "v2", "decided_by": "yolo"}), "resolved", None)
            .unwrap();
        let a = t
            .yolo
            .record(&steve_store::yolo::NewAction {
                workspace_id: WS.into(),
                decision_id: Some(d.clone()),
                item_id: Some(item.into()),
                kind: "approve".into(),
                subject_type: "release".into(),
                rule: "release.self".into(),
                outcome: "scheduled".into(),
                undo: Some(json!({"kind": "rollback"})),
                ..Default::default()
            })
            .unwrap();
        t.yolo.finish(&a.id, "decided", None).unwrap();
        t.yolo
            .store()
            .reader(|c| steve_store::yolo::action_get(c, &a.id))
            .unwrap()
            .unwrap()
    };
    let (i1, s1, _) = t.item();
    let first = release(&i1, &s1);
    assert_eq!(t.engine.undo_kind(&first).as_deref(), Some("rollback"));
    std::thread::sleep(std::time::Duration::from_millis(5));
    let (i2, s2, _) = t.item();
    let second = release(&i2, &s2);
    assert_eq!(t.engine.undo_kind(&first), None, "a later release went out");
    assert_eq!(t.engine.undo_kind(&second).as_deref(), Some("rollback"));
    assert_eq!(
        t.engine
            .undo(WS, ME, &first.id, None)
            .unwrap_err()
            .to_string(),
        CANNOT
    );
}

#[test]
fn now_and_cancel_during_the_cool_down() {
    let t = t();
    let (item, sid, _) = t.item();
    let d = t.card(
        &item,
        &sid,
        "fail",
        "item",
        "会话出错：x",
        "",
        &["continue", "delivered", "look"],
    );
    let a = t.engine.consider(&d).unwrap().unwrap();
    assert_eq!(a.outcome, "scheduled");
    let done = t.engine.run_now(WS, &a.id).unwrap();
    assert_eq!(done.outcome, "decided");
    assert_eq!(t.decision(&d).status, "resolved");

    let (item, sid, _) = t.item();
    let d = t.card(
        &item,
        &sid,
        "fail",
        "item",
        "会话出错：y",
        "",
        &["continue", "delivered", "look"],
    );
    let a = t.engine.consider(&d).unwrap().unwrap();
    let c = t.engine.cancel(WS, &a.id).unwrap();
    assert_eq!(c.outcome, "cancelled");
    t.clock.advance(3_600_000);
    t.engine.tick();
    assert!(
        t.engine.consider(&d).unwrap().is_none(),
        "the card stays yours this window"
    );
    assert_eq!(t.decision(&d).status, "open");
    assert!(t.engine.cancel(WS, &a.id).is_err());
}

#[test]
fn the_report_gets_one_yolo_line_since_the_last_report() {
    let t = t();
    accepted(&t);
    let notes = Notes {
        store: t.kit.db().store().clone(),
    };
    let n = notes.take(WS);
    assert_eq!(
        n,
        vec!["YOLO 替你做了 1 件（验收 1），留给你 0 件；摘要见 #/yolo".to_string()]
    );
    assert!(notes.take(WS).is_empty(), "taken");
}

fn modules(t: &T) -> Modules {
    t.kit
        .db()
        .store()
        .with_conn(|c| {
            c.execute_batch(
                "INSERT OR IGNORE INTO users (id, workspace_id, subject, name, role, created) VALUES ('usr_dev', 'default', 'dev', 'Dev', 'owner', 1);
                 INSERT OR IGNORE INTO workspace_members (workspace_id, user_id, role, created) VALUES ('default', 'usr_dev', 'owner', 1);",
            )?;
            Ok(())
        })
        .unwrap();
    let mut r = Registry::new();
    super::draft::register(&mut r, None);
    let m = Modules::new(t.kit.db().store().clone(), t.kit.core.module_gateway(), r);
    t.kit.core.attach_modules(m.clone());
    m
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn on_until_eight_except_the_frontend_is_two_operations_applied_and_undone() {
    let t = t();
    t.yolo
        .put(
            WS,
            "me",
            super::Switch {
                full: false,
                on: false,
                preview: false,
                include_open: false,
            },
        )
        .unwrap();
    let dir = t.kit.home().join("front");
    std::fs::create_dir_all(&dir).unwrap();
    let p = t
        .kit
        .db()
        .project_create(
            WS,
            "前端",
            "",
            &dir.to_string_lossy(),
            "git",
            true,
            Some("dev"),
            None,
        )
        .unwrap()
        .id;
    let m = modules(&t);
    t.kit.gw.set(|prompt, _| {
        assert!(prompt.contains("yolo.scope"));
        Ok(r#"{"ops":[{"op":"yolo.set","on":true,"until":"明早 08:00"},{"op":"yolo.scope","type":"project","name":"前端","mode":"off"}]}"#.into())
    });
    let me = caller(WS, ME);
    let draft = changes::draft(
        &m,
        &me,
        "yolo",
        Some(WS),
        "开 YOLO 到明早 8 点，前端项目除外",
        Origin::default(),
    )
    .await
    .unwrap();
    let lines: Vec<String> = draft
        .changes
        .iter()
        .map(|c| format!("{}：{} → {}", c.label, c.from, c.to))
        .collect();
    assert_eq!(lines[0], "YOLO：关 → 开（不限时，开着直到你关）");
    assert!(
        lines
            .iter()
            .any(|l| l == "项目「前端」的 YOLO：跟随 → 从不"),
        "{lines:?}"
    );
    let id = draft.id.clone();
    let m2 = m.clone();
    let me2 = me.clone();
    tokio::task::spawn_blocking(move || changes::apply(&m2, &me2, &id).unwrap().unwrap())
        .await
        .unwrap();
    assert_eq!(t.yolo.status(WS).unwrap()["on"], true);
    let i = t.kit.item("run", "执行", Some(&p));
    assert!(
        !t.yolo.effective(WS, Some(&i)).unwrap().on,
        "the frontend is left out"
    );
    let id = draft.id.clone();
    tokio::task::spawn_blocking(move || changes::undo(&m, &me, &id).unwrap().unwrap())
        .await
        .unwrap();
    assert_eq!(t.yolo.status(WS).unwrap()["on"], false);
    let scopes = t.yolo.status(WS).unwrap()["scopes"].clone();
    assert!(
        scopes
            .as_array()
            .unwrap()
            .iter()
            .all(|s| s["object_id"] != p.as_str()),
        "{scopes}"
    );
}
