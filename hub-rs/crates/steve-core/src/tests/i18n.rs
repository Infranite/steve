//! Text the Hub shows in the request's language: English under `Accept-Language: en`, the same Chinese
//! as before otherwise (the other tests run in zh-CN).

use crate::changes::Target;
use crate::repo::WS;
use crate::testkit::Kit;
use serde_json::json;

async fn en<R>(f: impl FnOnce() -> R) -> R {
    steve_i18n::scope(steve_i18n::EN, async { f() }).await
}

fn target(kind: &str, id: Option<&str>) -> Target {
    Target {
        kind: kind.into(),
        id: id.map(String::from),
    }
}

#[tokio::test]
async fn describe_in_english() {
    let k = Kit::new();
    let (title, _, now) = en(|| {
        k.core
            .describe(WS, &target("settings", Some("items")))
            .unwrap()
    })
    .await;
    assert_eq!(title, "Settings · Items and acceptance");
    assert_eq!(
        now,
        [
            ("Default acceptance", "You accept it"),
            ("When prerequisites finish", "Start automatically"),
            ("Handoff threshold", "80%"),
            ("Archive automatically after finishing", "30 days"),
            ("Item number prefix", "ST"),
        ]
        .map(|(a, b)| (a.to_string(), b.to_string()))
    );
    let e = en(|| k.core.describe(WS, &target("nope", None)).unwrap_err()).await;
    assert_eq!(e.to_string(), "No such setting");
    // the same in zh-CN, as before
    let (title, _, _) = k
        .core
        .describe(WS, &target("settings", Some("items")))
        .unwrap();
    assert_eq!(title, "设置 · 事项与验收");
}

#[tokio::test]
async fn errors_and_labels_in_english() {
    en(|| {
        assert_eq!(
            crate::cycle::closed_msg(),
            "This item has ended; start a new cycle if you need to"
        );
        assert_eq!(
            crate::models::validate(&json!({"purposes": {"nope": null}})).unwrap_err(),
            "No such purpose: nope"
        );
        assert_eq!(crate::models::purpose_label("route"), "Routing");
        assert_eq!(
            crate::yolo::judge::reason_shown("设置里没有让 YOLO 验收"),
            "The settings do not let YOLO accept deliveries"
        );
        assert_eq!(
            crate::yolo::judge::reason_shown("选了 Agent 推荐的：Agent 的推荐"),
            "Picked what the agent recommended: The agent's recommendation"
        );
        // a model's own words stay as they are
        assert_eq!(crate::yolo::judge::reason_shown("目标写了"), "目标写了");
    })
    .await;
    assert_eq!(
        crate::cycle::closed_msg(),
        "这个事项已经结束了，需要时先新建周期"
    );
    assert_eq!(crate::models::purpose_label("route"), "分流");
}

/// A start failure from an English request is stored as it always was: the fail card, the timeline line and the
/// same-card check are zh-CN, for every member.
#[tokio::test]
async fn a_start_failure_is_stored_in_zh_cn() {
    let k = Kit::new();
    let p = k
        .db()
        .project_create(
            WS,
            "Gone",
            "",
            "/nonexistent/steve-test",
            "dir",
            false,
            None,
            None,
        )
        .unwrap();
    let i = en(|| {
        k.core
            .create_item(crate::CreateItem {
                ws: WS.into(),
                goal: "g".into(),
                title: "x".into(),
                project: Some(Some(p.id)),
                ..Default::default()
            })
            .unwrap()
    })
    .await;
    let why = "开不了会话：项目目录不存在：/nonexistent/steve-test";
    assert_eq!(k.open_fails(&i.id), vec![why]);
    let tl: Vec<String> = k
        .db()
        .timeline(&i.id)
        .unwrap()
        .into_iter()
        .map(|t| t.text)
        .collect();
    assert!(tl.iter().any(|t| t == why), "{tl:?}");
}
