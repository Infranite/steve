//! Item categories (ST-34): six fixed tags of category 类型 per workspace, exactly one on every task item.

use steve_store::testkit::SqliteFixtures;
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
        .write(|tx| {
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

mod delivery_seeds {
    use super::*;
    use steve_store::kinds::{DELIVERIES, DELIVERY_CAT};

    fn tag(s: &Store, name: &str) -> Tag {
        s.tags().by_name(WS, name).unwrap().unwrap()
    }

    fn task(s: &Store, title: &str, tags: Vec<String>) -> Item {
        steve(s);
        s.items()
            .create(
                WS,
                &NewItem {
                    title: title.into(),
                    goal: title.into(),
                    bot_id: "steve".into(),
                    tags,
                    ..Default::default()
                },
            )
            .unwrap()
            .unwrap()
    }

    fn changes(s: &Store) -> u64 {
        s.fixture_write(|c| Ok(c.total_changes())).unwrap()
    }

    #[test]
    fn default_workspace_and_its_seeds_roll_back_together_on_seed_failure() {
        let dir = tempfile::tempdir().unwrap();
        let events = super::common::Events::default();
        let sink = events.clone();
        let s = Store::open(
            dir.path().join("steve.sqlite3"),
            FnPublisher(move |event| sink.0.lock().unwrap().push(event)),
        )
        .unwrap();
        s.fixture_write(|c| {
            c.execute_batch("CREATE TRIGGER fail_last_delivery BEFORE INSERT ON tags WHEN NEW.cat='交付' AND NEW.name='操作' BEGIN SELECT RAISE(ABORT,'injected delivery seed failure'); END;")?;
            Ok(())
        }).unwrap();
        events.take();
        let error = s.ensure_workspace().unwrap_err();
        assert!(
            error.to_string().contains("injected delivery seed failure"),
            "{error}"
        );
        assert!(s.workspaces().all().unwrap().is_empty());
        assert!(s.tags().list(WS).unwrap().is_empty());
        assert!(
            events.take().is_empty(),
            "rolled-back seeds must not publish"
        );
        s.fixture_write(|c| {
            c.execute_batch("DROP TRIGGER fail_last_delivery")?;
            Ok(())
        })
        .unwrap();
        // Also exercise re-entry into the public transaction scope used by callers.
        s.write(|_| s.ensure_workspace()).unwrap();
        assert_eq!(s.workspaces().all().unwrap().len(), 1);
        assert_eq!(
            s.tags()
                .list(WS)
                .unwrap()
                .iter()
                .filter(|t| t.cat == DELIVERY_CAT)
                .count(),
            6
        );
    }

    #[test]
    fn new_workspace_members_and_seeds_roll_back_together_on_seed_failure() {
        let t = fresh();
        let s = &t.store;
        let me = s
            .users()
            .create("atomic-workspace-owner", "Owner", None, "owner")
            .unwrap();
        let before = serde_json::to_value(s.workspaces().all().unwrap()).unwrap();
        let counts = || {
            s.fixture_write(|c| {
                Ok((
                    count(c, "SELECT COUNT(*) FROM workspace_members"),
                    count(c, "SELECT COUNT(*) FROM tags"),
                ))
            })
            .unwrap()
        };
        let rows = counts();
        s.fixture_write(|c| {
            c.execute_batch("CREATE TRIGGER fail_last_delivery BEFORE INSERT ON tags WHEN NEW.cat='交付' AND NEW.name='操作' BEGIN SELECT RAISE(ABORT,'injected delivery seed failure'); END;")?;
            Ok(())
        }).unwrap();
        t.events.take();
        let p = NewWorkspace {
            name: "atomic workspace".into(),
            ..Default::default()
        };
        let error = s.workspaces().create(&p, &me).unwrap_err();
        assert!(
            error.to_string().contains("injected delivery seed failure"),
            "{error}"
        );
        assert_eq!(
            serde_json::to_value(s.workspaces().all().unwrap()).unwrap(),
            before
        );
        assert_eq!(
            counts(),
            rows,
            "neither owner membership nor partial seeds remain"
        );
        assert!(
            t.events.take().is_empty(),
            "rolled-back creation must not publish"
        );
        s.fixture_write(|c| {
            c.execute_batch("DROP TRIGGER fail_last_delivery")?;
            Ok(())
        })
        .unwrap();
        let made = s.write(|_| s.workspaces().create(&p, &me)).unwrap();
        assert_eq!(
            s.tags()
                .list(&made.id)
                .unwrap()
                .iter()
                .filter(|t| t.cat == DELIVERY_CAT)
                .count(),
            6
        );
        assert_eq!(counts(), (rows.0 + 1, rows.1 + 12));
    }

    #[test]
    fn every_new_workspace_has_delivery_seeds_and_repeated_ensure_writes_nothing() {
        let t = fresh();
        let s = &t.store;
        let me = s
            .users()
            .create("delivery-owner", "Owner", None, "owner")
            .unwrap();
        let other = s
            .workspaces()
            .create(
                &NewWorkspace {
                    name: "other".into(),
                    ..Default::default()
                },
                &me,
            )
            .unwrap();
        for ws in [WS, &other.id] {
            let seeds: Vec<_> = s
                .tags()
                .list(ws)
                .unwrap()
                .into_iter()
                .filter(|t| t.cat == DELIVERY_CAT)
                .collect();
            assert_eq!(seeds.len(), 6);
            for (name, desc) in DELIVERIES {
                assert!(
                    seeds
                        .iter()
                        .any(|t| t.name == name && t.description == desc)
                );
            }
            let before = s.tags().list(ws).unwrap();
            t.events.take();
            let writes = changes(s);
            s.tags().kinds(ws).unwrap();
            s.tags().kinds(ws).unwrap();
            assert_eq!(changes(s), writes);
            assert_eq!(s.tags().list(ws).unwrap(), before);
            assert!(t.events.take().is_empty());
        }
    }

    #[test]
    fn deleted_delivery_tags_stay_deleted_and_do_not_claim_user_names() {
        let t = fresh();
        let s = &t.store;
        let report = tag(s, "报告");
        let feat = tag(s, "feat").id;
        s.tags().remove(&report.id).unwrap();
        let item = task(s, "调研报告", vec![feat.clone()]);
        assert!(s.tags().by_name(WS, "报告").unwrap().is_none());
        assert_eq!(s.tags().of_item(&item.id).unwrap(), [feat.clone()]);
        s.ensure_workspace().unwrap();
        assert!(
            s.tags().by_name(WS, "报告").unwrap().is_none(),
            "startup must not resurrect it"
        );
        let ordinary = s
            .tags()
            .create(
                WS,
                &NewTag {
                    name: "报告".into(),
                    cat: "领域".into(),
                    description: "user report".into(),
                },
                "me",
            )
            .unwrap();
        assert_eq!(ordinary.cat, "领域");
        assert_ne!(ordinary.id, report.id);
        let after = task(s, "调研报告", vec![feat.clone()]);
        s.tags()
            .set_item(&after.id, std::slice::from_ref(&feat))
            .unwrap();
        s.artifacts()
            .add_as(&after.id, "报告", "调研结论", None, Some("result"))
            .unwrap();
        assert_eq!(s.tags().of_item(&after.id).unwrap(), [feat]);
        assert_eq!(tag(s, "报告"), ordinary);
        assert!(
            !s.tags()
                .list(WS)
                .unwrap()
                .iter()
                .any(|t| t.name == "报告" && t.cat == DELIVERY_CAT)
        );
    }

    #[test]
    fn an_ordinary_delivery_name_is_preserved_and_does_not_block_writes() {
        let t = fresh();
        let s = &t.store;
        let report = tag(s, "报告");
        let feat = tag(s, "feat").id;
        s.tags()
            .patch(
                &report.id,
                &TagPatch {
                    cat: Some("领域".into()),
                    description: Some("user report".into()),
                    ..Default::default()
                },
            )
            .unwrap();
        let ordinary = tag(s, "报告");
        let old = task(s, "ordinary task", vec![feat.clone(), ordinary.id.clone()]);
        t.events.take();
        let writes = changes(s);
        s.tags().kinds(WS).unwrap();
        assert_eq!(changes(s), writes);
        assert!(t.events.take().is_empty());
        s.ensure_workspace().unwrap();
        assert_eq!(tag(s, "报告"), ordinary);
        assert_eq!(
            s.tags().of_item(&old.id).unwrap(),
            [feat.clone(), ordinary.id.clone()]
        );
        s.tags()
            .create(
                WS,
                &NewTag {
                    name: "ordinary topic".into(),
                    cat: "主题".into(),
                    ..Default::default()
                },
                "me",
            )
            .unwrap();
        let new = task(s, "设计方案与调研报告", vec![feat.clone()]);
        let labels = s.tags().of_item(&new.id).unwrap();
        assert!(labels.contains(&tag(s, "方案").id));
        assert!(
            !labels.contains(&ordinary.id),
            "never mistake the ordinary tag for delivery"
        );
        s.artifacts()
            .add_as(&new.id, "报告", "调研结论", None, Some("result"))
            .unwrap();
        assert!(!s.tags().of_item(&new.id).unwrap().contains(&ordinary.id));
        s.artifacts()
            .add_as(&new.id, "部署记录", "升级完成", None, Some("result"))
            .unwrap();
        assert!(
            s.tags()
                .of_item(&new.id)
                .unwrap()
                .contains(&tag(s, "操作").id)
        );
        assert_eq!(tag(s, "报告"), ordinary);

        // Explicitly releasing the name leaves it free; existing associations keep the old id.
        s.tags()
            .patch(
                &ordinary.id,
                &TagPatch {
                    name: Some("user report".into()),
                    ..Default::default()
                },
            )
            .unwrap();
        s.tags().kinds(WS).unwrap();
        assert!(s.tags().by_name(WS, "报告").unwrap().is_none());
        assert_eq!(s.tags().get(&ordinary.id).unwrap().unwrap().cat, "领域");
        assert_eq!(
            s.tags().of_item(&old.id).unwrap(),
            [feat.clone(), ordinary.id]
        );
        let after = task(s, "调研报告", vec![feat.clone()]);
        assert_eq!(s.tags().of_item(&after.id).unwrap(), [feat]);
    }

    #[test]
    fn ensuring_seeds_never_infers_existing_items_or_artifacts() {
        let t = fresh();
        let s = &t.store;
        let feat = tag(s, "feat").id;
        let item = task(s, "设计方案与调研报告", vec![feat.clone()]);
        s.artifacts()
            .add_as(&item.id, "部署记录", "升级完成", None, Some("result"))
            .unwrap();
        s.tags()
            .set_item(&item.id, std::slice::from_ref(&feat))
            .unwrap();
        let before = serde_json::to_value(s.items().get(&item.id).unwrap()).unwrap();
        for marker in [None, Some("true")] {
            s.fixture_write(|c| {
            c.execute("DELETE FROM settings WHERE workspace_id=? AND key='__delivery_tags_backfilled_v1'", [WS])?;
            if let Some(value) = marker {
                c.execute("INSERT INTO settings(workspace_id,key,value,updated) VALUES(?, '__delivery_tags_backfilled_v1', ?, 1)", rusqlite::params![WS,value])?;
            }
            Ok(())
        }).unwrap();
            t.events.take();
            let writes = changes(s);
            s.ensure_workspace().unwrap();
            assert_eq!(
                changes(s),
                writes + u64::from(marker.is_some()),
                "only obsolete checkpoint cleanup may write"
            );
            assert!(t.events.take().is_empty());
            assert_eq!(s.tags().of_item(&item.id).unwrap(), [feat.clone()]);
            assert_eq!(
                serde_json::to_value(s.items().get(&item.id).unwrap()).unwrap(),
                before
            );
            assert_eq!(
                s.settings()
                    .get::<Option<bool>>(WS, "__delivery_tags_backfilled_v1", None)
                    .unwrap(),
                None
            );
        }
    }

    #[test]
    fn workspace_startup_removes_only_the_obsolete_checkpoint_once() {
        let t = fresh();
        let s = &t.store;
        let me = s
            .users()
            .create("checkpoint-owner", "Owner", None, "owner")
            .unwrap();
        let other = s
            .workspaces()
            .create(
                &NewWorkspace {
                    name: "other".into(),
                    ..Default::default()
                },
                &me,
            )
            .unwrap();
        s.fixture_write(|c| {
        for ws in [WS, &other.id] {
            c.execute("INSERT INTO settings(workspace_id,key,value,updated) VALUES(?, '__delivery_tags_backfilled_v1', 'true', 1)", [ws])?;
            c.execute("INSERT INTO settings(workspace_id,key,value,updated) VALUES(?, 'user_preference', 'true', 1)", [ws])?;
        }
        Ok(())
    }).unwrap();
        assert!(
            s.settings()
                .all(WS)
                .unwrap()
                .contains_key("__delivery_tags_backfilled_v1"),
            "settings reads have no legacy-key filter"
        );
        let writes = changes(s);
        s.ensure_workspace().unwrap();
        assert_eq!(changes(s), writes + 2);
        for ws in [WS, &other.id] {
            let settings = s.settings().all(ws).unwrap();
            assert!(!settings.contains_key("__delivery_tags_backfilled_v1"));
            assert_eq!(settings["user_preference"], serde_json::json!(true));
        }
        let writes = changes(s);
        s.ensure_workspace().unwrap();
        assert_eq!(
            changes(s),
            writes,
            "later startups do not repeat a data change"
        );
    }
}

#[test]
fn nonreserved_tags_in_the_type_category_remain_editable_removable_and_mergeable() {
    let t = fresh();
    let s = &t.store;
    let a = domain(s, "legacy-a");
    let b = domain(s, "legacy-b");
    let into = domain(s, "topic");
    s.fixture_write(|c| {
        c.execute(
            "UPDATE tags SET cat=? WHERE id IN (?,?)",
            rusqlite::params![KIND_CAT, a, b],
        )?;
        Ok(())
    })
    .unwrap();
    let changed = s
        .tags()
        .patch(
            &a,
            &TagPatch {
                name: Some("renamed legacy".into()),
                cat: Some(KIND_CAT.into()),
                description: Some("edited".into()),
            },
        )
        .unwrap()
        .unwrap();
    assert_eq!(changed.name, "renamed legacy");
    assert_eq!(changed.description, "edited");
    s.tags().merge(&a, &into).unwrap();
    s.tags().remove(&b).unwrap();
    assert!(s.tags().get(&a).unwrap().is_none());
    assert!(s.tags().get(&b).unwrap().is_none());
    assert!(s.tags().get(&into).unwrap().is_some());
}
