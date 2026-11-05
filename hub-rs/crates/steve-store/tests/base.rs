//! Migration 4 and the shared base repositories (B0 acceptance).

mod common;

use common::*;
use rusqlite::Connection;
use serde_json::json;
use steve_store::base::{
    self, Level, ObjRef, Viewer, Visibility, changesets, decisions, links, mems, tags,
};
use steve_store::{ChangeEvent, NewDecision, NoPublisher, Store, StoreError, db};

const A: &str = "usr_a";
const B: &str = "usr_b";

/// A fresh store with users A and B in `default`, a second workspace `w2`, and a tag in each.
fn setup() -> T {
    let t = fresh();
    t.store
        .with_conn(|c| {
            c.execute_batch(
                "INSERT INTO users (id, workspace_id, subject, name, role, created) VALUES ('usr_a', 'default', 'a', 'A', 'owner', 1), ('usr_b', 'default', 'b', 'B', 'member', 1);
                 INSERT INTO workspaces (id, name, created) VALUES ('w2', '另一个', 1);
                 INSERT INTO tags (id, workspace_id, name, cat, description, created_by, created) VALUES ('tag_1', 'default', '客户', '客户', '', 'me', 1), ('tag_2', 'default', '重要', '主题', '', 'me', 1), ('tag_w2', 'w2', '别处', '主题', '', 'me', 1);",
            )?;
            Ok(())
        })
        .unwrap();
    t
}

fn tx<T>(s: &Store, f: impl FnOnce(&steve_store::Tx) -> steve_store::Result<T>) -> T {
    s.transaction(f).unwrap()
}

fn party(s: &Store, id: &str, ws: &str, ty: &str, user: Option<&str>) {
    s.with_conn(|c| {
        c.execute(
            "INSERT INTO parties (id, workspace_id, type, name, user_id, created_by, created, updated) VALUES (?, ?, ?, ?, ?, 'usr_a', 1, 1)",
            rusqlite::params![id, ws, ty, id, user],
        )?;
        Ok(())
    })
    .unwrap();
}

fn team(s: &Store, id: &str, parent: Option<&str>, members: &[&str]) {
    party(s, id, "default", "team", None);
    s.with_conn(|c| {
        c.execute(
            "INSERT INTO team_details (party_id, workspace_id, kind, parent_id) VALUES (?, 'default', 'chart', ?)",
            rusqlite::params![id, parent],
        )?;
        for m in members {
            c.execute(
                "INSERT INTO team_members (workspace_id, team_id, contact_id, joined) VALUES ('default', ?, ?, 1)",
                [id, m],
            )?;
        }
        Ok(())
    })
    .unwrap();
}

#[test]
fn empty_database_gets_the_current_version_with_builtin_attributes() {
    let t = fresh();
    t.store
        .with_conn(|c| {
            assert_eq!(count(c, "SELECT v FROM schema_version"), db::SCHEMA_VERSION);
            assert_eq!(
                count(
                    c,
                    "SELECT count(*) FROM attr_defs WHERE workspace_id = '*' AND source = 'builtin'"
                ),
                8
            );
            assert_eq!(
                c.query_row(
                    "SELECT label || default_level FROM attr_defs WHERE key = 'call'",
                    [],
                    |r| r.get::<_, String>(0)
                )?,
                "偏好的称呼public"
            );
            // module migrations are idempotent: a second run changes nothing
            c.execute_batch(db::MIGRATIONS[3])?;
            assert_eq!(count(c, "SELECT count(*) FROM attr_defs"), 8);
            Ok(())
        })
        .unwrap();
}

