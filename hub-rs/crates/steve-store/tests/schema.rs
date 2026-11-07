//! Migrations: identical schema to the TS Hub, same schema_version semantics.

mod common;

use common::*;
use rusqlite::Connection;
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

/// Number of migrations that are TS strings; the rest exist only in the Rust Hub.
const TS_MIGRATIONS: usize = 3;

#[test]
fn ts_migrations_give_the_ts_schema() {
    let c = Connection::open_in_memory().unwrap();
    c.execute_batch("CREATE TABLE IF NOT EXISTS schema_version (v INTEGER NOT NULL)")
        .unwrap();
    for m in &db::MIGRATIONS[..TS_MIGRATIONS] {
        c.execute_batch(m).unwrap();
    }
    // Produced by running hub/src/db.ts (better-sqlite3) on an empty STEVE_HOME.
    assert_eq!(schema(&c), include_str!("fixtures/ts_schema.txt"));
}

fn objects(c: &Connection) -> Vec<(String, String, String, String)> {
    let mut st = c
        .prepare(
            "select type, name, tbl_name, coalesce(sql, '') from sqlite_master order by type, name",
        )
        .unwrap();
    st.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))
        .unwrap()
        .map(Result::unwrap)
        .collect()
}

#[test]
fn fresh_database_is_ts_schema_plus_module_migrations() {
    let t = fresh();
    let got = t.store.with_conn(|c| Ok(objects(c))).unwrap();
    let ts = Connection::open_in_memory().unwrap();
    ts.execute_batch("CREATE TABLE IF NOT EXISTS schema_version (v INTEGER NOT NULL)")
        .unwrap();
    for m in &db::MIGRATIONS[..TS_MIGRATIONS] {
        ts.execute_batch(m).unwrap();
    }
    // the runner's own bookkeeping besides schema_version: the reader gate (`db::compat`)
    ts.execute_batch("CREATE TABLE IF NOT EXISTS schema_compat (min_reader INTEGER NOT NULL)")
        .unwrap();
    ts.execute_batch("ALTER TABLE bots ADD COLUMN reasoning_effort TEXT; ALTER TABLE sessions ADD COLUMN reasoning_effort TEXT;").unwrap();
    // 027's item columns, added to the TS table the same way (the backfill finds nothing here)
    ts.execute_batch("CREATE TABLE item_cycles (item_id TEXT, cycle INTEGER, revision INTEGER, outcome TEXT, acceptance TEXT, reason TEXT, closed INTEGER, source TEXT)").unwrap();
    ts.execute_batch(db::ITEM_LIFECYCLE).unwrap();
    ts.execute_batch("DROP TABLE item_cycles; CREATE INDEX items_closing ON items(phase) WHERE phase = 'closing';").unwrap();
    // 034 extends the original events table. Compare its altered definition, while the module
    // side below still contributes its new indexes and tables.
    for statement in db::MIGRATIONS[33].split(';') {
        if statement.trim_start().starts_with("ALTER TABLE events ") {
            ts.execute_batch(statement).unwrap();
        }
    }
    // What module migrations create, on stubs of the original tables they touch.
    const STUBS: [&str; 9] = [
        "workspaces",
        "users",
        "tags",
        "links",
        "sessions",
        "decisions",
        "queued",
        "events",
        "items",
    ];
    let only = Connection::open_in_memory().unwrap();
    for t in STUBS {
        only.execute_batch(&format!("CREATE TABLE {t} (id TEXT PRIMARY KEY, workspace_id TEXT, object_type TEXT, object_id TEXT, subject_type TEXT, subject_id TEXT, item_id TEXT, no INTEGER, created INTEGER, session_id TEXT)")).unwrap();
    }
    // 018's backfill and 019's space merge read these TS tables (and change nothing on empty ones)
    const READ_STUBS: [(&str, &str); 7] = [
        ("revisions", "object_type TEXT, object_id TEXT"),
        (
            "audit",
            "workspace_id TEXT, ts INTEGER, actor TEXT, action TEXT, target TEXT, detail TEXT",
        ),
        ("settings", "workspace_id TEXT, key TEXT, value TEXT"),
        (
            "bots",
            "id TEXT PRIMARY KEY, workspace_id TEXT, deleted INTEGER",
        ),
        ("bot_workspaces", "bot_id TEXT, workspace_id TEXT"),
        (
            "workspace_members",
            "workspace_id TEXT, user_id TEXT, role TEXT, created INTEGER",
        ),
        ("devices", "id TEXT PRIMARY KEY"),
    ];
    for (t, cols) in READ_STUBS {
        only.execute_batch(&format!("CREATE TABLE {t} ({cols})"))
            .unwrap();
    }
    for m in &db::MIGRATIONS[TS_MIGRATIONS..34] {
        only.execute_batch(m).unwrap();
    }
    // 035 indexes both kinds of table: an index on a TS table belongs to the TS side, the rest to
    // the module side (each statement is one line)
    for statement in db::MIGRATIONS[34]
        .lines()
        .filter(|l| l.starts_with("CREATE "))
    {
        let (_, on) = statement.split_once(" ON ").unwrap();
        let table = on.split('(').next().unwrap().trim();
        let in_ts: bool = ts
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ?)",
                [table],
                |r| r.get(0),
            )
            .unwrap();
        if in_ts {
            ts.execute_batch(statement).unwrap();
        } else {
            only.execute_batch(statement).unwrap();
        }
    }
    // 036 and later: module objects, including 042's trigger on the existing items table.
    // Its stub is excluded below, but the trigger stays in the full schema comparison.
    for m in &db::MIGRATIONS[35..] {
        only.execute_batch(m).unwrap();
    }
    let stub = |t: &str| STUBS.contains(&t) || READ_STUBS.iter().any(|(n, _)| *n == t);
    let added = objects(&only).into_iter().filter(|(ty, name, tbl, _)| {
        !(ty == "table" && stub(name) || name.starts_with("sqlite_autoindex_") && stub(tbl))
    });
    let mut want: Vec<_> = objects(&ts).into_iter().chain(added).collect();
    want.sort();
    // both sides create `sqlite_sequence` once an AUTOINCREMENT table exists (001 and 015)
    want.dedup();
    // the planner's statistics (`PRAGMA optimize` at open) are not schema
    let mut got: Vec<_> = got
        .into_iter()
        .filter(|(_, name, _, _)| !name.starts_with("sqlite_stat"))
        .collect();
    got.sort();
    assert_eq!(got, want);
}

#[test]
fn migrations_are_the_ts_strings() {
    assert_eq!(db::MIGRATIONS.len(), 42);
    assert_eq!(db::SCHEMA_VERSION, 42);
    assert!(db::MIGRATIONS[0].starts_with("\n  CREATE TABLE workspaces (id TEXT PRIMARY KEY, name TEXT NOT NULL, created INTEGER NOT NULL);"));
    assert!(
        db::MIGRATIONS[1]
            .trim_end()
            .ends_with("ALTER TABLE messages ADD COLUMN meta TEXT;")
    );
    assert!(db::MIGRATIONS[2].starts_with("\n  CREATE TABLE files (id TEXT PRIMARY KEY,"));
    assert!(
        db::MIGRATIONS[2]
            .trim_end()
            .ends_with("ALTER TABLE queued ADD COLUMN files TEXT;")
    );
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
    assert_eq!(keys(&item), cols("items"));
    assert_eq!(
        keys(&s.bots().get("steve").unwrap().unwrap()),
        without("bots", &["project_dir", "worktree", "deleted"])
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
