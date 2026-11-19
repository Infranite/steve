//! Every item creation path keeps exactly one category.

use super::flows::kind_id;
use crate::orchestrator::CreateItem;
use crate::repo::WS;
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
