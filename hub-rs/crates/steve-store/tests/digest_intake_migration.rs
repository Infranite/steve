//! Upgrade preserves 112's initial lookback; 114 moves subsequent effects to explicit calls.
use rusqlite::Connection;
use steve_store_sqlite::digest::{self, Source};
const MIGRATION: &str = include_str!("../migrations/112.sql");
#[test]
fn upgrade_seeds_only_unconsumed_messages_and_keeps_the_old_lookback_once() {
    let c = Connection::open_in_memory().unwrap();
    c.execute_batch("CREATE TABLE connections(id TEXT PRIMARY KEY);
        CREATE TABLE chan_messages(id TEXT PRIMARY KEY,workspace_id TEXT,space_id TEXT,status TEXT,created INTEGER);
        CREATE TABLE mail_messages(id TEXT PRIMARY KEY,workspace_id TEXT,thread_id TEXT,status TEXT,date INTEGER);
        CREATE TABLE digest_cursors(workspace_id TEXT,conv TEXT,since INTEGER);
        CREATE TABLE digest_receipts(workspace_id TEXT,conv TEXT,message_id TEXT);
        INSERT INTO digest_cursors VALUES('ws','space:s',10);
        INSERT INTO chan_messages VALUES('old','ws','s','received',1),('done','ws','s','received',10),('waiting','ws','s','received',11),('out','ws','s','pending',12);
        INSERT INTO digest_receipts VALUES('ws','space:s','done');
        INSERT INTO mail_messages VALUES('mail','ws','t','received',1);").unwrap();
    c.execute_batch(MIGRATION).unwrap();
    assert_eq!(
        c.query_row(
            "SELECT messages,oldest,newest FROM digest_pending_convs WHERE conv='space:s'",
            [],
            |r| Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, i64>(1)?,
                r.get::<_, i64>(2)?
            ))
        )
        .unwrap(),
        (1, 11, 11)
    );
    c.execute_batch(include_str!("../migrations/113.sql"))
        .unwrap();
    c.execute_batch(include_str!("../migrations/114.sql"))
        .unwrap();
    assert_eq!(
        c.query_row(
            "SELECT count(*) FROM sqlite_schema WHERE name='chan_ext_ws_dead_letters'",
            [],
            |r| r.get::<_, i64>(0),
        )
        .unwrap(),
        1
    );
    assert_eq!(
        c.query_row(
            "SELECT count(*) FROM sqlite_schema WHERE type='trigger'",
            [],
            |r| r.get::<_, i64>(0)
        )
        .unwrap(),
        0
    );
    let tx = c.unchecked_transaction().unwrap();
    tx.execute(
        "INSERT OR IGNORE INTO chan_messages VALUES('late','ws','s','received',0)",
        [],
    )
    .unwrap();
    digest::sync_message(&tx, Source::ChanMessage, "late", None).unwrap();
    let old = Source::ChanMessage.scope(&tx, "out").unwrap();
    tx.execute("UPDATE chan_messages SET status='sent' WHERE id='out'", [])
        .unwrap();
    digest::sync_message(&tx, Source::ChanMessage, "out", old.as_ref()).unwrap();
    assert_eq!(
        tx.query_row(
            "SELECT messages FROM digest_pending_convs WHERE conv='space:s'",
            [],
            |r| r.get::<_, i64>(0)
        )
        .unwrap(),
        3
    );
    tx.execute(
        "INSERT INTO digest_receipts VALUES('ws','space:s','late')",
        [],
    )
    .unwrap();
    digest::dequeue_on_receipt(&tx, "ws", "space:s", ["late"]).unwrap();
    let old = Source::MailMessage.scope(&tx, "mail").unwrap();
    tx.execute(
        "UPDATE mail_messages SET status='pending' WHERE id='mail'",
        [],
    )
    .unwrap();
    digest::sync_message(&tx, Source::MailMessage, "mail", old.as_ref()).unwrap();
    let old = Source::MailMessage.scope(&tx, "mail").unwrap();
    tx.execute(
        "UPDATE mail_messages SET status='sent',date=-1 WHERE id='mail'",
        [],
    )
    .unwrap();
    digest::sync_message(&tx, Source::MailMessage, "mail", old.as_ref()).unwrap();
    tx.execute(
        "INSERT INTO mail_messages VALUES('mail-late','ws','t','received',-2)",
        [],
    )
    .unwrap();
    digest::sync_message(&tx, Source::MailMessage, "mail-late", None).unwrap();
    assert_eq!(
        tx.query_row(
            "SELECT messages FROM digest_pending_convs WHERE conv='mail_thread:t'",
            [],
            |r| r.get::<_, i64>(0)
        )
        .unwrap(),
        2
    );
    let old = Source::MailMessage.scope(&tx, "mail").unwrap().unwrap();
    tx.execute("DELETE FROM mail_messages WHERE id='mail'", [])
        .unwrap();
    digest::remove_message(&tx, [(old, "mail".into())]).unwrap();
    tx.execute(
        "INSERT INTO digest_receipts VALUES('ws','mail_thread:t','mail-late')",
        [],
    )
    .unwrap();
    digest::dequeue_on_receipt(&tx, "ws", "mail_thread:t", ["mail-late"]).unwrap();
    assert_eq!(
        tx.query_row(
            "SELECT count(*) FROM digest_pending_convs WHERE conv='mail_thread:t'",
            [],
            |r| r.get::<_, i64>(0)
        )
        .unwrap(),
        0
    );
    tx.commit().unwrap();
    assert!(digest::verify_pending(&c, "ws").unwrap().is_empty());
}