#[test]
fn ts_v3_database_with_data_upgrades_to_current() {
    // what a TS Hub on main leaves behind: migrations 1–3 and some rows
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("steve.sqlite3");
    {
        let c = Connection::open(&path).unwrap();
        c.execute_batch("CREATE TABLE schema_version (v INTEGER NOT NULL); INSERT INTO schema_version (v) VALUES (3);").unwrap();
        for m in &db::MIGRATIONS[..3] {
            c.execute_batch(m).unwrap();
        }
        c.execute_batch(
            "INSERT INTO workspaces (id, name, created) VALUES ('default', '个人', 1);
             INSERT INTO items (id, workspace_id, no, title, status, stage, created, updated) VALUES ('ST-1', 'default', 1, 't', 'open', 'x', 1, 1);
             INSERT INTO decisions (id, workspace_id, kind, subject_type, subject_id, item_id, question, status, requested_by, created) VALUES ('dec_1', 'default', 'ask', 'item', 'ST-1', 'ST-1', 'q', 'open', 'steve', 1);
             INSERT INTO links (workspace_id, subject_type, subject_id, predicate, object_type, object_id, source, created) VALUES ('default', 'item', 'ST-1', 'about', 'project', 'prj_1', 'me', 1);",
        )
        .unwrap();
    }
    let s = Store::open(&path, NoPublisher).unwrap();
    s.with_conn(|c| {
        assert_eq!(count(c, "SELECT v FROM schema_version"), db::SCHEMA_VERSION);
        assert_eq!(count(c, "SELECT count(*) FROM schema_version"), 1);
        assert_eq!(count(c, "SELECT count(*) FROM decisions"), 1);
        assert_eq!(count(c, "SELECT count(*) FROM links"), 1);
        assert_eq!(count(c, "SELECT count(*) FROM attr_defs"), 8);
        Ok(())
    })
    .unwrap();
    assert_eq!(s.decisions().of_item("ST-1").unwrap().len(), 1);
    drop(s);
    assert_eq!(schema_of(&path), schema_of(fresh().store.path()));
}

#[test]
fn transaction_publishes_after_commit_and_rolls_back_on_error() {
    let t = setup();
    let s = &t.store;
    let m = tx(s, |tx| {
        mems::create(
            tx,
            "default",
            &mems::NewMem {
                text: "x".into(),
                created_by: A.into(),
                ..Default::default()
            },
        )
    });
    assert_eq!(
        t.events.take(),
        [ChangeEvent::Mm {
            id: m.id.clone(),
            ws: "default".into()
        }]
    );
    let r: Result<(), StoreError> = s.transaction(|tx| {
        mems::create(
            tx,
            "default",
            &mems::NewMem {
                text: "y".into(),
                created_by: A.into(),
                ..Default::default()
            },
        )?;
        Err(StoreError::NotFound("没有这个记忆"))
    });
    assert!(r.is_err());
    assert!(
        t.events.take().is_empty(),
        "no event for a rolled-back write"
    );
    s.with_conn(|c| {
        assert_eq!(count(c, "SELECT count(*) FROM mems"), 1);
        Ok(())
    })
    .unwrap();
}

#[test]
fn visibility_codes() {
    for (s, v) in [
        ("workspace", Visibility::Workspace),
        ("private", Visibility::Private),
        ("team:tm_1", Visibility::Team("tm_1".into())),
        ("space:spc_1", Visibility::Space("spc_1".into())),
    ] {
        assert_eq!(Visibility::parse(s), Some(v.clone()));
        assert_eq!(v.code(), s);
        assert_eq!(serde_json::to_value(&v).unwrap(), json!(s));
    }
    for bad in ["", "team:", "group:x", "Private", "team"] {
        assert_eq!(Visibility::parse(bad), None, "{bad}");
    }
    assert_eq!(Visibility::from_db("garbage"), Visibility::Private);
    assert_eq!(Level::parse("reasoning"), Some(Level::Reasoning));
    assert_eq!(Level::from_db("garbage"), Level::Never);
    assert_eq!(
        serde_json::to_value(Level::Conclusion).unwrap(),
        json!("conclusion")
    );
    assert_eq!(
        serde_json::to_value(base::DecisionKind::Confirm).unwrap(),
        json!("confirm")
    );
}

