//! Item categories (ST-34): six fixed tags of category 类型 per workspace, exactly one on every task item.

mod common;

use common::*;
use rusqlite::Connection;
use steve_store::kinds::{KIND_CAT, KINDS};
use steve_store::*;

fn steve(s: &Store) {
    let b = NewBot {
        name: "Steve".into(),
        harness: "claude-code".into(),
        ..Default::default()
    };
    s.bots().seed("steve", true, &b, WS).unwrap();
}

fn item(s: &Store, tags: Vec<String>) -> Result<Option<Item>> {
    s.items().create(
        WS,
        &NewItem {
            title: "t".into(),
            goal: "g".into(),
            bot_id: "steve".into(),
            tags,
            ..Default::default()
        },
    )
}

fn kind(s: &Store, name: &str) -> String {
    s.tags()
        .kinds(WS)
        .unwrap()
        .into_iter()
        .find(|k| k.name == name)
        .unwrap()
        .id
}

fn domain(s: &Store, name: &str) -> String {
    let t = NewTag {
        name: name.into(),
        cat: "领域".into(),
        description: "d".into(),
    };
    s.tags().create(WS, &t, "me").unwrap().id
}

fn conflict<T: std::fmt::Debug>(r: Result<T>) -> String {
    match r {
        Err(StoreError::Conflict(m)) => m,
        other => panic!("expected a conflict, got {other:?}"),
    }
}

#[test]
fn every_workspace_has_the_six_and_seeding_again_changes_nothing() {
    let t = fresh();
    let s = &t.store;
    let first = s.tags().kinds(WS).unwrap();
    assert_eq!(
        first.iter().map(|k| k.name.as_str()).collect::<Vec<_>>(),
        KINDS.map(|(k, _)| k)
    );
    assert!(
        first
            .iter()
            .all(|k| k.cat == KIND_CAT && !k.description.is_empty())
    );
    t.events.take();
    assert_eq!(s.tags().kinds(WS).unwrap(), first);
    assert!(t.events.take().is_empty());
    let me = s.users().create("sub-1", "Lee", None, "owner").unwrap();
    let other = s
        .workspaces()
        .create(
            &NewWorkspace {
                name: "w2".into(),
                ..Default::default()
            },
            &me,
        )
        .unwrap();
    let theirs = s.tags().list(&other.id).unwrap();
    assert_eq!(theirs.iter().filter(|k| k.cat == KIND_CAT).count(), 6);
    assert!(theirs.iter().all(|k| !first.iter().any(|f| f.id == k.id)));
}

#[test]
fn an_older_tag_of_that_name_becomes_the_category_and_strays_leave_it() {
    let t = fresh();
    let c = Connection::open(t.dir.path().join("steve.sqlite3")).unwrap();
    // a database from before the categories: none seeded yet
    c.execute_batch(
        "DELETE FROM tags;
         INSERT INTO tags VALUES ('tag_old_bug', 'default', 'bug', '主题', '老的', '你', 1);
         INSERT INTO tags VALUES ('tag_old_type', 'default', '需求', '类型', '老的', '你', 1);",
    )
    .unwrap();
    let s = &t.store;
    assert_eq!(kind(s, "bug"), "tag_old_bug");
    let old = s.tags().get("tag_old_bug").unwrap().unwrap();
    assert_eq!(
        (old.cat.as_str(), old.description.as_str()),
        (KIND_CAT, "老的")
    );
    assert_eq!(s.tags().get("tag_old_type").unwrap().unwrap().cat, "主题");
    let cats: Vec<Tag> = s
        .tags()
        .list(WS)
        .unwrap()
        .into_iter()
        .filter(|t| t.cat == KIND_CAT)
        .collect();
    assert_eq!(cats.len(), 6);
}

