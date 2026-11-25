//! Migrations 5–22 (slice M0, YOLO Y0, stats ST0, thread routing R0, reasoning effort, ownership, space attention, IM reactions): the new module tables appear on an empty database and on databases left
//! at v2, v3 and v4, running them again changes nothing, and the bundled SQLite has what `kb_fts` needs.

use steve_store::testkit::SqliteFixtures;
mod common;

use common::*;
use rusqlite::Connection;
use steve_store::{NoPublisher, Store, db};

/// One table of each module migration, in migration order.
const NEW_TABLES: [(&str, i64); 34] = [
    ("connections", 5),
    ("ext_events", 6),
    ("spaces", 7),
    ("kb_chunks", 8),
    ("mail_drafts", 9),
    ("gh_prs", 10),
    ("chan_cursors", 11),
    ("agent_turns", 15),
    ("usage_records", 15),
    ("item_marks", 15),
    ("stat_daily", 15),
    ("item_stats", 15),
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
    ("approval_policies", 121),
    ("approval_turns", 121),
    ("telemetry_daily", 125),
    ("thread_submit_keys", 130),
    ("thread_reply_tasks", 130),
    ("thread_reply_receipts", 130),
    ("thread_dispatch_refs", 130),
    ("thread_forwarded", 130),
    ("thread_delivery_refs", 130),
    ("thread_notices", 130),
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
    for (i, m) in db::MIGRATIONS[..v].iter().enumerate() {
        if i == 119 {
            for table in ["bots", "sessions"] {
                let present: bool = c
                    .query_row(
                        &format!("SELECT EXISTS(SELECT 1 FROM pragma_table_info('{table}') WHERE name = 'reasoning_effort')"),
                        [],
                        |r| r.get(0),
                    )
                    .unwrap();
                if !present {
                    c.execute_batch(&format!(
                        "ALTER TABLE {table} ADD COLUMN reasoning_effort TEXT"
                    ))
                    .unwrap();
                }
            }
        }
        c.execute_batch(m).unwrap();
        if i + 1 == 122 {
            c.execute_batch(db::ITEM_LIFECYCLE).unwrap();
        }
    }
    c.execute("INSERT INTO schema_version (v) VALUES (?)", [v as i64])
        .unwrap();
    c.execute_batch("INSERT INTO workspaces (id, name, created) VALUES ('default', '个人', 1);")
        .unwrap();
    (dir, path)
}

// Use the contract boundary so a merge-time renumber cannot silently skip a published migration.
fn before_usage_ledger() -> usize {
    db::MIGRATIONS
        .iter()
        .position(|sql| sql.contains("CREATE TABLE usage_reports"))
        .unwrap()
}

fn seed_pre_usage_ledger(c: &Connection) {
    c.execute_batch(
        "INSERT INTO devices(id,name,roles,max_sessions,revision,seen,created)
         VALUES ('ledger-device','Device','[]',32,9,1,1);
         INSERT INTO usage_records(workspace_id,ts,day,source,purpose,harness,model,
             input,output,cache_read,cache_write,reasoning,cost_reported,detail)
         VALUES ('default',1,'2026-01-01','gateway','legacy','custom','model',
             40,20,60,7,5,0.25,'{\"scope\":\"all\"}');",
    )
    .unwrap();
}

fn assert_pre_usage_ledger_facts(c: &Connection) {
    assert_eq!(
        count(c, "SELECT revision FROM devices WHERE id='ledger-device'"),
        9
    );
    assert_eq!(
        count(
            c,
            "SELECT max_sessions FROM devices WHERE id='ledger-device'"
        ),
        32
    );
    assert_eq!(count(c, "SELECT count(*) FROM usage_records"), 1);
    assert_eq!(
        count(
            c,
            "SELECT count(*) FROM usage_records WHERE input=40 AND output=20
             AND cache_read=60 AND cache_write=7 AND reasoning=5 AND cost_reported=0.25
             AND detail='{\"scope\":\"all\"}'",
        ),
        1
    );
    assert_eq!(count(c, "SELECT count(*) FROM pragma_foreign_key_check"), 0);
}

#[test]
fn usage_ledger_upgrade_preserves_published_device_revision_and_legacy_bill() {
    let (_dir, path) = at_version(before_usage_ledger());
    let c = Connection::open(&path).unwrap();
    seed_pre_usage_ledger(&c);
    db::migrate(&c).unwrap();
    db::migrate(&c).unwrap();
    assert_pre_usage_ledger_facts(&c);
    assert_eq!(
        count(&c, "SELECT v FROM schema_version"),
        db::SCHEMA_VERSION
    );
    assert_eq!(count(&c, "SELECT count(*) FROM usage_reports"), 0);
    assert_eq!(count(&c, "SELECT count(*) FROM gateway_result_ready"), 0);
    drop(c);
    let store = Store::open(&path, NoPublisher).unwrap();
    store
        .fixture_read(|c| -> steve_store::Result<()> {
            assert_pre_usage_ledger_facts(c);
            Ok::<_, steve_store::StoreError>(())
        })
        .unwrap();
}

#[test]
fn usage_ledger_upgrade_rolls_back_all_pending_migrations_then_retries() {
    let before = before_usage_ledger();
    let (_dir, path) = at_version(before);
    let c = Connection::open(&path).unwrap();
    seed_pre_usage_ledger(&c);
    // Fail the last migration after the earlier pending migrations have run in the transaction.
    c.execute_batch("CREATE TABLE gateway_result_ready(collision INTEGER)")
        .unwrap();
    assert!(db::migrate(&c).is_err());
    assert_eq!(count(&c, "SELECT v FROM schema_version"), before as i64);
    assert!(!has_table(&c, "usage_reports"));
    assert!(!has_table(&c, "gateway_calls"));
    assert!(!columns(&c, "usage_records").contains(&"live_turn_id".to_string()));
    assert_pre_usage_ledger_facts(&c);
    c.execute_batch("DROP TABLE gateway_result_ready").unwrap();
    db::migrate(&c).unwrap();
    assert_eq!(
        count(&c, "SELECT v FROM schema_version"),
        db::SCHEMA_VERSION
    );
    assert_pre_usage_ledger_facts(&c);
}

