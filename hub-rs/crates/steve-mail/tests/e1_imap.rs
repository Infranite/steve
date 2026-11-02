//! The IMAP adapter against the fake servers: verification errors, STARTTLS, 163's `ID`, IDLE wakes
//! the runner, reconnect after a dropped connection, flags with and without CONDSTORE, very large
//! mail, Gmail threads, archive/read, refetch, sending with APPEND.

mod support;

use std::time::Duration;

use steve_mail::fake::{ImapConfig, TlsMode, fixtures};
use steve_mail::{FlagChange, SendMeta};
use steve_modules::ApiError;
use steve_modules::testkit::WS;
use steve_modules::vault::VaultError;
use support::*;

#[tokio::test(flavor = "multi_thread")]
async fn verify_failures_are_400_in_chinese() {
    let h = setup().await;
    let (f, config) = h.fields("wrong-password");
    let e = h.conns.create(&me(), "imap", &f, config).await.unwrap_err();
    match e {
        VaultError::Api(ApiError::Rule(m)) => {
            assert_eq!(m, "验证没有通过：用户名或应用专用密码不对");
            assert!(!m.contains("wrong-password"));
        }
        e => panic!("{e:?}"),
    }
    // nothing was created, the credential is gone
    assert_eq!(
        h.count("SELECT COUNT(*) FROM connections WHERE adapter = ?", "imap"),
        0
    );

    // a server that offers plain text only is refused before any password is sent
    let plain = steve_mail::fake::FakeImap::start(ImapConfig {
        tls: TlsMode::Plain,
        ..Default::default()
    })
    .await
    .unwrap();
    let (mut f, config) = h.fields(PASS);
    f.insert("imap_port".into(), plain.port().to_string().into());
    let e = h.conns.create(&me(), "imap", &f, config).await.unwrap_err();
    assert!(e.to_string().starts_with("验证没有通过："), "{e}");
    assert!(plain.commands().iter().all(|c| !c.contains("LOGIN")));
}

#[tokio::test(flavor = "multi_thread")]
async fn starttls_and_163_id() {
    let h = setup_with(Opts {
        imap: ImapConfig {
            tls: TlsMode::StartTls,
            require_id: true,
            ..Default::default()
        },
        smtp_tls: TlsMode::StartTls,
        ..Default::default()
    })
    .await;
    h.inject("INBOX", fixtures::get("gbk.eml").unwrap());
    let conn = h.connect().await;
    let r = h.sync(&conn).await;
    assert_eq!(r.fetched, 1);
    let cmds = h.imap.commands();
    let id = cmds.iter().position(|c| c.contains(" ID ")).unwrap();
    let ex = cmds.iter().position(|c| c.contains("EXAMINE")).unwrap();
    assert!(id < ex, "ID before EXAMINE: {cmds:?}");
}

fn inbox_synced(h: &H, conn: &str) -> bool {
    h.count("SELECT COUNT(*) FROM mail_folders WHERE connection_id = ? AND ext_id = 'INBOX' AND synced IS NOT NULL AND last_uid IS NOT NULL", conn) == 1
}

#[tokio::test(flavor = "multi_thread")]
async fn idle_wakes_the_runner_and_it_reconnects() {
    let h = setup().await;
    let conn = h.connect().await;
    h.mail.start(WS, &conn);
    // Watch and initial sync run independently. Login/IDLE only prove the watcher is ready:
    // injecting before the first sync commits makes the message historical backfill, not Ingest.
    assert!(
        h.until(5, |h| inbox_synced(h, &conn)).await,
        "initial inbox cursor must be committed before testing live IDLE ingestion"
    );
    // let the watcher enter IDLE
    assert!(
        h.until(5, |h| h.imap.commands().iter().any(|c| c.ends_with("IDLE")))
            .await
    );
    h.inject("INBOX", fixtures::get("multipart-alternative.eml").unwrap());
    assert!(
        h.until(5, |h| h.events.ingested().len() == 1).await,
        "IDLE → EXISTS → sync, well before the 2-minute poll"
    );

    // the server drops every connection: the watcher backs off (5 s) and reconnects
    h.imap.drop_connections();
    tokio::time::sleep(Duration::from_millis(200)).await;
    h.inject("INBOX", fixtures::get("gbk.eml").unwrap());
    assert!(
        h.until(12, |h| h.events.ingested().len() == 2).await,
        "after the reconnect the missed mail arrives"
    );
    h.mail.stop(&conn);
}

