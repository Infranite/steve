//! E1 acceptance: connect → the 30-day look-back is all `recorded` and runs no flow;
//! a new message afterwards → one `mail.new` event and the sender identified; a reply chain is one
//! thread; no duplicates after UIDVALIDITY changes; GBK and RFC 2047 parse; after retention the raw
//! file is gone and the snippet stays. All against the in-process fake IMAP/SMTP servers.

mod support;

use serde_json::Value;
use steve_mail::Delivery;
use steve_mail::fake::fixtures;
use steve_modules::contacts::TimelineSource;
use steve_modules::testkit::{ME, WS};
use steve_store::base::Viewer;
use support::*;

#[tokio::test(flavor = "multi_thread")]
async fn connect_backfill_then_live_mail() {
    let h = setup().await;
    let [t1, t2, t3] = fixtures::thread();
    h.inject("INBOX", t1);
    h.inject("Sent", t2);
    h.inject("INBOX", fixtures::get("newsletter.eml").unwrap());
    // outside the 30-day window: not synced
    h.inject_at("INBOX", fixtures::get("big5.eml").unwrap(), days_ago(45));

    let conn = h.connect().await;
    let r = h.sync(&conn).await;
    assert_eq!(r.fetched, 3, "{r:?}");

    // every old message is recorded with the backfill label, none ingested
    let calls = h.events.calls();
    assert_eq!(
        calls.len(),
        2,
        "two received mails in the inbox; sent mail makes no event"
    );
    for c in &calls {
        assert_eq!(c.how, Delivery::backfill());
        assert_eq!(
            c.how,
            Delivery::Fixed {
                outcome: "recorded".into(),
                label: "接入时同步的旧邮件".into()
            }
        );
    }
    assert!(h.events.ingested().is_empty());
    let msgs = h.messages(&conn);
    assert_eq!(msgs.len(), 3);
    assert!(msgs.iter().all(|m| m.subject != "課程地點變更"));
    let nl = h.by_subject(&conn, "This Week in Rust 670");
    assert!(nl.bulk);
    assert!(nl.event_id.is_some(), "event id linked back");
    let sent = h.by_subject(&conn, "Re: 预算审批流程");
    assert_eq!(sent.direction, "out");
    assert_eq!(sent.folders, ["Sent"]);

    // scopes written with the defaults: inbox receive, sent record, others ignore
    let scopes: Value = serde_json::from_str(
        &h.env
            .store()
            .reader(|c| {
                Ok::<String, steve_store::StoreError>(c.query_row(
                    "SELECT scopes FROM source_settings WHERE connection_id = ?",
                    [&conn],
                    |r| r.get(0),
                )?)
            })
            .unwrap(),
    )
    .unwrap();
    let mode = |name: &str| {
        scopes
            .as_array()
            .unwrap()
            .iter()
            .find(|s| s["ext_id"] == name)
            .map(|s| s["mode"].as_str().unwrap().to_string())
    };
    assert_eq!(mode("INBOX").as_deref(), Some("receive"));
    assert_eq!(mode("Sent").as_deref(), Some("record"));
    assert_eq!(mode("Trash").as_deref(), Some("ignore"));
    // settings row with defaults
    assert_eq!(
        h.count(
            "SELECT sync_days FROM mail_account_settings WHERE connection_id = ?",
            &conn
        ),
        30
    );

    // a new message afterwards: one mail.new event, the sender identified
    h.inject("INBOX", fixtures::get("multipart-alternative.eml").unwrap());
    let r = h.sync(&conn).await;
    assert_eq!(r.fetched, 1);
    let live = h.events.ingested();
    assert_eq!(live.len(), 1);
    let ev = &live[0].event;
    assert_eq!(ev.type_key, "mail.new");
    assert_eq!(ev.also, ["mail.new"]);
    let m = h.by_subject(&conn, "关于下周三的会议改期到周四下午");
    assert_eq!(ev.external_id, m.id);
    assert_eq!(ev.thread_key.as_deref(), Some(m.thread_id.as_str()));
    assert!(ev.summary.starts_with("王小明"), "{}", ev.summary);
    assert_eq!(ev.payload["mail_message_id"], m.id);
    assert!(
        ev.payload["text"]
            .as_str()
            .unwrap()
            .contains("周四下午 3 点")
    );
    let party = m.from_party_id.clone().expect("sender identified");
    let name: String = h
        .env
        .store()
        .reader(|c| {
            Ok::<_, steve_store::StoreError>(c.query_row(
                "SELECT name FROM parties WHERE id = ?",
                [&party],
                |r| r.get(0),
            )?)
        })
        .unwrap();
    assert_eq!(name, "王小明");
    assert_eq!(
        ev.actor.as_ref().unwrap().party_id.as_deref(),
        Some(party.as_str())
    );
    assert_eq!(m.event_id.as_deref(), Some(live[0].id.as_str()));
    assert!(!h.notified.lock().unwrap().is_empty(), "SSE for the thread");

    // the reply chain: thread-3 answers my thread-2 → mail.reply, same thread as 1 and 2
    h.inject("INBOX", t3);
    h.sync(&conn).await;
    let live = h.events.ingested();
    assert_eq!(live.len(), 2);
    assert_eq!(live[1].event.type_key, "mail.reply");
    assert_eq!(live[1].event.also, ["mail.new"]);
    let all = h.messages(&conn);
    let chain: Vec<_> = all
        .iter()
        .filter(|m| m.subject.contains("预算审批流程"))
        .collect();
    assert_eq!(chain.len(), 3);
    assert!(chain.iter().all(|m| m.thread_id == chain[0].thread_id));
    let t = h.thread(&chain[0].thread_id);
    assert_eq!(t.count, 3);
    assert_eq!(t.unread, 2, "two received, unseen");
    assert_eq!(t.thread_key, "thread-1.alice@acme.example");
    assert_eq!(t.state, "open", "Alice answered after me");
    assert_eq!(t.visibility, "private");
    assert_eq!(t.created_by, ME);
    // Bob (Cc of a received mail) is only looked up, not created
    assert_eq!(
        h.count(
            "SELECT COUNT(*) FROM account_handles WHERE handle = ?",
            "bob@acme.example"
        ),
        0
    );
    // Alice (recipient of my mail, sender of hers) is one contact
    let alice = chain[0].from_party_id.clone().unwrap();
    assert_eq!(chain[2].from_party_id.as_deref(), Some(alice.as_str()));
    assert_eq!(chain[1].to_addrs[0]["party_id"], alice.as_str());

    // the contact's “往来”
    let viewer = Viewer::bare(WS, ME);
    let lines = h
        .env
        .store()
        .reader(|c| steve_mail::timeline::MailTimeline.entries(c, &viewer, &alice, None, 50))
        .unwrap();
    assert_eq!(lines.len(), 3);
    assert!(lines.iter().all(|l| l.kind == "mail"));
    assert!(lines.iter().any(|l| l.text.starts_with("发出邮件：")));
    assert!(lines[0].href.starts_with("#/mail/t/mt_"));
    // another user does not see my private mailbox
    let other = Viewer::bare(WS, steve_modules::testkit::OTHER);
    let none = h
        .env
        .store()
        .reader(|c| steve_mail::timeline::MailTimeline.entries(c, &other, &alice, None, 50))
        .unwrap();
    assert!(none.is_empty());

    // nothing new: no events, no duplicates
    let before = h.messages(&conn).len();
    let r = h.sync(&conn).await;
    assert_eq!(r.fetched, 0);
    assert_eq!(h.messages(&conn).len(), before);
}