#[test]
fn private_memory_is_invisible_to_another_user() {
    let t = setup();
    let s = &t.store;
    party(s, "ct_x", "default", "contact", None);
    let about = vec![ObjRef::new("party", "ct_x")];
    let new = |text: &str, vis: Visibility, level: Level| mems::NewMem {
        text: text.into(),
        visibility: vis,
        level,
        created_by: A.into(),
        about: about.clone(),
        ..Default::default()
    };
    let private = tx(s, |tx| {
        mems::create(
            tx,
            "default",
            &new("只给 A", Visibility::Private, Level::Conclusion),
        )
    });
    let shared = tx(s, |tx| {
        mems::create(
            tx,
            "default",
            &new("大家都能看", Visibility::Workspace, Level::Public),
        )
    });
    let never = tx(s, |tx| {
        mems::create(
            tx,
            "default",
            &new("不可使用", Visibility::Workspace, Level::Never),
        )
    });
    assert_eq!(private.about, about);
    assert_eq!(private.writer, "me");

    let (a, b) = s
        .reader(|c| {
            Ok::<_, StoreError>((
                Viewer::load(c, "default", A)?,
                Viewer::load(c, "default", B)?,
            ))
        })
        .unwrap();
    let texts = |v: &Viewer, f: &mems::MemFilter| -> Vec<String> {
        s.reader(|c| mems::about(c, v, &about[0], f))
            .unwrap()
            .into_iter()
            .map(|m| m.text)
            .collect()
    };
    let mut ta = texts(&a, &mems::MemFilter::default());
    ta.sort();
    assert_eq!(ta, ["只给 A", "大家都能看"]);
    assert_eq!(texts(&b, &mems::MemFilter::default()), ["大家都能看"]);
    assert_eq!(
        texts(
            &b,
            &mems::MemFilter {
                include_never: true,
                ..Default::default()
            }
        )
        .len(),
        2
    );
    // ownership check
    s.reader(|c| {
        assert!(mems::visible(c, &a, &private.id)?.is_some());
        assert!(mems::visible(c, &b, &private.id)?.is_none());
        assert!(mems::visible(c, &b, &shared.id)?.is_some());
        assert!(
            mems::visible(c, &Viewer::bare("w2", A), &shared.id)?.is_none(),
            "other workspace"
        );
        assert_eq!(
            mems::all(c, &b, &mems::MemFilter::usable())?.len(),
            1,
            "never left out"
        );
        assert!(
            mems::visible(c, &b, &never.id)?.is_some(),
            "never still owned, just not retrieved"
        );
        Ok::<_, StoreError>(())
    })
    .unwrap();
}

#[test]
fn team_scope_includes_sub_teams_and_space_scopes_are_added() {
    let t = setup();
    let s = &t.store;
    party(s, "ct_b", "default", "contact", Some(B));
    team(s, "tm_top", None, &[]);
    team(s, "tm_sub", Some("tm_top"), &["ct_b"]);
    team(s, "tm_other", None, &[]);
    let b = s.reader(|c| Viewer::load(c, "default", B)).unwrap();
    assert_eq!(b.scopes, ["team:tm_sub", "team:tm_top"]);
    assert!(
        s.reader(|c| Viewer::load(c, "w2", B))
            .unwrap()
            .scopes
            .is_empty()
    );
    let b = b.with_scopes(["space:spc_1".to_string(), "team:tm_top".to_string()]);
    assert_eq!(b.scopes.len(), 3);

    let mk = |vis: &str| {
        tx(s, |tx| {
            mems::create(
                tx,
                "default",
                &mems::NewMem {
                    text: vis.into(),
                    visibility: Visibility::parse(vis).unwrap(),
                    created_by: A.into(),
                    ..Default::default()
                },
            )
        })
    };
    for v in [
        "team:tm_top",
        "team:tm_sub",
        "team:tm_other",
        "space:spc_1",
        "space:spc_2",
        "private",
    ] {
        mk(v);
    }
    let seen = |v: &Viewer| {
        let mut x: Vec<String> = s
            .reader(|c| mems::all(c, v, &mems::MemFilter::default()))
            .unwrap()
            .into_iter()
            .map(|m| m.text)
            .collect();
        x.sort();
        x
    };
    assert_eq!(seen(&b), ["space:spc_1", "team:tm_sub", "team:tm_top"]);
    assert_eq!(
        seen(&Viewer::bare("default", A)).len(),
        6,
        "the creator sees everything they made"
    );
    assert!(b.sees(&Visibility::Team("tm_top".into()), A));
    assert!(!b.sees(&Visibility::Team("tm_other".into()), A));
    assert!(b.sees(&Visibility::Private, B));
    let sc = b.sql("m");
    assert_eq!(sc.params.len(), 4);
    assert_eq!(sc.sql.matches('?').count(), 4);
}

