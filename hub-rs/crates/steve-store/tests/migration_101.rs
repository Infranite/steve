//! 101: a starred contact's message as a personal notification. `personal_notifications.kind`
//! (`completion` for what was there) and `message_id`, `notification_policies.starred_in_quiet`, and
//! `feed_entries` rebuilt to allow the `starred` kind: entries, reads and the sequence survive, the
//! insert trigger names the kind, and 098's attention index keeps following the feed.

use rusqlite::Connection;
use steve_store::db;

fn one<T: rusqlite::types::FromSql>(c: &Connection, sql: &str) -> T {
    c.query_row(sql, [], |r| r.get(0)).unwrap()
}

#[test]
fn applies_on_an_099_database_and_keeps_the_feed() {
    let c = Connection::open_in_memory().unwrap();
    c.execute_batch(
        "CREATE TABLE schema_version(v INTEGER NOT NULL); INSERT INTO schema_version VALUES(100);",
    )
    .unwrap();
    for m in &db::MIGRATIONS[..100] {
        c.execute_batch(m).unwrap();
    }
    // before 101: a completion notification (its feed entry by the trigger), a report, a read, and a
    // newest entry removed again (its number must not come back)
    c.execute_batch(
        "INSERT OR IGNORE INTO workspaces(id, name, created) VALUES ('default', 'Default', 1);
         INSERT INTO users(id, workspace_id, subject, name, role, created) VALUES ('alice', 'default', 'alice', 'A', 'member', 1);
         INSERT INTO workspace_members(workspace_id, user_id, role, created) VALUES ('default', 'alice', 'member', 1);
         INSERT INTO personal_notifications(id, workspace_id, user_id, event_id, delivery_key, mode, body, state, created)
           VALUES ('ntf_1', 'default', 'alice', 'evt_1', 'notification:evt_1:alice', 'log', 'Work item completed: X', 'held', 10);
         INSERT INTO feed_entries(id, workspace_id, audience, kind, source_type, source_key, source_id, run_id, node_id, content, result, published_at)
           VALUES ('feed_r', 'default', 'workspace', 'report', 'flow', 'k1', 'flow_1', 'run_1', 'n1', '{}', '{}', 11);
         INSERT INTO feed_reads(workspace_id, user_id, entry_id, read_at) VALUES ('default', 'alice', 'feed_ntf_1', 12);
         INSERT INTO feed_entries(id, workspace_id, audience, kind, source_type, source_key, source_id, run_id, node_id, content, result, published_at)
           VALUES ('feed_gone', 'default', 'workspace', 'report', 'flow', 'k2', 'flow_1', 'run_1', 'n2', '{}', '{}', 13);
         DELETE FROM feed_entries WHERE id = 'feed_gone';",
    )
    .unwrap();
    let before: Vec<(i64, String, String)> = {
        let mut st = c
            .prepare("SELECT seq, id, kind FROM feed_entries ORDER BY seq")
            .unwrap();
        st.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap()
    };
    assert_eq!(before.len(), 2);
    let unread = |c: &Connection| -> Vec<String> {
        let mut st = c
            .prepare(
                "SELECT entry_id FROM attention_feed_unread WHERE user_id = 'alice' ORDER BY seq",
            )
            .unwrap();
        st.query_map([], |r| r.get(0))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap()
    };
    // 098's unread index: the report is unread, the read completion is not
    assert_eq!(unread(&c), vec!["feed_r".to_string()]);
    db::migrate(&c).unwrap();
    db::migrate(&c).unwrap();
    assert_eq!(db::compat(&c).unwrap().schema, db::SCHEMA_VERSION);

    let after: Vec<(i64, String, String)> = {
        let mut st = c
            .prepare("SELECT seq, id, kind FROM feed_entries ORDER BY seq")
            .unwrap();
        st.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap()
    };
    assert_eq!(after, before);
    assert_eq!(one::<i64>(&c, "SELECT count(*) FROM feed_reads"), 1);
    assert_eq!(
        one::<i64>(&c, "SELECT count(*) FROM pragma_foreign_key_check"),
        0
    );
    assert_eq!(
        one::<String>(
            &c,
            "SELECT kind FROM personal_notifications WHERE id = 'ntf_1'"
        ),
        "completion"
    );
    assert_eq!(
        one::<i64>(
            &c,
            "SELECT starred_in_quiet FROM notification_policies UNION ALL SELECT 0 LIMIT 1"
        ),
        0
    );

    // a starred contact's notification: its own kind in the feed, after the removed number
    c.execute_batch(
        "INSERT INTO personal_notifications(id, workspace_id, user_id, event_id, delivery_key, mode, body, state, kind, message_id, created)
           VALUES ('ntf_2', 'default', 'alice', 'cm_1', 'notification:cm_1:alice', 'now', 'A 私聊你', 'waiting', 'starred', 'cm_1', 20);",
    )
    .unwrap();
    let (seq, kind): (i64, String) = c
        .query_row(
            "SELECT seq, kind FROM feed_entries WHERE notification_id = 'ntf_2'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(kind, "starred");
    // the rebuilt table still feeds 098's unread index (its triggers on `feed_entries` are back)
    assert_eq!(
        unread(&c),
        vec!["feed_r".to_string(), "feed_ntf_2".to_string()]
    );
    c.execute_batch(
        "INSERT INTO feed_reads(workspace_id, user_id, entry_id, read_at) VALUES ('default', 'alice', 'feed_ntf_2', 21);",
    )
    .unwrap();
    assert_eq!(unread(&c), vec!["feed_r".to_string()]);
    assert_eq!(
        one::<i64>(
            &c,
            "SELECT unread_count FROM attention_feed_scopes WHERE user_id = 'alice'"
        ),
        1
    );
    assert!(seq > before.last().unwrap().0 + 1, "{seq}");
    // no other kind
    assert!(
        c.execute(
            "UPDATE personal_notifications SET kind = 'other' WHERE id = 'ntf_2'",
            []
        )
        .is_err()
    );
    assert!(
        c.execute(
            "UPDATE feed_entries SET kind = 'starred' WHERE id = 'feed_r'",
            []
        )
        .is_err()
    );
    // an entry removed after the rebuild leaves the unread index too (its foreign key follows the new table)
    c.execute("PRAGMA foreign_keys = ON", []).unwrap();
    c.execute("DELETE FROM feed_entries WHERE id = 'feed_r'", [])
        .unwrap();
    assert!(unread(&c).is_empty());
}