#[tokio::test(flavor = "multi_thread")]
async fn uidvalidity_change_does_not_duplicate() {
    let h = setup().await;
    let [t1, t2, _] = fixtures::thread();
    h.inject("INBOX", t1);
    h.inject("INBOX", fixtures::get("no-message-id.eml").unwrap());
    h.inject("Sent", t2);
    let conn = h.connect().await;
    h.sync(&conn).await;
    let before = h.messages(&conn);
    assert_eq!(before.len(), 3);

    let v = h.imap.bump_uidvalidity(ADDR, "INBOX").unwrap();
    let r = h.sync(&conn).await;
    assert_eq!(r.reset, ["INBOX"]);
    assert_eq!(r.fetched, 2, "the window is rescanned");
    let after = h.messages(&conn);
    assert_eq!(
        after.len(),
        3,
        "deduped by Message-ID (or sender, date, subject)"
    );
    for m in after.iter().filter(|m| m.folders == ["INBOX"]) {
        assert!(m.ext_id.starts_with(&format!("INBOX:{v}:")), "{}", m.ext_id);
    }
    // the rescan is a look-back too: no flows
    assert!(h.events.ingested().is_empty());
    // and new mail after it is live again
    h.inject("INBOX", fixtures::get("gbk.eml").unwrap());
    h.sync(&conn).await;
    assert_eq!(h.events.ingested().len(), 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn charsets_attachments_and_signature() {
    let h = setup().await;
    let conn = h.connect().await;
    h.sync(&conn).await;
    for f in [
        "gbk.eml",
        "gb18030.eml",
        "iso-2022-jp.eml",
        "invoice-pdf.eml",
        "inline-cid.eml",
        "outlook-quote.eml",
        "html-tracking.eml",
    ] {
        h.inject("INBOX", fixtures::get(f).unwrap());
    }
    h.sync(&conn).await;

    let gbk = h.by_subject(&conn, "回复：报价单（修改版）");
    assert_eq!(gbk.from_name, "张伟");
    assert!(gbk.body_text.as_deref().unwrap().contains("单价下调 5%"));
    assert!(gbk.snippet.contains("单价下调 5%"), "{}", gbk.snippet);
    let ev = h
        .events
        .ingested()
        .into_iter()
        .find(|d| d.event.external_id == gbk.id)
        .unwrap();
    assert_eq!(
        ev.event.summary,
        "张伟（华东销售部）：回复：报价单（修改版）"
    );
    assert_eq!(
        ev.event.actor.as_ref().unwrap().signature_org.as_deref(),
        Some("华东销售部")
    );
    assert!(
        h.by_subject(&conn, "会议纪要 䶮")
            .body_text
            .unwrap()
            .contains("龑䶮项目延期一周")
    );
    assert!(
        h.by_subject(&conn, "打ち合わせ資料の送付")
            .body_text
            .unwrap()
            .contains("来週の打ち合わせ資料")
    );

    // attachments: metadata only, the PDF is a real one, the cid image is inline
    let inv = h
        .messages(&conn)
        .into_iter()
        .find(|m| m.has_attachments)
        .expect("the invoice has an attachment");
    let atts = h
        .env
        .store()
        .reader(|c| steve_mail::repo::attachments(c, &inv.id))
        .unwrap();
    assert_eq!(atts.len(), 1);
    assert_eq!(atts[0].1, "发票-2026-10.pdf");
    assert_eq!(atts[0].2, "application/pdf");
    assert!(!atts[0].4);
    let ev = h
        .events
        .ingested()
        .into_iter()
        .find(|d| d.event.external_id == inv.id)
        .unwrap();
    assert_eq!(ev.event.type_key, "mail.attachment");
    assert_eq!(ev.event.also, ["mail.new"]);
    let cid = h
        .messages(&conn)
        .into_iter()
        .find(|m| {
            h.env
                .store()
                .reader(|c| steve_mail::repo::attachments(c, &m.id))
                .unwrap()
                .iter()
                .any(|a| a.4)
        })
        .expect("inline image registered");
    assert!(!cid.has_attachments, "an inline image is not an attachment");

    // Outlook: the quoted block is cut, the signature stays
    let o = h.by_subject(&conn, "RE: Q4 contract renewal");
    let new = o.body_new.unwrap();
    assert!(new.starts_with("Thanks, the new terms look fine."));
    assert!(new.contains("Partner Co."));
    assert!(!new.contains("renewal terms for Q4"));

    // HTML only: text from the HTML, has_html
    let html = h.by_subject(&conn, "Votre commande est confirmée à la caisse");
    assert!(html.has_html);
    assert!(html.body_text.unwrap().contains("Café de la Gare"));

    // raw file 0600 under data/mail/<ws>/
    use std::os::unix::fs::PermissionsExt;
    let rel = gbk.raw_path.unwrap();
    assert!(rel.starts_with(&format!("data/mail/{WS}/")));
    let meta = std::fs::metadata(h.env.dir.path().join(&rel)).unwrap();
    assert_eq!(meta.permissions().mode() & 0o777, 0o600);
}

#[tokio::test(flavor = "multi_thread")]
async fn retention_drops_raw_and_text_keeps_snippet() {
    let h = setup().await;
    h.inject("INBOX", fixtures::get("gbk.eml").unwrap());
    let conn = h.connect().await;
    h.sync(&conn).await;
    let m = h.by_subject(&conn, "回复：报价单（修改版）");
    let path = h.env.dir.path().join(m.raw_path.as_deref().unwrap());
    assert!(path.exists());

    // not expired yet
    assert_eq!(h.mail.retention().unwrap(), 0);
    // 30 days kept in this workspace; a hundred days later the body goes
    h.env
        .store()
        .settings()
        .set(
            WS,
            &serde_json::Map::from_iter([("keepMail".to_string(), Value::String("30".into()))]),
            ME,
        )
        .unwrap();
    assert_eq!(steve_mail::retention::keep_days(h.env.store(), WS), 30);
    h.env.clock.advance(100 * DAY_MS);
    assert_eq!(h.mail.retention().unwrap(), 1);
    let after = h.by_subject(&conn, "回复：报价单（修改版）");
    assert!(!path.exists(), "raw file deleted");
    assert_eq!(after.raw_path, None);
    assert_eq!(after.body_text, None);
    assert_eq!(after.body_new, None);
    assert!(after.purged.is_some());
    assert_eq!(after.snippet, m.snippet);
    assert!(!after.snippet.is_empty());
    assert_eq!(h.mail.retention().unwrap(), 0, "once");
}

#[tokio::test(flavor = "multi_thread")]
async fn one_sync_logs_in_once() {
    let h = setup().await;
    h.inject("INBOX", fixtures::get("gbk.eml").unwrap());
    let conn = h.connect().await;
    let before = h.imap.logins();
    h.sync(&conn).await;
    assert_eq!(
        h.imap.logins() - before,
        1,
        "LIST and sync share the session"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn archiving_a_thread_logs_in_once() {
    let h = setup().await;
    let [t1, t2, t3] = fixtures::thread();
    h.inject("INBOX", t1);
    h.inject("Sent", t2);
    h.inject("INBOX", t3);
    let conn = h.connect().await;
    h.sync(&conn).await;
    let thread = h.by_subject(&conn, "预算审批流程").thread_id;
    let before = h.imap.logins();
    assert_eq!(h.mail.archive_thread(WS, &thread).await.unwrap(), 2);
    assert_eq!(
        h.imap.logins() - before,
        1,
        "one session for the whole thread"
    );
    assert!(h.imap.messages(ADDR, "INBOX").unwrap().is_empty());
    assert_eq!(h.imap.messages(ADDR, "Archive").unwrap().len(), 2);
}

#[tokio::test(flavor = "multi_thread")]
async fn retention_skips_a_file_it_cannot_delete_and_finishes() {
    let h = setup().await;
    h.inject("INBOX", fixtures::get("gbk.eml").unwrap());
    h.inject("INBOX", fixtures::get("newsletter.eml").unwrap());
    h.inject("INBOX", fixtures::get("invoice-pdf.eml").unwrap());
    let conn = h.connect().await;
    h.sync(&conn).await;
    let msgs = h.messages(&conn);
    assert_eq!(msgs.len(), 3);
    // one raw file replaced by a non-empty directory: remove_file fails with something other
    // than NotFound
    let stuck = h.env.dir.path().join(msgs[0].raw_path.as_deref().unwrap());
    std::fs::remove_file(&stuck).unwrap();
    std::fs::create_dir(&stuck).unwrap();
    std::fs::write(stuck.join("x"), b"x").unwrap();

    h.env.clock.advance(400 * DAY_MS);
    // pages of one: a page that purges nothing used to be fetched again forever
    let mail = h.mail.clone();
    let n = tokio::time::timeout(
        std::time::Duration::from_secs(20),
        tokio::task::spawn_blocking(move || mail.retention_paged(1)),
    )
    .await
    .expect("retention finishes")
    .unwrap()
    .unwrap();
    assert_eq!(n, 2);
    let left = h
        .messages(&conn)
        .into_iter()
        .filter(|m| m.purged.is_none())
        .map(|m| m.id)
        .collect::<Vec<_>>();
    assert_eq!(left, [msgs[0].id.clone()]);
}

#[tokio::test(flavor = "multi_thread")]
async fn disconnect_removes_the_mail() {
    let h = setup().await;
    h.inject("INBOX", fixtures::get("invoice-pdf.eml").unwrap());
    let conn = h.connect().await;
    h.sync(&conn).await;
    let m = h.messages(&conn).remove(0);
    let path = h.env.dir.path().join(m.raw_path.unwrap());
    assert!(path.exists());
    h.conns.disconnect(&me(), &conn).await.expect("disconnect");
    for t in [
        "mail_messages",
        "mail_threads",
        "mail_folders",
        "mail_account_settings",
        "source_settings",
    ] {
        assert_eq!(
            h.count(
                &format!("SELECT COUNT(*) FROM {t} WHERE connection_id = ?"),
                &conn
            ),
            0,
            "{t}"
        );
    }
    assert_eq!(
        h.count(
            "SELECT COUNT(*) FROM mail_attachments WHERE message_id = ?",
            &m.id
        ),
        0
    );
    assert!(!path.exists());
}

#[tokio::test(flavor = "multi_thread")]
async fn utf7_folder_names_and_settings() {
    let h = setup().await;
    h.imap.add_folder(ADDR, "已发送", None).unwrap();
    h.imap
        .inject(ADDR, "已发送", fixtures::thread()[1])
        .unwrap();
    let conn = h.connect().await;
    h.sync(&conn).await;
    let m = h.by_subject(&conn, "Re: 预算审批流程");
    assert_eq!(m.folders, ["已发送"], "decoded name, role by name → record");
    assert!(m.ext_id.starts_with("已发送:"));

    // settings: validated, revision written, another workspace → 404
    let s = h.mail.settings(&me(), &conn).unwrap();
    assert_eq!((s.send_delay_s, s.sync_days, s.revision), (10, 30, 1));
    let bad = steve_mail::settings::SettingsPatch {
        send_delay_s: Some(61),
        sync_days: Some(14),
        ..Default::default()
    };
    let e = h.mail.patch_settings(&me(), &conn, &bad).unwrap_err();
    assert_eq!(e.to_string(), "参数不对：send_delay_s、sync_days");
    let ok = steve_mail::settings::SettingsPatch {
        signature: Some("— Peng".into()),
        send_delay_s: Some(30),
        ..Default::default()
    };
    let s = h.mail.patch_settings(&me(), &conn, &ok).unwrap();
    assert_eq!(
        (s.signature.as_str(), s.send_delay_s, s.revision),
        ("— Peng", 30, 2)
    );
    assert_eq!(
        h.count(
            "SELECT COUNT(*) FROM revisions WHERE object_type = 'mail_account' AND object_id = ?",
            &conn
        ),
        1
    );
    let other = steve_modules::testkit::caller(steve_modules::testkit::WS2, ME);
    assert!(h.mail.settings(&other, &conn).is_err());
}
