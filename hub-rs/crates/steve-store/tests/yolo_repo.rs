//! The YOLO repository (migration 14): scope rows with revisions and events, windows, the ledger.

mod common;

use common::*;
use serde_json::json;
use steve_store::yolo::{self, ScopeChange};
use steve_store::{ChangeEvent, WS};

#[test]
fn scope_put_bumps_revision_writes_revisions_and_publishes() {
    let t = fresh();
    t.events.take();
    let (before, after) = t
        .store
        .transaction(|tx| {
            yolo::scope_put(
                tx,
                WS,
                yolo::ITEM,
                "ST-1",
                &ScopeChange {
                    mode: Some("on"),
                    ..Default::default()
                },
                "u1",
                10,
            )
        })
        .unwrap();
    assert_eq!(before, None);
    assert_eq!(
        (after.mode.as_str(), after.revision, after.set_by.as_str()),
        ("on", 1, "u1")
    );
    assert_eq!(t.events.take(), vec![ChangeEvent::Yolo { ws: WS.into() }]);

    // the same values again: nothing written, nothing published
    let (b, a) = t
        .store
        .transaction(|tx| {
            yolo::scope_put(
                tx,
                WS,
                yolo::ITEM,
                "ST-1",
                &ScopeChange {
                    mode: Some("on"),
                    ..Default::default()
                },
                "u2",
                11,
            )
        })
        .unwrap();
    assert_eq!(b.as_ref(), Some(&a));
    assert_eq!(a.revision, 1);
    assert!(t.events.take().is_empty());

    let paused = json!({"code": "B1", "reason": "r", "at": 12});
    let (_, a) = t
        .store
        .transaction(|tx| {
            yolo::scope_put(
                tx,
                WS,
                yolo::WORKSPACE,
                WS,
                &ScopeChange {
                    paused: Some(Some(&paused)),
                    ..Default::default()
                },
                "yolo",
                12,
            )
        })
        .unwrap();
    assert_eq!((a.mode.as_str(), a.paused.clone()), ("off", Some(paused)));
    let revs = t
        .store
        .reader(|c| steve_store::base::revisions(c, WS, yolo::REVISION_TYPE, "item:ST-1"))
        .unwrap();
    assert_eq!(revs.len(), 1);
    assert_eq!(
        revs[0].1,
        json!({"before": null, "after": {"mode": "on", "paused": null, "reason": ""}})
    );
    let all = t.store.reader(|c| yolo::scopes(c, WS)).unwrap();
    assert_eq!(
        all.iter()
            .map(|s| s.object_type.as_str())
            .collect::<Vec<_>>(),
        vec!["workspace", "item"]
    );
}

#[test]
fn windows_and_ledger() {
    let t = fresh();
    let w = t
        .store
        .transaction(|tx| yolo::window_open(tx, WS, true, 100))
        .unwrap();
    assert!(w.include_open && w.ended.is_none());
    assert_eq!(
        t.store.reader(|c| yolo::window_current(c, WS)).unwrap(),
        Some(w.clone())
    );
    t.events.take();
    let add = |outcome: &str| {
        t.store
            .transaction(|tx| {
                yolo::action_add(
                    tx,
                    &yolo::NewAction {
                        workspace_id: WS.into(),
                        window_id: Some(w.id.clone()),
                        decision_id: Some("dc_1".into()),
                        kind: "accept".into(),
                        subject_type: "item".into(),
                        rule: "accept.checks".into(),
                        outcome: outcome.into(),
                        reason: "检查全部通过".into(),
                        checks: json!({"checks": [{"name": "test", "passed": true}]}),
                        undo: Some(json!({"kind": "decision"})),
                        ..Default::default()
                    },
                    101,
                )
            })
            .unwrap()
    };
    let a = add("decided");
    add("declined");
    let s = add("scheduled");
    assert!(a.id.starts_with("ya_"));
    assert_eq!(a.undo, Some(json!({"kind": "decision"})));
    assert_eq!(a.checks["checks"][0]["name"], "test");
    assert_eq!(
        t.store.reader(|c| yolo::counts(c, &w.id)).unwrap(),
        yolo::Counts {
            decided: 1,
            declined: 1,
            scheduled: 1
        }
    );
    assert_eq!(t.events.take().len(), 3);
    let n = t
        .store
        .transaction(|tx| yolo::cancel_scheduled(tx, WS, &w.id, 200))
        .unwrap();
    assert_eq!(n, 1);
    let s = t
        .store
        .reader(|c| yolo::action_get(c, &s.id))
        .unwrap()
        .unwrap();
    assert_eq!((s.outcome.as_str(), s.done_at), ("cancelled", Some(200)));
    t.store
        .transaction(|tx| yolo::window_close(tx, &w.id, "off", 300))
        .unwrap();
    t.store
        .transaction(|tx| yolo::window_close(tx, &w.id, "tripped", 400))
        .unwrap();
    let w = t
        .store
        .reader(|c| yolo::window_get(c, &w.id))
        .unwrap()
        .unwrap();
    assert_eq!(
        (w.ended, w.ended_why.as_deref()),
        (Some(300), Some("off")),
        "the first end stays"
    );
    assert_eq!(
        t.store.reader(|c| yolo::window_current(c, WS)).unwrap(),
        None
    );
    assert_eq!(
        t.store
            .reader(|c| yolo::actions_of_window(c, &w.id))
            .unwrap()
            .len(),
        3
    );
}
