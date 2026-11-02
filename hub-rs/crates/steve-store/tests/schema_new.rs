//! Migrations 5–22 (slice M0, YOLO Y0, stats ST0, thread routing R0, reasoning effort, ownership, space attention, IM reactions): the new module tables appear on an empty database and on databases left
//! at v2, v3 and v4, running them again changes nothing, and the bundled SQLite has what `kb_fts` needs.

mod common;

use common::*;
use rusqlite::Connection;
use steve_store::{NoPublisher, Store, db};

/// One table of each module migration, in migration order.
const NEW_TABLES: [(&str, i64); 31] = [
    ("connections", 5),
    ("ext_events", 6),
    ("spaces", 7),
    ("kb_chunks", 8),
    ("mail_drafts", 9),
    ("gh_prs", 10),
    ("chan_cursors", 11),
    ("yolo_scopes", 14),
    ("yolo_windows", 14),
    ("yolo_actions", 14),
    ("agent_turns", 15),
    ("usage_records", 15),
    ("item_marks", 15),
    ("stat_daily", 15),
    ("item_stats", 15),
    ("route_log", 16),
    ("thread_runs", 16),
    ("thread_items", 16),
    ("stewards", 18),
    ("space_prefs", 19),
    ("chan_reactions", 20),
    ("outbox_events", 21),
    ("outbox_consumers", 21),
    ("outbox_receipts", 21),
    ("outbox_leases", 21),
    ("event_payloads", 22),
    ("event_payload_refs", 22),
    ("chan_app_registrations", 23),
    ("session_resume", 24),
    ("session_dispatch", 24),
    ("thread_resume", 24),
];

fn has_table(c: &Connection, name: &str) -> bool {
    count(
        c,
        &format!("SELECT count(*) FROM sqlite_master WHERE type = 'table' AND name = '{name}'"),
    ) == 1
}

/// A database the way a build with `v` migrations left it, with a row in an old table.
fn at_version(v: usize) -> (tempfile::TempDir, std::path::PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("steve.sqlite3");
    let c = Connection::open(&path).unwrap();
    c.execute_batch("CREATE TABLE schema_version (v INTEGER NOT NULL)")
        .unwrap();
    for m in &db::MIGRATIONS[..v] {
        c.execute_batch(m).unwrap();
    }
    c.execute("INSERT INTO schema_version (v) VALUES (?)", [v as i64])
        .unwrap();
    c.execute_batch("INSERT INTO workspaces (id, name, created) VALUES ('default', '个人', 1);")
        .unwrap();
    (dir, path)
}

#[test]
fn schema_version_is_23() {
    assert_eq!(db::SCHEMA_VERSION, 24);
}

#[test]
fn reasoning_migration_accepts_columns_previously_added_by_ts_hub() {
    for v in [12, 16] {
        let (_dir, path) = at_version(v);
        let c = Connection::open(&path).unwrap();
        c.execute_batch("ALTER TABLE bots ADD COLUMN reasoning_effort TEXT; ALTER TABLE sessions ADD COLUMN reasoning_effort TEXT;").unwrap();
        db::migrate(&c).unwrap();
        db::migrate(&c).unwrap();
        assert_eq!(
            count(&c, "SELECT v FROM schema_version"),
            db::SCHEMA_VERSION
        );
    }
}

#[test]
fn empty_database_migrates_to_v20() {
    let t = fresh();
    t.store
        .with_conn(|c| {
            assert_eq!(count(c, "SELECT v FROM schema_version"), db::SCHEMA_VERSION);
            for (name, _) in NEW_TABLES {
                assert!(has_table(c, name), "{name}");
            }
            assert_eq!(
                c.query_row("PRAGMA integrity_check", [], |r| r.get::<_, String>(0))?,
                "ok"
            );
            assert_eq!(count(c, "SELECT count(*) FROM pragma_foreign_key_check"), 0);
            Ok(())
        })
        .unwrap();
}

