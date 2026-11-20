//! Current schema, connection policy and repository decoding.

mod common;

use common::*;
use rusqlite::Connection;

use steve_store::{NoPublisher, Store};

#[test]
fn pragmas_and_version() {
    let t = fresh();
    t.store
        .with_conn(|c| {
            let mode: String = c.query_row("PRAGMA journal_mode", [], |r| r.get(0))?;
            assert_eq!(mode, "wal");
            assert_eq!(
                c.query_row("PRAGMA foreign_keys", [], |r| r.get::<_, i64>(0))?,
                1
            );
            assert_eq!(
                c.query_row("PRAGMA busy_timeout", [], |r| r.get::<_, i64>(0))?,
                5000
            );
            // synchronous = NORMAL (1) under WAL
            assert_eq!(
                c.query_row("PRAGMA synchronous", [], |r| r.get::<_, i64>(0))?,
                1
            );
            assert_eq!(
                count(c, "SELECT min_reader FROM schema_compat"),
                steve_store_sqlite::compat::LEGACY_READER_FENCE
            );
            assert_eq!(count(c, "SELECT count(*) FROM domain_schema"), 3);
            Ok(())
        })
        .unwrap();
}

#[test]
fn reopen_changes_nothing() {
    let t = fresh();
    let path = t.store.path().to_path_buf();
    t.store
        .bots()
        .seed(
            "steve",
            true,
            &steve_store::NewBot {
                name: "Steve".into(),
                harness: "claude-code".into(),
                ..Default::default()
            },
            "default",
        )
        .unwrap();
    t.store
        .items()
        .create(
            "default",
            &steve_store::NewItem {
                tags: common::task_tags(&t.store, "default"),
                title: "a".into(),
                goal: "g".into(),
                bot_id: "steve".into(),
                ..Default::default()
            },
        )
        .unwrap();
    drop(t.store);
    let before = schema_of(&path);
    let s = Store::open(&path, NoPublisher).unwrap();
    assert_eq!(schema_of(&path), before);
    s.with_conn(|c| {
        assert_eq!(
            count(c, "SELECT min_reader FROM schema_compat"),
            steve_store_sqlite::compat::LEGACY_READER_FENCE
        );
        assert_eq!(count(c, "SELECT count(*) FROM items"), 1);
        Ok(())
    })
    .unwrap();
}

#[test]
fn row_types_follow_column_order() {
    use steve_store::*;
    let t = fresh();
    let s = &t.store;
    let c = Connection::open(s.path()).unwrap();
    let cols = |t: &str| columns(&c, t);
    let without = |t: &str, drop: &[&str]| {
        cols(t)
            .into_iter()
            .filter(|k| !drop.contains(&k.as_str()))
            .collect::<Vec<_>>()
    };

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
    let item = s
        .items()
        .create(
            WS,
            &NewItem {
                tags: common::task_tags(&s, WS),
                title: "t".into(),
                goal: "g".into(),
                bot_id: "steve".into(),
                ..Default::default()
            },
        )
        .unwrap()
        .unwrap();
    // who asked for it is read where a run's principal is decided, not with every item
    assert_eq!(keys(&item), without("items", &["requested_by"]));
    assert_eq!(
        keys(&s.bots().get("steve").unwrap().unwrap()),
        without("bots", &["project_dir", "worktree", "deleted", "owner"])
    );
    let prj = s
        .projects()
        .create(
            WS,
            &NewProject {
                name: "p".into(),
                path: "/x".into(),
                kind: "dir".into(),
                device_id: "hub".into(),
                ..Default::default()
            },
        )
        .unwrap();
    assert_eq!(keys(&prj), cols("projects"));
    assert_eq!(
        keys(&s.workspaces().get(WS).unwrap().unwrap()),
        cols("workspaces")
    );
    let ses = s
        .sessions()
        .create(&NewSession {
            workspace_id: WS.into(),
            harness: "claude-code".into(),
            cwd: "/".into(),
            token: "x".into(),
            ..Default::default()
        })
        .unwrap();
    // when its first tool call came is read by the statistics, not with every session
    assert_eq!(keys(&ses), without("sessions", &["first_tool_at"]));
    let dec = s
        .decisions()
        .create(&NewDecision {
            workspace_id: WS.into(),
            kind: "ask".into(),
            subject_type: "item".into(),
            subject_id: item.id.clone(),
            question: "q".into(),
            requested_by: "steve".into(),
            ..Default::default()
        })
        .unwrap()
        .unwrap();
    assert_eq!(keys(&dec), cols("decisions"));
    let mid = s
        .messages()
        .add(WS, "me", "hi", None, &serde_json::Value::Null, "steve")
        .unwrap();
    assert_eq!(
        keys(&s.messages().get(&mid).unwrap().unwrap()),
        cols("messages")
    );
    let tag = s
        .tags()
        .create(
            WS,
            &NewTag {
                name: "n".into(),
                cat: "c".into(),
                description: "d".into(),
            },
            "me",
        )
        .unwrap();
    assert_eq!(keys(&tag), cols("tags"));
    let mem = s.memories().add("steve", WS, "m", "me").unwrap();
    assert_eq!(
        keys(&s.memories().get(&mem).unwrap().unwrap()),
        cols("bot_memories")
    );
    s.devices()
        .upsert(&DeviceInfo {
            id: "hub".into(),
            name: "Hub".into(),
            roles: vec!["Hub".into()],
            os: "darwin".into(),
            version: "1".into(),
        })
        .unwrap();
    assert_eq!(keys(&s.devices().list().unwrap()[0]), cols("devices"));
    s.artifacts().add(&item.id, "pr", "t", None).unwrap();
    // delivery_id is a derived, scope-checked association, not a second stored location.
    let mut artifact_keys = cols("artifacts");
    artifact_keys.push("delivery_id".into());
    let artifacts = s.artifacts().list(&item.id).unwrap();
    assert_eq!(keys(&artifacts[0]), artifact_keys);
    assert!(artifacts[0].delivery_id.is_none());
    s.timeline()
        .add(&item.id, "me", "me", "x", None, &[])
        .unwrap();
    assert_eq!(
        keys(&s.timeline().list(&item.id).unwrap()[0]),
        without("timeline", &["item_id"])
    );
    s.queued().add(&ses.id, "q", "me", &[]).unwrap();
    assert_eq!(keys(&s.queued().list(&ses.id).unwrap()[0]), cols("queued"));
    s.links()
        .add(
            WS,
            ("item", &item.id),
            "about",
            ("project", &prj.id),
            "me",
            &serde_json::json!({}),
        )
        .unwrap();
    assert_eq!(
        keys(&s.links().from(WS, "item", &item.id).unwrap()[0]),
        cols("links")
    );
    let uid = s.users().create("sub", "Lee", None, "owner").unwrap();
    assert_eq!(keys(&s.users().get(&uid).unwrap().unwrap()), cols("users"));
    let mut ws_role = cols("workspaces");
    ws_role.push("role".into());
    assert_eq!(keys(&s.workspaces().of_user(&uid).unwrap()[0]), ws_role);
}

