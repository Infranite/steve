//! Migration 12: `spaces` is rebuilt so that a space outlives its connection
//! (`connection_id` nullable, `ON DELETE SET NULL`). Checked from v3 (production) and from v11 with
//! channel data: rows survive, foreign keys hold, and deleting the connection keeps the history.

mod common;

use common::*;
use rusqlite::Connection;
use steve_store::{NoPublisher, Store, db};

/// A database the way a build with `v` migrations left it.
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

/// One connected Feishu bot with two spaces, members, settings, messages and a poll cursor.
const CHANNEL_DATA: &str = r#"
INSERT INTO parties (id, workspace_id, type, name, created_by, created, updated)
  VALUES ('p1', 'default', 'org', '本公司', 'u', 1, 1), ('p2', 'default', 'person', '张三', 'u', 1, 1);
INSERT INTO accounts (id, workspace_id, system_id, family, party_id, kind, handle, handle_display, source, created, updated)
  VALUES ('a1', 'default', 'feishu', 'im', 'p1', 'app_bot', 'cli_x', 'cli_x', 'connect', 1, 1),
         ('a2', 'default', 'feishu', 'im', 'p2', 'personal', 'ou_2', 'ou_2', 'identify', 1, 1);
INSERT INTO connections (id, workspace_id, account_id, adapter, auth, created_by, created, updated)
  VALUES ('c1', 'default', 'a1', 'feishu', 'credential', 'u', 1, 1);
INSERT INTO channel_settings (connection_id, workspace_id, mode, rules, updated)
  VALUES ('c1', 'default', 'bot', '[]', 1);
INSERT INTO spaces (id, workspace_id, system_id, connection_id, kind, external_id, name, member_count, meeting, status, last_message, created, updated)
  VALUES ('s1', 'default', 'feishu', 'c1', 'group', 'oc_1', '值班群', 3, NULL, 'active', 5, 2, 3),
         ('s2', 'default', 'feishu', 'c1', 'meeting', 'm_1', '周会', 0, '{"start":1}', 'archived', NULL, 2, 4);
INSERT INTO space_members (workspace_id, space_id, account_id, role, joined)
  VALUES ('default', 's1', 'a2', 'member', 2);
INSERT INTO space_settings (space_id, workspace_id, disclosure, updated)
  VALUES ('s1', 'default', 'conclusion', 2);
INSERT INTO chan_messages (id, workspace_id, space_id, external_id, direction, text, created)
  VALUES ('m1', 'default', 's1', 'om_1', 'in', '你好', 5), ('m2', 'default', 's2', NULL, 'out', '纪要', 6);
INSERT INTO chan_cursors (connection_id, workspace_id, container, chat_id, created, updated)
  VALUES ('c1', 'default', 'oc_1', 'oc_1', 1, 1);
"#;

fn spaces(c: &Connection, migrated: bool) -> Vec<String> {
    let acc = if migrated {
        "coalesce(account_id, '-')"
    } else {
        "'-'"
    };
    let mut st = c
        .prepare(&format!(
            "SELECT id || '|' || coalesce(connection_id, '-') || '|' || kind || '|' || external_id || '|' || name || '|'
               || member_count || '|' || coalesce(meeting, '-') || '|' || status || '|' || coalesce(last_message, '-')
               || '|' || created || '|' || updated || '|' || {acc} FROM spaces ORDER BY id"
        ))
        .unwrap();
    st.query_map([], |r| r.get(0))
        .unwrap()
        .map(Result::unwrap)
        .collect()
}

fn fk_ok(c: &Connection) {
    assert_eq!(count(c, "SELECT count(*) FROM pragma_foreign_key_check"), 0);
    assert_eq!(
        c.query_row("PRAGMA integrity_check", [], |r| r.get::<_, String>(0))
            .unwrap(),
        "ok"
    );
}

/// `(from, to, on_delete)` of the foreign keys of `table`.
fn fks(c: &Connection, table: &str) -> Vec<(String, String, String)> {
    let mut st = c
        .prepare(&format!(
            "SELECT \"from\", \"table\", on_delete FROM pragma_foreign_key_list('{table}') ORDER BY \"from\""
        ))
        .unwrap();
    st.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
        .unwrap()
        .map(Result::unwrap)
        .collect()
}

fn connection_id_nullable(c: &Connection) -> bool {
    count(
        c,
        "SELECT \"notnull\" FROM pragma_table_info('spaces') WHERE name = 'connection_id'",
    ) == 0
}

