use rusqlite::Connection;
use steve_store_sqlite::digest::{self, Source};
fn fixture(explicit: bool) -> Connection {
    let c = Connection::open_in_memory().unwrap();
    c.execute_batch("CREATE TABLE connections(id TEXT PRIMARY KEY);
        CREATE TABLE chan_messages(id TEXT PRIMARY KEY,workspace_id TEXT,space_id TEXT,status TEXT,created INTEGER);
        CREATE TABLE mail_messages(id TEXT PRIMARY KEY,workspace_id TEXT,thread_id TEXT,status TEXT,date INTEGER);
        CREATE TABLE digest_receipts(workspace_id TEXT,conv TEXT,message_id TEXT,PRIMARY KEY(workspace_id,conv,message_id));
        CREATE TABLE digest_cursors(workspace_id TEXT,conv TEXT,since INTEGER);").unwrap();
    c.execute_batch(include_str!("../legacy/migrations/112.sql"))
        .unwrap();
    c.execute_batch(include_str!("../legacy/migrations/113.sql"))
        .unwrap();
    if explicit {
        c.execute_batch(include_str!("../legacy/migrations/114.sql"))
            .unwrap();
    }
    c
}
type Inbox = Vec<(i64, String, String, String, i64)>;
type Conversations = Vec<(String, String, i64, Option<i64>, Option<i64>)>;
fn state(c: &Connection) -> (Inbox, Conversations) {
    let pending = c
        .prepare(
            "SELECT position,workspace_id,conv,message_id,at FROM digest_pending ORDER BY position",
        )
        .unwrap()
        .query_map([], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))
        })
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap();
    let convs=c.prepare("SELECT workspace_id,conv,messages,oldest,newest FROM digest_pending_convs ORDER BY workspace_id,conv").unwrap().query_map([],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?))).unwrap().collect::<rusqlite::Result<_>>().unwrap();
    (pending, convs)
}
fn audit(c: &Connection) {
    for ws in ["w", "other"] {
        assert_eq!(digest::verify_pending(c, ws).unwrap(), vec![]);
    }
}
#[test]
fn explicit_calls_match_all_nine_triggers_including_update_of_same_values() {
    let old = fixture(false);
    let new = fixture(true);
    for n in 0..14 {
        for (c, explicit) in [(&old, false), (&new, true)] {
            let tx = c.unchecked_transaction().unwrap();
            match n {
                0 => {
                    tx.execute(
                        "INSERT INTO chan_messages VALUES('a','w','one','received',10)",
                        [],
                    )
                    .unwrap();
                    if explicit {
                        digest::enqueue_digestible(&tx, Source::ChanMessage, "a").unwrap();
                    }
                }
                1 => {
                    tx.execute(
                        "INSERT INTO chan_messages VALUES('b','w','one','sent',20)",
                        [],
                    )
                    .unwrap();
                    if explicit {
                        digest::enqueue_digestible(&tx, Source::ChanMessage, "b").unwrap();
                    }
                }
                2 => {
                    tx.execute(
                        "INSERT INTO mail_messages VALUES('a','w','t','queued',5)",
                        [],
                    )
                    .unwrap();
                    if explicit {
                        digest::enqueue_digestible(&tx, Source::MailMessage, "a").unwrap();
                    }
                }
                3 => {
                    tx.execute("UPDATE mail_messages SET status='sent' WHERE id='a'", [])
                        .unwrap();
                    if explicit {
                        digest::sync_message(&tx, Source::MailMessage, "a", None).unwrap();
                    }
                }
                4 => {
                    tx.execute(
                        "UPDATE chan_messages SET status='received' WHERE id='a'",
                        [],
                    )
                    .unwrap();
                    if explicit {
                        digest::sync_message(&tx, Source::ChanMessage, "a", None).unwrap();
                    }
                }
                5 => {
                    let scope = digest::message_scope(&tx, Source::ChanMessage, "a").unwrap();
                    tx.execute("UPDATE chan_messages SET workspace_id='other',space_id='two',created=-1 WHERE id='a'",[]).unwrap();
                    if explicit {
                        digest::sync_message(&tx, Source::ChanMessage, "a", scope.as_ref())
                            .unwrap();
                    }
                }
                6 => {
                    tx.execute(
                        "INSERT INTO digest_receipts VALUES('other','space:two','a')",
                        [],
                    )
                    .unwrap();
                    if explicit {
                        digest::dequeue_on_receipt(&tx, "other", "space:two", "a").unwrap();
                    }
                }
                7 => {
                    tx.execute("UPDATE chan_messages SET status='sent' WHERE id='a'", [])
                        .unwrap();
                    if explicit {
                        digest::sync_message(&tx, Source::ChanMessage, "a", None).unwrap();
                    }
                }
                8 => {
                    tx.execute(
                        "UPDATE chan_messages SET status='recalled' WHERE id='b'",
                        [],
                    )
                    .unwrap();
                    if explicit {
                        digest::sync_message(&tx, Source::ChanMessage, "b", None).unwrap();
                    }
                }
                9 => {
                    tx.execute(
                        "UPDATE mail_messages SET thread_id='t2',date=30 WHERE id='a'",
                        [],
                    )
                    .unwrap();
                    if explicit {
                        digest::sync_message(&tx, Source::MailMessage, "a", None).unwrap();
                    }
                }
                10 => {
                    tx.execute(
                        "INSERT INTO mail_messages VALUES('b','w','t2','received',50)",
                        [],
                    )
                    .unwrap();
                    if explicit {
                        digest::enqueue_digestible(&tx, Source::MailMessage, "b").unwrap();
                    }
                }
                11 => {
                    tx.execute("DELETE FROM mail_messages WHERE id='a'", [])
                        .unwrap();
                    if explicit {
                        digest::remove_message(&tx, Source::MailMessage, "a").unwrap();
                    }
                }
                12 => {
                    tx.execute("DELETE FROM chan_messages WHERE id='a'", [])
                        .unwrap();
                    if explicit {
                        digest::remove_message(&tx, Source::ChanMessage, "a").unwrap();
                    }
                }
                13 => {
                    tx.execute(
                        "INSERT INTO digest_receipts VALUES('w','mail_thread:t2','b')",
                        [],
                    )
                    .unwrap();
                    if explicit {
                        digest::dequeue_on_receipt(&tx, "w", "mail_thread:t2", "b").unwrap();
                    }
                }
                _ => unreachable!(),
            }
            tx.commit().unwrap();
        }
        assert_eq!(state(&old), state(&new), "step {n}");
        audit(&new);
    }
}
#[test]
fn duplicate_intake_receipt_and_conversation_sync_do_not_reuse_fifo_or_acknowledgements() {
    let c = fixture(true);
    let tx = c.unchecked_transaction().unwrap();
    tx.execute(
        "INSERT INTO chan_messages VALUES('a','w','s','received',10)",
        [],
    )
    .unwrap();
    digest::enqueue_digestible(&tx, Source::ChanMessage, "a").unwrap();
    let before = state(&tx);
    digest::enqueue_digestible(&tx, Source::ChanMessage, "a").unwrap();
    assert_eq!(before, state(&tx));
    tx.execute("INSERT INTO digest_receipts VALUES('w','space:s','a')", [])
        .unwrap();
    digest::dequeue_on_receipt(&tx, "w", "space:s", "a").unwrap();
    digest::enqueue_digestible(&tx, Source::ChanMessage, "a").unwrap();
    digest::sync_pending_conversation(&tx, "w", "space:s").unwrap();
    assert!(state(&tx).0.is_empty());
    tx.execute(
        "INSERT INTO chan_messages VALUES('b','w','s','received',-10)",
        [],
    )
    .unwrap();
    digest::enqueue_digestible(&tx, Source::ChanMessage, "b").unwrap();
    assert!(state(&tx).0[0].0 > before.0[0].0);
    tx.commit().unwrap();
    audit(&c);
}
#[test]
fn caller_transaction_rolls_back_source_queue_and_projection_and_autocommit_is_rejected() {
    let c = fixture(true);
    assert!(digest::enqueue_digestible(&c, Source::ChanMessage, "a").is_err());
    {
        let tx = c.unchecked_transaction().unwrap();
        tx.execute(
            "INSERT INTO chan_messages VALUES('a','w','s','received',1)",
            [],
        )
        .unwrap();
        digest::enqueue_digestible(&tx, Source::ChanMessage, "a").unwrap();
        assert_eq!(state(&tx).0.len(), 1);
    }
    assert!(state(&c).0.is_empty());
    audit(&c);
}
#[test]
fn verifier_detects_missing_stale_and_wrong_projection_without_repairing() {
    let c = fixture(true);
    let tx = c.unchecked_transaction().unwrap();
    tx.execute(
        "INSERT INTO chan_messages VALUES('a','w','s','received',1)",
        [],
    )
    .unwrap();
    assert_eq!(digest::verify_pending(&tx, "w").unwrap()[0].kind, "missing");
    digest::enqueue_digestible(&tx, Source::ChanMessage, "a").unwrap();
    tx.execute("UPDATE digest_pending SET at=9", []).unwrap();
    assert!(
        digest::verify_pending(&tx, "w")
            .unwrap()
            .iter()
            .any(|m| m.kind == "timestamp")
    );
    tx.execute("UPDATE digest_pending_convs SET messages=9", [])
        .unwrap();
    assert!(
        digest::verify_pending(&tx, "w")
            .unwrap()
            .iter()
            .any(|m| m.kind == "conversation")
    );
    tx.execute("DELETE FROM chan_messages WHERE id='a'", [])
        .unwrap();
    assert!(
        digest::verify_pending(&tx, "w")
            .unwrap()
            .iter()
            .any(|m| m.kind == "unexpected")
    );
    digest::remove_message(&tx, Source::ChanMessage, "a").unwrap();
    tx.commit().unwrap();
    audit(&c);
}
#[test]
fn zero_match_status_update_does_not_change_position_and_lookback_recounts() {
    let c = fixture(true);
    let tx = c.unchecked_transaction().unwrap();
    tx.execute(
        "INSERT INTO chan_messages VALUES('a','w','s','received',1),('b','w','s','received',2)",
        [],
    )
    .unwrap();
    for id in ["a", "b"] {
        digest::enqueue_digestible(&tx, Source::ChanMessage, id).unwrap();
    }
    let before = state(&tx);
    let n = tx
        .execute(
            "UPDATE chan_messages SET status='sent' WHERE id='a' AND status='draft'",
            [],
        )
        .unwrap();
    digest::status_changed(&tx, Source::ChanMessage, "a", n).unwrap();
    assert_eq!(state(&tx), before);
    digest::remove_before(&tx, "w", "space:s", 2).unwrap();
    assert_eq!(state(&tx).1[0].2, 1);
    assert_eq!(state(&tx).1[0].3, Some(2));
    tx.execute("INSERT INTO digest_cursors VALUES('w','space:s',2)", [])
        .unwrap();
    tx.commit().unwrap();
    audit(&c);
}