#[test]
fn v2_v3_v4_databases_migrate_to_the_fresh_schema() {
    let want = schema_of(fresh().store.path());
    for v in [2, 3, 4, 12, 16, 17, 18, 19] {
        let (_dir, path) = at_version(v);
        let s = Store::open(&path, NoPublisher).unwrap();
        s.with_conn(|c| {
            assert_eq!(
                count(c, "SELECT v FROM schema_version"),
                db::SCHEMA_VERSION,
                "from v{v}"
            );
            assert_eq!(count(c, "SELECT count(*) FROM schema_version"), 1);
            assert_eq!(count(c, "SELECT count(*) FROM workspaces"), 1, "data kept");
            assert_eq!(
                c.query_row("PRAGMA integrity_check", [], |r| r.get::<_, String>(0))?,
                "ok"
            );
            Ok(())
        })
        .unwrap();
        drop(s);
        assert_eq!(schema_of(&path), want, "from v{v}");
    }
}

#[test]
fn rerunning_the_module_migrations_changes_nothing() {
    let t = fresh();
    let path = t.store.path().to_path_buf();
    drop(t.store);
    let before = schema_of(&path);
    let c = Connection::open(&path).unwrap();
    for m in &db::MIGRATIONS[3..] {
        c.execute_batch(m).unwrap();
    }
    drop(c);
    assert_eq!(schema_of(&path), before);
    // and opening again at v20 runs nothing
    drop(Store::open(&path, NoPublisher).unwrap());
    assert_eq!(schema_of(&path), before);
}

#[test]
fn each_migration_brings_its_own_tables() {
    for (name, v) in NEW_TABLES {
        let (_dir, path) = at_version(v as usize - 1);
        let c = Connection::open(&path).unwrap();
        assert!(!has_table(&c, name), "{name} before v{v}");
        c.execute_batch(db::MIGRATIONS[v as usize - 1]).unwrap();
        assert!(has_table(&c, name), "{name} after v{v}");
    }
}

#[test]
fn kb_fts_writes_queries_and_deletes() {
    let t = fresh();
    t.store
        .with_conn(|c| {
            let ver: String = c.query_row("SELECT sqlite_version()", [], |r| r.get(0))?;
            assert!(
                c.query_row("SELECT sqlite_compileoption_used('ENABLE_FTS5')", [], |r| r
                    .get::<_, i64>(0))?
                    == 1,
                "FTS5 in SQLite {ver}"
            );
            c.execute(
                "INSERT INTO kb_fts (rowid, seg, title) VALUES (1, '合同 同说 说明 acme contract', 'Acme 合同说明')",
                [],
            )?;
            c.execute(
                "INSERT INTO kb_fts (rowid, seg, title) VALUES (2, '周报 报告 weekly', '周报')",
                [],
            )?;
            let hits = |q: &str| -> rusqlite::Result<Vec<i64>> {
                let mut st =
                    c.prepare("SELECT rowid FROM kb_fts WHERE kb_fts MATCH ? ORDER BY rank")?;
                st.query_map([q], |r| r.get(0))?.collect()
            };
            assert_eq!(hits("合同")?, vec![1]);
            assert_eq!(hits("weekly OR contract")?.len(), 2);
            // contentless_delete = 1: rows can be deleted by rowid (needs SQLite >= 3.43)
            c.execute("DELETE FROM kb_fts WHERE rowid = 1", [])?;
            assert!(hits("合同")?.is_empty());
            assert_eq!(hits("周报")?, vec![2]);
            Ok(())
        })
        .unwrap();
}