#[test]
fn spaces_reference_connections_with_set_null() {
    let t = fresh();
    t.store
        .with_conn(|c| {
            assert_eq!(
                fks(c, "spaces"),
                vec![(
                    "connection_id".into(),
                    "connections".into(),
                    "SET NULL".into()
                )]
            );
            assert!(connection_id_nullable(c));
            assert_eq!(columns(c, "spaces").last().unwrap(), "account_id");
            // the children still point at `spaces`, not at the temporary name
            for child in ["space_members", "space_settings", "chan_messages"] {
                assert!(
                    fks(c, child).iter().any(|(_, to, _)| to == "spaces"),
                    "{child}"
                );
            }
            assert_eq!(
                count(
                    c,
                    "SELECT count(*) FROM sqlite_master WHERE sql LIKE '%spaces_new%' OR name = 'spaces_new'"
                ),
                0
            );
            assert_eq!(
                count(
                    c,
                    "SELECT count(*) FROM sqlite_master WHERE type = 'index' AND name = 'spaces_conn'"
                ),
                1
            );
            Ok(())
        })
        .unwrap();
}

#[test]
fn v3_database_migrates_with_foreign_keys_intact() {
    let (_dir, path) = at_version(3);
    let s = Store::open(&path, NoPublisher).unwrap();
    s.with_conn(|c| {
        assert_eq!(count(c, "SELECT v FROM schema_version"), db::SCHEMA_VERSION);
        assert_eq!(count(c, "SELECT count(*) FROM workspaces"), 1);
        assert!(connection_id_nullable(c));
        assert_eq!(
            c.query_row("PRAGMA foreign_keys", [], |r| r.get::<_, i64>(0))?,
            1
        );
        fk_ok(c);
        Ok(())
    })
    .unwrap();
    drop(s);
    assert_eq!(schema_of(&path), schema_of(fresh().store.path()));
}

#[test]
fn v11_database_with_channel_data_keeps_every_row() {
    let (_dir, path) = at_version(11);
    let c = Connection::open(&path).unwrap();
    c.execute_batch(CHANNEL_DATA).unwrap();
    let before = spaces(&c, false);
    drop(c);

    let s = Store::open(&path, NoPublisher).unwrap();
    s.with_conn(|c| {
        assert_eq!(count(c, "SELECT v FROM schema_version"), db::SCHEMA_VERSION);
        assert_eq!(
            spaces(c, true),
            before
        );
        assert_eq!(count(c, "SELECT count(*) FROM space_members"), 1);
        assert_eq!(count(c, "SELECT count(*) FROM space_settings"), 1);
        assert_eq!(count(c, "SELECT count(*) FROM chan_messages"), 2);
        assert_eq!(count(c, "SELECT count(*) FROM chan_cursors"), 1);
        assert_eq!(count(c, "SELECT count(*) FROM channel_settings"), 1);
        fk_ok(c);
        // the external-id uniqueness survived the rebuild
        assert!(
            c.execute(
                "INSERT INTO spaces (id, workspace_id, system_id, connection_id, kind, external_id, name, created, updated)
                 VALUES ('s3', 'default', 'feishu', 'c1', 'group', 'oc_1', 'x', 1, 1)",
                [],
            )
            .is_err()
        );

        // disconnecting: the channel module drops settings and cursors, the connection row goes,
        // the spaces and their messages stay with connection_id NULL
        c.execute_batch(
            "UPDATE spaces SET status = 'left', account_id = 'a1' WHERE connection_id = 'c1';
             DELETE FROM space_settings; DELETE FROM chan_cursors; DELETE FROM channel_settings;
             DELETE FROM connections WHERE id = 'c1';",
        )?;
        assert_eq!(
            count(
                c,
                "SELECT count(*) FROM spaces WHERE connection_id IS NULL AND status = 'left' AND account_id = 'a1'"
            ),
            2
        );
        assert_eq!(count(c, "SELECT count(*) FROM chan_messages"), 2);
        assert_eq!(count(c, "SELECT count(*) FROM space_members"), 1);
        fk_ok(c);
        Ok(())
    })
    .unwrap();
}

#[test]
fn a_migration_that_breaks_foreign_keys_is_rolled_back() {
    let (_dir, path) = at_version(11);
    let c = Connection::open(&path).unwrap();
    // an orphan that only foreign_keys = OFF lets in
    c.pragma_update(None, "foreign_keys", "OFF").unwrap();
    c.execute_batch(
        "INSERT INTO chan_messages (id, workspace_id, space_id, direction, created) VALUES ('m', 'default', 'nope', 'in', 1);",
    )
    .unwrap();
    drop(c);
    let err = Store::open(&path, NoPublisher).err().expect("refused");
    assert!(err.to_string().contains("chan_messages -> spaces"), "{err}");
    let c = Connection::open(&path).unwrap();
    assert_eq!(count(&c, "SELECT v FROM schema_version"), 11);
    assert!(!connection_id_nullable(&c));
}
