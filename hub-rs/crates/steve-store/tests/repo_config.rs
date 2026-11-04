#![allow(clippy::unit_arg, clippy::cloned_ref_to_slice_refs)] // terse assertions in tests
//! Workspaces, users, auth, settings, counters, bots, memories, projects, tags, devices, links.

mod common;

use common::*;
use serde_json::{Map, json};
use steve_store::*;

fn steve(s: &Store) {
    let b = NewBot {
        name: "Steve".into(),
        harness: "claude-code".into(),
        role: Some("你的助手".into()),
        avatar: Some(Avatar {
            color: Some("#7aa7c7".into()),
            skin: None,
            round: Some(70.into()),
        }),
        ..Default::default()
    };
    s.bots().seed("steve", true, &b, WS).unwrap();
}

#[test]
fn reasoning_effort_persists_and_can_be_reset_to_default() {
    let t = fresh();
    let s = &t.store;
    let bot = s
        .bots()
        .create(
            WS,
            &NewBot {
                name: "思考".into(),
                harness: "codex".into(),
                reasoning_effort: Some("high".into()),
                ..Default::default()
            },
            "me",
        )
        .unwrap()
        .unwrap();
    assert_eq!(bot.reasoning_effort.as_deref(), Some("high"));
    let updated = s
        .bots()
        .patch(
            &bot.id,
            &BotPatch {
                reasoning_effort: Some(Some("low".into())),
                ..Default::default()
            },
            "me",
        )
        .unwrap()
        .unwrap();
    assert_eq!(updated.reasoning_effort.as_deref(), Some("low"));
    s.bots()
        .patch(
            &bot.id,
            &BotPatch {
                reasoning_effort: Some(None),
                ..Default::default()
            },
            "me",
        )
        .unwrap();
    assert!(
        s.bots()
            .get(&bot.id)
            .unwrap()
            .unwrap()
            .reasoning_effort
            .is_none()
    );
}

#[test]
fn helpers() {
    let id = uid("ses");
    assert!(
        id.starts_with("ses_") && id.len() == 20 && id[4..].chars().all(|c| c.is_ascii_hexdigit())
    );
    assert_eq!(
        sha("abc"),
        "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
    );
    assert!(now() > 1_700_000_000_000);
}

#[test]
fn ensure_workspace_renames_and_carries_counter() {
    let t = fresh();
    let s = &t.store;
    s.with_conn(|c| {
        c.execute_batch("UPDATE workspaces SET name = '默认工作空间'; INSERT INTO counters VALUES ('default', 'item', 5), ('*', 'item:ST', 9);")?;
        Ok(())
    })
    .unwrap();
    s.ensure_workspace().unwrap();
    let ws = s.workspaces().get(WS).unwrap().unwrap();
    assert_eq!(
        (
            ws.name.as_str(),
            ws.item_prefix.as_str(),
            ws.description.as_str()
        ),
        ("个人", "ST", "")
    );
    assert_eq!(
        s.peek_counter("item:ST").unwrap(),
        9,
        "carry-over keeps the larger value"
    );
    s.with_conn(|c| {
        Ok(assert_eq!(
            count(
                c,
                "SELECT count(*) FROM counters WHERE workspace_id = 'default'"
            ),
            0
        ))
    })
    .unwrap();
    s.ensure_workspace().unwrap();
    assert_eq!(s.workspaces().all().unwrap().len(), 1);
}