#[test]
fn an_item_has_exactly_one_category() {
    let t = fresh();
    let s = &t.store;
    steve(s);
    let (bug, feat, web) = (kind(s, "bug"), kind(s, "feat"), domain(s, "前端"));
    let i = item(s, vec![web.clone(), bug.clone()]).unwrap().unwrap();
    assert_eq!(s.tags().of_item(&i.id).unwrap(), [bug.clone(), web.clone()]);

    // without one, with two, or after removing the only one: refused, nothing written
    let n = s.items().all(WS).unwrap().len();
    assert!(conflict(item(s, vec![web.clone()])).contains("必须有一个分类"));
    assert!(conflict(item(s, vec![bug.clone(), feat.clone()])).contains("只能有一个分类"));
    assert_eq!(
        s.items().all(WS).unwrap().len(),
        n,
        "a refused item leaves no row"
    );
    assert!(conflict(s.tags().set_item(&i.id, std::slice::from_ref(&web))).contains("必须"));
    assert!(conflict(s.tags().set_item(&i.id, &[])).contains("必须"));
    assert!(conflict(s.tags().set_item(&i.id, &[bug.clone(), feat.clone()])).contains("bug、feat"));
    assert_eq!(s.tags().of_item(&i.id).unwrap(), [bug.clone(), web.clone()]);

    // switching is one write; duplicates once; the category first; another workspace's tag dropped
    let kept = s
        .tags()
        .set_item(
            &i.id,
            &[web.clone(), feat.clone(), web.clone(), "tag_nope".into()],
        )
        .unwrap();
    assert_eq!(kept, [feat.clone(), web.clone()]);
    assert_eq!(s.tags().of_item(&i.id).unwrap(), [feat, web]);
}

#[test]
fn categories_cannot_be_deleted_renamed_recategorised_or_merged() {
    let t = fresh();
    let s = &t.store;
    let (bug, feat, web) = (kind(s, "bug"), kind(s, "feat"), domain(s, "前端"));
    assert!(conflict(s.tags().remove(&bug)).contains("不能删除"));
    assert!(conflict(s.tags().merge(&bug, &feat)).contains("不能合并"));
    assert!(conflict(s.tags().merge(&web, &bug)).contains("不能合并"));
    let p = |name: Option<&str>, cat: Option<&str>| TagPatch {
        name: name.map(String::from),
        cat: cat.map(String::from),
        ..Default::default()
    };
    assert!(conflict(s.tags().patch(&bug, &p(Some("fix"), None))).contains("改名"));
    assert!(conflict(s.tags().patch(&bug, &p(None, Some("领域")))).contains("类别"));
    assert!(conflict(s.tags().patch(&web, &p(None, Some(KIND_CAT)))).contains("六个分类"));
    assert!(conflict(s.tags().patch(&web, &p(Some("ops"), None))).contains("分类标签的名字"));
    // only its description may change
    let d = TagPatch {
        description: Some("线上问题".into()),
        name: Some("bug".into()),
        cat: Some(KIND_CAT.into()),
    };
    assert_eq!(
        s.tags().patch(&bug, &d).unwrap().unwrap().description,
        "线上问题"
    );
    // no new tag joins them; a category's name gives the category back
    let nt = |name: &str, cat: &str| NewTag {
        name: name.into(),
        cat: cat.into(),
        description: "d".into(),
    };
    assert!(conflict(s.tags().create(WS, &nt("需求", KIND_CAT), "me")).contains("六个分类"));
    assert_eq!(
        s.tags().create(WS, &nt("bug", "领域"), "me").unwrap().id,
        bug
    );
    assert_eq!(s.tags().kinds(WS).unwrap().len(), 6);
}

#[test]
fn contacts_and_spaces_do_not_take_item_categories() {
    let t = fresh();
    let s = &t.store;
    let (bug, web) = (kind(s, "bug"), domain(s, "前端"));
    let kept = s
        .transaction(|tx| {
            base::tags::set(
                tx,
                WS,
                &base::ObjRef::new("party", "ct_x"),
                &[bug, web.clone()],
            )
        })
        .unwrap();
    assert_eq!(kept, [web]);
}