/// 014 (YOLO) does not depend on 013: it runs after the comment-only placeholder, after a stand-in for
/// a real 013, and a second time, leaving the same tables; a database left at v13 migrates to v20.
#[test]
fn yolo_migration_does_not_depend_on_013() {
    fn yolo(c: &Connection) -> Vec<String> {
        let mut st = c
            .prepare("SELECT type || ' ' || name FROM sqlite_master WHERE name LIKE '%yolo%' ORDER BY name")
            .unwrap();
        st.query_map([], |r| r.get(0))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap()
    }
    let (_a, path) = at_version(12);
    let c = Connection::open(&path).unwrap();
    assert!(
        db::MIGRATIONS[12]
            .lines()
            .all(|l| l.trim().is_empty() || l.starts_with("--"))
    );
    c.execute_batch(db::MIGRATIONS[12]).unwrap();
    c.execute_batch(db::MIGRATIONS[13]).unwrap();
    let want = yolo(&c);
    for t in ["yolo_scopes", "yolo_windows", "yolo_actions"] {
        assert!(has_table(&c, t), "{t}");
    }

    let (_b, path) = at_version(12);
    let c = Connection::open(&path).unwrap();
    c.execute_batch(
        "CREATE TABLE delivery_batches (id TEXT PRIMARY KEY, workspace_id TEXT NOT NULL);",
    )
    .unwrap();
    c.execute_batch(db::MIGRATIONS[13]).unwrap();
    assert_eq!(yolo(&c), want);
    c.execute_batch(db::MIGRATIONS[13]).unwrap();
    assert_eq!(yolo(&c), want);

    let (_c, path) = at_version(13);
    let s = Store::open(&path, NoPublisher).unwrap();
    s.with_conn(|c| {
        assert_eq!(count(c, "SELECT v FROM schema_version"), db::SCHEMA_VERSION);
        assert_eq!(yolo(c), want);
        Ok(())
    })
    .unwrap();
}

#[test]
fn placeholder_13_is_comments_only() {
    assert!(
        db::MIGRATIONS[12]
            .lines()
            .all(|l| l.trim().is_empty() || l.starts_with("--")),
        "013 stays comment-only"
    );
    for v in 14..=16 {
        assert!(
            db::MIGRATIONS[v - 1]
                .lines()
                .any(|l| !l.trim().is_empty() && !l.starts_with("--")),
            "{v:03} is a real migration"
        );
    }
}

#[test]
fn v12_database_gets_016_without_013_to_015() {
    // production is at v12; 016 must not depend on anything 13–15 would create
    let (_dir, path) = at_version(12);
    let c = Connection::open(&path).unwrap();
    c.execute_batch(db::MIGRATIONS[15]).unwrap();
    for t in ["route_log", "thread_runs", "thread_items"] {
        assert!(has_table(&c, t), "{t}");
    }
    // and again: IF NOT EXISTS throughout
    c.execute_batch(db::MIGRATIONS[15]).unwrap();
}

/// 15 (statistics) does not depend on 13 or 14: it applies on a v12 database directly, twice.
#[test]
fn migration_15_does_not_need_13_or_14() {
    let (_dir, path) = at_version(12);
    let c = Connection::open(&path).unwrap();
    c.execute_batch(db::MIGRATIONS[14]).unwrap();
    c.execute_batch(db::MIGRATIONS[14]).unwrap();
    for t in [
        "agent_turns",
        "usage_records",
        "item_marks",
        "stat_daily",
        "item_stats",
    ] {
        assert!(has_table(&c, t), "{t}");
    }
}

