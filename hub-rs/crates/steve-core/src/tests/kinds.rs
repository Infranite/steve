//! Item categories (ST-34): every way an item is made gives it exactly one; the backfill fills in the
//! rest once, lists what it is unsure of, and finds nothing to do the second time.

use super::flows::kind_id;
use crate::error::msg;
use crate::orchestrator::CreateItem;
use crate::repo::{NewItem, WS};
use crate::testkit::Kit;

fn kinds_of(k: &Kit, item: &str) -> Vec<String> {
    let kinds = k.db().kinds(WS).unwrap();
    k.db()
        .tags_of_item(item)
        .unwrap()
        .iter()
        .filter_map(|t| kinds.iter().find(|x| &x.id == t).map(|x| x.name.clone()))
        .collect()
}

/// An item from before the categories: no tags at all (the store's low-level create allows it).
fn legacy(k: &Kit, title: &str, goal: &str) -> String {
    k.db()
        .item_create(
            WS,
            &NewItem {
                title: title.into(),
                goal: goal.into(),
                bot_id: "steve".into(),
                status: Some("done".into()),
                ..Default::default()
            },
        )
        .unwrap()
        .id
}

fn make(k: &Kit, title: &str, goal: &str, tags: Vec<String>) -> String {
    k.core
        .create_item(CreateItem {
            ws: WS.into(),
            goal: goal.into(),
            title: title.into(),
            tag_ids: tags,
            propose: true,
            ..Default::default()
        })
        .unwrap()
        .id
}

#[test]
fn an_item_made_without_a_category_gets_steves_and_never_two() {
    let k = Kit::new();
    let web = k
        .db()
        .tag_create(WS, "前端", "领域", "界面", "你")
        .unwrap()
        .id;
    // nobody named one (a flow, a mail rule, a channel bot): read from the title and goal
    let a = make(&k, "修复登录失败", "点登录没反应", vec![web.clone()]);
    assert_eq!(kinds_of(&k, &a), ["bug"]);
    assert_eq!(k.db().tags_of_item(&a).unwrap()[1], web);
    assert!(k.timeline_texts(&a)[0].ends_with("，按目标归为 bug"));
    // two named: the first stays; a tag of another workspace or none at all is dropped
    let (doc, ops) = (kind_id(&k, "doc"), kind_id(&k, "ops"));
    let b = make(&k, "x", "y", vec![doc.clone(), "tag_ghost".into(), ops]);
    assert_eq!(k.db().tags_of_item(&b).unwrap(), [doc]);
}

