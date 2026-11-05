//! Statistics marks (stats.md slice ST3): every status change leaves exactly one `status` mark, and the
//! orchestrator marks deliveries, fail cards and release outcomes.

use crate::Check;
use crate::orchestrator::ItemAction;
use crate::patch;
use crate::repo::WS;
use crate::stall::STALL_AFTER_MS;
use crate::testkit::Kit;
use crate::util::now;
use serde_json::{Value, json};

/// `(kind, from_v, to_v, ref)` of the item's marks, oldest first.
/// (kind, from_v, to_v, ref)
type MarkRow = (String, Option<String>, Option<String>, Option<String>);

fn marks(k: &Kit, item: &str) -> Vec<MarkRow> {
    k.db()
        .store()
        .with_conn(|c| {
            Ok(c.prepare(
                "SELECT kind, from_v, to_v, ref FROM item_marks WHERE item_id = ? ORDER BY id",
            )?
            .query_map([item], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))?
            .collect::<rusqlite::Result<Vec<_>>>()?)
        })
        .unwrap()
}

fn of_kind(k: &Kit, item: &str, kind: &str) -> Vec<(Option<String>, Option<String>)> {
    marks(k, item)
        .into_iter()
        .filter(|m| m.0 == kind)
        .map(|m| (m.2, m.3))
        .collect()
}

/// The status marks chain without gaps or repeats and end at the item's status.
fn assert_chain(k: &Kit, item: &str) -> Vec<String> {
    let st: Vec<_> = marks(k, item)
        .into_iter()
        .filter(|m| m.0 == "status")
        .collect();
    let mut prev: Option<String> = None;
    for m in &st {
        assert_eq!(m.1, prev, "{st:?}");
        assert_ne!(m.1, m.2, "{st:?}");
        prev = m.2.clone();
    }
    assert_eq!(prev.as_deref(), Some(k.status(item).0.as_str()));
    st.into_iter().filter_map(|m| m.2).collect()
}

#[test]
fn every_status_change_leaves_one_mark() {
    let k = Kit::new();
    let i = k.item("run", "执行", None);
    k.session(&i, "idle");
    assert_eq!(assert_chain(&k, &i), ["run"]);
    // a patch that does not change the status leaves nothing
    k.db()
        .item_patch(&i, patch![status: "run", now: "x"])
        .unwrap();
    k.db().item_patch(&i, patch![now: "y"]).unwrap();
    assert_eq!(assert_chain(&k, &i), ["run"]);
    k.core.item_action(&i, ItemAction::Pause, "me").unwrap();
    k.core.item_action(&i, ItemAction::Resume, "me").unwrap();
    k.core.item_action(&i, ItemAction::Drop, "me").unwrap();
    // the resumed turn is stopped; the cancel commits when it ended
    let sid = k.db().sessions_of_item(&i).unwrap()[0].id.clone();
    k.rt.end_turn(&k.core, &sid, "cancelled", None);
    assert_eq!(k.status(&i).0, "drop");
    k.core.item_action(&i, ItemAction::Reopen, "me").unwrap();
    let chain = assert_chain(&k, &i);
    assert_eq!(chain.first().map(String::as_str), Some("run"));
    assert!(chain.contains(&"pause".to_string()), "{chain:?}");
    assert!(chain.contains(&"drop".to_string()), "{chain:?}");

    // accepting finishes the item: one more mark, to done
    let x = k.delivered(false);
    k.core.resolve(&x.decision, "pass", "", "me").unwrap();
    assert_eq!(k.status(&x.id).0, "done");
    assert_eq!(
        assert_chain(&k, &x.id).last().map(String::as_str),
        Some("done")
    );

    // a dependency released: wait → run
    let a = k.item("run", "执行", None);
    let b = k.item("run", "执行", None);
    k.core.add_dep(&b, &a).unwrap();
    k.core.rm_dep(&b, &a).unwrap();
    assert_chain(&k, &b);
}

#[test]
fn manual_delivery_marks_its_accept_card() {
    let k = Kit::new();
    let x = k.delivered(false);
    k.db()
        .decision_resolve(&x.decision, json!({"choice": "skip"}), "resolved", None)
        .unwrap();
    let s = k.db().session(&x.session).unwrap().unwrap();
    k.core.tool_deliver(&s, "ok", &[], &[], &[]).unwrap();
    let card = k
        .db()
        .decisions_of_item(&x.id)
        .unwrap()
        .into_iter()
        .find(|d| d.kind == "accept" && d.status == "open")
        .unwrap();
    assert_eq!(
        of_kind(&k, &x.id, "deliver"),
        [(Some("manual".into()), Some(card.id))]
    );
}

