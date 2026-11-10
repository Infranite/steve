//! Migrations and the schema_version they leave.

mod common;

use common::*;
use rusqlite::Connection;
use sha2::{Digest, Sha256};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use steve_store::{NoPublisher, Store, db};

/// Counts WARN events (enough to see the newer-schema warning without a subscriber crate).
struct Warns(Arc<AtomicUsize>);

impl tracing::Subscriber for Warns {
    fn enabled(&self, _: &tracing::Metadata<'_>) -> bool {
        true
    }
    fn new_span(&self, _: &tracing::span::Attributes<'_>) -> tracing::span::Id {
        tracing::span::Id::from_u64(1)
    }
    fn record(&self, _: &tracing::span::Id, _: &tracing::span::Record<'_>) {}
    fn record_follows_from(&self, _: &tracing::span::Id, _: &tracing::span::Id) {}
    fn event(&self, e: &tracing::Event<'_>) {
        if *e.metadata().level() == tracing::Level::WARN {
            self.0.fetch_add(1, Ordering::SeqCst);
        }
    }
    fn enter(&self, _: &tracing::span::Id) {}
    fn exit(&self, _: &tracing::span::Id) {}
}

#[test]
fn compiled_migrations_match_the_release_lock() {
    // The compatibility test checks disk files against LOCK, including helper
    // files such as 027_items.sql. This checks what the release actually compiles.
    let locked: Vec<_> = include_str!("../migrations/LOCK")
        .lines()
        .filter(|line| !line.trim().is_empty() && !line.starts_with('#'))
        .map(|line| line.split_once(' ').expect("`<file> <sha256>`"))
        .filter(|(file, _)| {
            file.len() == "000.sql".len()
                && file.ends_with(".sql")
                && file.as_bytes()[..3].iter().all(u8::is_ascii_digit)
        })
        .collect();
    assert_eq!(
        db::MIGRATIONS.len(),
        locked.len(),
        "compiled migration count"
    );
    for (index, ((file, expected), sql)) in locked.iter().zip(db::MIGRATIONS).enumerate() {
        assert_eq!(*file, format!("{:03}.sql", index + 1), "migration order");
        let actual: String = Sha256::digest(sql.as_bytes())
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect();
        assert_eq!(actual, *expected, "compiled migration differs from {file}");
    }
}

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
            assert_eq!(count(c, "SELECT count(*) FROM schema_version"), 1);
            assert_eq!(count(c, "SELECT v FROM schema_version"), db::SCHEMA_VERSION);
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
        assert_eq!(count(c, "SELECT count(*) FROM schema_version"), 1);
        assert_eq!(count(c, "SELECT v FROM schema_version"), db::SCHEMA_VERSION);
        assert_eq!(count(c, "SELECT count(*) FROM items"), 1);
        Ok(())
    })
    .unwrap();
}

#[test]
fn upgrades_a_version_1_database() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("steve.sqlite3");
    {
        let c = Connection::open(&path).unwrap();
        c.execute_batch("CREATE TABLE schema_version (v INTEGER NOT NULL); INSERT INTO schema_version (v) VALUES (1);").unwrap();
        c.execute_batch(db::MIGRATIONS[0]).unwrap();
        c.execute_batch(
            "INSERT INTO workspaces (id, name, created) VALUES ('default', '默认工作空间', 1);
             INSERT INTO users (id, workspace_id, subject, name, email, role, created) VALUES ('usr_1', 'default', 'sub', 'Lee', NULL, 'owner', 2);
             INSERT INTO bots (id, workspace_id, name, harness, updated) VALUES ('steve', 'default', 'Steve', 'claude-code', 3);
             INSERT INTO counters (workspace_id, name, value) VALUES ('default', 'item', 7);",
        )
        .unwrap();
    }
    let s = Store::open(&path, NoPublisher).unwrap();
    s.ensure_workspace().unwrap();
    s.with_conn(|c| {
        assert_eq!(count(c, "SELECT v FROM schema_version"), db::SCHEMA_VERSION);
        assert_eq!(count(c, "SELECT count(*) FROM workspace_members WHERE user_id = 'usr_1' AND role = 'owner' AND created = 2"), 1);
        assert_eq!(count(c, "SELECT count(*) FROM bot_workspaces WHERE bot_id = 'steve' AND workspace_id = 'default' AND created = 3"), 1);
        Ok(())
    })
    .unwrap();
    assert_eq!(s.workspaces().get("default").unwrap().unwrap().name, "个人");
    assert_eq!(s.peek_counter("item:ST").unwrap(), 7);
    let fresh_schema = schema(&Connection::open(fresh().store.path()).unwrap());
    assert_eq!(schema_of(&path), fresh_schema);
}

