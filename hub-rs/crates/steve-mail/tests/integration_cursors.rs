//! Cursor regressions through the existing TLS fake and the real sync/store job.
mod support;

use std::time::Duration;

use steve_mail::adapters::imap::MISSING_UID_ATTEMPTS;
use steve_mail::fake::fixtures;
use steve_mail::{AdapterError, FolderState, MailError, SyncReport};
use steve_modules::testkit::WS;
use support::*;

fn last_uid(h: &H, conn: &str) -> i64 {
    h.count("SELECT COALESCE(last_uid, 0) FROM mail_folders WHERE connection_id = ? AND ext_id = 'INBOX'", conn)
}

// Script unsolicited FETCH data between the size command's completion and the body command's
// completion. This uses the fake's existing response injection, including a genuine BODY[] NIL.
async fn body_gap(h: &H, conn: &str, first: bool) -> SyncReport {
    let (entered, release) = h
        .imap
        .pause_next(if first { "UID SEARCH" } else { "UID FETCH" });
    let (mail, conn) = (h.mail.clone(), conn.to_string());
    let sync = tokio::spawn(async move { mail.sync_now(WS, &conn).await });
    entered.await.unwrap();
    let (sizes, release_sizes) = h.imap.pause_next("UID FETCH");
    release.send(()).unwrap();
    sizes.await.unwrap();
    let mut reply = String::from("OK scripted sizes\r\n");
    for (i, raw) in fixtures::thread().into_iter().enumerate() {
        if i == 1 {
            reply.push_str("* 2 FETCH (UID 2 BODY[] NIL)\r\n");
        } else {
            reply.push_str(&format!(
                "* {} FETCH (UID {} BODY[] {{{}}}\r\n{})\r\n",
                i + 1,
                i + 1,
                raw.len(),
                String::from_utf8_lossy(raw)
            ));
        }
    }
    h.imap
        .fail_next("UID FETCH", reply.trim_end_matches("\r\n"));
    h.imap.fail_next("UID FETCH", "OK scripted bodies");
    release_sizes.send(()).unwrap();
    sync.await.unwrap().unwrap()
}

#[tokio::test(flavor = "multi_thread")]
async fn missing_middle_body_retries_before_advancing_even_on_first_sync() {
    let h = setup().await;
    for raw in fixtures::thread() {
        h.inject("INBOX", raw);
    }
    let conn = h.connect().await;
    let r = body_gap(&h, &conn, true).await;
    assert_eq!(r.fetched, 1);
    assert_eq!(
        last_uid(&h, &conn),
        1,
        "neither UID 3 nor UIDNEXT may cross UID 2"
    );
    assert_eq!(h.messages(&conn).len(), 1);
    assert_eq!(h.sync(&conn).await.fetched, 2);
    assert_eq!(last_uid(&h, &conn), 3);
    assert_eq!(h.messages(&conn).len(), 3);
    assert!(
        h.events.ingested().is_empty(),
        "resumed backfill stays historical"
    );
    assert_eq!(h.sync(&conn).await.fetched, 0);
}

