//! Y1: a stored mail's event goes to the outbox in the transaction that stores the mail, so a crash
//! before it reaches the signals pipeline loses nothing, and a failed delivery is tried again.
//! Only the in-process fake IMAP/SMTP.

mod support;

use std::sync::Arc;

use steve_mail::fake::fixtures;
use steve_mail::outbox::MailEventsOut;
use steve_modules::testkit::WS;
use steve_store::outbox::Dispatcher;
use support::*;

fn pending(h: &H) -> i64 {
    h.env
        .store()
        .reader(|c| {
            Ok::<i64, steve_store::StoreError>(c.query_row(
                "SELECT COUNT(*) FROM outbox_events e WHERE e.topic = 'mail.event.ready'
                  AND NOT EXISTS (SELECT 1 FROM outbox_receipts r WHERE r.event_seq = e.seq)",
                [],
                |r| r.get(0),
            )?)
        })
        .unwrap()
}

fn linked(h: &H, conn: &str) -> i64 {
    h.count(
        "SELECT COUNT(*) FROM mail_messages WHERE connection_id = ? AND event_id IS NOT NULL",
        conn,
    )
}

/// The mail and its event commit together; nothing is delivered until a dispatcher runs. Another
/// dispatcher (the Hub after a restart) delivers it once, and the mail links to its event.
#[tokio::test(flavor = "multi_thread")]
async fn a_stored_mail_s_event_survives_until_a_dispatcher_delivers_it_once() {
    let h = setup().await;
    let conn = h.connect().await;
    h.sync(&conn).await; // the first sync records what is there
    let before = h.events.calls().len();
    h.inject("INBOX", fixtures::get("multipart-alternative.eml").unwrap());
    h.mail.sync_now(WS, &conn).await.expect("sync"); // no delivery: the "crash"
    assert_eq!(h.events.calls().len(), before, "nothing delivered yet");
    assert_eq!(
        pending(&h),
        1,
        "but the event is in the outbox with the mail"
    );

    let restarted = Arc::new(Dispatcher::new(vec![MailEventsOut::new(h.mail.clone())]));
    restarted.register(h.env.store()).unwrap();
    let store = h.env.store().clone();
    let d = restarted.clone();
    let p = tokio::task::spawn_blocking(move || d.pump(&store, steve_store::now()).unwrap())
        .await
        .unwrap();
    assert_eq!(p.done, 1);
    assert_eq!(h.events.ingested().len(), before + 1);
    assert_eq!(pending(&h), 0);
    assert_eq!(linked(&h, &conn), h.events.calls().len() as i64);
    h.deliver().await;
    assert_eq!(h.events.ingested().len(), before + 1, "once");
}

/// The signals pipeline refuses a delivery: it stays owed and goes through on a later pass.
#[tokio::test(flavor = "multi_thread")]
async fn a_failed_delivery_is_tried_again() {
    let h = setup().await;
    let conn = h.connect().await;
    h.sync(&conn).await;
    let before = h.events.calls().len();
    h.inject("INBOX", fixtures::get("multipart-alternative.eml").unwrap());
    h.events.fail_next("flow 暂时不可用");
    h.sync(&conn).await;
    assert_eq!(h.events.calls().len(), before, "refused");
    assert_eq!(pending(&h), 1);
    // after the back-off
    let (d, store) = (h.dispatcher.clone(), h.env.store().clone());
    let later = steve_store::now() + steve_store::outbox::BACKOFF_MAX_MS;
    tokio::task::spawn_blocking(move || d.pump(&store, later).unwrap())
        .await
        .unwrap();
    assert_eq!(h.events.calls().len(), before + 1);
    assert_eq!(pending(&h), 0);
}