#[test]
fn newer_schema_is_opened_unchanged() {
    let t = fresh();
    let path = t.store.path().to_path_buf();
    t.store
        .with_conn(|c| {
            // what a newer build would have left behind
            c.execute(
                "UPDATE schema_version SET v = ?",
                [db::SCHEMA_VERSION + 1],
            )?;
            c.execute_batch("CREATE TABLE future (id TEXT PRIMARY KEY); ALTER TABLE queued ADD COLUMN later TEXT;")?;
            Ok(())
        })
        .unwrap();
    drop(t.store);
    let before = schema_of(&path);
    let warns = Arc::new(AtomicUsize::new(0));
    let s = tracing::subscriber::with_default(Warns(warns.clone()), || {
        Store::open(&path, NoPublisher).unwrap()
    });
    assert_eq!(warns.load(Ordering::SeqCst), 1, "one warning");
    assert_eq!(schema_of(&path), before, "no migration ran");
    s.with_conn(|c| {
        assert_eq!(
            count(c, "SELECT v FROM schema_version"),
            db::SCHEMA_VERSION + 1,
            "version is never written down"
        );
        assert_eq!(count(c, "SELECT count(*) FROM schema_version"), 1);
        Ok(())
    })
    .unwrap();
    // the repositories still work on the newer schema
    s.ensure_workspace().unwrap();
    assert!(s.workspaces().get("default").unwrap().is_some());
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
    assert_eq!(keys(&ses), cols("sessions"));
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
    assert_eq!(
        keys(&s.artifacts().list(&item.id).unwrap()[0]),
        cols("artifacts")
    );
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
fn upgrades_a_version_2_database_with_attachments() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("steve.sqlite3");
    {
        let c = Connection::open(&path).unwrap();
        c.execute_batch("CREATE TABLE schema_version (v INTEGER NOT NULL); INSERT INTO schema_version (v) VALUES (2);").unwrap();
        c.execute_batch(db::MIGRATIONS[0]).unwrap();
        c.execute_batch(db::MIGRATIONS[1]).unwrap();
        c.execute_batch(
            "INSERT INTO workspaces (id, name, created) VALUES ('default', '个人', 1);
             INSERT INTO items (id, workspace_id, no, title, status, stage, created, updated) VALUES ('ST-1', 'default', 1, 't', 'open', 'x', 1, 1);
             INSERT INTO timeline (item_id, ts, actor, kind, text) VALUES ('ST-1', 1, 'me', 'me', 'old');
             INSERT INTO sessions (id, workspace_id, harness, cwd, status, token_hash, created, updated) VALUES ('ses_1', 'default', 'claude-code', '/', 'idle', 'h', 1, 1);
             INSERT INTO queued (id, session_id, text, via, created) VALUES ('q_1', 'ses_1', 'old', 'me', 1);",
        )
        .unwrap();
    }
    let s = Store::open(&path, NoPublisher).unwrap();
    s.with_conn(|c| {
        assert_eq!(count(c, "SELECT v FROM schema_version"), db::SCHEMA_VERSION);
        Ok(())
    })
    .unwrap();
    assert_eq!(schema_of(&path), schema_of(fresh().store.path()));
    // rows from before the migration read with `files: []`
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