#[tokio::test(flavor = "multi_thread")]
async fn missing_uid_has_a_diagnostic_and_a_persisted_retry_limit() {
    for deleted in [false, true] {
        let h = setup().await;
        for raw in fixtures::thread() {
            h.inject("INBOX", raw);
        }
        let conn = h.connect().await;
        body_gap(&h, &conn, true).await;
        if deleted {
            h.imap.remove(ADDR, "INBOX", 2).unwrap();
        }
        for attempt in 2..=MISSING_UID_ATTEMPTS {
            let r = if deleted {
                h.sync(&conn).await
            } else {
                body_gap(&h, &conn, false).await
            };
            if attempt < MISSING_UID_ATTEMPTS {
                assert_eq!(last_uid(&h, &conn), 1);
                assert!(r.skipped.is_empty());
            } else {
                assert_eq!(last_uid(&h, &conn), 3);
                assert_eq!(r.skipped.len(), 1);
                assert!(r.skipped[0].ends_with(":2"));
            }
        }
        assert_eq!(h.messages(&conn).len(), 2);
        assert_eq!(h.sync(&conn).await.fetched, 0);
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn failed_store_mid_batch_saves_only_the_successful_prefix_and_replays_once() {
    let h = setup().await;
    let conn = h.connect().await;
    h.sync(&conn).await;
    for i in 1..=53 {
        let raw = format!(
            "From: alice@example.com\r\nTo: me@example.com\r\nMessage-ID: <cursor-{i}@example.com>\r\nSubject: Cursor {i}\r\n\r\nMessage {i}\r\n"
        );
        h.inject("INBOX", raw.as_bytes());
    }
    h.env.store().with_conn(|c| c.execute_batch(
        "CREATE TRIGGER fail_cursor_message BEFORE INSERT ON mail_messages WHEN NEW.ext_id LIKE '%:52' BEGIN SELECT RAISE(FAIL, 'injected store failure'); END;"
    ).map_err(Into::into)).unwrap();
    assert!(h.mail.sync_now(WS, &conn).await.is_err());
    assert_eq!(last_uid(&h, &conn), 50, "sync.rs saves state even on error");
    h.env
        .store()
        .with_conn(|c| {
            c.execute_batch("DROP TRIGGER fail_cursor_message")
                .map_err(Into::into)
        })
        .unwrap();
    h.sync(&conn).await;
    assert_eq!(last_uid(&h, &conn), 53);
    assert_eq!(h.messages(&conn).len(), 53);
    h.deliver().await; // the dispatcher pumps at most 50 entries per pass
    assert_eq!(
        h.events.ingested().len(),
        53,
        "already stored mail emits no duplicate event"
    );
    assert_eq!(h.sync(&conn).await.fetched, 0);
}

fn inbox(h: &H, conn: &str) -> FolderState {
    h.env
        .store()
        .reader(|c| steve_mail::repo::folders(c, conn))
        .unwrap()
        .into_iter()
        .find(|f| f.ext_id == "INBOX")
        .unwrap()
}

fn assert_rejected(error: MailError) {
    assert!(
        matches!(error, MailError::Adapter(AdapterError::Rejected(ref message))
        if message == "邮件服务器拒绝了这个操作"),
        "{error:?}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn rejected_first_search_keeps_the_persisted_window_for_retry() {
    for status in ["NO", "BAD"] {
        let h = setup().await;
        for raw in &fixtures::thread()[..2] {
            h.inject("INBOX", raw);
        }
        h.inject_at("INBOX", fixtures::get("big5.eml").unwrap(), days_ago(45));
        let conn = h.connect().await;
        h.imap.fail_next(
            "UID SEARCH",
            &format!("{status} [UNAVAILABLE] private-server-detail"),
        );
        let start = h.imap.commands().len();
        assert_rejected(h.mail.sync_now(WS, &conn).await.unwrap_err());
        let saved = inbox(&h, &conn);
        assert_eq!(
            (saved.last_uid, saved.synced, saved.cursor, saved.modseq),
            (None, None, None, None),
            "{status}"
        );
        assert!(h.messages(&conn).is_empty(), "{status}");
        assert!(
            h.imap.commands()[start..]
                .iter()
                .all(|c| !c.contains(" UID FETCH ")),
            "no fallback or body fetch after {status}"
        );
        let report = h.sync(&conn).await;
        assert_eq!(report.fetched, 2, "{status}: only the date window");
        assert_eq!(
            last_uid(&h, &conn),
            3,
            "UIDNEXT applies only after the successful search"
        );
        assert_eq!(h.messages(&conn).len(), 2);
        assert!(
            h.events.ingested().is_empty(),
            "retried first window is still backfill"
        );
        assert_eq!(h.sync(&conn).await.fetched, 0, "no duplicate delivery");
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn successful_empty_search_can_complete_the_window() {
    let h = setup().await;
    h.inject_at("INBOX", fixtures::get("big5.eml").unwrap(), days_ago(45));
    let conn = h.connect().await;
    assert_eq!(h.sync(&conn).await.fetched, 0);
    assert_eq!(last_uid(&h, &conn), 1);
    assert!(inbox(&h, &conn).synced.is_some());
    assert!(h.messages(&conn).is_empty());
    h.inject("INBOX", fixtures::get("gbk.eml").unwrap());
    assert_eq!(h.sync(&conn).await.fetched, 1);
    assert_eq!(
        h.events.ingested().len(),
        1,
        "mail after a confirmed empty window is live"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn rejected_incremental_uid_fetch_preserves_the_saved_prefix() {
    for status in ["NO", "BAD"] {
        let h = setup().await;
        h.inject("INBOX", fixtures::get("gbk.eml").unwrap());
        let conn = h.connect().await;
        h.sync(&conn).await;
        let before = inbox(&h, &conn);
        h.inject("INBOX", fixtures::get("big5.eml").unwrap());
        h.imap.fail_next(
            "UID FETCH",
            &format!("{status} [UNAVAILABLE] private-server-detail"),
        );
        assert_rejected(h.mail.sync_now(WS, &conn).await.unwrap_err());
        assert_eq!(inbox(&h, &conn), before, "{status}");
        assert_eq!(h.sync(&conn).await.fetched, 1);
        assert_eq!(last_uid(&h, &conn), 2);
        assert_eq!(h.messages(&conn).len(), 2);
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn disconnected_search_is_transient_and_does_not_confirm_the_window() {
    let h = setup().await;
    h.inject("INBOX", fixtures::get("gbk.eml").unwrap());
    let conn = h.connect().await;
    let (entered, release) = h.imap.pause_next("UID SEARCH");
    let (mail, id) = (h.mail.clone(), conn.clone());
    let task = tokio::spawn(async move { mail.sync_now(WS, &id).await });
    tokio::time::timeout(Duration::from_secs(5), entered)
        .await
        .unwrap()
        .unwrap();
    h.imap.drop_connections();
    let _ = release.send(());
    let error = tokio::time::timeout(Duration::from_secs(5), task)
        .await
        .unwrap()
        .unwrap()
        .unwrap_err();
    assert!(
        matches!(error, MailError::Adapter(AdapterError::Transient(_))),
        "{error:?}"
    );
    let saved = inbox(&h, &conn);
    assert_eq!((saved.last_uid, saved.synced), (None, None));
    assert_eq!(h.sync(&conn).await.fetched, 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn message_id_search_rejection_is_not_reported_as_gone() {
    let h = setup().await;
    let raw = fixtures::get("gbk.eml").unwrap();
    let uid = h.inject("INBOX", raw);
    let conn = h.connect().await;
    h.sync(&conn).await;
    let message = h.messages(&conn).remove(0);
    let adapter = h.mail.adapter("imap").unwrap();
    let cx = h.conns.cx(WS, &conn).unwrap();
    let old_ext = format!("INBOX:0:{uid}");
    for status in ["NO", "BAD"] {
        h.imap.fail_next(
            "UID SEARCH",
            &format!("{status} [UNAVAILABLE] private-server-detail"),
        );
        let error = adapter
            .fetch_raw(&cx, &old_ext, message.message_id.as_deref())
            .await
            .unwrap_err();
        assert_eq!(
            error,
            AdapterError::Rejected("邮件服务器拒绝了这个操作".into())
        );
    }
    assert_eq!(
        adapter
            .fetch_raw(&cx, &old_ext, message.message_id.as_deref())
            .await
            .unwrap(),
        raw
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn partial_search_data_and_another_tags_ok_do_not_authorize_a_window() {
    let h = setup().await;
    h.inject("INBOX", fixtures::get("gbk.eml").unwrap());
    let conn = h.connect().await;
    // Data buffered after EXAMINE's completion precedes the SEARCH completion. The fake's
    // own SEARCH is rejected: neither a partial set nor a different tag's OK is authoritative.
    h.imap.fail_next(
        "EXAMINE",
        "OK scripted examine\r\nstale OK another command\r\n* SEARCH 1",
    );
    h.imap
        .fail_next("UID SEARCH", "NO [UNAVAILABLE] private-server-detail");
    let start = h.imap.commands().len();
    assert_rejected(h.mail.sync_now(WS, &conn).await.unwrap_err());
    assert!(
        h.imap.commands()[start..]
            .iter()
            .all(|c| !c.contains(" UID FETCH "))
    );
    let saved = inbox(&h, &conn);
    assert_eq!((saved.last_uid, saved.synced), (None, None));
    assert_eq!(h.sync(&conn).await.fetched, 1);
}