#[test]
fn workspaces_users_auth() {
    let t = fresh();
    let s = &t.store;
    assert_eq!(s.users().count().unwrap(), 0);
    let me = s
        .users()
        .create("sub-1", "Lee", Some("lee@example.com"), "owner")
        .unwrap();
    assert!(t.events.take().is_empty(), "users.create publishes nothing");
    assert_eq!(s.users().by_subject("sub-1").unwrap().unwrap().id, me);
    assert_eq!(s.users().count().unwrap(), 1);

    let w = s
        .workspaces()
        .create(
            &NewWorkspace {
                name: "工作".into(),
                item_prefix: Some("WK".into()),
                ..Default::default()
            },
            &me,
        )
        .unwrap();
    assert_eq!((w.sort, w.item_prefix.as_str()), (1, "WK"));
    // a new workspace comes with the six item categories (ST-34)
    let ev = t.events.take();
    assert_eq!(
        ev.last(),
        Some(&ChangeEvent::Workspace { id: w.id.clone() })
    );
    assert_eq!(
        ev.iter()
            .filter(|e| matches!(e, ChangeEvent::Tag { .. }))
            .count(),
        6
    );
    let mine = s.workspaces().of_user(&me).unwrap();
    assert_eq!(
        mine.iter()
            .map(|m| (m.workspace.id.as_str(), m.role.as_str()))
            .collect::<Vec<_>>(),
        [(WS, "owner"), (w.id.as_str(), "owner")]
    );
    assert!(s.workspaces().is_member(&w.id, &me).unwrap());
    assert!(!s.workspaces().is_member(&w.id, "usr_x").unwrap());

    let p = s
        .workspaces()
        .patch(
            &w.id,
            &WorkspacePatch {
                description: Some("d".into()),
                ..Default::default()
            },
        )
        .unwrap()
        .unwrap();
    assert_eq!((p.name.as_str(), p.description.as_str()), ("工作", "d"));
    assert_eq!(
        t.events.take(),
        vec![ChangeEvent::Workspace { id: w.id.clone() }]
    );
    let mut ids = s.workspaces().ids().unwrap();
    ids.sort();
    assert_eq!(ids.len(), 2);

    s.auth_sessions().create(&me, "tok", 60_000).unwrap();
    s.auth_sessions().create(&me, "old", -1).unwrap();
    assert_eq!(s.auth_sessions().user("tok").unwrap().unwrap().id, me);
    assert!(s.auth_sessions().user("old").unwrap().is_none(), "expired");
    s.with_conn(|c| {
        Ok(assert_eq!(
            count(
                c,
                &format!(
                    "SELECT count(*) FROM auth_sessions WHERE token_hash = '{}'",
                    sha("tok")
                )
            ),
            1
        ))
    })
    .unwrap();
    s.auth_sessions().drop("tok").unwrap();
    assert!(s.auth_sessions().user("tok").unwrap().is_none());

    s.audit("me", "user.create", &me, &json!({"role": "owner"}), WS)
        .unwrap();
    s.with_conn(|c| {
        Ok(assert_eq!(
            count(
                c,
                r#"SELECT count(*) FROM audit WHERE detail = '{"role":"owner"}'"#
            ),
            1
        ))
    })
    .unwrap();
}

