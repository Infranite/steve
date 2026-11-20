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
fn a_legacy_reserved_name_is_refused_without_reclassifying_its_items() {
    let t = fresh();
    let s = &t.store;
    steve(s);
    let feat = kind(s, "feat");
    let legacy = domain(s, "legacy chore");
    let item = item(s, vec![feat.clone(), legacy.clone()])
        .unwrap()
        .unwrap();
    // Model an old namespace collision at the last seed name, with other categories absent.
    let c = Connection::open(t.dir.path().join("steve.sqlite3")).unwrap();
    c.execute("DELETE FROM tags WHERE cat=? AND name<>'feat'", [KIND_CAT])
        .unwrap();
    c.execute("UPDATE tags SET name='chore' WHERE id=?", [&legacy])
        .unwrap();
    c.execute(
        "INSERT INTO tags VALUES ('tag_old_type','default','需求','类型','老的','你',1)",
        [],
    )
    .unwrap();
    t.events.take();

    assert!(conflict(s.tags().kinds(WS)).contains("chore"));
    assert!(
        t.events.take().is_empty(),
        "a refused initialization publishes no seed events"
    );
    let tags = s.tags().list(WS).unwrap();
    assert_eq!(
        tags.iter()
            .filter(|tag| tag.cat == KIND_CAT && steve_store::kinds::is_kind(&tag.name))
            .map(|tag| tag.name.as_str())
            .collect::<Vec<_>>(),
        ["feat"],
        "no missing category was partially seeded"
    );
    let old = s.tags().get(&legacy).unwrap().unwrap();
    assert_eq!(
        (
            old.name.as_str(),
            old.cat.as_str(),
            old.description.as_str()
        ),
        ("chore", "领域", "d")
    );
    assert_eq!(
        s.tags().of_item(&item.id).unwrap(),
        [feat.clone(), legacy.clone()]
    );
    assert_eq!(s.tags().get("tag_old_type").unwrap().unwrap().cat, KIND_CAT);

    // Resolution is an explicit public rename, never automatic metadata conversion.
    s.tags()
        .patch(
            &legacy,
            &TagPatch {
                name: Some("legacy chore".into()),
                ..Default::default()
            },
        )
        .unwrap();
    let seeded = s.tags().kinds(WS).unwrap();
    assert_eq!(seeded.len(), 6);
    assert_ne!(
        seeded.iter().find(|tag| tag.name == "chore").unwrap().id,
        legacy
    );
    assert_eq!(s.tags().get(&legacy).unwrap().unwrap().cat, "领域");
    assert_eq!(s.tags().get("tag_old_type").unwrap().unwrap().cat, KIND_CAT);
    assert_eq!(s.tags().of_item(&item.id).unwrap(), [feat, legacy]);
}

#[test]
fn a_conflicting_workspace_refuses_item_writes_without_side_effects() {
    let t = fresh();
    let s = &t.store;
    steve(s);
    let feat = kind(s, "feat");
    let chore = kind(s, "chore");
    let topic = domain(s, "topic");
    let existing = item(s, vec![feat.clone(), topic.clone()]).unwrap().unwrap();
    // Simulate a legacy reserved name in the wrong category, without using production data.
    let c = Connection::open(t.dir.path().join("steve.sqlite3")).unwrap();
    c.execute("UPDATE tags SET cat='领域' WHERE id=?", [&chore])
        .unwrap();
    let tags = s.tags().list(WS).unwrap();
    let counter = s.peek_counter("item:ST").unwrap();
    let items = serde_json::to_value(s.items().all(WS).unwrap()).unwrap();
    let counts = || {
        c.query_row(
            "SELECT (SELECT COUNT(*) FROM items), (SELECT COUNT(*) FROM item_goals), (SELECT COUNT(*) FROM item_tags)",
            [],
            |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?, r.get::<_, i64>(2)?)),
        ).unwrap()
    };
    let rows = counts();
    t.events.take();
    let unchanged = || {
        assert_eq!(
            counts(),
            rows,
            "no item, goal or tag association was written"
        );
        assert_eq!(
            serde_json::to_value(s.items().all(WS).unwrap()).unwrap(),
            items
        );
        assert_eq!(s.tags().list(WS).unwrap(), tags);
        assert_eq!(s.peek_counter("item:ST").unwrap(), counter);
        assert!(t.events.take().is_empty(), "a refusal publishes no event");
    };
    let diagnostic = conflict(s.ensure_workspace());
    for field in [
        chore.as_str(),
        "chore",
        "领域",
        "改名后重试",
        "离线改名后重启",
    ] {
        assert!(
            diagnostic.contains(field),
            "missing {field:?}: {diagnostic}"
        );
    }
    let english = steve_i18n::with(steve_i18n::EN, || conflict(s.ensure_workspace()));
    for field in [
        chore.as_str(),
        "chore",
        "领域",
        "rename it and retry",
        "offline and restart",
    ] {
        assert!(english.contains(field), "missing {field:?}: {english}");
    }
    unchanged();
    assert_eq!(conflict(item(s, vec![feat.clone()])), diagnostic);
    unchanged();
    assert_eq!(
        conflict(s.tags().set_item(&existing.id, std::slice::from_ref(&feat))),
        diagnostic
    );
    unchanged();

    // The documented offline recovery changes only this tag's name, preserving its identity.
    let tx = c.unchecked_transaction().unwrap();
    assert_eq!(
        tx.execute(
            "UPDATE tags SET name='legacy-chore' WHERE id=? AND name='chore' AND cat='领域'",
            [&chore],
        )
        .unwrap(),
        1
    );
    tx.commit().unwrap();
    s.ensure_workspace().unwrap();
    assert_eq!(s.tags().get(&chore).unwrap().unwrap().cat, "领域");
    assert_eq!(s.tags().get(&chore).unwrap().unwrap().name, "legacy-chore");
    let next = item(s, vec![feat.clone()]).unwrap().unwrap();
    assert_eq!(next.no, counter + 1);
    assert_eq!(
        s.tags()
            .set_item(&existing.id, &[feat.clone(), topic.clone()])
            .unwrap(),
        [feat, topic]
    );
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
    assert!(conflict(item(s, vec![])).contains("必须有一个分类"));
    assert!(conflict(item(s, vec![web.clone()])).contains("必须有一个分类"));
    assert!(conflict(item(s, vec![bug.clone(), feat.clone()])).contains("只能有一个分类"));
    assert_eq!(
        s.items().all(WS).unwrap().len(),
        n,
        "a refused item leaves no row"
    );
    let next = item(s, vec![feat.clone()]).unwrap().unwrap();
    assert_eq!(
        next.no,
        i.no + 1,
        "refused creates do not consume item numbers"
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

#[test]
fn a_chat_does_not_require_a_task_category() {
    let t = fresh();
    let s = &t.store;
    steve(s);
    let chat = s
        .items()
        .create(
            WS,
            &NewItem {
                kind: Some("chat".into()),
                bot_id: "steve".into(),
                ..Default::default()
            },
        )
        .unwrap()
        .unwrap();
    assert_eq!(chat.kind, "chat");
    assert!(s.tags().of_item(&chat.id).unwrap().is_empty());
}
