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
            ("Default acceptance", "Accepted automatically"),
            ("When prerequisites finish", "Start automatically"),
            ("Auto-handoff on quota exhaustion", "On"),
            ("Quota recovery wait threshold", "10 minutes"),
            ("Handoff threshold", "Never"),
            ("Archive automatically after finishing", "30 days"),
            ("Item number prefix", "ST"),
        ]
        .map(|(a, b)| (a.to_string(), b.to_string()))
    );
    let e = en(|| k.core.describe(WS, &target("nope", None)).unwrap_err()).await;
    assert_eq!(e.to_string(), "No such setting");
    // the same in zh-CN, as before
    let (title, _, now) = k
        .core
        .describe(WS, &target("settings", Some("items")))
        .unwrap();
    assert_eq!(title, "设置 · 事项与验收");
    assert_eq!(
        now,
        [
            ("默认验收方式", "自动验收"),
            ("前置完成后", "自动开始"),
            ("额度不足自动交接", "开"),
            ("额度恢复等待阈值", "10 分钟"),
            ("交接阈值", "不换会话"),
            ("完成后自动归档", "30 天"),
            ("事项编号前缀", "ST"),
        ]
        .map(|(a, b)| (a.to_string(), b.to_string()))
    );
    // Non-default values of the quota handoff settings translate cleanly in both locales.
    let mut overrides = serde_json::Map::new();
    overrides.insert("quotaAutoHandoff".into(), json!("off"));
    overrides.insert("quotaWaitMinutes".into(), json!("0"));
    k.db().settings_set(WS, &overrides, "me").unwrap();
    let (_, _, en_over) = en(|| {
        k.core
            .describe(WS, &target("settings", Some("items")))
            .unwrap()
    })
    .await;
    assert!(en_over.contains(&("Auto-handoff on quota exhaustion".into(), "Off".into())));
    assert!(en_over.contains(&("Quota recovery wait threshold".into(), "Don't wait".into())));
    let (_, _, zh_over) = k
        .core
        .describe(WS, &target("settings", Some("items")))
        .unwrap();
    assert!(zh_over.contains(&("额度不足自动交接".into(), "关".into())));
    assert!(zh_over.contains(&("额度恢复等待阈值".into(), "不等待".into())));
}

#[tokio::test]
async fn errors_and_labels_in_english() {
    en(|| {
        assert_eq!(
            crate::cycle::closed_msg(),
            "This item has ended; start a new cycle if you need to"
        );
        assert_eq!(
            crate::models::validate(
                &json!({"purposes": {"nope": null}}),
                &crate::harnesses::builtin()
            )
            .unwrap_err(),
            "No such purpose: nope"
        );
        assert_eq!(crate::models::purpose_label("route"), "Routing");
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