#[test]
fn deleting_only_about_keeps_shared_memories_and_retarget_moves_them() {
    let t = setup();
    let s = &t.store;
    let x = ObjRef::new("party", "ct_x");
    let y = ObjRef::new("party", "ct_y");
    let mk = |about: Vec<ObjRef>| {
        tx(s, |tx| {
            mems::create(
                tx,
                "default",
                &mems::NewMem {
                    text: "m".into(),
                    created_by: A.into(),
                    about,
                    ..Default::default()
                },
            )
        })
    };
    let only_x = mk(vec![x.clone()]);
    let both = mk(vec![x.clone(), y.clone()]);
    let only_y = mk(vec![y.clone()]);
    assert_eq!(tx(s, |tx| mems::delete_only_about(tx, "default", &x)), 1);
    s.reader(|c| {
        assert!(mems::get(c, &only_x.id)?.is_none());
        assert_eq!(
            mems::get(c, &both.id)?.unwrap().about,
            [x.clone(), y.clone()]
        );
        Ok::<_, StoreError>(())
    })
    .unwrap();
    // merge y into x: memories about y move, the shared one stays once
    let mut moved = tx(s, |tx| {
        mems::retarget_about(tx, "default", "party", "ct_y", "ct_x", None)
    });
    moved.sort();
    let mut want = vec![both.id.clone(), only_y.id.clone()];
    want.sort();
    assert_eq!(moved, want);
    s.reader(|c| {
        assert_eq!(
            mems::get(c, &both.id)?.unwrap().about,
            std::slice::from_ref(&x)
        );
        assert_eq!(
            mems::get(c, &only_y.id)?.unwrap().about,
            std::slice::from_ref(&x)
        );
        Ok::<_, StoreError>(())
    })
    .unwrap();
    // split: move only the recorded one back
    tx(s, |tx| {
        mems::retarget_about(
            tx,
            "default",
            "party",
            "ct_x",
            "ct_y",
            Some(std::slice::from_ref(&only_y.id)),
        )
    });
    assert_eq!(
        s.reader(|c| mems::get(c, &only_y.id))
            .unwrap()
            .unwrap()
            .about,
        std::slice::from_ref(&y)
    );

    let p = tx(s, |tx| {
        mems::patch(
            tx,
            &both.id,
            &mems::MemPatch {
                text: Some("新".into()),
                level: Some(Level::Reasoning),
                status: Some("stale".into()),
                ..Default::default()
            },
        )
    })
    .unwrap();
    assert_eq!(
        (p.text.as_str(), p.level, p.status.as_str()),
        ("新", Level::Reasoning, "stale")
    );
    assert!(
        tx(s, |tx| mems::patch(
            tx,
            "mm_nope",
            &mems::MemPatch::default()
        ))
        .is_none()
    );
    assert!(tx(s, |tx| mems::delete(tx, &both.id)));
    assert!(!tx(s, |tx| mems::delete(tx, &both.id)));
    s.with_conn(|c| {
        assert_eq!(
            count(
                c,
                &format!(
                    "SELECT count(*) FROM mem_about WHERE mem_id = '{}'",
                    both.id
                )
            ),
            0
        );
        Ok(())
    })
    .unwrap();
}