#[tokio::test]
async fn the_backfill_fills_each_item_once_and_lists_the_unsure() {
    let k = Kit::new();
    let web = k
        .db()
        .tag_create(WS, "前端", "领域", "界面", "你")
        .unwrap()
        .id;
    let a = legacy(&k, "排队消息可撤回", "思考时也能发补充消息");
    let b = legacy(&k, "交付队列", "先给方案");
    let c = legacy(&k, "随便看看", "没说清");
    let kept = make(&k, "修复按钮", "按钮失效", vec![]);
    // one with two categories (written around the store, as an old Hub could have)
    let (feat, chore) = (kind_id(&k, "feat"), kind_id(&k, "chore"));
    let two = legacy(&k, "整理", "整理一下");
    k.db()
        .store()
        .with_conn(|cn| {
            for t in [&feat, &chore, &web] {
                cn.execute(
                    "INSERT INTO item_tags (item_id, tag_id) VALUES (?, ?)",
                    [&two, t],
                )?;
            }
            Ok(())
        })
        .unwrap();
    let (ia, ib, itwo) = (a.clone(), b.clone(), two.clone());
    k.gw.set(move |p, _| {
        assert!(p.contains("exactly ONE category") && p.contains("chore:"));
        assert!(!p.contains("修复按钮"), "an item with one category is not asked about");
        Ok(format!(
            r#"{{"items":[{{"id":"{ia}","kind":"feat","confidence":0.9,"why":"新能力"}},{{"id":"{ib}","kind":"doc","confidence":0.55,"why":"只出方案"}},{{"id":"{itwo}","kind":"bug","confidence":0.9,"why":"x"}}]}}"#
        ))
    });

    // a dry run says what it would do and writes nothing
    let dry = k.core.backfill_kinds(WS, true).await.unwrap();
    assert_eq!((dry.checked, dry.kept, dry.filled.len()), (5, 1, 4));
    assert!(kinds_of(&k, &a).is_empty());

    let r = k.core.backfill_kinds(WS, false).await.unwrap();
    let by = |id: &str| r.filled.iter().find(|f| f.id == id).unwrap();
    assert_eq!(
        (by(&a).kind.as_str(), by(&a).by.as_str(), by(&a).uncertain),
        ("feat", "model", false)
    );
    assert_eq!((by(&b).kind.as_str(), by(&b).uncertain), ("doc", true));
    // the model skipped it: the rules decide, and with nothing to go on it is an unsure feat, not chore
    assert_eq!(
        (by(&c).kind.as_str(), by(&c).by.as_str(), by(&c).uncertain),
        ("feat", "rule", true)
    );
    // with two, the model's pick must be one of them (bug is not): the rules pick among them
    assert_eq!(by(&two).had, ["feat", "chore"]);
    assert!(["feat", "chore"].contains(&by(&two).kind.as_str()) && by(&two).uncertain);
    assert_eq!(kinds_of(&k, &kept), ["bug"]);
    for id in [&a, &b, &c, &two, &kept] {
        assert_eq!(kinds_of(&k, id).len(), 1, "{id}: exactly one category");
    }
    // the domain tag stays, after the category
    assert_eq!(k.db().tags_of_item(&two).unwrap().last(), Some(&web));
    assert!(
        k.timeline_texts(&b)
            .contains(&"Steve 补齐分类：doc，拿不准，请你确认".to_string())
    );

    // the unsure ones are listed together
    let review = k.core.kinds_review(WS).unwrap();
    let mut ids: Vec<&str> = review.iter().map(|x| x.id.as_str()).collect();
    ids.sort();
    let mut want = vec![b.as_str(), c.as_str(), two.as_str()];
    want.sort();
    assert_eq!(ids, want);

    // a second run finds nothing: nothing asked, nothing written
    k.gw.set(|_, _| Err(msg("not asked")));
    let (notes, calls) = (k.timeline_texts(&a).len(), k.gw.calls.lock().unwrap().len());
    let again = k.core.backfill_kinds(WS, false).await.unwrap();
    assert_eq!((again.checked, again.kept, again.filled.len()), (5, 5, 0));
    assert_eq!(k.timeline_texts(&a).len(), notes);
    assert_eq!(k.gw.calls.lock().unwrap().len(), calls);
    for id in [&a, &b, &c, &two, &kept] {
        assert_eq!(kinds_of(&k, id).len(), 1, "{id}: still exactly one");
    }

    // changing a listed item's category takes it off the list; “没问题” does too
    let bug = kind_id(&k, "bug");
    k.db().set_item_tags(&b, &[bug]).unwrap();
    assert!(!k.core.kinds_review(WS).unwrap().iter().any(|x| x.id == b));
    let left = k.core.kinds_reviewed(WS, std::slice::from_ref(&c)).unwrap();
    assert_eq!(
        left.iter().map(|x| x.id.as_str()).collect::<Vec<_>>(),
        [two.as_str()]
    );
}

#[tokio::test]
async fn without_the_model_the_rules_fill_in() {
    let k = Kit::new();
    let a = legacy(&k, "修复思考时无法发送消息", "");
    let b = legacy(&k, "发布新版本", "部署到生产");
    k.gw.set(|_, _| Err(msg("model down")));
    let r = k.core.backfill_kinds(WS, false).await.unwrap();
    assert_eq!(r.filled.len(), 2);
    assert!(r.filled.iter().all(|f| f.by == "rule" && !f.uncertain));
    assert_eq!(
        (kinds_of(&k, &a), kinds_of(&k, &b)),
        (vec!["bug".to_string()], vec!["ops".to_string()])
    );
    assert!(k.core.kinds_review(WS).unwrap().is_empty());
}