/// Force the exact ordering that made the old login-count readiness check flaky on Linux.
#[tokio::test(flavor = "multi_thread")]
async fn idle_can_be_ready_while_initial_sync_is_still_backfilling() {
    let h = setup().await;
    let conn = h.connect().await;
    let (search_entered, release_search) = h.imap.pause_next("UID SEARCH");
    h.mail.start(WS, &conn);
    tokio::time::timeout(Duration::from_secs(5), search_entered)
        .await
        .unwrap()
        .unwrap();
    assert!(
        h.until(5, |h| h.imap.commands().iter().any(|c| c.ends_with("IDLE")))
            .await
    );
    assert!(
        h.imap.logins() >= 3,
        "the previous readiness heuristic is satisfied"
    );
    assert!(
        !inbox_synced(&h, &conn),
        "but the initial cursor has not committed"
    );
    h.inject("INBOX", fixtures::get("multipart-alternative.eml").unwrap());
    release_search.send(()).unwrap();
    assert!(
        h.until(5, |h| inbox_synced(h, &conn)
            && h.messages(&conn).len() == 1
            && h.events.calls().len() == 1)
            .await
    );
    assert_eq!(
        h.events.calls().len(),
        1,
        "the message was synced, not lost"
    );
    assert!(
        h.events.ingested().is_empty(),
        "initial look-back stays backfill even with IDLE active"
    );
    // Once the real completion condition holds, live mail must be ingested without the poll.
    h.inject("INBOX", fixtures::get("gbk.eml").unwrap());
    assert!(h.until(5, |h| h.events.ingested().len() == 1).await);
    h.mail.stop(&conn);
}