/// 18 backfills §3 of ownership.md once: “我” owns the live bots (in every workspace they serve), the
/// personas, the Hub device and the connections of their own accounts; objects that already have a
/// steward, deleted bots and other devices are left alone, and running it again adds nothing.
#[test]
fn migration_18_backfills_owners() {
    let (_dir, path) = at_version(17);
    let c = Connection::open(&path).unwrap();
    c.execute_batch(
        "INSERT INTO workspaces (id, name, created) VALUES ('w2', '团队', 1);
         INSERT INTO users (id, workspace_id, subject, name, role, created) VALUES ('usr_1', 'default', 's1', '李', 'owner', 1);
         INSERT INTO workspace_members (workspace_id, user_id, role, created) VALUES ('default', 'usr_1', 'owner', 1), ('w2', 'usr_1', 'owner', 2);
         INSERT INTO parties (id, workspace_id, type, name, contact_kind, user_id, created_by, created, updated)
           VALUES ('ct_me', 'default', 'contact', '李', 'real', 'usr_1', 'usr_1', 1, 1),
                  ('ct_me2', 'w2', 'contact', '李', 'real', 'usr_1', 'usr_1', 1, 1),
                  ('ct_zhang', 'default', 'contact', '张三', 'real', NULL, 'usr_1', 1, 1);
         INSERT INTO bots (id, workspace_id, name, harness, updated) VALUES ('steve', 'default', 'Steve', 'claude-code', 1), ('dev', 'default', '开发', 'claude-code', 1), ('old', 'default', '旧', 'claude-code', 1), ('ops', 'default', '运维', 'claude-code', 1);
         UPDATE bots SET deleted = 1 WHERE id = 'old';
         INSERT INTO bot_workspaces (bot_id, workspace_id, created) VALUES ('steve', 'w2', 1);
         INSERT INTO personas (id, workspace_id, name, bot_id, created, updated) VALUES ('per_1', 'default', 'Steve', 'steve', 1, 1);
         INSERT INTO devices (id, name, roles, seen, created) VALUES ('hub', 'Mac', '[\"Hub\"]', 1, 1), ('dev_2', 'Laptop', '[]', 1, 1);
         INSERT INTO systems (id, workspace_id, name, kind, family, created) VALUES ('mail', '*', '邮箱', 'comm', 'email', 1);
         INSERT INTO accounts (id, workspace_id, system_id, family, party_id, kind, handle, handle_display, source, created, updated)
           VALUES ('acc_1', 'default', 'mail', 'email', 'ct_me', 'personal', 'li@x.com', 'li@x.com', 'connect', 1, 1);
         INSERT INTO connections (id, workspace_id, account_id, adapter, auth, created_by, created, updated)
           VALUES ('cn_1', 'default', 'acc_1', 'imap', 'credential', 'usr_1', 1, 1);
         CREATE TABLE IF NOT EXISTS stewards (workspace_id TEXT NOT NULL, object_type TEXT NOT NULL, object_id TEXT NOT NULL, person_id TEXT NOT NULL REFERENCES parties(id), role TEXT NOT NULL DEFAULT 'owner', source TEXT NOT NULL, confidence REAL NOT NULL DEFAULT 1, status TEXT NOT NULL DEFAULT 'confirmed', source_ref TEXT, decision_id TEXT, created INTEGER NOT NULL, updated INTEGER NOT NULL, PRIMARY KEY (workspace_id, object_type, object_id, person_id, role));
         INSERT INTO stewards (workspace_id, object_type, object_id, person_id, source, created, updated) VALUES ('default', 'bot', 'ops', 'ct_zhang', 'manual', 1, 1);",
    )
    .unwrap();
    db::migrate(&c).unwrap();
    let rows = |c: &Connection| -> Vec<String> {
        let mut st = c
            .prepare("SELECT workspace_id || ' ' || object_type || ' ' || object_id || ' ' || person_id || ' ' || source FROM stewards ORDER BY 1")
            .unwrap();
        st.query_map([], |r| r.get(0))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap()
    };
    let want = vec![
        "default bot dev ct_me seed",
        "default bot ops ct_zhang manual",
        "default bot steve ct_me seed",
        "default connection cn_1 ct_me seed",
        "default device hub ct_me seed",
        "default persona per_1 ct_me seed",
        "w2 bot steve ct_me2 seed",
        "w2 device hub ct_me2 seed",
    ];
    assert_eq!(rows(&c), want);
    c.execute_batch(db::MIGRATIONS[17]).unwrap();
    assert_eq!(rows(&c), want);
    assert_eq!(
        count(&c, "SELECT count(*) FROM pragma_foreign_key_check"),
        0
    );
}

/// 20 (IM reactions) runs on a database left at v19 (space attention) and a second time without change.
#[test]
fn migration_20_reactions_after_19() {
    let (_dir, path) = at_version(19);
    let c = Connection::open(&path).unwrap();
    c.execute_batch(db::MIGRATIONS[19]).unwrap();
    c.execute_batch(db::MIGRATIONS[19]).unwrap();
    assert!(has_table(&c, "chan_reactions"));
    assert!(has_table(&c, "space_prefs"));
}
