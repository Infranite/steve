use rusqlite::Connection;
use steve_store_sqlite::digest::{self, MismatchKind, Source};
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
fn explicit_membership_and_projection_match_all_nine_triggers() {
    let old = fixture(false);
    let new = fixture(true);
    for n in 0..14 {
        for (c, explicit) in [(&old, false), (&new, true)] {
            let tx = c.unchecked_transaction().unwrap();
            let old_a = Source::ChanMessage.scope(&tx, "a").unwrap();
            let old_b = Source::ChanMessage.scope(&tx, "b").unwrap();
            let old_mail = Source::MailMessage.scope(&tx, "a").unwrap();
            match n {
                0 => {
                    tx.execute(
                        "INSERT INTO chan_messages VALUES('a','w','one','received',10)",
                        [],
                    )
                    .unwrap();
                    if explicit {
                        digest::sync_message(&tx, Source::ChanMessage, "a", None).unwrap();
                    }
                }
                1 => {
                    tx.execute(
                        "INSERT INTO chan_messages VALUES('b','w','one','sent',20)",
                        [],
                    )
                    .unwrap();
                    if explicit {
                        digest::sync_message(&tx, Source::ChanMessage, "b", None).unwrap();
                    }
                }
                2 => {
                    tx.execute(
                        "INSERT INTO mail_messages VALUES('a','w','t','queued',5)",
                        [],
                    )
                    .unwrap();
                    if explicit {
                        digest::sync_message(&tx, Source::MailMessage, "a", None).unwrap();
                    }
                }
                3 => {
                    tx.execute("UPDATE mail_messages SET status='sent' WHERE id='a'", [])
                        .unwrap();
                    if explicit {
                        digest::sync_message(&tx, Source::MailMessage, "a", old_mail.as_ref())
                            .unwrap();
                    }
                }
                4 => {
                    tx.execute(
                        "UPDATE chan_messages SET status='received' WHERE id='a'",
                        [],
                    )
                    .unwrap();
                    if explicit {
                        digest::sync_message(&tx, Source::ChanMessage, "a", old_a.as_ref())
                            .unwrap();
                    }
                }
                5 => {
                    let scope = Source::ChanMessage.scope(&tx, "a").unwrap();
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
                        digest::sync_message(&tx, Source::ChanMessage, "a", old_a.as_ref())
                            .unwrap();
                    }
                }
                8 => {
                    tx.execute(
                        "UPDATE chan_messages SET status='recalled' WHERE id='b'",
                        [],
                    )
                    .unwrap();
                    if explicit {
                        digest::sync_message(&tx, Source::ChanMessage, "b", old_b.as_ref())
                            .unwrap();
                    }
                }
                9 => {
                    tx.execute(
                        "UPDATE mail_messages SET thread_id='t2',date=30 WHERE id='a'",
                        [],
                    )
                    .unwrap();
                    if explicit {
                        digest::sync_message(&tx, Source::MailMessage, "a", old_mail.as_ref())
                            .unwrap();
                    }
                }
                10 => {
                    tx.execute(
                        "INSERT INTO mail_messages VALUES('b','w','t2','received',50)",
                        [],
                    )
                    .unwrap();
                    if explicit {
                        digest::sync_message(&tx, Source::MailMessage, "b", None).unwrap();
                    }
                }
                11 => {
                    tx.execute("DELETE FROM mail_messages WHERE id='a'", [])
                        .unwrap();
                    if explicit {
                        digest::remove_message(&tx, [(old_mail.unwrap(), "a".into())]).unwrap();
                    }
                }
                12 => {
                    tx.execute("DELETE FROM chan_messages WHERE id='a'", [])
                        .unwrap();
                    if explicit {
                        digest::remove_message(&tx, [(old_a.unwrap(), "a".into())]).unwrap();
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
        // Explicit synchronization intentionally retains existing FIFO positions; compare the
        // membership set and projection, not 112's delete/reinsert position churn.
        let comparable = |c: &Connection| {
            let (pending, convs) = state(c);
            (
                pending
                    .into_iter()
                    .map(|(_, w, c, m, at)| (w, c, m, at))
                    .collect::<std::collections::BTreeSet<_>>(),
                convs,
            )
        };
        assert_eq!(comparable(&old), comparable(&new), "step {n}");
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
    digest::sync_message(&tx, Source::ChanMessage, "a", None).unwrap();
    let before = state(&tx);
    // Repeated synchronization also repairs only the derived conversation counts.
    tx.execute("UPDATE digest_pending_convs SET messages=99", [])
        .unwrap();
    digest::sync_message(&tx, Source::ChanMessage, "a", None).unwrap();
    assert_eq!(before, state(&tx));
    tx.execute("INSERT INTO digest_receipts VALUES('w','space:s','a')", [])
        .unwrap();
    digest::dequeue_on_receipt(&tx, "w", "space:s", "a").unwrap();
    digest::sync_message(&tx, Source::ChanMessage, "a", None).unwrap();
    digest::sync_message(&tx, Source::ChanMessage, "a", None).unwrap();
    assert!(state(&tx).0.is_empty());
    tx.execute(
        "INSERT INTO chan_messages VALUES('b','w','s','received',-10)",
        [],
    )
    .unwrap();
    digest::sync_message(&tx, Source::ChanMessage, "b", None).unwrap();
    assert!(state(&tx).0[0].0 > before.0[0].0);
    tx.commit().unwrap();
    audit(&c);
}
#[test]
fn caller_transaction_rolls_back_source_queue_and_projection_and_autocommit_is_rejected() {
    let c = fixture(true);
    assert!(digest::sync_message(&c, Source::ChanMessage, "a", None).is_err());
    {
        let tx = c.unchecked_transaction().unwrap();
        tx.execute(
            "INSERT INTO chan_messages VALUES('a','w','s','received',1)",
            [],
        )
        .unwrap();
        digest::sync_message(&tx, Source::ChanMessage, "a", None).unwrap();
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
    assert_eq!(
        digest::verify_pending(&tx, "w").unwrap()[0].kind,
        MismatchKind::Missing
    );
    digest::sync_message(&tx, Source::ChanMessage, "a", None).unwrap();
    tx.execute("UPDATE digest_pending SET at=9", []).unwrap();
    assert!(
        digest::verify_pending(&tx, "w")
            .unwrap()
            .iter()
            .any(|m| m.kind == MismatchKind::Timestamp)
    );
    tx.execute("UPDATE digest_pending_convs SET messages=9", [])
        .unwrap();
    assert!(
        digest::verify_pending(&tx, "w")
            .unwrap()
            .iter()
            .any(|m| m.kind == MismatchKind::Conversation)
    );
    let old_a = Source::ChanMessage.scope(&tx, "a").unwrap();
    tx.execute("DELETE FROM chan_messages WHERE id='a'", [])
        .unwrap();
    assert!(
        digest::verify_pending(&tx, "w")
            .unwrap()
            .iter()
            .any(|m| m.kind == MismatchKind::Unexpected)
    );
    digest::remove_message(&tx, [(old_a.unwrap(), "a".into())]).unwrap();
    tx.commit().unwrap();
    audit(&c);
}
#[test]
fn lookback_recounts_projection() {
    let c = fixture(true);
    let tx = c.unchecked_transaction().unwrap();
    tx.execute(
        "INSERT INTO chan_messages VALUES('a','w','s','received',1),('b','w','s','received',2)",
        [],
    )
    .unwrap();
    for id in ["a", "b"] {
        digest::sync_message(&tx, Source::ChanMessage, id, None).unwrap();
    }
    digest::remove_before(&tx, "w", "space:s", 2).unwrap();
    assert_eq!(state(&tx).1[0].2, 1);
    assert_eq!(state(&tx).1[0].3, Some(2));
    tx.execute("INSERT INTO digest_cursors VALUES('w','space:s',2)", [])
        .unwrap();
    tx.commit().unwrap();
    audit(&c);
}

#[test]
fn projection_updates_preserve_additive_columns_while_the_row_exists() {
    fn extra(c: &Connection, conv: &str) -> Option<String> {
        c.query_row(
            "SELECT extra FROM digest_pending_convs WHERE workspace_id='w' AND conv=?",
            [conv],
            |r| r.get(0),
        )
        .unwrap()
    }
    fn assert_preserved(c: &Connection, step: &str) {
        for conv in ["space:one", "space:two"] {
            assert_eq!(
                extra(c, conv),
                Some(format!("keep:{conv}")),
                "{step}: {conv}"
            );
        }
    }

    let c = fixture(true);
    c.execute_batch("ALTER TABLE digest_pending_convs ADD COLUMN extra TEXT")
        .unwrap();
    let tx = c.unchecked_transaction().unwrap();
    tx.execute(
        "INSERT INTO chan_messages VALUES
         ('hold-one','w','one','received',1),('hold-two','w','two','received',2)",
        [],
    )
    .unwrap();
    for id in ["hold-one", "hold-two"] {
        digest::sync_message(&tx, Source::ChanMessage, id, None).unwrap();
    }
    tx.execute("UPDATE digest_pending_convs SET extra='keep:' || conv", [])
        .unwrap();

    tx.execute(
        "INSERT INTO chan_messages VALUES('receipt','w','one','received',10)",
        [],
    )
    .unwrap();
    digest::sync_message(&tx, Source::ChanMessage, "receipt", None).unwrap();
    assert_preserved(&tx, "enqueue");
    digest::sync_message(&tx, Source::ChanMessage, "receipt", None).unwrap();
    assert_preserved(&tx, "duplicate enqueue");
    tx.execute(
        "INSERT INTO digest_receipts VALUES('w','space:one','receipt')",
        [],
    )
    .unwrap();
    digest::dequeue_on_receipt(&tx, "w", "space:one", "receipt").unwrap();
    assert_preserved(&tx, "receipt dequeue");

    tx.execute(
        "INSERT INTO chan_messages VALUES('moving','w','one','received',20)",
        [],
    )
    .unwrap();
    digest::sync_message(&tx, Source::ChanMessage, "moving", None).unwrap();
    assert_preserved(&tx, "enqueue before status changes");
    for status in ["pending", "sent"] {
        let old = Source::ChanMessage.scope(&tx, "moving").unwrap();
        let changed = tx
            .execute(
                "UPDATE chan_messages SET status=? WHERE id='moving'",
                [status],
            )
            .unwrap();
        assert_eq!(changed, 1);
        digest::sync_message(&tx, Source::ChanMessage, "moving", old.as_ref()).unwrap();
        assert_preserved(&tx, status);
    }
    let old_scope = Source::ChanMessage.scope(&tx, "moving").unwrap();
    tx.execute(
        "UPDATE chan_messages SET space_id='two',created=30 WHERE id='moving'",
        [],
    )
    .unwrap();
    digest::sync_message(&tx, Source::ChanMessage, "moving", old_scope.as_ref()).unwrap();
    assert_preserved(&tx, "conversation move");

    // The queue becoming empty intentionally deletes its projection row. Extra fields
    // survive while the row exists; retaining failure state beyond deletion is a separate policy.
    let old = Source::ChanMessage.scope(&tx, "hold-one").unwrap().unwrap();
    tx.execute("DELETE FROM chan_messages WHERE id='hold-one'", [])
        .unwrap();
    digest::remove_message(&tx, [(old, "hold-one".into())]).unwrap();
    assert_eq!(
        tx.query_row(
            "SELECT count(*) FROM digest_pending_convs WHERE workspace_id='w' AND conv='space:one'",
            [],
            |r| r.get::<_, i64>(0),
        )
        .unwrap(),
        0
    );
    assert_eq!(extra(&tx, "space:two"), Some("keep:space:two".into()));
    tx.commit().unwrap();
    assert_eq!(extra(&c, "space:two"), Some("keep:space:two".into()));
    audit(&c);
}

#[test]
fn sole_pending_message_preserves_fifo_and_extra_on_same_value_status_and_time_updates() {
    for source in [Source::ChanMessage, Source::MailMessage] {
        let c = fixture(true);
        c.execute_batch("ALTER TABLE digest_pending_convs ADD COLUMN extra TEXT")
            .unwrap();
        let tx = c.unchecked_transaction().unwrap();
        let (table, time) = match source {
            Source::ChanMessage => ("chan_messages", "created"),
            Source::MailMessage => ("mail_messages", "date"),
        };
        tx.execute(
            &format!("INSERT INTO {table} VALUES('only','w','s','received',10)"),
            [],
        )
        .unwrap();
        digest::sync_message(&tx, source, "only", None).unwrap();
        tx.execute("UPDATE digest_pending_convs SET extra='keep'", [])
            .unwrap();
        let position = state(&tx).0[0].0;
        for (status, at) in [("received", 10), ("sent", 10), ("sent", 30)] {
            let old = source.scope(&tx, "only").unwrap();
            tx.execute(
                &format!("UPDATE {table} SET status=?1,{time}=?2 WHERE id='only'"),
                rusqlite::params![status, at],
            )
            .unwrap();
            digest::sync_message(&tx, source, "only", old.as_ref()).unwrap();
            assert_eq!(state(&tx).0[0].0, position);
            assert_eq!(state(&tx).0[0].4, at);
            assert_eq!(
                tx.query_row("SELECT extra FROM digest_pending_convs", [], |r| r
                    .get::<_, String>(0))
                    .unwrap(),
                "keep"
            );
        }
        tx.commit().unwrap();
        audit(&c);
    }
}

#[test]
fn batch_deletion_aggregates_each_affected_conversation_once() {
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };
    let c = fixture(true);
    let tx = c.unchecked_transaction().unwrap();
    tx.execute("INSERT INTO chan_messages VALUES('a','w','one','received',1),('b','w','one','sent',2),('hold','w','one','sent',3),('c','w','two','received',4)",[]).unwrap();
    for id in ["a", "b", "hold", "c"] {
        digest::sync_message(&tx, Source::ChanMessage, id, None).unwrap();
    }
    let updates = Arc::new(AtomicUsize::new(0));
    let count = updates.clone();
    c.update_hook(Some(
        move |_: rusqlite::hooks::Action, _: &str, table: &str, _: i64| {
            if table == "digest_pending_convs" {
                count.fetch_add(1, Ordering::SeqCst);
            }
        },
    ))
    .unwrap();
    let messages = ["a", "b", "c"]
        .into_iter()
        .map(|id| {
            (
                Source::ChanMessage.scope(&tx, id).unwrap().unwrap(),
                id.to_owned(),
            )
        })
        .collect::<Vec<_>>();
    tx.execute("DELETE FROM chan_messages WHERE id IN ('a','b','c')", [])
        .unwrap();
    digest::remove_message(&tx, messages).unwrap();
    assert_eq!(
        updates.load(Ordering::SeqCst),
        2,
        "one projection update and one empty-conversation deletion"
    );
    updates.store(0, Ordering::SeqCst);
    digest::remove_before(&tx, "w", "space:one", 3).unwrap();
    assert_eq!(
        updates.load(Ordering::SeqCst),
        1,
        "lookback aggregates once"
    );
    tx.commit().unwrap();
    audit(&c);
}