#[test]
fn current_rows_and_attachments_round_trip() {
    let t = fresh();
    let s = &t.store;
    let path = s.path();
    s.with_conn(|c| {
        c.execute_batch(
            "INSERT INTO items (id, workspace_id, no, title, status, stage, created, updated) VALUES ('ST-1', 'default', 1, 't', 'open', 'x', 1, 1);
             INSERT INTO timeline (item_id, ts, actor, kind, text) VALUES ('ST-1', 1, 'me', 'me', 'old');
             INSERT INTO sessions (id, workspace_id, harness, cwd, status, token_hash, created, updated) VALUES ('ses_1', 'default', 'claude-code', '/', 'idle', 'h', 1, 1);
             INSERT INTO queued (id, session_id, text, via, created) VALUES ('q_1', 'ses_1', 'old', 'me', 1);",
        )?;
        Ok(())
    }).unwrap();
    // Rows written without attachments read with `files: []`.
    assert!(s.timeline().list("ST-1").unwrap()[0].files.is_empty());
    assert!(s.queued().list("ses_1").unwrap()[0].files.is_empty());

    let f = s
        .files()
        .create(&steve_store::NewFile {
            id: "fil_0123456789abcdef".into(),
            workspace_id: "default".into(),
            sha256: "ab".repeat(32),
            name: "a.png".into(),
            mime: "image/png".into(),
            size: 3,
            created_by: "usr_1".into(),
        })
        .unwrap();
    assert_eq!(s.files().get(&f.id).unwrap().unwrap(), f);
    assert_eq!(
        s.files()
            .same("default", &f.sha256, "a.png")
            .unwrap()
            .map(|x| x.id),
        Some(f.id.clone())
    );
    assert!(
        s.files()
            .same("other", &f.sha256, "a.png")
            .unwrap()
            .is_none()
    );
    assert!(
        s.files()
            .same("default", &f.sha256, "b.png")
            .unwrap()
            .is_none()
    );
    let r = f.file_ref();
    s.timeline()
        .add("ST-1", "me", "me", "with", None, std::slice::from_ref(&r))
        .unwrap();
    s.queued()
        .add("ses_1", "with", "me", std::slice::from_ref(&r))
        .unwrap();
    assert_eq!(
        s.timeline().list("ST-1").unwrap()[1].files,
        std::slice::from_ref(&r)
    );
    assert_eq!(
        s.queued().list("ses_1").unwrap()[1].files,
        std::slice::from_ref(&r)
    );
    s.with_conn(|c| {
        // stored as JSON FileRef[]; an empty list is NULL like TS
        let j: String = c.query_row("SELECT files FROM queued WHERE text = 'with'", [], |r| {
            r.get(0)
        })?;
        assert_eq!(
            j,
            r#"[{"id":"fil_0123456789abcdef","name":"a.png","mime":"image/png","size":3}]"#
        );
        assert_eq!(
            count(c, "SELECT count(*) FROM timeline WHERE files IS NULL"),
            1
        );
        Ok(())
    })
    .unwrap();
    assert_eq!(
        keys(&f),
        columns(&Connection::open(&path).unwrap(), "files")
    );
}