#[test]
fn entity_tags_filter_to_the_workspace_and_follow_tag_merges() {
    let t = setup();
    let s = &t.store;
    let x = ObjRef::new("party", "ct_x");
    let kept = tx(s, |tx| {
        tags::set(
            tx,
            "default",
            &x,
            &[
                "tag_2".into(),
                "tag_w2".into(),
                "tag_none".into(),
                "tag_1".into(),
                "tag_2".into(),
            ],
        )
    });
    assert_eq!(kept, ["tag_2", "tag_1"]);
    assert_eq!(
        s.reader(|c| tags::of(c, "default", &x)).unwrap(),
        ["tag_2", "tag_1"]
    );
    tx(s, |tx| tags::set(tx, "default", &x, &["tag_1".into()]));
    assert_eq!(s.reader(|c| tags::of(c, "default", &x)).unwrap(), ["tag_1"]);
    assert_eq!(
        s.reader(|c| tags::objects(c, "default", "tag_1", "party"))
            .unwrap(),
        ["ct_x"]
    );

    let y = ObjRef::new("party", "ct_y");
    tx(s, |tx| {
        tags::set(tx, "default", &y, &["tag_1".into(), "tag_2".into()])
    });
    let moved = tx(s, |tx| {
        tags::retarget(tx, "default", "party", "ct_y", "ct_x", None)
    });
    assert_eq!(moved, ["tag_1", "tag_2"]);
    let mut on_x = s.reader(|c| tags::of(c, "default", &x)).unwrap();
    on_x.sort();
    assert_eq!(on_x, ["tag_1", "tag_2"]);
    assert!(s.reader(|c| tags::of(c, "default", &y)).unwrap().is_empty());

    // the old tag management keeps working with entity tags pointing at the tag
    s.tags().merge("tag_2", "tag_1").unwrap();
    assert_eq!(s.reader(|c| tags::of(c, "default", &x)).unwrap(), ["tag_1"]);
    s.tags().remove("tag_1").unwrap();
    assert!(s.reader(|c| tags::of(c, "default", &x)).unwrap().is_empty());
    assert_eq!(tx(s, |tx| tags::clear(tx, "default", &x)), 0);
}