/// A notification arriving between EXAMINE and the IDLE continuation must not disappear.
#[tokio::test(flavor = "multi_thread")]
async fn idle_registration_window_keeps_arrivals() {
    let h = setup().await;
    let conn = h.connect().await;
    h.sync(&conn).await;
    assert!(inbox_synced(&h, &conn));
    let (idle_entered, release_idle) = h.imap.pause_next("IDLE");
    let (wake, mut rx) = tokio::sync::mpsc::channel(4);
    let stop = tokio_util::sync::CancellationToken::new();
    let adapter = h.mail.adapter("imap").unwrap();
    let cx = h.conns.cx(WS, &conn).unwrap();
    let token = stop.clone();
    let watcher = tokio::spawn(async move { adapter.watch(&cx, token, wake).await });
    tokio::time::timeout(Duration::from_secs(5), idle_entered)
        .await
        .unwrap()
        .unwrap();
    h.inject("INBOX", fixtures::get("multipart-alternative.eml").unwrap());
    release_idle.send(()).unwrap();
    tokio::time::timeout(Duration::from_secs(5), rx.recv())
        .await
        .expect("EXISTS after registration wakes the consumer")
        .expect("watcher remains live");
    h.sync(&conn).await;
    assert_eq!(h.events.ingested().len(), 1);
    stop.cancel();
    tokio::time::timeout(Duration::from_secs(5), watcher)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn flags_follow_the_server() {
    for condstore in [true, false] {
        let h = setup_with(Opts {
            imap: ImapConfig {
                condstore,
                ..Default::default()
            },
            ..Default::default()
        })
        .await;
        let uid = h.inject("INBOX", fixtures::get("gbk.eml").unwrap());
        let conn = h.connect().await;
        h.sync(&conn).await;
        let m = h.messages(&conn).remove(0);
        assert!(m.flags.is_empty());
        assert_eq!(h.thread(&m.thread_id).unread, 1);

        h.imap
            .set_flags(ADDR, "INBOX", uid, &["\\Seen", "\\Flagged"])
            .unwrap();
        if !condstore {
            // without CONDSTORE the newest 200 are re-checked every 10 minutes
            h.sync(&conn).await;
            assert!(h.messages(&conn)[0].flags.is_empty(), "not yet");
            h.env.clock.advance(11 * 60 * 1000);
        }
        let r = h.sync(&conn).await;
        assert!(r.flag_updates >= 1, "{condstore}: {r:?}");
        let m = h.messages(&conn).remove(0);
        assert_eq!(m.flags, ["flagged", "seen"], "{condstore}");
        assert_eq!(h.thread(&m.thread_id).unread, 0);
        if condstore {
            assert!(
                h.imap.commands().iter().any(|c| c.contains("CHANGEDSINCE")),
                "{:?}",
                h.imap.commands()
            );
        }
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn very_large_mail_is_header_and_structure_only() {
    let h = setup().await;
    h.inject("INBOX", &fixtures::large(26 * 1024 * 1024));
    let conn = h.connect().await;
    h.sync(&conn).await;
    let m = h.by_subject(&conn, "Scanned documents (large)");
    assert_eq!(m.raw_path, None);
    assert_eq!(m.body_text, None);
    assert!(m.size > 25 * 1024 * 1024);
    assert!(m.has_attachments);
    let atts = h
        .env
        .store()
        .reader(|c| steve_mail::repo::attachments(c, &m.id))
        .unwrap();
    assert_eq!(atts.len(), 1);
    assert_eq!(atts[0].1, "scan.bin");
    assert_eq!(atts[0].2, "application/octet-stream");
    assert!(
        h.imap
            .commands()
            .iter()
            .any(|c| c.contains("BODY.PEEK[HEADER] BODYSTRUCTURE"))
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn gmail_threads_by_thrid_and_ignores_all_mail() {
    let h = setup_with(Opts {
        imap: ImapConfig {
            gmail: true,
            ..Default::default()
        },
        ..Default::default()
    })
    .await;
    let [t1, t2, t3] = fixtures::thread();
    h.inject("INBOX", t1);
    h.inject("[Gmail]/Sent Mail", t2);
    h.inject("INBOX", t3);
    let conn = h.connect().await;
    h.sync(&conn).await;
    let msgs = h.messages(&conn);
    assert_eq!(msgs.len(), 3, "\\All is ignored: no duplicates");
    assert!(msgs.iter().all(|m| m.thread_id == msgs[0].thread_id));
    let t = h.thread(&msgs[0].thread_id);
    assert!(
        t.thread_key.chars().all(|c| c.is_ascii_digit()),
        "X-GM-THRID: {}",
        t.thread_key
    );
    assert!(h.imap.commands().iter().any(|c| c.contains("X-GM-THRID")));
}

#[tokio::test(flavor = "multi_thread")]
async fn read_archive_and_refetch() {
    let h = setup().await;
    let uid = h.inject("INBOX", fixtures::get("gbk.eml").unwrap());
    let conn = h.connect().await;
    h.sync(&conn).await;
    let m = h.messages(&conn).remove(0);
    let adapter = h.mail.adapter("imap").unwrap();
    let cx = h.conns.cx(WS, &conn).unwrap();

    let raw = adapter
        .fetch_raw(&cx, &m.ext_id, m.message_id.as_deref())
        .await
        .unwrap();
    assert_eq!(raw, fixtures::get("gbk.eml").unwrap());

    adapter
        .set_flags(
            &cx,
            &m.ext_id,
            &FlagChange {
                seen: Some(true),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert!(
        h.imap.messages(ADDR, "INBOX").unwrap()[0]
            .flags
            .contains(&"\\Seen".to_string())
    );
    adapter
        .set_flags(
            &cx,
            &m.ext_id,
            &FlagChange {
                archive: true,
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert!(
        h.imap
            .messages(ADDR, "INBOX")
            .unwrap()
            .iter()
            .all(|x| x.uid != uid)
    );
    assert_eq!(h.imap.messages(ADDR, "Archive").unwrap().len(), 1);

    // moved: found again by Message-ID
    let raw = adapter
        .fetch_raw(&cx, &m.ext_id, m.message_id.as_deref())
        .await
        .unwrap();
    assert_eq!(raw, fixtures::get("gbk.eml").unwrap());
    // gone everywhere → 410's message
    let e = adapter
        .fetch_raw(&cx, &m.ext_id, Some("nobody@nowhere"))
        .await
        .unwrap_err();
    assert_eq!(e.to_string(), "原件已经不在邮箱里了");
}

#[tokio::test(flavor = "multi_thread")]
async fn send_over_smtp_and_append_to_sent() {
    let h = setup().await;
    let conn = h.connect().await;
    let adapter = h.mail.adapter("imap").unwrap();
    let cx = h.conns.cx(WS, &conn).unwrap();
    let mime = b"From: me@example.com\r\nTo: alice@acme.example\r\nSubject: hi\r\nMessage-ID: <s1@steve.local>\r\n\r\nhello\r\n";
    adapter
        .send(
            &cx,
            mime,
            &SendMeta {
                from: ADDR.into(),
                rcpt: vec!["alice@acme.example".into()],
                thread: None,
                append_sent: None,
            },
        )
        .await
        .unwrap();
    // Submission and optional copy are separate so a copy failure cannot undo confirmation.
    adapter
        .copy_sent(
            &cx,
            mime,
            &SendMeta {
                from: ADDR.into(),
                rcpt: vec!["alice@acme.example".into()],
                thread: None,
                append_sent: None,
            },
        )
        .await
        .unwrap();
    let sent = h.smtp.sent();
    assert_eq!(sent.len(), 1);
    assert_eq!(sent[0].rcpt, ["alice@acme.example"]);
    assert!(sent[0].tls);
    assert_eq!(sent[0].auth_user.as_deref(), Some(ADDR));
    let copy = h.imap.messages(ADDR, "Sent").unwrap();
    assert_eq!(
        copy.len(),
        1,
        "APPENDed to the sent folder (not a Gmail host)"
    );
    assert!(copy[0].flags.contains(&"\\Seen".to_string()));
    // the next sync records it as mine
    h.sync(&conn).await;
    let m = h.by_subject(&conn, "hi");
    assert_eq!(m.direction, "out");
    assert!(h.events.ingested().is_empty());

    // without the copy
    adapter
        .send(
            &cx,
            mime,
            &SendMeta {
                from: ADDR.into(),
                rcpt: vec!["alice@acme.example".into()],
                thread: None,
                append_sent: Some(false),
            },
        )
        .await
        .unwrap();
    assert_eq!(h.imap.messages(ADDR, "Sent").unwrap().len(), 1);
}
