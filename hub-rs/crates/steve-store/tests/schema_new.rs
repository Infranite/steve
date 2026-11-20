//! Current module tables, FTS operations, message indexes, and device defaults.

mod common;

use common::*;
use rusqlite::Connection;
use steve_store::{NoPublisher, Store};

/// Representative tables that every fresh database provides.
const NEW_TABLES: [&str; 31] = [
    "connections",
    "ext_events",
    "spaces",
    "kb_chunks",
    "mail_drafts",
    "gh_prs",
    "chan_cursors",
    "yolo_scopes",
    "yolo_windows",
    "yolo_actions",
    "agent_turns",
    "usage_records",
    "item_marks",
    "stat_daily",
    "item_stats",
    "route_log",
    "thread_runs",
    "thread_items",
    "stewards",
    "space_prefs",
    "chan_reactions",
    "outbox_events",
    "outbox_consumers",
    "outbox_receipts",
    "outbox_leases",
    "event_payloads",
    "event_payload_refs",
    "chan_app_registrations",
    "session_resume",
    "session_dispatch",
    "thread_resume",
];

fn has_table(c: &Connection, name: &str) -> bool {
    count(
        c,
        &format!("SELECT count(*) FROM sqlite_master WHERE type = 'table' AND name = '{name}'"),
    ) == 1
}

#[test]
fn fresh_database_has_current_module_tables() {
    let t = fresh();
    t.store
        .with_conn(|c| {
            assert_eq!(count(c, "SELECT count(*) FROM domain_schema"), 3);
            for name in NEW_TABLES {
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

#[test]
fn message_delivery_ids_follow_their_messages() {
    let t = fresh();
    let c = Connection::open(t.store.path()).unwrap();
    c.execute_batch("PRAGMA foreign_keys = ON;").unwrap();
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
}

#[test]
fn message_content_keys_use_the_index() {
    let t = fresh();
    let c = Connection::open(t.store.path()).unwrap();
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
}

#[test]
fn message_start_lookup_uses_the_index() {
    let t = fresh();
    let c = Connection::open(t.store.path()).unwrap();
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
}

#[test]
fn failure_clock_and_compaction_tables_are_present() {
    let t = fresh();
    let c = Connection::open(t.store.path()).unwrap();
    let col = |t: &str, n: &str| {
        count(
            &c,
            &format!("SELECT count(*) FROM pragma_table_info('{t}') WHERE name = '{n}'"),
        )
    };
    assert_eq!(col("outbox_leases", "failing_since"), 1);
    assert!(has_table(&c, "session_compactions"));
    assert_eq!(col("session_compactions", "upto"), 1);
}

#[test]
fn capacity_defaults_constraints_and_custom_values_survive_reopen() {
    let t = fresh();
    let path = t.store.path().to_path_buf();
    drop(t.store);
    let c = Connection::open(&path).unwrap();
    c.execute_batch("INSERT INTO devices(id,name,roles,max_sessions,seen,created) VALUES ('hub','Hub','[]',32,1,1),('custom','Custom','[]',8,1,1);
        INSERT INTO nodes(node_id,owner,name,pubkey,fingerprint,state,created) VALUES ('n','u','Node',zeroblob(32),'fp','active',1);") .unwrap();
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
            .reader(|c| Ok::<_, steve_store::StoreError>(count(
                c,
                "SELECT max_sessions FROM devices WHERE id='hub'"
            )))
            .unwrap(),
        32
    );
}

#[test]
fn device_revision_defaults_and_existing_limits_survive_reopen() {
    let t = fresh();
    let path = t.store.path().to_path_buf();
    drop(t.store);
    let c = Connection::open(&path).unwrap();
    c.execute_batch(
        "INSERT INTO devices(id,name,roles,status,max_sessions,seen,created)
        VALUES ('hub','Hub','[]','暂停接活',32,1,1),('custom','Custom','[]','正常',256,1,1);",
    )
    .unwrap();
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
        .reader(|c| -> steve_store::Result<()> {
            assert_eq!(count(c, "SELECT revision FROM devices WHERE id='hub'"), 7);
            assert_eq!(
                count(c, "SELECT max_sessions FROM devices WHERE id='custom'"),
                256
            );
            Ok(())
        })
        .unwrap();
}