#[test]
fn links_by_object_and_retarget_both_ends() {
    let t = setup();
    let s = &t.store;
    let ct = |id: &str| ObjRef::new("party", id);
    let item = ObjRef::new("item", "ST-1");
    tx(s, |tx| {
        let mut w = links::NewLink::new(ct("ct_y"), "works_at", ct("org_1"), "manual");
        w.props = json!({"title": "PM", "to": null});
        assert!(links::add(tx, "default", &w)?);
        assert!(!links::add(tx, "default", &w)?, "same link once");
        let mut r = links::NewLink::new(item.clone(), "related", ct("ct_y"), "infer");
        r.confidence = 0.6;
        links::add(tx, "default", &r)?;
        links::add(
            tx,
            "default",
            &links::NewLink::new(item.clone(), "related", ct("ct_x"), "mention"),
        )?;
        links::add(
            tx,
            "default",
            &links::NewLink::new(ct("ct_x"), "reports_to", ct("ct_y"), "sync"),
        )?;
        Ok(())
    });
    let to_y = s
        .reader(|c| links::to(c, "default", &ct("ct_y"), None))
        .unwrap();
    assert_eq!(to_y.len(), 2);
    assert_eq!(
        s.reader(|c| links::to(c, "default", &ct("ct_y"), Some("related")))
            .unwrap()[0]
            .confidence,
        0.6
    );
    assert_eq!(
        s.reader(|c| links::from(c, "default", &ct("ct_y"), Some("works_at")))
            .unwrap()[0]
            .props["title"],
        "PM"
    );

    let moved = tx(s, |tx| {
        links::retarget(tx, "default", &ct("ct_y"), &ct("ct_x"))
    });
    // works_at moves; item related ct_y collides with the existing related ct_x and is dropped;
    // ct_x reports_to ct_y becomes a self-link ct_x → ct_x
    assert_eq!(moved.len(), 2);
    assert!(
        s.reader(|c| links::from(c, "default", &ct("ct_y"), None))
            .unwrap()
            .is_empty()
    );
    assert!(
        s.reader(|c| links::to(c, "default", &ct("ct_y"), None))
            .unwrap()
            .is_empty()
    );
    let w = s
        .reader(|c| links::from(c, "default", &ct("ct_x"), Some("works_at")))
        .unwrap();
    assert_eq!(w[0].props["title"], "PM");
    // split moves the recorded keys back
    let back = tx(s, |tx| links::restore(tx, "default", &moved));
    assert_eq!(back, 2);
    assert_eq!(
        s.reader(|c| links::from(c, "default", &ct("ct_y"), Some("works_at")))
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        s.reader(|c| links::to(c, "default", &ct("ct_y"), Some("reports_to")))
            .unwrap()[0]
            .subject_id,
        "ct_x"
    );
    assert!(
        s.reader(|c| links::from(c, "default", &ct("ct_x"), Some("works_at")))
            .unwrap()
            .is_empty()
    );

    let k = links::LinkKey {
        subject: ct("ct_y"),
        predicate: "works_at".into(),
        object: ct("org_1"),
    };
    assert!(tx(s, |tx| links::set_props(
        tx,
        "default",
        &k,
        &json!({"to": "2026-10"})
    )));
    assert_eq!(
        s.reader(|c| links::get(c, "default", &k))
            .unwrap()
            .unwrap()
            .props["to"],
        "2026-10"
    );
    assert!(tx(s, |tx| links::remove(tx, "default", &k)));
    assert!(
        s.reader(|c| links::get(c, "default", &k))
            .unwrap()
            .is_none()
    );
}

#[test]
fn changesets_apply_once_and_undo_within_24_hours() {
    let t = setup();
    let s = &t.store;
    let cs = tx(s, |tx| {
        changesets::create(
            tx,
            "default",
            &changesets::NewChangeset {
                target_type: "contact".into(),
                target_id: Some("ct_x".into()),
                target_revision: Some(3),
                title: "Sarah 的资料".into(),
                changes: vec![base::Change::new("名字", "Sarah", "Sarah Chen")],
                ops: vec![json!({"op": "party.patch", "name": "Sarah Chen"})],
                origin: "me".into(),
                created_by: A.into(),
                ..Default::default()
            },
        )
    });
    assert!(cs.id.starts_with("chs_"));
    assert_eq!((cs.status.as_str(), cs.before.as_ref()), ("draft", None));
    assert_eq!(
        t.events.take(),
        [ChangeEvent::Changeset {
            id: cs.id.clone(),
            ws: "default".into()
        }]
    );
    assert!(tx(s, |tx| changesets::mark_applied(
        tx,
        &cs,
        &[json!({"name": "Sarah"})]
    )));
    assert!(
        !tx(s, |tx| changesets::mark_applied(tx, &cs, &[])),
        "second apply refused"
    );
    let cs = s.reader(|c| changesets::get(c, &cs.id)).unwrap().unwrap();
    assert_eq!(cs.before, Some(vec![json!({"name": "Sarah"})]));
    let at = cs.applied.unwrap();
    assert!(cs.undoable(at + changesets::UNDO_WINDOW_MS));
    assert!(!cs.undoable(at + changesets::UNDO_WINDOW_MS + 1));
    assert!(tx(s, |tx| changesets::mark_undone(tx, &cs)));
    assert!(!tx(s, |tx| changesets::mark_undone(tx, &cs)));
    assert!(!tx(s, |tx| changesets::close_draft(tx, &cs, "dropped")));
    let r = s
        .reader(|c| changesets::recent(c, "default", Some("undone"), 10))
        .unwrap();
    assert_eq!(r.len(), 1);
    assert!(
        s.reader(|c| changesets::recent(c, "w2", None, 10))
            .unwrap()
            .is_empty()
    );
}

