//! Migration 4 and the shared base repositories (B0 acceptance).

use steve_store::testkit::SqliteFixtures;
mod common;

use common::*;
use rusqlite::Connection;
use serde_json::json;
use steve_store::base::{self, Level, ObjRef, Viewer, Visibility, changesets, links, mems, tags};
use steve_store::{NewDecision, NoPublisher, Store, StoreError, db};

const A: &str = "usr_a";
const B: &str = "usr_b";

/// A fresh store with users A and B in `default`, a second workspace `w2`, and a tag in each.
fn setup() -> T {
    let t = fresh();
    t.store
        .fixture_write(|c| {
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

fn tx<T>(s: &Store, f: impl FnOnce(&mut steve_store::W<'_>) -> steve_store::Result<T>) -> T {
    s.write(f).unwrap()
}

fn party(s: &Store, id: &str, ws: &str, ty: &str, user: Option<&str>) {
    s.fixture_write(|c| {
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
    s.fixture_write(|c| {
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
        .fixture_write(|c| {
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
             INSERT INTO items(id, workspace_id, no, title, status, stage, created, updated) VALUES ('ST-1', 'default', 1, 't', 'run', 'Work', 1, 1);
             INSERT INTO decisions (id, workspace_id, kind, subject_type, subject_id, item_id, question, status, requested_by, created) VALUES ('dec_1', 'default', 'ask', 'item', 'ST-1', 'ST-1', 'q', 'open', 'steve', 1);
             INSERT INTO links (workspace_id, subject_type, subject_id, predicate, object_type, object_id, source, created) VALUES ('default', 'item', 'ST-1', 'about', 'project', 'prj_1', 'me', 1);",
        )
        .unwrap();
    }
    let s = Store::open(&path, NoPublisher).unwrap();
    s.fixture_write(|c| {
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
        [steve_types::BusEvent::Domain(
            steve_types::DomainEvent::Config(steve_types::ConfigEvent::Mm {
                id: {
                    let value: String = m.id.clone();
                    steve_types::MemoryId::from(value)
                },
                ws: {
                    let value: String = "default".into();
                    steve_types::WorkspaceId::from(value)
                }
            })
        )]
    );
    let r: Result<(), StoreError> = s.write(|tx| {
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
    s.fixture_write(|c| {
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
fn single_user_reads_private_memory_without_crossing_workspaces_or_disclosure_levels() {
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
        .fixture_read(|c| {
            Ok::<_, StoreError>((
                Viewer::load(c, "default", A)?,
                Viewer::load(c, "default", B)?,
            ))
        })
        .unwrap();
    let texts = |v: &Viewer, f: &mems::MemFilter| -> Vec<String> {
        s.fixture_read(|c| mems::about(c, v, &about[0], f))
            .unwrap()
            .into_iter()
            .map(|m| m.text)
            .collect()
    };
    let mut ta = texts(&a, &mems::MemFilter::default());
    ta.sort();
    assert_eq!(ta, ["只给 A", "大家都能看"]);
    let mut tb = texts(&b, &mems::MemFilter::default());
    tb.sort();
    assert_eq!(tb, ta);
    assert_eq!(
        texts(
            &b,
            &mems::MemFilter {
                include_never: true,
                ..Default::default()
            }
        )
        .len(),
        3
    );
    // Workspace isolation and disclosure-level filtering remain independent.
    s.fixture_read(|c| {
        assert!(mems::visible(c, &a, &private.id)?.is_some());
        assert!(mems::visible(c, &b, &private.id)?.is_some());
        assert!(mems::visible(c, &b, &shared.id)?.is_some());
        assert!(
            mems::visible(c, &Viewer::bare("w2", A), &shared.id)?.is_none(),
            "other workspace"
        );
        assert_eq!(
            mems::all(c, &b, &mems::MemFilter::usable())?.len(),
            2,
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
fn service_cannot_read_its_own_private_memory_candidates() {
    let t = setup();
    for user in ["", "svc:default"] {
        let create = |visibility| {
            tx(&t.store, |tx| {
                mems::create(
                    tx,
                    "default",
                    &mems::NewMem {
                        text: "Service-proposed memory".into(),
                        status: "candidate".into(),
                        visibility,
                        created_by: user.into(),
                        ..Default::default()
                    },
                )
            })
        };
        let private = create(Visibility::Private);
        let shared = create(Visibility::Workspace);
        t.store
            .fixture_read(|c| {
                let viewer = Viewer::bare("default", user);
                assert!(mems::visible(c, &viewer, &private.id)?.is_none());
                assert!(mems::visible(c, &viewer, &shared.id)?.is_some());
                let listed = mems::all(c, &viewer, &mems::MemFilter::default())?;
                assert!(!listed.iter().any(|m| m.id == private.id));
                assert!(listed.iter().any(|m| m.id == shared.id));
                assert!(mems::visible(c, &Viewer::bare("default", A), &private.id)?.is_some());
                assert!(mems::visible(c, &Viewer::bare("w2", A), &private.id)?.is_none());
                Ok::<_, StoreError>(())
            })
            .unwrap();
    }
}

#[test]
fn service_audiences_remain_workspace_only_with_legacy_bindings() {
    let c = Connection::open_in_memory().unwrap();
    c.execute_batch("CREATE TABLE objects (visibility TEXT, created_by TEXT);
        INSERT INTO objects VALUES ('workspace', NULL), ('private', ''), ('private', 'svc:default'), ('team:t1', 'usr_a'), (NULL, NULL);")
        .unwrap();
    for user in ["", "svc:default"] {
        let viewer = Viewer {
            scopes: vec!["team:t1".into()],
            ..Viewer::bare("default", user)
        };
        assert!(viewer.is_service());
        assert!(viewer.sees(&Visibility::Workspace, A));
        assert!(!viewer.sees(&Visibility::Private, user));
        assert!(!viewer.sees(&Visibility::Team("t1".into()), A));
        let scope = viewer.sql("o");
        assert_eq!(scope.params.len(), 2);
        let found = c
            .prepare(&format!(
                "SELECT visibility FROM objects o WHERE {}",
                scope.sql
            ))
            .unwrap()
            .query_map(rusqlite::params_from_iter(scope.params), |r| {
                r.get::<_, String>(0)
            })
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap();
        assert_eq!(found, ["workspace"]);
    }
    assert!(!Viewer::bare("default", A).is_service());
}

#[test]
fn single_user_visibility_keeps_bindings_and_does_not_load_team_scopes() {
    let t = setup();
    party(&t.store, "ct_b", "default", "contact", Some(B));
    team(&t.store, "tm_top", None, &[]);
    team(&t.store, "tm_sub", Some("tm_top"), &["ct_b"]);
    let viewer = t
        .store
        .fixture_read(|c| Viewer::load(c, "default", B))
        .unwrap();
    assert!(viewer.scopes.is_empty());
    // Loading no longer needs any team or party schema at all.
    let c = Connection::open_in_memory().unwrap();
    assert_eq!(Viewer::load(&c, "default", B).unwrap(), viewer);
    let viewer = Viewer {
        scopes: vec!["team:tm_top".into(), "space:spc_1".into()],
        ..viewer
    };
    for code in [
        "workspace",
        "private",
        "team:tm_top",
        "team:other",
        "space:spc_1",
        "space:other",
    ] {
        assert!(viewer.sees(&Visibility::parse(code).unwrap(), A));
        let scope = base::visible_sql("m", &viewer);
        assert_eq!(
            scope.params,
            [B, "team:tm_top", "space:spc_1"]
                .map(|value| rusqlite::types::Value::Text(value.into()))
                .to_vec()
        );
        let sql = format!(
            "SELECT count(*) FROM (SELECT ? AS visibility, ? AS created_by) m WHERE {}",
            scope.sql
        );
        let mut params = vec![
            rusqlite::types::Value::Text(code.into()),
            rusqlite::types::Value::Text(A.into()),
        ];
        params.extend(scope.params);
        assert_eq!(
            c.query_row(&sql, rusqlite::params_from_iter(params), |r| r
                .get::<_, i64>(0))
                .unwrap(),
            1
        );
    }
    for viewer in [Viewer::bare("default", B), viewer] {
        let scope = viewer.sql("m");
        let sql = format!(
            "SELECT count(*) FROM (SELECT NULL AS visibility, NULL AS created_by) m WHERE {}",
            scope.sql
        );
        for params in [
            scope.params.clone(),
            vec![rusqlite::types::Value::Null; scope.params.len()],
        ] {
            assert_eq!(
                c.query_row(&sql, rusqlite::params_from_iter(params), |r| r
                    .get::<_, i64>(0))
                    .unwrap(),
                1
            );
        }
    }
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
    s.read(|c| {
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
    s.read(|c| {
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
        s.read(|c| mems::get(c, &only_y.id)).unwrap().unwrap().about,
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
    s.fixture_write(|c| {
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
        s.fixture_read(|c| tags::of(c, "default", &x)).unwrap(),
        ["tag_2", "tag_1"]
    );
    tx(s, |tx| tags::set(tx, "default", &x, &["tag_1".into()]));
    assert_eq!(
        s.fixture_read(|c| tags::of(c, "default", &x)).unwrap(),
        ["tag_1"]
    );
    assert_eq!(
        s.fixture_read(|c| tags::objects(c, "default", "tag_1", "party"))
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
    let mut on_x = s.fixture_read(|c| tags::of(c, "default", &x)).unwrap();
    on_x.sort();
    assert_eq!(on_x, ["tag_1", "tag_2"]);
    assert!(
        s.fixture_read(|c| tags::of(c, "default", &y))
            .unwrap()
            .is_empty()
    );

    // the old tag management keeps working with entity tags pointing at the tag
    s.tags().merge("tag_2", "tag_1").unwrap();
    assert_eq!(
        s.fixture_read(|c| tags::of(c, "default", &x)).unwrap(),
        ["tag_1"]
    );
    s.tags().remove("tag_1").unwrap();
    assert!(
        s.fixture_read(|c| tags::of(c, "default", &x))
            .unwrap()
            .is_empty()
    );
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
        .fixture_read(|c| links::to(c, "default", &ct("ct_y"), None))
        .unwrap();
    assert_eq!(to_y.len(), 2);
    assert_eq!(
        s.fixture_read(|c| links::to(c, "default", &ct("ct_y"), Some("related")))
            .unwrap()[0]
            .confidence,
        0.6
    );
    assert_eq!(
        s.fixture_read(|c| links::from(c, "default", &ct("ct_y"), Some("works_at")))
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
        s.fixture_read(|c| links::from(c, "default", &ct("ct_y"), None))
            .unwrap()
            .is_empty()
    );
    assert!(
        s.fixture_read(|c| links::to(c, "default", &ct("ct_y"), None))
            .unwrap()
            .is_empty()
    );
    let w = s
        .fixture_read(|c| links::from(c, "default", &ct("ct_x"), Some("works_at")))
        .unwrap();
    assert_eq!(w[0].props["title"], "PM");
    // split moves the recorded keys back
    let back = tx(s, |tx| links::restore(tx, "default", &moved));
    assert_eq!(back, 2);
    assert_eq!(
        s.fixture_read(|c| links::from(c, "default", &ct("ct_y"), Some("works_at")))
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        s.fixture_read(|c| links::to(c, "default", &ct("ct_y"), Some("reports_to")))
            .unwrap()[0]
            .subject_id,
        "ct_x"
    );
    assert!(
        s.fixture_read(|c| links::from(c, "default", &ct("ct_x"), Some("works_at")))
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
        s.fixture_read(|c| links::get(c, "default", &k))
            .unwrap()
            .unwrap()
            .props["to"],
        "2026-10"
    );
    assert!(tx(s, |tx| links::remove(tx, "default", &k)));
    assert!(
        s.fixture_read(|c| links::get(c, "default", &k))
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
        [steve_types::BusEvent::Domain(
            steve_types::DomainEvent::Config(steve_types::ConfigEvent::Changeset {
                id: {
                    let value: String = cs.id.clone();
                    steve_types::ChangesetId::from(value)
                },
                ws: {
                    let value: String = "default".into();
                    steve_types::WorkspaceId::from(value)
                }
            })
        )]
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
    let cs = s.read(|c| changesets::get(c, &cs.id)).unwrap().unwrap();
    assert_eq!(cs.before, Some(vec![json!({"name": "Sarah"})]));
    let at = cs.applied.unwrap();
    assert!(cs.undoable(at + changesets::UNDO_WINDOW_MS));
    assert!(!cs.undoable(at + changesets::UNDO_WINDOW_MS + 1));
    assert!(tx(s, |tx| changesets::mark_undone(tx, &cs)));
    assert!(!tx(s, |tx| changesets::mark_undone(tx, &cs)));
    assert!(!tx(s, |tx| changesets::close_draft(tx, &cs, "dropped")));
    let r = s
        .fixture_read(|c| changesets::recent(c, "default", Some("undone"), 10))
        .unwrap();
    assert_eq!(r.len(), 1);
    assert!(
        s.fixture_read(|c| changesets::recent(c, "w2", None, 10))
            .unwrap()
            .is_empty()
    );
}

#[test]
fn confirm_decisions_without_an_item() {
    let t = setup();
    let s = &t.store;
    let d = tx(s, |tx| {
        steve_store::DecisionRows(tx).open(&NewDecision {
            workspace_id: "default".into(),
            kind: base::DecisionKind::Confirm.code().into(),
            subject_type: "mem".into(),
            subject_id: "mm_1".into(),
            question: "记住：喜欢简短的邮件？".into(),
            options: Some(base::CONFIRM_OPTIONS.map(String::from).to_vec()),
            requested_by: "steve".into(),
            ..Default::default()
        })
    });
    assert_eq!(
        (d.kind.as_str(), d.item_id.as_ref(), d.options.clone()),
        ("confirm", None, vec!["yes".to_string(), "no".into()])
    );
    assert_eq!(
        t.events.take(),
        [steve_types::BusEvent::Domain(
            steve_types::DomainEvent::Work(steve_types::WorkEvent::Decision {
                id: {
                    let value: String = d.id.clone();
                    steve_types::DecisionId::from(value)
                },
                item_id: {
                    let value: Option<Option<String>> = Some(None);
                    value.map(|value| value.map(Into::into))
                },
                ws: steve_types::WorkspaceId::from(steve_store::WS)
            })
        )]
    );
    // the old repository sees it (待处理 lists every open decision)
    assert_eq!(s.decisions().open("default").unwrap().len(), 1);
    assert_eq!(
        s.read(|r| steve_store::DecisionRows(r).open_for("default", "mem", "mm_1"))
            .unwrap()
            .unwrap()
            .id,
        d.id
    );
    assert!(tx(s, |tx| steve_store::DecisionRows(tx).settle(
        &d.id,
        &json!({"choice": "yes"}),
        "resolved",
        &serde_json::Value::Null
    )));
    assert!(!tx(s, |tx| steve_store::DecisionRows(tx).settle(
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
        s.read(|r| steve_store::DecisionRows(r).open_for("default", "mem", "mm_1"))
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
        .fixture_read(|c| base::revisions(c, "default", "party", "ct_x"))
        .unwrap();
    assert_eq!(r.len(), 1);
    assert_eq!(r[0].0, 2);
    assert_eq!(
        r[0].1,
        json!({"before": {"name": "a"}, "after": {"name": "b"}})
    );
    s.fixture_write(|c| {
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
