mod common;

use common::*;
use steve_store::kinds::{DELIVERIES, DELIVERY_CAT};
use steve_store::*;

fn tag(s: &Store, name: &str) -> Tag {
    s.tags().by_name(WS, name).unwrap().unwrap()
}

fn task(s: &Store, title: &str, tags: Vec<String>) -> Item {
    s.bots()
        .seed(
            "seed-test",
            false,
            &NewBot {
                name: "fixture".into(),
                ..Default::default()
            },
            WS,
        )
        .unwrap();
    s.items()
        .create(
            WS,
            &NewItem {
                title: title.into(),
                goal: title.into(),
                bot_id: "seed-test".into(),
                tags,
                ..Default::default()
            },
        )
        .unwrap()
        .unwrap()
}

fn changes(s: &Store) -> u64 {
    s.with_conn(|c| Ok(c.total_changes())).unwrap()
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
fn missing_delivery_seeds_are_recreated_even_with_the_old_checkpoint() {
    let t = fresh();
    let s = &t.store;
    let report = tag(s, "报告");
    let code = tag(s, "代码");
    s.tags()
        .patch(
            &code.id,
            &TagPatch {
                description: Some("my description".into()),
                ..Default::default()
            },
        )
        .unwrap();
    s.tags().remove(&report.id).unwrap();
    s.with_conn(|c| {
        c.execute("INSERT INTO settings(workspace_id,key,value,updated) VALUES(?, '__delivery_tags_backfilled_v1', 'true', 1)", [WS])?;
        Ok(())
    }).unwrap();
    t.events.take();
    s.tags().kinds(WS).unwrap();
    assert_ne!(tag(s, "报告").id, report.id);
    assert_eq!(tag(s, "代码").id, code.id);
    assert_eq!(tag(s, "代码").description, "my description");
    assert_eq!(
        t.events.take().len(),
        1,
        "only the missing tag is published"
    );
    let writes = changes(s);
    s.tags().kinds(WS).unwrap();
    assert_eq!(changes(s), writes);
    assert!(t.events.take().is_empty());
    assert!(
        s.settings()
            .get::<bool>(WS, "__delivery_tags_backfilled_v1", false)
            .unwrap()
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

    // Explicitly releasing the name permits a new seed; existing associations keep the old id.
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
    let seeded = tag(s, "报告");
    assert_eq!(seeded.cat, DELIVERY_CAT);
    assert_ne!(seeded.id, ordinary.id);
    assert_eq!(s.tags().get(&ordinary.id).unwrap().unwrap().cat, "领域");
    assert_eq!(
        s.tags().of_item(&old.id).unwrap(),
        [feat.clone(), ordinary.id]
    );
    let after = task(s, "调研报告", vec![feat]);
    assert!(s.tags().of_item(&after.id).unwrap().contains(&seeded.id));
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
        s.with_conn(|c| {
            c.execute("DELETE FROM settings WHERE workspace_id=? AND key='__delivery_tags_backfilled_v1'", [WS])?;
            if let Some(value) = marker {
                c.execute("INSERT INTO settings(workspace_id,key,value,updated) VALUES(?, '__delivery_tags_backfilled_v1', ?, 1)", rusqlite::params![WS,value])?;
            }
            Ok(())
        }).unwrap();
        t.events.take();
        let writes = changes(s);
        s.tags().kinds(WS).unwrap();
        assert_eq!(changes(s), writes, "no marker, item or artifact write");
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
            marker.map(|_| true)
        );
    }
}