#[test]
fn auto_delivery_is_marked_auto() {
    let k = Kit::new();
    let x = k.delivered(false);
    k.db()
        .decision_resolve(&x.decision, json!({"choice": "skip"}), "resolved", None)
        .unwrap();
    k.db()
        .settings_set(WS, json!({"accDefault": "auto"}).as_object().unwrap(), "me")
        .unwrap();
    let s = k.db().session(&x.session).unwrap().unwrap();
    let ok = Check {
        name: "tests".into(),
        passed: true,
        operated: false,
    };
    k.core.tool_deliver(&s, "ok", &[], &[ok], &[]).unwrap();
    assert_eq!(of_kind(&k, &x.id, "deliver"), [(Some("auto".into()), None)]);
    assert_eq!(
        assert_chain(&k, &x.id).last().map(String::as_str),
        Some("done")
    );
}

#[test]
fn fail_cards_are_marked_with_their_reason() {
    let k = Kit::new();
    let i = k.item("run", "执行", None);
    k.session(&i, "idle");
    k.core.fail_item(&i, "会话出错：boom").unwrap();
    k.core
        .fail_item(&i, "Hub 重启时这个会话正在运行，这一回合被中断了")
        .unwrap();
    k.core.fail_item(&i, "随便什么").unwrap();
    let reasons: Vec<_> = of_kind(&k, &i, "fail")
        .into_iter()
        .map(|m| m.0.unwrap())
        .collect();
    assert_eq!(reasons, ["error", "restart", "other"]);
    // every mark points at its card
    assert!(
        of_kind(&k, &i, "fail")
            .iter()
            .all(|m| m.1.as_deref().is_some_and(|r| r.starts_with("dec_")))
    );

    // the stall watch's card is a stall
    let j = k.item("run", "执行", None);
    let s = k.session(&j, "idle");
    k.db()
        .event_add(&s, "system", json!({"text": "你中断了当前回合"}))
        .unwrap();
    k.core
        .tx()
        .check_stalls(now() + STALL_AFTER_MS + 1_000)
        .unwrap();
    assert_eq!(
        of_kind(&k, &j, "fail")
            .into_iter()
            .map(|m| m.0.unwrap())
            .collect::<Vec<_>>(),
        ["stall"]
    );
}

/// An approved release decision on a fresh item; returns (item, decision).
fn approved(k: &Kit) -> (String, String) {
    let i = k.item("wait", "收尾", None);
    let s = k.session(&i, "idle");
    let d = k.decision(&i, Some(&s), "approve", "release", &["yes", "no"]);
    k.db()
        .decision_resolve(&d, json!({"approved": true, "text": ""}), "resolved", None)
        .unwrap();
    (i, d)
}

#[test]
fn release_outcomes_are_marked_once_each() {
    let k = Kit::new();
    let cases: [(&str, Value, Option<&str>); 5] = [
        ("released", json!({"version": "v2"}), Some("released")),
        (
            "failed",
            json!({"reason": "健康检查没有通过", "note": "已回到 v1"}),
            Some("rolled_back"),
        ),
        (
            "failed",
            json!({"reason": "前端构建失败", "note": "Hub 已是新版本，前端仍是 w2"}),
            Some("failed"),
        ),
        (
            "failed",
            json!({"reason": crate::release::NO_RESULT}),
            Some("unknown"),
        ),
        (
            "not_released",
            json!({"reason": "rebase 冲突"}),
            Some("not_released"),
        ),
    ];
    for (outcome, extra, want) in cases {
        let (i, d) = approved(&k);
        // the start of a release is not an outcome yet
        k.core
            .tx()
            .release_outcome(&i, "releasing", json!({}))
            .unwrap();
        k.core.tx().release_outcome(&i, outcome, extra).unwrap();
        assert_eq!(
            of_kind(&k, &i, "release"),
            [(want.map(String::from), Some(d.clone()))],
            "{outcome}"
        );
    }
}

#[test]
fn a_reported_release_marks_released_without_finishing_the_item() {
    let k = Kit::new();
    let (i, d) = approved(&k);
    k.core
        .tx()
        .release_outcome(&i, "releasing", json!({}))
        .unwrap();
    std::fs::create_dir_all(k.home().join("releases")).unwrap();
    std::fs::write(
        k.home().join("releases/last.json"),
        json!({"ok": true, "kind": "hub", "version": "v9", "item": i}).to_string(),
    )
    .unwrap();
    k.core.report_release().unwrap();
    assert_eq!(
        of_kind(&k, &i, "release"),
        [(Some("released".into()), Some(d))]
    );
    // a release is a fact about the item, never its completion
    assert_ne!(
        assert_chain(&k, &i).last().map(String::as_str),
        Some("done")
    );
}