#[test]
fn settings() {
    let t = fresh();
    let s = &t.store;
    assert_eq!(
        s.settings().get(WS, "archDays", "30".to_string()).unwrap(),
        "30"
    );
    let mut p = Map::new();
    p.insert("archDays".into(), json!("7"));
    p.insert("handoff".into(), json!({"a": 1}));
    s.settings().set(WS, &p, "me").unwrap();
    assert_eq!(
        t.events.take(),
        vec![ChangeEvent::Settings { id: WS.into() }]
    );
    assert_eq!(
        s.settings().get(WS, "archDays", "30".to_string()).unwrap(),
        "7"
    );
    assert_eq!(
        s.settings().get(WS, "archDays", 0i64).unwrap(),
        0,
        "undecodable → default"
    );
    assert_eq!(
        serde_json::to_value(s.settings().all(WS).unwrap()).unwrap(),
        json!({"archDays": "7", "handoff": {"a": 1}})
    );
    s.with_conn(|c| {
        c.execute("INSERT INTO settings VALUES ('default', 'bad', 'not json', 0)", [])?;
        Ok(assert_eq!(count(c, r#"SELECT count(*) FROM revisions WHERE object_type = 'settings' AND patch = '{"archDays":"7","handoff":{"a":1}}' AND version = 0"#), 1))
    })
    .unwrap();
    assert_eq!(s.settings().all(WS).unwrap()["bad"], json!(null));
}

#[test]
fn bots_seed_create_patch() {
    let t = fresh();
    let s = &t.store;
    steve(s);
    assert!(t.events.take().is_empty(), "seed publishes nothing");
    // seeding again never overwrites
    s.bots()
        .seed(
            "steve",
            true,
            &NewBot {
                name: "Other".into(),
                harness: "codex".into(),
                ..Default::default()
            },
            WS,
        )
        .unwrap();
    let b = s.bots().get("steve").unwrap().unwrap();
    assert_eq!(
        (
            b.name.as_str(),
            b.main,
            b.accept_mode.as_str(),
            b.proactive,
            b.revision
        ),
        ("Steve", true, "inherit", 1, 1)
    );
    let j = serde_json::to_value(&b).unwrap();
    assert_eq!(j["avatar"], json!({"color": "#7aa7c7", "round": 70}));
    assert_eq!(j["main"], json!(true));
    assert!(
        j.get("deleted").is_none() && j.get("project_dir").is_none() && j.get("worktree").is_none()
    );

    let dev = s
        .bots()
        .create(
            WS,
            &NewBot {
                name: "Dev".into(),
                harness: "codex".into(),
                ..Default::default()
            },
            "me",
        )
        .unwrap()
        .unwrap();
    assert!(!dev.main && dev.enabled && dev.id.starts_with("bot_"));
    assert_eq!(
        t.events.take(),
        vec![ChangeEvent::Bot { id: dev.id.clone() }]
    );
    s.with_conn(|c| {
        Ok(assert_eq!(
            count(
                c,
                "SELECT count(*) FROM audit WHERE action = 'bot.create' AND detail = '{}'"
            ),
            1
        ))
    })
    .unwrap();
    assert_eq!(
        s.bots()
            .list(Some(WS))
            .unwrap()
            .iter()
            .map(|b| b.name.as_str())
            .collect::<Vec<_>>(),
        ["Steve", "Dev"]
    );
    assert_eq!(s.bots().main().unwrap().unwrap().id, "steve");

    let p = BotPatch {
        name: Some("开发".into()),
        model: Some(None),
        enabled: Some(false),
        ..Default::default()
    };
    let after = s.bots().patch(&dev.id, &p, "me").unwrap().unwrap();
    assert_eq!(
        (after.name.as_str(), after.enabled, after.revision),
        ("开发", false, 2)
    );
    assert_eq!(
        t.events.take(),
        vec![ChangeEvent::Bot { id: dev.id.clone() }]
    );
    s.with_conn(|c| {
        let (v, patch): (i64, String) = c.query_row("SELECT version, patch FROM revisions WHERE object_type = 'bot'", [], |r| Ok((r.get(0)?, r.get(1)?)))?;
        assert_eq!(v, 2);
        assert_eq!(patch, r#"{"before":{"name":"Dev","model":null,"enabled":true},"after":{"name":"开发","model":null,"enabled":false}}"#);
        Ok(())
    })
    .unwrap();
    // an empty patch still writes a revision row but does not bump
    assert_eq!(
        s.bots()
            .patch(&dev.id, &BotPatch::default(), "me")
            .unwrap()
            .unwrap()
            .revision,
        2
    );
    match s.bots().patch("nope", &p, "me") {
        Err(e) => assert_eq!(e.to_string(), "没有这个 Bot"),
        Ok(_) => panic!("expected an error"),
    }

    let w2 = s.workspaces().create(
        &NewWorkspace {
            name: "B".into(),
            ..Default::default()
        },
        "usr_none",
    );
    assert!(w2.is_err(), "membership needs a real user (foreign key)");
    s.with_conn(|c| {
        Ok(c.execute(
            "INSERT INTO workspaces (id, name, created) VALUES ('w2', 'B', 1)",
            [],
        )?)
    })
    .unwrap();
    s.bots().set_workspaces(&dev.id, &["w2".into()]).unwrap();
    assert!(
        s.bots().in_workspace(&dev.id, "w2").unwrap()
            && !s.bots().in_workspace(&dev.id, WS).unwrap()
    );
    s.bots().set_workspaces(&dev.id, &[]).unwrap();
    assert!(s.bots().workspaces(&dev.id).unwrap().is_empty());
    t.events.take();

    s.bots().remove("steve").unwrap();
    assert!(
        s.bots().get("steve").unwrap().is_some(),
        "main bot cannot be removed"
    );
    s.bots().remove(&dev.id).unwrap();
    assert!(s.bots().get(&dev.id).unwrap().is_none());
    s.bots().restore(&dev.id).unwrap();
    assert!(s.bots().get(&dev.id).unwrap().unwrap().enabled);
}

#[test]
fn seed_fixups() {
    let t = fresh();
    let s = &t.store;
    s.bots()
        .seed(
            "steve",
            true,
            &NewBot {
                name: "Steve".into(),
                harness: "claude-code".into(),
                ..Default::default()
            },
            WS,
        )
        .unwrap();
    s.bots()
        .seed(
            "dev",
            false,
            &NewBot {
                name: "Dev".into(),
                harness: "claude-code".into(),
                ..Default::default()
            },
            WS,
        )
        .unwrap();
    s.bots().seed_fixups(Some("prj_1")).unwrap();
    assert_eq!(
        serde_json::to_string(&s.bots().get("steve").unwrap().unwrap().avatar).unwrap(),
        r##"{"color":"#7aa7c7","round":70}"##
    );
    let dev = s.bots().get("dev").unwrap().unwrap();
    assert_eq!(dev.default_project.as_deref(), Some("prj_1"));
    assert_eq!(dev.avatar.round, Some(45.into()));
}

#[test]
fn memories() {
    let t = fresh();
    let s = &t.store;
    steve(s);
    let a = s.memories().add("steve", WS, "一", "me").unwrap();
    std::thread::sleep(std::time::Duration::from_millis(2));
    let b = s.memories().add("steve", WS, "二", "steve").unwrap();
    assert_eq!(s.memories().count("steve", WS).unwrap(), 2);
    assert_eq!(
        s.memories()
            .list("steve", WS)
            .unwrap()
            .iter()
            .map(|m| m.id.as_str())
            .collect::<Vec<_>>(),
        [b.as_str(), a.as_str()]
    );
    s.memories().edit(&a, "改").unwrap();
    assert_eq!(s.memories().get(&a).unwrap().unwrap().text, "改");
    s.memories().remove(&b).unwrap();
    s.memories().remove("mem_missing").unwrap();
    s.memories().edit("mem_missing", "x").unwrap();
    let bot = ChangeEvent::Bot { id: "steve".into() };
    assert_eq!(
        t.events.take(),
        vec![bot.clone(), bot.clone(), bot.clone(), bot]
    );
    s.memories().clear(WS).unwrap();
    assert_eq!(t.events.take(), vec![ChangeEvent::Bot { id: "*".into() }]);
    s.memories().add("steve", WS, "三", "me").unwrap();
    s.memories().remove_of_bot("steve").unwrap();
    assert_eq!(s.memories().count("steve", WS).unwrap(), 0);
}

#[test]
fn projects() {
    let t = fresh();
    let s = &t.store;
    let rel = Release {
        kind: "script".into(),
        cmd: "/x/release.sh".into(),
        done_when: None,
    };
    let np = NewProject {
        name: "steve".into(),
        path: "/repo".into(),
        kind: "git".into(),
        device_id: "hub".into(),
        worktree: true,
        release: Some(rel.clone()),
        ..Default::default()
    };
    let p = s.projects().create(WS, &np).unwrap();
    assert_eq!(
        t.events.take(),
        vec![ChangeEvent::Project { id: p.id.clone() }]
    );
    let j = serde_json::to_value(&p).unwrap();
    assert_eq!(
        (
            j["worktree"].clone(),
            j["archived"].clone(),
            j["release"].clone()
        ),
        (
            json!(true),
            json!(false),
            json!({"kind": "script", "cmd": "/x/release.sh"})
        )
    );
    assert_eq!(s.projects().by_path(WS, "/repo").unwrap().unwrap().id, p.id);
    let q = s
        .projects()
        .patch(
            &p.id,
            &ProjectPatch {
                release: Some(None),
                archived: Some(true),
                ..Default::default()
            },
            "me",
        )
        .unwrap();
    assert!(q.release.is_none() && q.archived);
    assert!(s.projects().list(WS, false).unwrap().is_empty());
    assert_eq!(s.projects().list(WS, true).unwrap().len(), 1);
    s.with_conn(|c| Ok(assert_eq!(count(c, r#"SELECT count(*) FROM revisions WHERE object_type = 'project' AND patch = '{"release":null,"archived":true}'"#), 1))).unwrap();
    assert_eq!(
        s.projects()
            .patch("prj_x", &ProjectPatch::default(), "me")
            .unwrap_err()
            .to_string(),
        "没有这个项目"
    );
}

#[test]
fn tags() {
    let t = fresh();
    let s = &t.store;
    steve(s);
    let item = |title: &str| {
        s.items()
            .create(
                WS,
                &NewItem {
                    title: title.into(),
                    goal: "g".into(),
                    bot_id: "steve".into(),
                    ..Default::default()
                },
            )
            .unwrap()
            .unwrap()
            .id
    };
    let (i1, i2) = (item("a"), item("b"));
    let feat = s.tags().kinds(WS).unwrap()[1].id.clone();
    let nt = |n: &str| NewTag {
        name: n.into(),
        cat: "主题".into(),
        description: "".into(),
    };
    let a = s.tags().create(WS, &nt("前端"), "me").unwrap();
    let again = s.tags().create(WS, &nt("前端"), "steve").unwrap();
    assert_eq!(a.id, again.id);
    let b = s.tags().create(WS, &nt("界面"), "me").unwrap();
    t.events.take();
    s.tags()
        .set_item(&i1, &[a.id.clone(), b.id.clone(), feat.clone()])
        .unwrap();
    s.tags()
        .set_item(&i2, &[b.id.clone(), feat.clone()])
        .unwrap();
    assert_eq!(
        t.events.take(),
        vec![
            ChangeEvent::Item { id: i1.clone() },
            ChangeEvent::Item { id: i2.clone() }
        ]
    );
    assert_eq!(s.tags().uses(WS).unwrap()[&b.id], 2);
    s.tags().merge(&b.id, &a.id).unwrap();
    assert_eq!(t.events.take(), vec![ChangeEvent::Tag { id: a.id.clone() }]);
    assert!(s.tags().get(&b.id).unwrap().is_none());
    assert_eq!(s.tags().of_item(&i2).unwrap(), [feat.clone(), a.id.clone()]);
    assert_eq!(s.tags().uses(WS).unwrap()[&a.id], 2);
    let renamed = s
        .tags()
        .patch(
            &a.id,
            &TagPatch {
                name: Some("Web".into()),
                ..Default::default()
            },
        )
        .unwrap()
        .unwrap();
    assert_eq!(renamed.name, "Web");
    s.tags().remove(&a.id).unwrap();
    // the category stays: it cannot be removed, and only it is left
    assert_eq!(s.tags().of_item(&i1).unwrap(), [feat.clone()]);
    assert!(s.tags().list(WS).unwrap().iter().all(|t| t.cat == "类型"));
}

#[test]
fn devices_and_links() {
    let t = fresh();
    let s = &t.store;
    let d = DeviceInfo {
        id: "hub".into(),
        name: "Hub".into(),
        roles: vec!["Hub".into(), "执行节点".into()],
        os: "darwin".into(),
        version: "1".into(),
    };
    s.devices().upsert(&d).unwrap();
    s.devices()
        .upsert(&DeviceInfo {
            name: "Renamed?".into(),
            version: "2".into(),
            ..d.clone()
        })
        .unwrap();
    assert!(t.events.take().is_empty(), "upsert publishes nothing");
    let got = &s.devices().list().unwrap()[0];
    assert_eq!(
        (
            got.name.as_str(),
            got.version.as_str(),
            got.status.as_str(),
            got.max_sessions
        ),
        ("Hub", "2", "正常", 4)
    );
    assert_eq!(
        serde_json::to_value(got).unwrap()["roles"],
        json!(["Hub", "执行节点"])
    );
    s.devices()
        .patch(
            "hub",
            &DevicePatch {
                max_sessions: Some(8),
                ..Default::default()
            },
        )
        .unwrap();
    assert_eq!(
        t.events.take(),
        vec![ChangeEvent::Device { id: "hub".into() }]
    );
    assert_eq!(s.devices().list().unwrap()[0].max_sessions, 8);

    assert!(
        s.links()
            .add(
                WS,
                ("item", "ST-1"),
                "about",
                ("project", "p"),
                "steve",
                &json!({"w": 1})
            )
            .unwrap()
    );
    assert!(
        !s.links()
            .add(
                WS,
                ("item", "ST-1"),
                "about",
                ("project", "p"),
                "me",
                &json!({})
            )
            .unwrap()
    );
    let l = &s.links().from(WS, "item", "ST-1").unwrap()[0];
    assert_eq!(
        (l.props.clone(), l.confidence, l.source.as_str()),
        (json!({"w": 1}), 1.0, "steve")
    );
}
