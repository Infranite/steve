//! Current explicit intake preserves an existing lookback and receipt boundary.
#[path = "../../steve-store-sqlite/tests/support/digest.rs"]
mod fixture;
use rusqlite::Connection;
use steve_store_sqlite::digest::{self, Source};
#[test]
fn explicit_intake_keeps_existing_membership_and_receipts() {
    let c = Connection::open_in_memory().unwrap();
    fixture::install(&c).unwrap();
    c.execute("INSERT INTO digest_cursors(workspace_id,conv,at,since,updated) VALUES('ws','space:s',10,10,0)", []).unwrap();
    for (id, status, at) in [
        ("old", "received", 1),
        ("done", "received", 10),
        ("waiting", "received", 11),
        ("out", "pending", 12),
    ] {
        fixture::put(
            &c,
            Source::ChanMessage,
            id,
            "ws",
            "space:s",
            status,
            at,
            true,
        )
        .unwrap();
    }
    fixture::receipt(&c, "ws", "space:s", "done").unwrap();
    fixture::put(
        &c,
        Source::MailMessage,
        "mail",
        "ws",
        "mail_thread:t",
        "received",
        1,
        true,
    )
    .unwrap();
    // Represent existing current membership directly; historical 112 replay is no longer supported.
    let tx = c.unchecked_transaction().unwrap();
    digest::sync_message(&tx, Source::ChanMessage, "waiting", None).unwrap();
    digest::sync_message(&tx, Source::MailMessage, "mail", None).unwrap();
    tx.commit().unwrap();
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
    fixture::put(
        &tx,
        Source::ChanMessage,
        "late",
        "ws",
        "space:s",
        "received",
        0,
        true,
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
    fixture::receipt(&tx, "ws", "space:s", "late").unwrap();
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
    fixture::put(
        &tx,
        Source::MailMessage,
        "mail-late",
        "ws",
        "mail_thread:t",
        "received",
        -2,
        true,
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
    fixture::receipt(&tx, "ws", "mail_thread:t", "mail-late").unwrap();
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