#[test]
fn confirm_decisions_without_an_item() {
    let t = setup();
    let s = &t.store;
    let d = tx(s, |tx| {
        decisions::open(
            tx,
            &NewDecision {
                workspace_id: "default".into(),
                kind: base::DecisionKind::Confirm.code().into(),
                subject_type: "mem".into(),
                subject_id: "mm_1".into(),
                question: "记住：喜欢简短的邮件？".into(),
                options: Some(base::CONFIRM_OPTIONS.map(String::from).to_vec()),
                requested_by: "steve".into(),
                ..Default::default()
            },
        )
    });
    assert_eq!(
        (d.kind.as_str(), d.item_id.as_ref(), d.options.clone()),
        ("confirm", None, vec!["yes".to_string(), "no".into()])
    );
    assert_eq!(
        t.events.take(),
        [ChangeEvent::Decision {
            id: d.id.clone(),
            item_id: Some(None)
        }]
    );
    // the old repository sees it (待处理 lists every open decision)
    assert_eq!(s.decisions().open("default").unwrap().len(), 1);
    assert_eq!(
        s.reader(|c| decisions::open_for(c, "default", "mem", "mm_1"))
            .unwrap()
            .unwrap()
            .id,
        d.id
    );
    assert!(tx(s, |tx| decisions::settle(
        tx,
        &d.id,
        &json!({"choice": "yes"}),
        "resolved",
        &serde_json::Value::Null
    )));
    assert!(!tx(s, |tx| decisions::settle(
        tx,
        &d.id,
        &json!({}),
        "resolved",
        &serde_json::Value::Null
    )));
    let d = s.decisions().get(&d.id).unwrap().unwrap();
    assert_eq!(
        (d.status.as_str(), d.result.clone()),
        ("resolved", json!({"choice": "yes"}))
    );
    assert!(
        s.reader(|c| decisions::open_for(c, "default", "mem", "mm_1"))
            .unwrap()
            .is_none()
    );
}

#[test]
fn audit_and_revisions() {
    let t = setup();
    let s = &t.store;
    tx(s, |tx| {
        base::audit(
            tx,
            "default",
            A,
            "party.merge",
            "ct_x",
            &json!({"attrs": 2}),
        )?;
        base::revision(
            tx,
            "default",
            "party",
            "ct_x",
            2,
            &json!({"name": "a"}),
            &json!({"name": "b"}),
            A,
        )
    });
    let r = s
        .reader(|c| base::revisions(c, "default", "party", "ct_x"))
        .unwrap();
    assert_eq!(r.len(), 1);
    assert_eq!(r[0].0, 2);
    assert_eq!(
        r[0].1,
        json!({"before": {"name": "a"}, "after": {"name": "b"}})
    );
    s.with_conn(|c| {
        assert_eq!(count(c, "SELECT count(*) FROM audit WHERE action = 'party.merge' AND detail = '{\"attrs\":2}'"), 1);
        Ok(())
    })
    .unwrap();
}

#[test]
fn show_matches_changes_ts() {
    assert_eq!(base::show(""), "（空）");
    assert_eq!(base::show("短"), "短");
    let long = "一".repeat(41);
    assert_eq!(base::show(&long), format!("{}…", "一".repeat(40)));
    assert_eq!(base::show(&"a".repeat(40)), "a".repeat(40));
}
