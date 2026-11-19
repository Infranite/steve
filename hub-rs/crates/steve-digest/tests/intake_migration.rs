//! Upgrade and source-write contracts of the digest's insertion inbox.
use rusqlite::Connection;

const MIGRATION: &str = include_str!("../../steve-store/migrations/112.sql");

#[test]
fn upgrade_seeds_only_unconsumed_messages_and_keeps_the_old_lookback_once() {
    let c = Connection::open_in_memory().unwrap();
    c.execute_batch("CREATE TABLE chan_messages (id TEXT PRIMARY KEY, workspace_id TEXT, space_id TEXT, status TEXT, created INTEGER);
        CREATE TABLE mail_messages (id TEXT PRIMARY KEY, workspace_id TEXT, thread_id TEXT, status TEXT, date INTEGER);
        CREATE TABLE digest_cursors (workspace_id TEXT, conv TEXT, since INTEGER);
        CREATE TABLE digest_receipts (workspace_id TEXT, conv TEXT, message_id TEXT);
        INSERT INTO digest_cursors VALUES ('ws', 'space:s', 10);
        INSERT INTO chan_messages VALUES ('old', 'ws', 's', 'received', 1), ('done', 'ws', 's', 'received', 10),
            ('waiting', 'ws', 's', 'received', 11), ('out', 'ws', 's', 'pending', 12);
        INSERT INTO digest_receipts VALUES ('ws', 'space:s', 'done');
        INSERT INTO mail_messages VALUES ('mail', 'ws', 't', 'received', 1);").unwrap();
    c.execute_batch(MIGRATION).unwrap();
    let stats = || {
        c.query_row(
            "SELECT messages, oldest, newest FROM digest_pending_convs WHERE conv = 'space:s'",
            [],
            |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, i64>(1)?,
                    r.get::<_, i64>(2)?,
                ))
            },
        )
        .unwrap()
    };
    assert_eq!(stats(), (1, 11, 11));
    assert_eq!(
        c.query_row("SELECT count(*) FROM digest_pending", [], |r| r
            .get::<_, i64>(0))
            .unwrap(),
        2
    );
    c.execute_batch(
        "INSERT OR IGNORE INTO chan_messages VALUES ('late', 'ws', 's', 'received', 0);
        UPDATE chan_messages SET status = 'sent' WHERE id = 'out';",
    )
    .unwrap();
    assert_eq!(stats(), (3, 0, 12));
    c.execute_batch("INSERT INTO digest_receipts VALUES ('ws', 'space:s', 'late');")
        .unwrap();
    assert_eq!(stats(), (2, 11, 12));
    // Mail uses provider date, not insertion time; status changes and deletion update its metadata too.
    c.execute_batch(
        "UPDATE mail_messages SET status = 'pending' WHERE id = 'mail';
        UPDATE mail_messages SET status = 'sent', date = -1 WHERE id = 'mail';
        INSERT INTO mail_messages VALUES ('mail-late', 'ws', 't', 'received', -2);",
    )
    .unwrap();
    assert_eq!(
        c.query_row(
            "SELECT messages FROM digest_pending_convs WHERE conv = 'mail_thread:t'",
            [],
            |r| r.get::<_, i64>(0)
        )
        .unwrap(),
        2
    );
    c.execute_batch(
        "DELETE FROM mail_messages WHERE id = 'mail';
        INSERT INTO digest_receipts VALUES ('ws', 'mail_thread:t', 'mail-late');",
    )
    .unwrap();
    assert_eq!(
        c.query_row(
            "SELECT count(*) FROM digest_pending_convs WHERE conv = 'mail_thread:t'",
            [],
            |r| r.get::<_, i64>(0)
        )
        .unwrap(),
        0
    );
}