#[test]
fn realtime_upgrade_preserves_baselines_and_repairs_backwards_stop_boundaries() {
    let (_dir, path) = at_version(74);
    let c = Connection::open(&path).unwrap();
    c.execute_batch("INSERT INTO sessions(id,workspace_id,harness,cwd,status,created,updated) VALUES ('s','default','custom','.','ended',1,100);
        INSERT INTO runtime_sessions(session_id,node_id,workspace_ref,node_session_id,config_revision,phase,updated) VALUES ('s','n','{}','s','r','idle',100);
        INSERT INTO runtime_turns(turn_id,session_id,state,created,updated) VALUES ('t','s','finished',1,100);
        INSERT INTO runtime_attempts(attempt_id,turn_id,ordinal,node_id,operation_id,execution,spec,digest,state,created,updated) VALUES ('a','t',0,'n','o','{}','{}','d','finished',1,100);
        INSERT INTO realtime_output_attempts(attempt_id,session_id,workspace_id,node_id,started,ended) VALUES ('a','s','default','n',1,20);
        INSERT INTO realtime_output_samples VALUES ('n','st','e',1,'text','a',99,99,100,'text',400);
        INSERT INTO realtime_output_samples VALUES ('n','st','e',2,'stop','a',100,20,100,'gap',0);").unwrap();
    db::migrate(&c).unwrap();
    db::migrate(&c).unwrap(); // the repair is migration-once, subsequent opens keep the same record
    assert_eq!(
        c.query_row(
            "SELECT baseline,ended FROM realtime_output_attempts",
            [],
            |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?))
        )
        .unwrap(),
        (1, 100)
    );
    assert_eq!(
        c.query_row(
            "SELECT source_at FROM realtime_output_samples WHERE event_id='stop'",
            [],
            |r| r.get::<_, i64>(0)
        )
        .unwrap(),
        20
    );
    assert_eq!(count(&c, "SELECT COUNT(*) FROM realtime_output_samples"), 2);
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
        .fixture_write(|c| {
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
    for v in [2, 3, 4, 12, 16, 17, 18, 19, 49] {
        let (_dir, path) = at_version(v);
        let s = Store::open(&path, NoPublisher).unwrap();
        s.fixture_write(|c| {
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
    // Historical module migrations are directly repeatable. 034 contains one-shot ALTERs;
    // their replay protection is the runner's schema_version transaction, exercised below. 035 is
    // indexes only and repeatable again (036's indexes went with their table).
    for m in db::MIGRATIONS[3..33].iter().chain(&db::MIGRATIONS[34..35]) {
        c.execute_batch(m).unwrap();
    }
    // 031 made the merge lane's tables, which a later migration dropped: replayed, they go again
    let dropped = db::MIGRATIONS
        .iter()
        .find(|m| m.contains("DROP TABLE IF EXISTS lanes;"))
        .unwrap();
    c.execute_batch(dropped).unwrap();
    // Replaying the early module SQL also recreates the tables retired by 121 and 127. Replay only
    // their drops here; production never reapplies those migrations to an up-to-date store.
    c.execute_batch(
        "DROP TABLE IF EXISTS yolo_actions;
         DROP TABLE IF EXISTS yolo_windows;
         DROP TABLE IF EXISTS yolo_scopes;
         DROP TABLE IF EXISTS thread_resume;
         DROP TABLE IF EXISTS thread_runs;
         DROP TABLE IF EXISTS route_log;
         DROP TABLE IF EXISTS thread_items;",
    )
    .unwrap();
    drop(c);
    assert_eq!(schema_of(&path), before);
    db::migrate(&Connection::open(&path).unwrap()).unwrap();
    // Opening again at the current version runs no one-shot ALTERs.
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
        .fixture_write(|c| {
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
    s.fixture_write(|c| {
        assert_eq!(count(c, "SELECT v FROM schema_version"), db::SCHEMA_VERSION);
        assert!(yolo(c).is_empty(), "obsolete engine tables are gone");
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

/// 18 backfills stewards once: “我” owns the live bots (in every workspace they serve), the
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

#[test]
fn migration_42_records_how_a_message_was_sent() {
    let (_dir, path) = at_version(41);
    let c = Connection::open(&path).unwrap();
    c.execute_batch("PRAGMA foreign_keys = ON;").unwrap();
    assert!(!columns(&c, "chan_messages").contains(&"sent_via".to_string()));
    assert!(!has_table(&c, "chan_message_seen"));
    db::migrate(&c).unwrap();
    let cols = columns(&c, "chan_messages");
    assert!(cols.contains(&"sent_via".to_string()), "{cols:?}");
    assert!(cols.contains(&"ext_created".to_string()), "{cols:?}");
    assert_eq!(
        columns(&c, "chan_message_seen"),
        ["connection_id", "source_id", "message_id"]
    );
    // a delivery id goes with its message
    c.execute_batch(
        "INSERT INTO spaces (id, workspace_id, system_id, kind, external_id, name, created, updated)
           VALUES ('sp1', 'default', 'feishu', 'group', 'oc_1', '群', 1, 1);
         INSERT INTO chan_messages (id, workspace_id, space_id, direction, status, text, created)
           VALUES ('cm1', 'default', 'sp1', 'out', 'sent', 'hi', 1);
         INSERT INTO chan_message_seen (connection_id, source_id, message_id) VALUES ('con1', 'ev1', 'cm1');
         DELETE FROM chan_messages WHERE id = 'cm1';",
    )
    .unwrap();
    assert_eq!(count(&c, "SELECT count(*) FROM chan_message_seen"), 0);
    // opening again runs nothing twice
    db::migrate(&c).unwrap();
    assert_eq!(
        count(&c, "SELECT v FROM schema_version"),
        db::SCHEMA_VERSION
    );
}

#[test]
fn migration_43_keeps_content_keys_of_messages() {
    let (_dir, path) = at_version(42);
    let c = Connection::open(&path).unwrap();
    assert!(!columns(&c, "chan_messages").contains(&"content_key".to_string()));
    db::migrate(&c).unwrap();
    let cols = columns(&c, "chan_messages");
    assert!(cols.contains(&"content_key".to_string()), "{cols:?}");
    assert!(cols.contains(&"fallback_from".to_string()), "{cols:?}");
    assert_eq!(
        count(
            &c,
            "SELECT count(*) FROM sqlite_master WHERE type = 'index' AND name = 'chan_messages_content_key'"
        ),
        1
    );
    // a lookup by space and key uses the index
    let plan: String = c
        .query_row(
            "EXPLAIN QUERY PLAN SELECT id FROM chan_messages
              WHERE space_id = 'sp1' AND content_key = 'v1:k' AND created BETWEEN 1 AND 2",
            [],
            |r| r.get(3),
        )
        .unwrap();
    assert!(plan.contains("chan_messages_content_key"), "{plan}");
    // opening again runs nothing twice
    db::migrate(&c).unwrap();
    assert_eq!(
        count(&c, "SELECT v FROM schema_version"),
        db::SCHEMA_VERSION
    );
}

#[test]
fn migration_44_finds_the_start_of_a_message_by_index() {
    let (_dir, path) = at_version(43);
    let c = Connection::open(&path).unwrap();
    db::migrate(&c).unwrap();
    assert_eq!(
        count(
            &c,
            "SELECT count(*) FROM sqlite_master WHERE type = 'index' AND name = 'events_message_start'"
        ),
        1
    );
    // the role lookup of a text delta goes by the index, not back through the session's rows
    let plan: String = c
        .query_row(
            "EXPLAIN QUERY PLAN SELECT json_extract(data,'$.role') FROM events
              WHERE session_id = 's1' AND kind = 'message' AND format = 'steve.v1'
                AND json_extract(data,'$.message') = 'm1' ORDER BY seq DESC LIMIT 1",
            [],
            |r| r.get(3),
        )
        .unwrap();
    assert!(plan.contains("events_message_start"), "{plan}");
    db::migrate(&c).unwrap();
    assert_eq!(
        count(&c, "SELECT v FROM schema_version"),
        db::SCHEMA_VERSION
    );
}

#[test]
fn migration_45_adds_the_failure_clock_and_the_compaction_table() {
    let (_dir, path) = at_version(44);
    let c = Connection::open(&path).unwrap();
    /* keepSess chosen while it did nothing must not start compacting after the upgrade */
    c.execute_batch(
        "INSERT INTO workspaces (id, name, created) VALUES ('w2', 'w2', 1), ('w3', 'w3', 1);
         INSERT INTO settings (workspace_id, key, value, updated) VALUES
           ('default', 'keepSess', '\"90 天\"', 1), ('w2', 'keepSess', '\"1 年\"', 1),
           ('w3', 'keepSess', '\"永久\"', 1), ('default', 'keepRaw', '\"30\"', 1);",
    )
    .unwrap();
    db::migrate(&c).unwrap();
    assert_eq!(
        count(
            &c,
            "SELECT count(*) FROM settings WHERE key = 'keepSess' AND value = '\"永久\"'"
        ),
        3
    );
    assert_eq!(
        count(
            &c,
            "SELECT count(*) FROM settings WHERE key = 'keepSess' AND updated = 1"
        ),
        1,
        "an untouched 永久 keeps its timestamp"
    );
    assert_eq!(
        count(
            &c,
            "SELECT count(*) FROM settings WHERE key = 'keepRaw' AND value = '\"30\"'"
        ),
        1
    );
    let col = |t: &str, n: &str| {
        count(
            &c,
            &format!("SELECT count(*) FROM pragma_table_info('{t}') WHERE name = '{n}'"),
        )
    };
    assert_eq!(col("outbox_leases", "failing_since"), 1);
    assert!(has_table(&c, "session_compactions"));
    assert_eq!(col("session_compactions", "upto"), 1);
    assert_eq!(
        count(&c, "SELECT v FROM schema_version"),
        db::SCHEMA_VERSION
    );
}

#[test]
fn migration_48_adds_the_model_of_a_turn() {
    let (_dir, path) = at_version(47);
    let c = Connection::open(&path).unwrap();
    assert!(!columns(&c, "agent_turns").contains(&"model".to_string()));
    c.execute_batch(
        "INSERT INTO agent_turns (id, workspace_id, session_id, harness, user_seq, attempt, started, ended)
         VALUES ('tn_old', 'default', 'ses_1', 'claude-code', 1, 0, 1, 2);",
    )
    .unwrap();
    db::migrate(&c).unwrap();
    assert!(columns(&c, "agent_turns").contains(&"model".to_string()));
    // a turn ended before keeps no model: it is attributed as before
    let m: Option<String> = c
        .query_row(
            "SELECT model FROM agent_turns WHERE id = 'tn_old'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(m, None);
    db::migrate(&c).unwrap();
    assert_eq!(
        count(&c, "SELECT v FROM schema_version"),
        db::SCHEMA_VERSION
    );
}

#[test]
fn capacity_migration_updates_old_display_defaults_preserves_custom_values_and_reopens() {
    // Locate by contract, not merge-time migration number (other branches also add migrations).
    let before = db::MIGRATIONS
        .iter()
        .position(|sql| sql.contains("CREATE INDEX node_commands_session_room"))
        .unwrap();
    let (_dir, path) = at_version(before);
    let c = Connection::open(&path).unwrap();
    c.execute_batch("INSERT INTO devices(id,name,roles,max_sessions,seen,created) VALUES ('hub','Hub','[]',4,1,1),('custom','Custom','[]',8,1,1);
        INSERT INTO nodes(node_id,owner,name,pubkey,fingerprint,state,created) VALUES ('n','u','Node',zeroblob(32),'fp','active',1);") .unwrap();
    db::migrate(&c).unwrap();
    db::migrate(&c).unwrap();
    assert_eq!(
        count(&c, "SELECT max_sessions FROM devices WHERE id='hub'"),
        32
    );
    assert_eq!(
        count(&c, "SELECT max_sessions FROM devices WHERE id='custom'"),
        8
    );
    assert_eq!(
        count(&c, "SELECT max_sessions FROM nodes WHERE node_id='n'"),
        32
    );
    c.execute(
        "INSERT INTO devices(id,name,roles,seen,created) VALUES ('new','New','[]',2,2)",
        [],
    )
    .unwrap();
    assert_eq!(
        count(&c, "SELECT max_sessions FROM devices WHERE id='new'"),
        32
    );
    for invalid in [0, 257] {
        assert!(
            c.execute(
                "UPDATE devices SET max_sessions=? WHERE id='new'",
                [invalid]
            )
            .is_err()
        );
        assert!(
            c.execute(
                "UPDATE nodes SET max_sessions=? WHERE node_id='n'",
                [invalid]
            )
            .is_err()
        );
    }
    assert_eq!(
        count(&c, "SELECT count(*) FROM pragma_foreign_key_check"),
        0
    );
    drop(c);
    let reopened = Store::open(&path, NoPublisher).unwrap();
    assert_eq!(
        reopened
            .fixture_read(|c| Ok::<_, steve_store::StoreError>(count(
                c,
                "SELECT max_sessions FROM devices WHERE id='hub'"
            )))
            .unwrap(),
        32
    );
}

#[test]
fn device_revision_migration_preserves_existing_limits_and_reopens() {
    let before = db::MIGRATIONS
        .iter()
        .position(|sql| sql.contains("ALTER TABLE devices ADD COLUMN revision"))
        .unwrap();
    let (_dir, path) = at_version(before);
    let c = Connection::open(&path).unwrap();
    assert!(!columns(&c, "devices").contains(&"revision".to_string()));
    c.execute_batch(
        "INSERT INTO devices(id,name,roles,status,max_sessions,seen,created)
        VALUES ('hub','Hub','[]','暂停接活',32,1,1),('custom','Custom','[]','正常',256,1,1);",
    )
    .unwrap();
    db::migrate(&c).unwrap();
    db::migrate(&c).unwrap();
    assert_eq!(
        count(&c, "SELECT count(*) FROM devices WHERE revision=0"),
        2
    );
    assert_eq!(
        count(
            &c,
            "SELECT max_sessions FROM devices WHERE id='hub' AND status='暂停接活'"
        ),
        32
    );
    assert_eq!(
        count(&c, "SELECT max_sessions FROM devices WHERE id='custom'"),
        256
    );
    c.execute("UPDATE devices SET revision=7 WHERE id='hub'", [])
        .unwrap();
    drop(c);
    let store = Store::open(path, NoPublisher).unwrap();
    store
        .fixture_read(|c| -> steve_store::Result<()> {
            assert_eq!(count(c, "SELECT revision FROM devices WHERE id='hub'"), 7);
            assert_eq!(
                count(c, "SELECT max_sessions FROM devices WHERE id='custom'"),
                256
            );
            Ok(())
        })
        .unwrap();
}

#[test]
fn replacing_legacy_approval_preserves_human_and_external_facts() {
    let before = db::MIGRATIONS
        .iter()
        .position(|sql| sql.contains("CREATE TABLE approval_policies"))
        .unwrap();
    let (_dir, path) = at_version(before);
    let c = Connection::open(&path).unwrap();
    c.execute_batch(
        r#"INSERT INTO settings VALUES ('default','yolo.policy','{}',1),
               ('default','models.yolo','{}',1), ('default','models.draft','{}',1);
           INSERT INTO yolo_scopes(workspace_id,object_type,object_id,mode,set_by,updated)
               VALUES ('default','workspace','default','full','owner',1);
           INSERT INTO yolo_windows(id,workspace_id,started)
               VALUES ('old-window','default',1);
           INSERT INTO yolo_actions(id,workspace_id,window_id,kind,subject_type,rule,outcome,reason,created)
               VALUES ('old-action','default','old-window','approve','tool_call','legacy','scheduled','fixture',1);
           INSERT INTO decisions(id,workspace_id,kind,subject_type,subject_id,question,status,result,requested_by,created)
               VALUES ('old','default','approve','mail_draft','x','old','resolved','{"decided_by":"yolo"}','steve',1),
                      ('human','default','approve','mail_draft','y','human','resolved','{"decided_by":"owner"}','steve',1),
                      ('unsent','default','approve','tool_call','r1','old','open','{"decided_by":"yolo"}','steve',1),
                      ('maybe','default','approve','tool_call','r2','old','open','{"decided_by":"yolo"}','steve',1);
           INSERT INTO sessions(id,workspace_id,harness,cwd,status,created,updated)
               VALUES ('s1','default','mock','/fixture','running',1,1);
           INSERT INTO runtime_sessions(session_id,node_id,workspace_ref,node_session_id,config_revision,phase,updated)
               VALUES ('s1','local','{}','s1','r1','active',1);
           INSERT INTO runtime_turns(turn_id,session_id,state,created,updated)
               VALUES ('t1','s1','active',1,1);
           INSERT INTO runtime_attempts(attempt_id,turn_id,ordinal,node_id,operation_id,execution,spec,digest,state,created,updated)
               VALUES ('a1','t1',0,'local','start','{}','{}','hash','active',1,1);
           INSERT INTO node_commands(operation_id,node_id,method,request,digest,target_attempt,state,maybe_sent,created,updated)
               VALUES ('never-sent','local','resolve_permission','{}','hash','a1','pending',0,1,1),
                      ('possibly-sent','local','resolve_permission','{}','hash','a1','pending',1,1,1);
           INSERT INTO runtime_permissions(decision_id,node_id,session_id,attempt_id,permission_id,request,expires_at,state,answer_operation,created,updated)
               VALUES ('unsent','local','s1','a1','r1','{}',100,'answering','never-sent',1,1),
                      ('maybe','local','s1','a1','r2','{}',100,'answering','possibly-sent',1,1);
           INSERT INTO item_stats(item_id,workspace_id,created,updated,yolo_decided,decisions)
               VALUES ('old-stat','default',1,1,4,7);
           INSERT INTO mail_drafts(id,workspace_id,connection_id,mode,to_addrs,subject,body,author,
               decision_id,content_hash,status,send_at,created,updated,sent)
             VALUES ('queued','default','test','new','[]','q','queued','bot','old','hash','approved',10,1,1,NULL),
               ('claimed','default','test','new','[]','q','claimed','bot','old','hash','sending',10,1,1,10),
               ('sent','default','test','new','[]','q','sent','bot','old','hash','sent',10,1,1,10),
               ('owner','default','test','new','[]','q','owner','bot','human','hash','approved',10,1,1,NULL);"#,
    )
    .unwrap();
    drop(c);
    let store = Store::open(&path, NoPublisher).unwrap();
    assert_eq!(
        store
            .read(|r| steve_store::approval::policy(r, "default"))
            .unwrap(),
        steve_types::ApprovalPolicy::default(),
    );
    store.fixture_write(|c| {
        assert_eq!(count(c, "SELECT COUNT(*) FROM decisions WHERE id='human' AND status='resolved'"), 1);
        assert_eq!(count(c, "SELECT COUNT(*) FROM decisions WHERE id='old' AND status='resolved' AND json_extract(result, '$.decided_by')='steve'"), 1);
        assert_eq!(count(c, "SELECT COUNT(*) FROM settings WHERE key IN ('yolo.policy','models.yolo')"), 0);
        assert_eq!(count(c, "SELECT COUNT(*) FROM settings WHERE key='models.draft'"), 1);
        assert_eq!(count(c, "SELECT COUNT(*) FROM mail_drafts WHERE id='queued' AND status='discarded' AND send_at IS NULL"), 1);
        assert_eq!(count(c, "SELECT COUNT(*) FROM mail_drafts WHERE id='claimed' AND status='sending' AND sent=10"), 1);
        assert_eq!(count(c, "SELECT COUNT(*) FROM mail_drafts WHERE id='sent' AND status='sent' AND sent=10"), 1);
        assert_eq!(count(c, "SELECT COUNT(*) FROM mail_drafts WHERE id='owner' AND status='approved' AND send_at=10"), 1);
        assert_eq!(count(c, "SELECT COUNT(*) FROM node_commands WHERE operation_id='never-sent' AND state='rejected'"), 1);
        assert_eq!(count(c, "SELECT COUNT(*) FROM decisions WHERE id='unsent' AND status='expired'"), 1);
        assert_eq!(count(c, "SELECT COUNT(*) FROM runtime_permissions WHERE decision_id='unsent' AND state='expired'"), 1);
        // Approval migration 121 leaves this answer for reconciliation. Migration 122 marks
        // possibly dispatched pre-cutover commands unknown and preserves their dispatch evidence.
        assert_eq!(count(c, "SELECT COUNT(*) FROM node_commands WHERE operation_id='possibly-sent' AND state='unknown' AND maybe_sent=1 AND json_extract(receipt, '$.payload.state.state')='unknown'"), 1);
        assert_eq!(count(c, "SELECT COUNT(*) FROM runtime_permissions WHERE decision_id='maybe' AND state='answering' AND resolver='legacy'"), 1);
        assert_eq!(count(c, "SELECT decisions FROM item_stats WHERE item_id='old-stat'"), 7);
        assert_eq!(count(c, "SELECT COUNT(*) FROM sqlite_master WHERE name LIKE 'yolo_%'"), 0);
        assert_eq!(count(c, "SELECT COUNT(*) FROM pragma_foreign_key_check"), 0);
        Ok(())
    }).unwrap();
}

#[test]
fn approval_policy_is_an_owner_cas_and_only_emits_changes() {
    use steve_types::ApprovalMode::*;
    let dir = tempfile::tempdir().unwrap();
    let (bus, mut receiver) = tokio::sync::broadcast::channel::<steve_types::BusEvent>(16);
    let store = Store::open(dir.path().join("approval.sqlite3"), bus).unwrap();
    store.ensure_workspace().unwrap();
    store.fixture_write(|c| {
        c.execute_batch(
            "INSERT INTO users(id,workspace_id,subject,name,role,created)
               VALUES ('o','default','o','Owner','owner',1),('m','default','m','Member','member',1);
             INSERT INTO workspace_members VALUES ('default','o','owner',1),('default','m','member',1);",
        )?;
        Ok(())
    }).unwrap();
    while receiver.try_recv().is_ok() {}
    let set = |who, revision, mode| {
        store.write(|w| steve_store::approval::set(w, "default", who, revision, mode))
    };
    assert!(matches!(
        set("m", 0, HarnessAuto),
        Err(steve_store::StoreError::Rejected(_))
    ));
    assert!(receiver.try_recv().is_err());
    let policy = set("o", 0, HarnessAuto).unwrap();
    assert_eq!(
        (
            policy.mode,
            policy.revision,
            policy.actor.as_ref().map(|s| s.as_str())
        ),
        (HarnessAuto, 1, Some("o"))
    );
    assert_eq!(set("o", 1, HarnessAuto).unwrap(), policy);
    assert!(matches!(
        set("o", 0, AskOwner),
        Err(steve_store::StoreError::Conflict(_))
    ));
    assert_eq!(set("o", 1, AskOwner).unwrap().revision, 2);
    let mut emitted = Vec::new();
    while let Ok(ev) = receiver.try_recv() {
        let value = serde_json::to_value(ev).unwrap();
        if value["type"] == "approval_policy" {
            emitted.push(value["policy"]["revision"].as_i64().unwrap());
        }
    }
    assert_eq!(emitted, [1, 2]);
}

#[test]
fn migration_135_retires_yolo_rows_and_backfills_node_bindings() {
    let before_135 = db::MIGRATIONS
        .iter()
        .position(|sql| sql.contains("DELETE FROM audit WHERE action LIKE 'yolo.%'"))
        .unwrap();
    let (_dir, path) = at_version(before_135);
    let c = Connection::open(&path).unwrap();
    c.execute_batch(
        r#"INSERT INTO bots(id,workspace_id,name,harness,updated)
             VALUES ('b1','default','Bot1','claude-code',1),
                    ('b2','default','Bot2','claude-code',1),
                    ('b3','default','Bot3','claude-code',1);
           INSERT INTO bot_workspaces(bot_id,workspace_id,created)
             VALUES ('b1','default',1), ('b2','default',1), ('b3','default',1);
           INSERT INTO bot_node_bindings(workspace_id,bot_id,nodes)
             VALUES ('default','b2','["remote-1"]');
           INSERT INTO nodes(node_id,owner,name,pubkey,fingerprint,state,local,created,revoked_at)
             VALUES ('local','owner','Hub',zeroblob(32),'fp-l','active',1,1,NULL),
                    ('remote-active','owner','Mac',zeroblob(32),'fp-a','active',0,1,NULL),
                    ('remote-revoked','owner','Old',zeroblob(32),'fp-r','revoked',0,1,10);
           INSERT INTO sessions(id,workspace_id,bot_id,harness,cwd,status,created,updated)
             VALUES ('s1','default','b3','claude-code','.','ended',1,1),
                    ('s2','default','b3','claude-code','.','ended',1,1),
                    ('s3','default','b3','claude-code','.','ended',1,1);
           INSERT INTO runtime_sessions(session_id,node_id,workspace_ref,node_session_id,config_revision,phase,updated)
             VALUES ('s1','local','{}','s1','r1','idle',1),
                    ('s2','remote-active','{}','s2','r1','idle',1),
                    ('s3','remote-revoked','{}','s3','r1','idle',1);
           INSERT INTO decisions(id,workspace_id,kind,subject_type,subject_id,question,status,result,requested_by,resolved_by,created)
             VALUES ('d_yolo','default','approve','mail_draft','m1','q','resolved','{"decided_by":"yolo","choice":"approve","yolo_action":"a1","yolo_reason":"auto","yolo_rule":"r1","hash":"h","send_at":10,"content_hash":"ch"}','steve','yolo',1),
                    ('d_human','default','approve','mail_draft','m2','q','resolved','{"decided_by":"owner","choice":"approve"}','steve','owner',1);
           INSERT INTO parties(id,workspace_id,type,name,created_by,created,updated)
             VALUES ('p1','default','person','Owner','owner',1,1);
           INSERT INTO accounts(id,workspace_id,system_id,family,party_id,kind,handle,handle_display,source,created,updated)
             VALUES ('acc1','default','feishu','feishu_app','p1','app_bot','bot','bot','me',1,1);
           INSERT INTO connections(id,workspace_id,account_id,adapter,auth,created_by,created,updated)
             VALUES ('c1','default','acc1','feishu','none','owner',1,1);
           INSERT INTO spaces(id,workspace_id,system_id,connection_id,kind,external_id,name,created,updated)
             VALUES ('sp','default','feishu','c1','group','ext_sp','Group',1,1);
           INSERT INTO chan_messages(id,workspace_id,space_id,direction,status,decision_id,created,sent)
             VALUES ('cm_pending','default','sp','out','pending','d_yolo',1,NULL),
                    ('cm_sent','default','sp','out','sent','d_yolo',1,10);
           INSERT INTO mail_drafts(id,workspace_id,connection_id,mode,to_addrs,subject,body,author,decision_id,content_hash,status,send_at,created,updated,sent)
             VALUES ('md_queued','default','c1','new','[]','s','b','bot','d_yolo','h','approved',10,1,1,NULL),
                    ('md_sending','default','c1','new','[]','s','b','bot','d_yolo','h','sending',10,1,1,10),
                    ('md_sent','default','c1','new','[]','s','b','bot','d_yolo','h','sent',10,1,1,10);
           INSERT INTO audit(ts,actor,action,target,detail,workspace_id)
             VALUES (1,'yolo','decision.resolve','d_yolo','{}','default'),
                    (2,'owner','yolo.window.start','w1','{}','default'),
                    (3,'owner','bot.update','b1','{}','default');"#,
    )
    .unwrap();
    drop(c);

    let store = Store::open(&path, NoPublisher).unwrap();
    store
        .fixture_write(|c| {
            assert_eq!(
                count(
                    c,
                    "SELECT COUNT(*) FROM decisions WHERE id='d_yolo' AND status='resolved'
                       AND resolved_by='steve'
                       AND json_extract(result,'$.decided_by')='steve'
                       AND json_extract(result,'$.choice')='approve'
                       AND json_extract(result,'$.yolo_action') IS NULL
                       AND json_extract(result,'$.yolo_reason') IS NULL
                       AND json_extract(result,'$.yolo_rule') IS NULL
                       AND json_extract(result,'$.hash') IS NULL
                       AND json_extract(result,'$.send_at') IS NULL
                       AND json_extract(result,'$.content_hash') IS NULL"
                ),
                1
            );
            assert_eq!(
                count(
                    c,
                    "SELECT COUNT(*) FROM decisions WHERE id='d_human' AND status='resolved' AND resolved_by='owner' AND json_extract(result,'$.decided_by')='owner'"
                ),
                1
            );
            assert_eq!(
                count(
                    c,
                    "SELECT COUNT(*) FROM chan_messages WHERE id='cm_pending' AND status='discarded'"
                ),
                1
            );
            assert_eq!(
                count(
                    c,
                    "SELECT COUNT(*) FROM chan_messages WHERE id='cm_sent' AND status='sent' AND sent=10"
                ),
                1
            );
            assert_eq!(
                count(
                    c,
                    "SELECT COUNT(*) FROM mail_drafts WHERE id='md_queued' AND status='discarded' AND send_at IS NULL"
                ),
                1
            );
            assert_eq!(
                count(
                    c,
                    "SELECT COUNT(*) FROM mail_drafts WHERE id='md_sending' AND status='sending' AND sent=10"
                ),
                1
            );
            assert_eq!(
                count(
                    c,
                    "SELECT COUNT(*) FROM mail_drafts WHERE id='md_sent' AND status='sent' AND sent=10"
                ),
                1
            );
            assert_eq!(count(c, "SELECT COUNT(*) FROM audit WHERE actor='yolo'"), 0);
            assert_eq!(
                count(c, "SELECT COUNT(*) FROM audit WHERE action LIKE 'yolo.%'"),
                0
            );
            assert_eq!(
                count(
                    c,
                    "SELECT COUNT(*) FROM audit WHERE actor='steve' AND target='d_yolo'"
                ),
                1
            );
            assert_eq!(
                count(
                    c,
                    "SELECT COUNT(*) FROM bot_node_bindings WHERE bot_id='b1' AND nodes='[\"local\"]'"
                ),
                1
            );
            assert_eq!(
                count(
                    c,
                    "SELECT COUNT(*) FROM bot_node_bindings WHERE bot_id='b2' AND nodes='[\"remote-1\"]'"
                ),
                1
            );
            assert_eq!(
                count(
                    c,
                    "SELECT COUNT(*) FROM bot_node_bindings WHERE bot_id='b3' AND nodes='[\"local\",\"remote-active\"]'"
                ),
                1
            );
            Ok(())
        })
        .unwrap();
}

#[test]
fn migration_136_migrates_bot_perm_modes_to_steve_vocabulary() {
    let before_136 = db::MIGRATIONS
        .iter()
        .position(|sql| sql.contains("perm_mode TEXT NOT NULL DEFAULT 'ask'"))
        .unwrap();
    let (_dir, path) = at_version(before_136);
    let c = Connection::open(&path).unwrap();
    c.execute_batch(
        r#"INSERT INTO bots(id,workspace_id,name,harness,perm_mode,updated)
             VALUES ('b_def','default','Default','claude-code','default',1),
                    ('b_ask','default','Ask','claude-code','ask',1),
                    ('b_auto','default','Auto','claude-code','auto',1),
                    ('b_edit','default','Edit','claude-code','acceptEdits',1),
                    ('b_edit2','default','Edit2','claude-code','accept_edits',1),
                    ('b_plan','default','Plan','claude-code','plan',1),
                    ('b_full','default','Full','claude-code','bypassPermissions',1),
                    ('b_full2','default','Full2','claude-code','full_access',1),
                    ('b_bad','default','Bad','claude-code','dontAsk',1);"#,
    )
    .unwrap();
    drop(c);

    let store = Store::open(&path, NoPublisher).unwrap();
    store
        .fixture_write(|c| {
            assert_eq!(
                count(
                    c,
                    "SELECT COUNT(*) FROM bots WHERE id IN ('b_def','b_ask','b_bad') AND perm_mode='ask'"
                ),
                3
            );
            assert_eq!(
                count(
                    c,
                    "SELECT COUNT(*) FROM bots WHERE id='b_auto' AND perm_mode='auto'"
                ),
                1
            );
            assert_eq!(
                count(
                    c,
                    "SELECT COUNT(*) FROM bots WHERE id IN ('b_edit','b_edit2') AND perm_mode='accept_edits'"
                ),
                2
            );
            assert_eq!(
                count(
                    c,
                    "SELECT COUNT(*) FROM bots WHERE id='b_plan' AND perm_mode='plan'"
                ),
                1
            );
            assert_eq!(
                count(
                    c,
                    "SELECT COUNT(*) FROM bots WHERE id IN ('b_full','b_full2') AND perm_mode='full_access'"
                ),
                2
            );
            // Column default is now Steve's 'ask', never Claude Code's 'default'.
            c.execute(
                "INSERT INTO bots(id,workspace_id,name,harness,updated) VALUES ('b_new','default','New','claude-code',1)",
                [],
            )?;
            assert_eq!(
                count(
                    c,
                    "SELECT COUNT(*) FROM bots WHERE id='b_new' AND perm_mode='ask'"
                ),
                1
            );
            assert_eq!(count(c, "SELECT COUNT(*) FROM pragma_foreign_key_check"), 0);
            Ok(())
        })
        .unwrap();
}

#[test]
fn migration_137_drops_legacy_project_dir_and_worktree_columns() {
    let before_137 = db::MIGRATIONS
        .iter()
        .position(|sql| sql.contains("ALTER TABLE bots DROP COLUMN project_dir"))
        .unwrap();
    let (_dir, path) = at_version(before_137);
    let c = Connection::open(&path).unwrap();
    c.execute_batch(
        r#"INSERT INTO bots(id,workspace_id,name,harness,project_dir,worktree,updated)
             VALUES ('b1','default','Bot1','claude-code','/old',1,1);
           INSERT INTO items(id,workspace_id,no,title,phase,bot_id,project_dir,created,updated)
             VALUES ('ST-1','default',1,'Item','active','b1','/old',1,1);"#,
    )
    .unwrap();
    drop(c);

    let store = Store::open(&path, NoPublisher).unwrap();
    store
        .fixture_write(|c| {
            let bot_cols = columns(c, "bots");
            assert!(!bot_cols.contains(&"project_dir".to_string()));
            assert!(!bot_cols.contains(&"worktree".to_string()));
            let item_cols = columns(c, "items");
            assert!(!item_cols.contains(&"project_dir".to_string()));
            assert_eq!(count(c, "SELECT COUNT(*) FROM bots WHERE id='b1'"), 1);
            assert_eq!(count(c, "SELECT COUNT(*) FROM items WHERE id='ST-1'"), 1);
            assert_eq!(count(c, "SELECT COUNT(*) FROM pragma_foreign_key_check"), 0);
            Ok(())
        })
        .unwrap();
    let item = store.items().get("ST-1").unwrap().unwrap();
    let j = serde_json::to_value(&item).unwrap();
    assert!(j.get("project_dir").is_none());
}

#[test]
fn migration_130_preserves_history_commits_seq_and_notices() {
    let before_130 = db::MIGRATIONS
        .iter()
        .position(|sql| sql.contains("CREATE TABLE thread_submit_keys"))
        .unwrap();
    let (_dir, path) = at_version(before_130);
    let c = Connection::open(&path).unwrap();
    c.execute_batch(
        "INSERT INTO users(id,workspace_id,subject,name,role,created) VALUES('u1','default','u1','U1','owner',1);
         INSERT INTO workspace_members(workspace_id,user_id,role,created) VALUES('default','u1','owner',1);
         INSERT INTO items(id,workspace_id,no,title,phase,created,updated) VALUES
           ('ST-1','default',1,'T1','active',1,1),
           ('ST-2','default',2,'T2','active',1,1),
           ('ST-3','default',3,'T3','active',1,1);
         INSERT INTO messages(id,workspace_id,space,author,text,created) VALUES
           ('m_done','default','steve','me','done q',10),
           ('m_done_r','default','steve','steve','done a',11),
           ('m_run','default','steve','me','running q',12),
           ('m_route','default','steve','me','route only q',13),
           ('m_route_r','default','steve','steve','route only a',14),
           ('m_ans','default','steve','me','ans fallback q',15),
           ('m_ans_r','default','steve','steve','ans fallback a',16),
           ('m_dup','default','steve','me','rerun q',17),
           ('m_dup_r','default','steve','steve','rerun a',18),
           ('m_item','default','steve','me','item q',19),
           ('m_item_r','default','steve','steve','item a',19),
           ('m_supp','default','steve','me','supp q',20),
           ('m_supp_r','default','steve','steve','supp a',20),
           ('n_fail_1','default','notify','steve','fail 1',20),
           ('n_ok_2','default','notify','steve','ok 2',21),
           ('n_fail_3','default','notify','steve','fail 3',22);
         UPDATE messages SET item_id='ST-1', meta='{\"notice\":{\"kind\":\"release\",\"level\":\"fail\"}}' WHERE id='n_fail_1';
         UPDATE messages SET item_id='ST-1', meta='{\"notice\":{\"kind\":\"release\",\"level\":\"success\"}}' WHERE id='n_ok_2';
         UPDATE messages SET item_id='ST-2', meta='{\"notice\":{\"kind\":\"release\",\"level\":\"fail\"}}' WHERE id='n_fail_3';
         INSERT INTO messages(id,workspace_id,space,author,text,item_id,meta,created) VALUES
           ('h_1','default','steve','steve','fail 1','ST-1','{\"notice\":{\"kind\":\"release\",\"level\":\"fail\",\"ref\":\"n_fail_1\"}}',20),
           ('h_3','default','steve','steve','fail 3','ST-2','{\"notice\":{\"kind\":\"release\",\"level\":\"fail\",\"ref\":\"n_fail_3\"}}',22);
         INSERT INTO settings(workspace_id,key,value,updated) VALUES
           ('default','noticeDismissed','[\"n_fail_3\",\"dec_alert_9\"]',25),
           ('default','thread.pending.m_run','{}',12);
         INSERT INTO route_log(id,workspace_id,message_id,reply_id,model_kind,kind,item_id,ms,created) VALUES
           ('rl_1','default','m_done','m_done_r','research','research','ST-2',50,10),
           ('rl_2','default','m_route','m_route_r','answer','answer',NULL,30,13),
           ('rl_ans','default','m_ans','m_ans_r','answer','answer',NULL,20,15),
           ('rl_dup','default','m_dup','m_dup_r','answer','answer',NULL,20,17),
           ('rl_item','default','m_item','m_item_r','item','item','ST-3',20,19),
           ('rl_supp','default','m_supp','m_supp_r','supplement','supplement','ST-3',20,20);
         INSERT INTO thread_runs(id,workspace_id,message_id,reply_id,user_id,status,created,started,ended) VALUES
           ('tr_done','default','m_done','m_done_r','u1','done',10,10,11),
           ('tr_run','default','m_run',NULL,'u1','running',12,12,NULL),
           ('tr_ans','default','m_ans',NULL,'u1','done',15,15,16),
           ('tr_dup_fail','default','m_dup',NULL,'u1','failed',17,17,17),
           ('tr_dup_done','default','m_dup',NULL,'u1','done',18,18,18);
         INSERT INTO thread_resume(run_id,created,attempts) VALUES('tr_run',12,1);
         INSERT INTO thread_items(item_id,workspace_id,origin_msg,reply_msg,from_question,follow,last_post,created) VALUES
           ('ST-1','default','m_done','m_done_r',1,1,15,10),
           ('ST-2','default','m_done',NULL,0,0,NULL,11),
           ('ST-3','default','m_item','m_item_r',0,1,19,19);
         INSERT INTO thread_reply_commits(seq,message_id,run_id,workspace_id,user_id) VALUES(7,'m_done_r','tr_done','default','u1');
         DELETE FROM sqlite_sequence WHERE name='thread_reply_commits';
         INSERT INTO sqlite_sequence(name,seq) VALUES('thread_reply_commits',19);",
    )
    .unwrap();
    drop(c);

    let store = Store::open(&path, NoPublisher).unwrap();
    store
        .fixture_read(|c| -> steve_store::Result<()> {
            assert_eq!(count(c, "SELECT count(*) FROM pragma_foreign_key_check"), 0);
            for gone in ["thread_resume", "thread_runs", "route_log", "thread_items"] {
                assert!(!has_table(c, gone), "{gone} must be dropped");
            }
            assert_eq!(
                c.query_row(
                    "SELECT outcome FROM thread_reply_receipts WHERE id='tr_done'",
                    [],
                    |r| r.get::<_, String>(0)
                )?,
                "answered"
            );
            assert_eq!(
                c.query_row(
                    "SELECT outcome FROM thread_reply_receipts WHERE id='tr_run'",
                    [],
                    |r| r.get::<_, String>(0)
                )?,
                "unknown"
            );
            assert_eq!(
                c.query_row(
                    "SELECT reply_message_id FROM thread_reply_receipts WHERE id='tr_ans'",
                    [],
                    |r| r.get::<_, Option<String>>(0)
                )?
                .as_deref(),
                Some("m_ans_r"),
                "null thread_runs.reply_id falls back to route_log.reply_id"
            );
            assert_eq!(
                c.query_row(
                    "SELECT reply_message_id FROM thread_reply_receipts WHERE id='tr_dup_done'",
                    [],
                    |r| r.get::<_, Option<String>>(0)
                )?
                .as_deref(),
                Some("m_dup_r"),
                "done rerun wins UNIQUE reply_message_id fallback"
            );
            assert_eq!(
                c.query_row(
                    "SELECT reply_message_id FROM thread_reply_receipts WHERE id='tr_dup_fail'",
                    [],
                    |r| r.get::<_, Option<String>>(0)
                )?,
                None,
                "failed sibling rerun stays NULL after losing UNIQUE fallback"
            );
            assert_eq!(
                c.query_row(
                    "SELECT dispatch_ref_id FROM thread_reply_receipts WHERE id='tr_done'",
                    [],
                    |r| r.get::<_, Option<String>>(0)
                )?
                .as_deref(),
                Some("mig_disp_ST-2"),
                "multi-item legacy origin prefers the dispatch ref matching receipt item_id"
            );
            assert_eq!(
                c.query_row(
                    "SELECT dispatch_ref_id FROM thread_reply_receipts WHERE id='mig_route_rl_item'",
                    [],
                    |r| r.get::<_, Option<String>>(0)
                )?
                .as_deref(),
                Some("mig_disp_ST-3"),
                "non-run route_log item receipt backfills dispatch_ref_id"
            );
            assert_eq!(
                c.query_row(
                    "SELECT dispatch_ref_id FROM thread_reply_receipts WHERE id='mig_route_rl_supp'",
                    [],
                    |r| r.get::<_, Option<String>>(0)
                )?,
                None,
                "supplement targeting an earlier origin's item does not borrow that origin's create_item ref"
            );
            assert_eq!(
                count(
                    c,
                    "SELECT count(*) FROM thread_reply_receipts WHERE origin_message_id='m_route'"
                ),
                1
            );
            assert_eq!(
                c.query_row(
                    "SELECT seq FROM thread_reply_commits WHERE message_id='m_done_r' AND receipt_id='tr_done'",
                    [],
                    |r| r.get::<_, i64>(0)
                )?,
                7
            );
            assert_eq!(
                c.query_row(
                    "SELECT seq FROM sqlite_sequence WHERE name='thread_reply_commits'",
                    [],
                    |r| r.get::<_, i64>(0)
                )?,
                19
            );
            assert_eq!(count(c, "SELECT count(*) FROM thread_dispatch_refs"), 3);
            assert_eq!(count(c, "SELECT count(*) FROM messages WHERE space='notify'"), 0);
            assert_eq!(
                count(
                    c,
                    "SELECT count(*) FROM settings WHERE key='noticeDismissed' OR key LIKE 'thread.pending.%'"
                ),
                0
            );
            assert_eq!(
                count(
                    c,
                    "SELECT count(*) FROM thread_notices WHERE id IN ('n_fail_1','n_fail_3') AND closed_at IS NOT NULL"
                ),
                2
            );
            assert_eq!(
                count(
                    c,
                    "SELECT count(*) FROM thread_notices WHERE kind='alert' AND causal_key='dec_alert_9' AND closed_at IS NOT NULL"
                ),
                1
            );
            Ok(())
        })
        .unwrap();
    drop(store);
    drop(Store::open(&path, NoPublisher).unwrap());
}

#[test]
fn migration_128_strips_main_bot_from_teams_and_rewrites_steve_fallback() {
    let before_128 = db::MIGRATIONS
        .iter()
        .position(|sql| sql.contains("CREATE TABLE item_placement_waits"))
        .unwrap();
    let (_dir, path) = at_version(before_128);
    let c = Connection::open(&path).unwrap();
    c.execute_batch(
        r#"INSERT INTO bots(id,workspace_id,name,main,harness,updated)
             VALUES ('steve','default','Steve',1,'claude-code',1),
                    ('dev','default','Dev',0,'claude-code',1);
           INSERT INTO bot_workspaces(workspace_id,bot_id,created)
             VALUES ('default','steve',1),('default','dev',1);
           INSERT INTO bot_teams(id,workspace_id,name,members)
             VALUES ('team-mixed','default','Mixed','[{"bot":"steve","nodes":null},{"bot":"dev","nodes":["local"]}]'),
                    ('team-only-main','default','OnlyMain','[{"bot":"steve","nodes":null}]');
           INSERT INTO items(id,workspace_id,no,title,bot_id,phase,created,updated)
             VALUES ('ST-1','default',1,'Legacy fallback','steve','active',1,1),
                    ('ST-2','default',2,'Normal placement','dev','active',1,1);
           INSERT INTO bot_placements(item_id,session_id,fact,blocked)
             VALUES ('ST-1',NULL,'{"rule_version":1,"bot_revision":1,"at":1,"node":"local","required_node":null,"node_priority":0,"harness":"claude-code","model":null,"reasoning_effort":null,"explanation":{"bot":"steve","reason":"steve_fallback","matched":[],"rejected":[{"bot":"dev","reason":"offline"}],"fallback":"offline"}}',NULL),
                    ('ST-2',NULL,'{"rule_version":1,"bot_revision":1,"at":1,"node":"local","required_node":null,"node_priority":0,"harness":"claude-code","model":null,"reasoning_effort":null,"explanation":{"bot":"dev","reason":"specialty","matched":["rust"],"rejected":[],"fallback":null}}',NULL);"#,
    )
    .unwrap();
    drop(c);

    let store = Store::open(&path, NoPublisher).unwrap();
    // Idempotent re-migration changes nothing.
    db::migrate_file(&path).unwrap();

    let teams = store
        .read(|r| steve_store::bot_dispatch::teams(r, "default"))
        .unwrap();
    let mixed = teams.iter().find(|t| t.id == "team-mixed").unwrap();
    assert_eq!(mixed.members.len(), 1);
    assert_eq!(mixed.members[0].bot, "dev");
    assert_eq!(mixed.members[0].nodes, Some(vec!["local".into()]));
    let only_main = teams.iter().find(|t| t.id == "team-only-main").unwrap();
    assert!(only_main.members.is_empty());

    let migrated = store
        .read(|r| steve_store::bot_dispatch::initial(r, "ST-1"))
        .unwrap()
        .unwrap();
    assert_eq!(
        migrated.explanation.reason,
        steve_types::PlacementReason::CoordinatorDeferred
    );
    assert_eq!(
        migrated.explanation.deferred_by,
        Some(steve_types::CoordinatorDeferSource::Setting)
    );
    assert_eq!(
        migrated.explanation.fallback,
        Some(steve_types::PlacementRejection::Offline)
    );

    let untouched = store
        .read(|r| steve_store::bot_dispatch::initial(r, "ST-2"))
        .unwrap()
        .unwrap();
    assert_eq!(
        untouched.explanation.reason,
        steve_types::PlacementReason::Specialty
    );
    assert_eq!(untouched.explanation.deferred_by, None);

    // Store contract refuses adding main bot into Team members.
    let err = store
        .write(|w| {
            steve_store::bot_admin::save(
                w,
                "default",
                None,
                "Bad Team",
                &[steve_store::bot_dispatch::Member {
                    bot: "steve".into(),
                    nodes: None,
                }],
            )
        })
        .unwrap_err();
    assert!(matches!(err, steve_store::StoreError::Rejected(_)), "{err}");

    store
        .fixture_write(|c| {
            assert!(has_table(c, "item_placement_waits"));
            assert_eq!(count(c, "SELECT count(*) FROM pragma_foreign_key_check"), 0);
            Ok(())
        })
        .unwrap();
}
