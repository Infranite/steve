//! E3 acceptance (email.md §14): no decision → `execute` refuses and writes `mail.send.blocked`;
//! undo within the 10 s delay → nothing sent; content edited before approval → 409; `reply_all`
//! drops my addresses; recipients in the model output are ignored; a vault secret in the body is
//! blocked; the MIME the fake SMTP receives carries the right `In-Reply-To`/`References`; a provider
//! failure opens a `fail` decision. Only the in-process fake SMTP/IMAP and a fixed fake gateway.

mod support;

use std::sync::{Arc, Mutex};

use serde_json::{Value, json};
use steve_mail::compose::{QuickDraft, SAFETY};
use steve_mail::drafts::{MSG_ALREADY_SENT, MSG_EDITED, NewDraft, RcptIn};
use steve_mail::fake::fixtures;
use steve_mail::repo;
use steve_mail::send::{ERR_NO_APPROVAL, ERR_SECRET};
use steve_mail::{DraftHooks, SecretScan, SendOutcome};
use steve_modules::decisions::dispatch;
use steve_modules::testkit::{ME, WS, WS2, call, caller};
use steve_modules::{ApiError, Clock};
use support::*;

/// The fixed model answer: tries to add recipients, which must be ignored.
const MODEL: &str = r#"{"subject":"Re: whatever the model says","body":"预算审批分三步：填表、主管签字、财务复核。","reply_all":true,"to":["evil@attacker.example"],"cc":["evil2@attacker.example"],"bcc":["spy@attacker.example"],"needs_attention":false,"why":"回答流程问题","cites":[]}"#;

struct Secrets;
impl SecretScan for Secrets {
    fn find(&self, _: &str, text: &str) -> Vec<String> {
        if text.contains("tok-7f3a9c") {
            vec!["API 令牌".into()]
        } else {
            Vec::new()
        }
    }
}

#[derive(Default)]
struct Hooks(Mutex<Vec<String>>);
impl DraftHooks for Hooks {
    fn rejected(&self, _: &str, session: &str, _: Option<&str>, message: &str) {
        self.0.lock().unwrap().push(format!("{session}|{message}"));
    }
    fn sent(&self, _: &str, item: &str, subject: &str) {
        self.0
            .lock()
            .unwrap()
            .push(format!("sent {item}|{subject}"));
    }
}

/// The three-message reply chain synced; returns `(connection, thread id)`.
async fn with_thread(h: &H) -> (String, String) {
    let [t1, t2, t3] = fixtures::thread();
    h.inject("INBOX", t1);
    h.inject("Sent", t2);
    h.inject("INBOX", t3);
    let conn = h.connect().await;
    h.sync(&conn).await;
    let t = h.by_subject(&conn, "预算审批流程").thread_id;
    (conn, t)
}

fn draft(h: &H, id: &str) -> repo::DraftRow {
    h.env
        .store()
        .reader(|c| repo::draft(c, id))
        .unwrap()
        .unwrap()
}

fn audits(h: &H, action: &str) -> Vec<Value> {
    h.env
        .store()
        .reader(|c| {
            let mut st = c.prepare("SELECT detail FROM audit WHERE action = ? ORDER BY rowid")?;
            let v = st
                .query_map([action], |r| r.get::<_, String>(0))?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok::<_, steve_store::StoreError>(v)
        })
        .unwrap()
        .into_iter()
        .map(|s| serde_json::from_str(&s).unwrap_or(Value::Null))
        .collect()
}

fn decision(h: &H, id: &str) -> steve_store::Decision {
    h.env
        .store()
        .reader(|c| steve_store::base::decisions::get(c, id))
        .unwrap()
        .unwrap()
}

fn my_draft(conn: &str, thread: Option<&str>, reply_to: Option<&str>, body: &str) -> NewDraft {
    NewDraft {
        connection_id: conn.into(),
        mode: if thread.is_some() { "reply" } else { "new" }.into(),
        thread_id: thread.map(String::from),
        reply_to: reply_to.map(String::from),
        to: vec![RcptIn::Full {
            addr: "alice@acme.example".into(),
            name: "Alice Zhou".into(),
        }],
        cc: None,
        bcc: Some(vec![RcptIn::Addr("boss@acme.example".into())]),
        subject: "Re: 预算审批流程".into(),
        body: body.into(),
        files: None,
    }
}

/// A quick draft by the bot with the fixed model answer.
async fn quick(h: &H, thread: &str) -> steve_mail::drafts::DraftOut {
    h.env.gateway.reply(MODEL);
    h.mail
        .quick_draft(
            &me(),
            QuickDraft {
                thread_id: thread.into(),
                ..Default::default()
            },
        )
        .await
        .expect("quick draft")
}

#[tokio::test(flavor = "multi_thread")]
async fn execute_without_a_decision_refuses() {
    let h = setup().await;
    let (conn, thread) = with_thread(&h).await;
    let d = h
        .mail
        .create_draft(&me(), my_draft(&conn, Some(&thread), None, "hi"))
        .unwrap();

    // a draft nobody approved
    let o = h.mail.execute(&d.id).await.unwrap();
    assert_eq!(
        o,
        SendOutcome::Blocked {
            reason: ERR_NO_APPROVAL.into()
        }
    );
    assert_eq!(draft(&h, &d.id).status, "draft");

    // marked approved behind the decision table's back (a bug, a hand edit): still refused
    h.env
        .store()
        .with_conn(|c| {
            c.execute(
                "UPDATE mail_drafts SET status = 'approved', send_at = 0 WHERE id = ?",
                [&d.id],
            )?;
            Ok(())
        })
        .unwrap();
    let o = h.mail.execute(&d.id).await.unwrap();
    assert_eq!(
        o,
        SendOutcome::Blocked {
            reason: ERR_NO_APPROVAL.into()
        }
    );
    let row = draft(&h, &d.id);
    assert_eq!(row.status, "failed");
    assert_eq!(row.error.as_deref(), Some(ERR_NO_APPROVAL));
    let blocked = audits(&h, "mail.send.blocked");
    assert_eq!(blocked.len(), 2, "{blocked:?}");
    assert_eq!(blocked[0]["reason"], "not_approved");
    assert_eq!(blocked[1]["reason"], "no_approval");
    assert!(h.smtp.sent().is_empty(), "nothing reached SMTP");
    // the queue does not pick it either
    assert!(h.mail.send_due().await.unwrap().is_empty());
    assert!(h.smtp.sent().is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn content_changed_after_approval_is_refused() {
    let h = setup().await;
    let (conn, thread) = with_thread(&h).await;
    let d = h
        .mail
        .create_draft(&me(), my_draft(&conn, Some(&thread), None, "原文"))
        .unwrap();
    let d = h.mail.send_draft(&me(), &d.id).unwrap();
    assert_eq!(d.status, "approved");
    // the body changes after the approval (not possible through the API): the hash no longer matches
    h.env
        .store()
        .with_conn(|c| {
            c.execute(
                "UPDATE mail_drafts SET body = '换过的正文', send_at = 0 WHERE id = ?",
                [&d.id],
            )?;
            Ok(())
        })
        .unwrap();
    let o = h.mail.execute(&d.id).await.unwrap();
    assert_eq!(
        o,
        SendOutcome::Blocked {
            reason: ERR_NO_APPROVAL.into()
        }
    );
    assert!(h.smtp.sent().is_empty());
    assert_eq!(audits(&h, "mail.send.blocked").len(), 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn undo_within_the_delay_sends_nothing() {
    let h = setup().await;
    let (conn, thread) = with_thread(&h).await;
    let d = h
        .mail
        .create_draft(&me(), my_draft(&conn, Some(&thread), None, "周五见"))
        .unwrap();
    let d = h.mail.send_draft(&me(), &d.id).unwrap();
    assert_eq!(d.status, "approved");
    let now = h.env.clock.now();
    assert_eq!(d.send_at, Some(now + 10_000), "10 s default delay");

    // not due yet
    h.env.clock.advance(5_000);
    assert!(h.mail.send_due().await.unwrap().is_empty());
    // undo (POST /mail/drafts/:id/cancel) at 5 s
    let d = h.mail.cancel_draft(&me(), &d.id).unwrap();
    assert_eq!(d.status, "pending");
    assert_eq!(d.send_at, None);
    let dec = decision(&h, d.decision_id.as_deref().unwrap());
    assert_eq!(dec.status, "open", "the approval is open again");
    h.env.clock.advance(60_000);
    assert!(h.mail.send_due().await.unwrap().is_empty());
    assert!(h.smtp.sent().is_empty(), "nothing went out");
    assert_eq!(
        h.mail.execute(&d.id).await.unwrap(),
        SendOutcome::Blocked {
            reason: ERR_NO_APPROVAL.into()
        }
    );

    // the same through undoDecision's hook: before_undo inside the delay → pending
    let r = dispatch(&h.env.modules, &me(), &dec.id, "send", "")
        .unwrap()
        .unwrap();
    assert_eq!(draft(&h, &d.id).status, "approved");
    h.env.clock.advance(3_000);
    h.env
        .store()
        .transaction(|tx| h.env.modules.registry.before_undo(tx, &r))
        .unwrap();
    assert_eq!(draft(&h, &d.id).status, "pending");

    // after sending, undo is refused
    let r = dispatch(&h.env.modules, &me(), &dec.id, "send", "").unwrap();
    assert!(r.is_err(), "the decision is already resolved: {r:?}");
    let d2 = h.mail.send_draft(&me(), &d.id).unwrap();
    h.env.clock.advance(10_001);
    let sent = h.mail.send_due().await.unwrap();
    assert!(matches!(sent[0].1, SendOutcome::Sent { .. }), "{sent:?}");
    assert_eq!(h.smtp.sent().len(), 1);
    let dec2 = decision(&h, d2.decision_id.as_deref().unwrap());
    let e = h
        .env
        .store()
        .transaction(|tx| h.env.modules.registry.before_undo(tx, &dec2))
        .unwrap_err();
    assert_eq!(e, ApiError::conflict(MSG_ALREADY_SENT));
    let e = h.mail.cancel_draft(&me(), &d.id).unwrap_err();
    assert_eq!(e.to_string(), MSG_ALREADY_SENT);
}

#[tokio::test(flavor = "multi_thread")]
async fn edited_before_approval_answers_409_and_reopens() {
    let h = setup().await;
    let (_conn, thread) = with_thread(&h).await;
    let d = quick(&h, &thread).await;
    assert_eq!(d.status, "pending");
    let dec = d.decision_id.clone().unwrap();

    // someone edits the pending draft; the open card still shows the old version
    h.mail
        .update_draft(
            &me(),
            &d.id,
            steve_mail::drafts::DraftPatch {
                body: Some("改过的正文".into()),
                ..Default::default()
            },
        )
        .unwrap();
    let e = dispatch(&h.env.modules, &me(), &dec, "send", "")
        .unwrap()
        .unwrap_err();
    assert_eq!(e, ApiError::conflict(MSG_EDITED));
    assert_eq!(e.status(), 409);
    assert_eq!(decision(&h, &dec).status, "open", "rolled back, still open");
    assert_eq!(draft(&h, &d.id).status, "pending");

    // the queue reopens it with the current version
    assert_eq!(h.mail.refresh_stale().unwrap(), 1);
    assert_eq!(decision(&h, &dec).status, "expired");
    let row = draft(&h, &d.id);
    let new_dec = row.decision_id.clone().unwrap();
    assert_ne!(new_dec, dec);
    assert!(decision(&h, &new_dec).draft.contains("改过的正文"));
    dispatch(&h.env.modules, &me(), &new_dec, "send", "")
        .unwrap()
        .unwrap();
    assert_eq!(draft(&h, &d.id).status, "approved");

    // your edit in the card's text is your approval of that text
    let d2 = quick(&h, &thread).await;
    let r = dispatch(
        &h.env.modules,
        &me(),
        d2.decision_id.as_deref().unwrap(),
        "send",
        "我改的：好的，周五见。",
    )
    .unwrap()
    .unwrap();
    let row = draft(&h, &d2.id);
    assert_eq!(row.body, "我改的：好的，周五见。");
    assert_eq!(r.result["content_hash"], row.content_hash);
}

#[tokio::test(flavor = "multi_thread")]
async fn quick_draft_recipients_are_the_servers() {
    let h = setup().await;
    let (conn, thread) = with_thread(&h).await;
    // a second address of mine, in the original's Cc
    h.env
        .store()
        .with_conn(|c| {
            let acc: String = c.query_row(
                "SELECT account_id FROM connections WHERE id = ?",
                [&conn],
                |r| r.get(0),
            )?;
            c.execute(
                "INSERT INTO account_handles (workspace_id, family, handle, account_id, created) VALUES (?, 'email', 'peng@alias.example', ?, 1)",
                rusqlite::params![WS, acc],
            )?;
            Ok(())
        })
        .unwrap();
    let eml = "Date: Tue, 27 Oct 2026 09:00:00 +0000\r\nFrom: Alice Zhou <alice@acme.example>\r\nReply-To: Alice Desk <desk@acme.example>\r\nTo: ME@example.com, Carol <carol@acme.example>\r\nCc: Bob Liu <bob@acme.example>, peng@alias.example, desk@acme.example\r\nSubject: Re: =?UTF-8?B?6aKE566X5a6h5om55rWB56iL?=\r\nMessage-ID: <thread-4.alice@acme.example>\r\nIn-Reply-To: <thread-3.alice@acme.example>\r\nReferences: <thread-1.alice@acme.example> <thread-3.alice@acme.example>\r\nContent-Type: text/plain; charset=utf-8\r\n\r\n还有一个问题：要附发票吗？\r\n";
    h.inject("INBOX", eml.as_bytes());
    h.sync(&conn).await;
    let d = quick(&h, &thread).await;

    assert!(h.env.gateway.last_prompt().unwrap().starts_with(SAFETY));
    assert_eq!(d.mode, "reply_all");
    let to: Vec<&str> = d.to.iter().map(|r| r.addr.as_str()).collect();
    let cc: Vec<&str> = d.cc.iter().map(|r| r.addr.as_str()).collect();
    assert_eq!(
        to,
        ["desk@acme.example", "carol@acme.example"],
        "Reply-To first, my address dropped"
    );
    assert_eq!(
        cc,
        ["bob@acme.example"],
        "my alias and the duplicate dropped"
    );
    assert!(d.bcc.is_empty());
    let all = format!("{to:?}{cc:?}");
    assert!(!all.contains("attacker"), "model recipients ignored");
    assert!(!all.to_lowercase().contains("me@example.com"));
    assert_eq!(
        d.subject, "Re: 预算审批流程",
        "server subject, not the model's"
    );
    assert_eq!(d.author, "steve");
    assert!(d.to.iter().all(|r| r.external));
    assert_eq!(d.basis.as_ref().unwrap()["verdict"], "unchecked");
    assert_eq!(h.thread(&thread).state, "needs_reply");
    let dec = decision(&h, d.decision_id.as_deref().unwrap());
    assert_eq!(dec.kind, "approve");
    assert_eq!(dec.subject_type, "mail_draft");
    assert_eq!(dec.options, ["send", "no"]);
    assert_eq!(
        dec.question,
        "发邮件给 desk@acme.example、Carol：Re: 预算审批流程"
    );
    assert!(dec.draft.starts_with("收件人：desk@acme.example、Carol <carol@acme.example>\n抄送：Bob Liu <bob@acme.example>\n主题：Re: 预算审批流程\n\n预算审批分三步"));

    // reply (not all): only the Reply-To
    h.env
        .gateway
        .reply(MODEL.replace("\"reply_all\":true", "\"reply_all\":false"));
    let d = h
        .mail
        .quick_draft(
            &me(),
            QuickDraft {
                thread_id: thread.clone(),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(d.mode, "reply");
    assert_eq!(d.to.len(), 1);
    assert_eq!(d.to[0].addr, "desk@acme.example");
    assert!(d.cc.is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn a_vault_secret_in_the_body_is_blocked() {
    let h = setup_with(Opts {
        secrets: Some(Arc::new(Secrets)),
        ..Default::default()
    })
    .await;
    let (conn, thread) = with_thread(&h).await;
    let d = h
        .mail
        .create_draft(
            &me(),
            my_draft(&conn, Some(&thread), None, "令牌是 tok-7f3a9c，别外传"),
        )
        .unwrap();
    h.mail.send_draft(&me(), &d.id).unwrap();
    h.env.clock.advance(10_000);
    let out = h.mail.send_due().await.unwrap();
    assert_eq!(
        out[0].1,
        SendOutcome::Blocked {
            reason: ERR_SECRET.into()
        }
    );
    let row = draft(&h, &d.id);
    assert_eq!(row.status, "failed");
    assert_eq!(row.error.as_deref(), Some(ERR_SECRET));
    assert!(h.smtp.sent().is_empty());
    let a = audits(&h, "mail.send.blocked");
    assert_eq!(a[0]["reason"], "secret");
    assert!(
        !a[0].to_string().contains("tok-7f3a9c"),
        "no secret in the audit"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn sent_mime_threads_correctly() {
    let hooks = Arc::new(Hooks::default());
    let h = setup_with(Opts {
        hooks: Some(hooks.clone()),
        ..Default::default()
    })
    .await;
    let (conn, thread) = with_thread(&h).await;
    let d = quick(&h, &thread).await;
    let orig = h
        .messages(&conn)
        .into_iter()
        .find(|m| m.message_id.as_deref() == Some("thread-3.alice@acme.example"))
        .unwrap();
    assert_eq!(d.reply_to.as_deref(), Some(orig.id.as_str()));
    let dec = d.decision_id.clone().unwrap();
    dispatch(&h.env.modules, &me(), &dec, "send", "")
        .unwrap()
        .unwrap();
    h.env.clock.advance(10_000);
    let out = h.mail.send_due().await.unwrap();
    let SendOutcome::Sent { message } = &out[0].1 else {
        panic!("{out:?}")
    };

    let sent = h.smtp.sent();
    assert_eq!(sent.len(), 1);
    let s = &sent[0];
    assert_eq!(s.from, ADDR);
    let mut rcpt = s.rcpt.clone();
    rcpt.sort();
    assert_eq!(rcpt, ["alice@acme.example", "bob@acme.example"]);
    let mime = String::from_utf8_lossy(&s.data).to_string();
    let parsed = steve_mail::parse::parse(&s.data, false).unwrap();
    assert_eq!(
        parsed.in_reply_to.as_deref(),
        Some("thread-3.alice@acme.example")
    );
    assert_eq!(
        parsed.refs,
        [
            "thread-1.alice@acme.example",
            "thread-2.me@example.com",
            "thread-3.alice@acme.example"
        ],
        "the parent's References, then its Message-ID"
    );
    assert!(
        mime.contains("In-Reply-To: <thread-3.alice@acme.example>"),
        "{mime}"
    );
    assert_eq!(parsed.subject, "Re: 预算审批流程");
    let mid = parsed.message_id.clone().unwrap();
    let mid = mid.as_str();
    assert!(
        mid.starts_with(&format!("{}.", d.id)) && mid.ends_with("@steve.local"),
        "{mid}"
    );
    assert!(parsed.text.as_deref().unwrap().contains("预算审批分三步"));
    assert!(parsed.has_html, "plain + simple html");
    assert!(!mime.contains("attacker"));

    // the library: the outgoing message, the thread waits, the draft is sent, the audit has no body
    let row = draft(&h, &d.id);
    assert_eq!(row.status, "sent");
    assert_eq!(row.sent_message.as_deref(), Some(message.as_str()));
    let m = h
        .env
        .store()
        .reader(|c| repo::message(c, message))
        .unwrap()
        .unwrap();
    assert_eq!(m.direction, "out");
    assert_eq!(m.thread_id, thread);
    assert_eq!(m.message_id.as_deref(), Some(mid));
    assert_eq!(h.thread(&thread).state, "waiting");
    let a = audits(&h, "mail.send");
    assert_eq!(a.len(), 1);
    assert_eq!(a[0], json!({"draft": d.id, "to_count": 2, "decision": dec}));

    // a sync picking up the APPENDed copy does not duplicate it
    h.sync(&conn).await;
    let n = h.count(
        "SELECT COUNT(*) FROM mail_messages WHERE message_id = ?",
        mid,
    );
    assert_eq!(n, 1);
    assert!(hooks.0.lock().unwrap().is_empty(), "no item, no hook");
}

#[tokio::test(flavor = "multi_thread")]
async fn bcc_is_in_the_envelope_only() {
    let h = setup().await;
    let (conn, thread) = with_thread(&h).await;
    let d = h
        .mail
        .create_draft(&me(), my_draft(&conn, Some(&thread), None, "hi"))
        .unwrap();
    h.mail.send_draft(&me(), &d.id).unwrap();
    h.env.clock.advance(10_000);
    h.mail.send_due().await.unwrap();
    let s = &h.smtp.sent()[0];
    assert!(s.rcpt.contains(&"boss@acme.example".to_string()));
    let mime = String::from_utf8_lossy(&s.data).to_lowercase();
    assert!(!mime.contains("boss@acme.example"), "no Bcc header");
}

#[tokio::test(flavor = "multi_thread")]
async fn provider_failure_opens_a_fail_decision() {
    let h = setup().await;
    let (conn, thread) = with_thread(&h).await;
    let d = h
        .mail
        .create_draft(&me(), my_draft(&conn, Some(&thread), None, "hi"))
        .unwrap();
    h.mail.send_draft(&me(), &d.id).unwrap();
    h.smtp.fail_next("MAIL", "550 5.7.1 not allowed");
    h.env.clock.advance(10_000);
    let out = h.mail.send_due().await.unwrap();
    assert!(matches!(out[0].1, SendOutcome::Failed { .. }), "{out:?}");
    let row = draft(&h, &d.id);
    assert_eq!(row.status, "failed");
    let open = h
        .env
        .store()
        .reader(|c| repo::open_draft_decisions(c, WS, &d.id))
        .unwrap();
    assert_eq!(open.len(), 1);
    let f = decision(&h, &open[0]);
    assert_eq!(f.kind, "fail");
    assert_eq!(f.options, ["retry", "discard"]);
    // retry → sent on the next pass (the original approval still holds)
    dispatch(&h.env.modules, &me(), &f.id, "retry", "")
        .unwrap()
        .unwrap();
    let out = h.mail.send_due().await.unwrap();
    assert!(matches!(out[0].1, SendOutcome::Sent { .. }), "{out:?}");
    assert_eq!(h.smtp.sent().len(), 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn no_discards_and_tells_the_session() {
    let hooks = Arc::new(Hooks::default());
    let h = setup_with(Opts {
        hooks: Some(hooks.clone()),
        ..Default::default()
    })
    .await;
    let (_conn, thread) = with_thread(&h).await;
    let d = quick(&h, &thread).await;
    h.env
        .store()
        .with_conn(|c| {
            c.execute(
                "UPDATE mail_drafts SET session_id = 'ses_1' WHERE id = ?",
                [&d.id],
            )?;
            Ok(())
        })
        .unwrap();
    dispatch(
        &h.env.modules,
        &me(),
        d.decision_id.as_deref().unwrap(),
        "no",
        "语气太生硬",
    )
    .unwrap()
    .unwrap();
    assert_eq!(draft(&h, &d.id).status, "discarded");
    assert_eq!(
        hooks.0.lock().unwrap().as_slice(),
        [
            "ses_1|用户没有批准发送这封邮件：语气太生硬。按意见修改后再用 steve_mail_draft，或者用 steve_ask 问用户。"
        ]
    );
    h.env.clock.advance(60_000);
    assert!(h.mail.send_due().await.unwrap().is_empty());
    assert!(h.smtp.sent().is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn rest_routes() {
    let h = setup().await;
    let (conn, thread) = with_thread(&h).await;
    let r = steve_mail::api::routes(h.mail.clone()).with_state(h.env.modules.clone());
    let who = me();

    let (s, v) = call(&r, Some(&who), "GET", "/mail/accounts", None).await;
    assert_eq!(s, 200, "{v}");
    assert_eq!(v[0]["connection_id"], conn);
    assert_eq!(v[0]["address"], ADDR);
    assert_eq!(v[0]["settings"]["send_delay_s"], 10);
    let (s, v) = call(
        &r,
        Some(&who),
        "PATCH",
        &format!("/mail/accounts/{conn}"),
        Some(&json!({"signature": "— Peng", "send_delay_s": 30})),
    )
    .await;
    assert_eq!(s, 200, "{v}");
    assert_eq!(v["settings"]["send_delay_s"], 30);
    let (s, v) = call(
        &r,
        Some(&who),
        "PATCH",
        &format!("/mail/accounts/{conn}"),
        Some(&json!({"send_delay_s": 61})),
    )
    .await;
    assert_eq!(
        (s, v["error"].as_str()),
        (400, Some("参数不对：send_delay_s"))
    );
    let (s, _) = call(
        &r,
        Some(&who),
        "POST",
        &format!("/mail/accounts/{conn}/sync"),
        None,
    )
    .await;
    assert_eq!(s, 202);
    let (s, _) = call(&r, Some(&caller(WS2, ME)), "GET", "/mail/drafts/x", None).await;
    assert_eq!(s, 404);

    // write one yourself
    let body = json!({"connection_id": conn, "mode": "reply", "thread_id": thread, "to": ["alice@acme.example", {"addr": "bad address"}], "subject": "Re: x", "body": "hi"});
    let (s, v) = call(&r, Some(&who), "POST", "/mail/drafts", Some(&body)).await;
    assert_eq!((s, v["error"].as_str()), (400, Some("参数不对：to.1")));
    let mut body = body;
    body["to"] = json!(["alice@acme.example"]);
    let (s, v) = call(&r, Some(&who), "POST", "/mail/drafts", Some(&body)).await;
    assert_eq!(s, 200, "{v}");
    assert_eq!(v["status"], "draft");
    assert_eq!(v["author"], "me");
    assert_eq!(
        v["to"][0],
        json!({"addr": "alice@acme.example", "name": "", "party_id": v["to"][0]["party_id"], "external": true})
    );
    let id = v["id"].as_str().unwrap().to_string();
    let (s, v) = call(
        &r,
        Some(&caller(WS2, ME)),
        "GET",
        &format!("/mail/drafts/{id}"),
        None,
    )
    .await;
    assert_eq!((s, v["error"].as_str()), (404, Some("没有这个草稿")));
    let (s, v) = call(
        &r,
        Some(&who),
        "PUT",
        &format!("/mail/drafts/{id}"),
        Some(&json!({"body": "hi again"})),
    )
    .await;
    assert_eq!((s, v["body"].as_str()), (200, Some("hi again")));
    let (s, v) = call(
        &r,
        Some(&who),
        "POST",
        &format!("/mail/drafts/{id}/send"),
        None,
    )
    .await;
    assert_eq!(s, 200, "{v}");
    assert_eq!(v["status"], "approved");
    assert_eq!(
        v["send_at"],
        h.env.clock.now() + 30_000,
        "the mailbox's delay"
    );
    let (s, v) = call(
        &r,
        Some(&who),
        "PUT",
        &format!("/mail/drafts/{id}"),
        Some(&json!({"body": "x"})),
    )
    .await;
    assert_eq!(
        (s, v["error"].as_str()),
        (409, Some("这封邮件已经不能改了"))
    );
    let (s, v) = call(
        &r,
        Some(&who),
        "POST",
        &format!("/mail/drafts/{id}/cancel"),
        None,
    )
    .await;
    assert_eq!((s, v["status"].as_str()), (200, Some("pending")));
    let (s, v) = call(
        &r,
        Some(&who),
        "POST",
        &format!("/mail/drafts/{id}/cancel"),
        None,
    )
    .await;
    assert_eq!(s, 409, "{v}");
    let (s, _) = call(
        &r,
        Some(&who),
        "DELETE",
        &format!("/mail/drafts/{id}"),
        None,
    )
    .await;
    assert_eq!(s, 200);
    let row = draft(&h, &id);
    assert_eq!(row.status, "discarded");
    assert_eq!(
        decision(&h, row.decision_id.as_deref().unwrap()).status,
        "expired"
    );

    // the bot drafts
    h.env.gateway.reply(MODEL);
    let (s, v) = call(
        &r,
        Some(&who),
        "POST",
        &format!("/mail/threads/{thread}/draft"),
        Some(&json!({"instruction": "简短一点"})),
    )
    .await;
    assert_eq!(s, 200, "{v}");
    assert_eq!(v["status"], "pending");
    assert!(
        v["body"].as_str().unwrap().ends_with("— Peng"),
        "signature appended: {v}"
    );
    assert!(
        h.env
            .gateway
            .last_prompt()
            .unwrap()
            .contains("用户的要求：简短一点")
    );
    let (s, _) = call(
        &r,
        Some(&caller(WS2, ME)),
        "POST",
        &format!("/mail/threads/{thread}/draft"),
        Some(&json!({})),
    )
    .await;
    assert_eq!(s, 404);
    let (s, _) = call(&r, None, "GET", "/mail/accounts", None).await;
    assert_eq!(s, 401);
}

/// What steve-core's `undoDecision` does for a module decision (`steve_modules::decisions::undo`, with
/// core's rule that it is resolved and carries an undo).
fn undo_decision(h: &H, id: &str) -> Result<steve_store::Decision, ApiError> {
    steve_modules::decisions::undo(&h.env.modules, id, ME, |d| {
        d.status == "resolved" && !d.undo.is_null()
    })
    .expect("a mail decision")
}

#[tokio::test(flavor = "multi_thread")]
async fn undoing_the_approval_decision_within_the_delay_returns_the_draft_to_pending() {
    let h = setup().await;
    let (conn, thread) = with_thread(&h).await;
    let d = h
        .mail
        .create_draft(&me(), my_draft(&conn, Some(&thread), None, "周五见"))
        .unwrap();
    let d = h.mail.send_draft(&me(), &d.id).unwrap();
    assert_eq!(d.status, "approved");
    let dec = d.decision_id.clone().unwrap();
    assert_eq!(
        decision(&h, &dec).undo,
        json!({ "module": steve_mail::drafts::SUBJECT_TYPE }),
        "the approval is undoable"
    );

    h.env.clock.advance(5_000);
    undo_decision(&h, &dec).unwrap();
    assert_eq!(draft(&h, &d.id).status, "pending");
    assert_eq!(draft(&h, &d.id).send_at, None);
    let back = decision(&h, &dec);
    assert_eq!(back.status, "open");
    assert!(back.undo.is_null());
    assert_eq!(audits(&h, "decision.undo").len(), 1);
    h.env.clock.advance(60_000);
    assert!(h.mail.send_due().await.unwrap().is_empty());
    assert!(h.smtp.sent().is_empty(), "nothing went out");

    // approved again and sent: the undo is refused and the decision stays answered
    dispatch(&h.env.modules, &me(), &dec, "send", "")
        .unwrap()
        .unwrap();
    h.env.clock.advance(10_001);
    let sent = h.mail.send_due().await.unwrap();
    assert!(matches!(sent[0].1, SendOutcome::Sent { .. }), "{sent:?}");
    let e = undo_decision(&h, &dec).unwrap_err();
    assert_eq!(e, ApiError::conflict(MSG_ALREADY_SENT));
    assert_eq!(e.status(), 409);
    assert_eq!(decision(&h, &dec).status, "resolved");
    assert_eq!(draft(&h, &d.id).status, "sent");
    assert_eq!(audits(&h, "decision.undo").len(), 1);
}

/// A persisted claim left by a stopped process is never presumed safe to resend.
#[tokio::test(flavor = "multi_thread")]
async fn startup_recovers_claim_to_unknown_and_only_human_can_retry() {
    let h = setup().await;
    let (conn, thread) = with_thread(&h).await;
    let d = h
        .mail
        .create_draft(&me(), my_draft(&conn, Some(&thread), None, "recovery"))
        .unwrap();
    h.mail.send_draft(&me(), &d.id).unwrap();
    h.env
        .store()
        .with_conn(|c| {
            c.execute(
                "UPDATE mail_drafts SET status = 'sending' WHERE id = ?",
                [&d.id],
            )?;
            Ok(())
        })
        .unwrap();
    h.env.clock.advance(7 * DAY_MS);
    assert!(h.mail.send_due().await.unwrap().is_empty());
    assert_eq!(h.mail.execute(&d.id).await.unwrap(), SendOutcome::Skipped);
    assert_eq!(h.mail.clone().recover_sending().unwrap(), 0);
    let row = draft(&h, &d.id);
    assert_eq!(row.status, "unknown");
    assert_eq!(row.send_at, None);
    assert!(h.smtp.sent().is_empty());
    assert_eq!(audits(&h, "mail.send.unknown").len(), 1);
    let dec = row.decision_id.unwrap();
    assert_eq!(
        decision(&h, &dec).options,
        ["confirmed_not_sent", "discard"]
    );
    assert!(
        dispatch(
            &h.env.modules,
            &caller(WS, "arbitrary-bot"),
            &dec,
            "confirmed_not_sent",
            ""
        )
        .unwrap()
        .is_err()
    );
    let yolo = caller(WS, steve_modules::outbound::YOLO_ACTOR);
    assert!(
        dispatch(&h.env.modules, &yolo, &dec, "confirmed_not_sent", "")
            .unwrap()
            .is_err()
    );
    assert!(
        dispatch(&h.env.modules, &yolo, &dec, "discard", "")
            .unwrap()
            .is_err()
    );
    assert!(
        dispatch(&h.env.modules, &me(), &dec, "retry", "")
            .unwrap()
            .is_err()
    );
    assert!(
        dispatch(
            &h.env.modules,
            &caller(WS2, ME),
            &dec,
            "confirmed_not_sent",
            ""
        )
        .unwrap()
        .is_err()
    );
    dispatch(
        &h.env.modules,
        &me(),
        &dec,
        "confirmed_not_sent",
        "已核实未发送",
    )
    .unwrap()
    .unwrap();
    let outcomes = h.mail.send_due().await.unwrap();
    assert!(matches!(outcomes[0].1, SendOutcome::Sent { .. }));
    assert_eq!(h.smtp.sent().len(), 1);
    assert_eq!(draft(&h, &d.id).status, "sent");
}

#[tokio::test(flavor = "multi_thread")]
async fn accepted_mail_with_lost_ack_is_unknown_and_not_sent_again() {
    let h = setup().await;
    let (conn, thread) = with_thread(&h).await;
    let d = h
        .mail
        .create_draft(&me(), my_draft(&conn, Some(&thread), None, "lost ack"))
        .unwrap();
    h.mail.send_draft(&me(), &d.id).unwrap();
    h.env.clock.advance(10_000);
    h.smtp.fail_next("AFTER_ACCEPT", "disconnect");
    let outcomes = h.mail.send_due().await.unwrap();
    assert!(
        matches!(outcomes[0].1, SendOutcome::Unknown { .. }),
        "{outcomes:?}"
    );
    assert_eq!(h.smtp.sent().len(), 1);
    let row = draft(&h, &d.id);
    assert_eq!(row.status, "unknown");
    assert!(h.mail.send_due().await.unwrap().is_empty());
    assert_eq!(h.mail.execute(&d.id).await.unwrap(), SendOutcome::Skipped);
    assert_eq!(h.smtp.sent().len(), 1);
    assert!(audits(&h, "mail.send.failed").is_empty());
    let audit = audits(&h, "mail.send.attempt");
    let mime = String::from_utf8_lossy(&h.smtp.sent()[0].data).to_string();
    assert!(mime.contains(audit[0]["message_id"].as_str().unwrap()));
    // Abandoning the pending action is explicitly not a provider recall.
    dispatch(
        &h.env.modules,
        &me(),
        &row.decision_id.unwrap(),
        "discard",
        "已核实发出，不再重发",
    )
    .unwrap()
    .unwrap();
    assert_eq!(draft(&h, &d.id).status, "discarded");
    assert_eq!(h.smtp.sent().len(), 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn accepted_mail_with_failed_local_commit_is_unknown() {
    let h = setup().await;
    let (conn, thread) = with_thread(&h).await;
    let d = h
        .mail
        .create_draft(&me(), my_draft(&conn, Some(&thread), None, "commit fault"))
        .unwrap();
    h.mail.send_draft(&me(), &d.id).unwrap();
    h.env.clock.advance(10_000);
    h.env.store().with_conn(|c| {
        c.execute_batch("CREATE TRIGGER reject_outgoing BEFORE INSERT ON mail_messages WHEN NEW.direction = 'out' BEGIN SELECT RAISE(ABORT, 'test outgoing record fault'); END")?;
        Ok(())
    }).unwrap();
    let outcomes = h.mail.send_due().await.unwrap();
    assert!(
        matches!(outcomes[0].1, SendOutcome::Unknown { .. }),
        "{outcomes:?}"
    );
    assert_eq!(h.smtp.sent().len(), 1);
    assert_eq!(draft(&h, &d.id).status, "unknown");
    assert!(h.mail.send_due().await.unwrap().is_empty());
    assert_eq!(h.smtp.sent().len(), 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn active_send_is_not_recovered_and_cancellation_becomes_unknown() {
    let h = setup().await;
    let (conn, thread) = with_thread(&h).await;
    let d = h
        .mail
        .create_draft(&me(), my_draft(&conn, Some(&thread), None, "cancel fault"))
        .unwrap();
    h.mail.send_draft(&me(), &d.id).unwrap();
    h.env.clock.advance(10_000);
    h.smtp.fail_next("AFTER_ACCEPT", "hold");
    let mail = h.mail.clone();
    let id = d.id.clone();
    let task = tokio::spawn(async move { mail.execute(&id).await });
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        while h.smtp.sent().is_empty() {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(h.mail.recover_sending().unwrap(), 0);
    assert_eq!(draft(&h, &d.id).status, "sending");
    assert_eq!(h.mail.execute(&d.id).await.unwrap(), SendOutcome::Skipped);
    assert!(h.mail.send_due().await.unwrap().is_empty());
    task.abort();
    assert!(task.await.unwrap_err().is_cancelled());
    assert_eq!(draft(&h, &d.id).status, "unknown");
    assert_eq!(h.smtp.sent().len(), 1);
    assert_eq!(audits(&h, "mail.send.unknown").len(), 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn sent_folder_copy_failure_does_not_turn_submission_into_failure() {
    let h = setup().await;
    let (conn, thread) = with_thread(&h).await;
    let d = h
        .mail
        .create_draft(&me(), my_draft(&conn, Some(&thread), None, "append fault"))
        .unwrap();
    h.mail.send_draft(&me(), &d.id).unwrap();
    h.env.clock.advance(10_000);
    h.imap.fail_next("APPEND", "NO append forbidden");
    let out = h.mail.send_due().await.unwrap();
    assert!(matches!(out[0].1, SendOutcome::Sent { .. }), "{out:?}");
    assert_eq!(draft(&h, &d.id).status, "sent");
    assert_eq!(h.smtp.sent().len(), 1);
    assert!(audits(&h, "mail.send.failed").is_empty());
    assert!(audits(&h, "mail.send.unknown").is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn preparation_and_failure_record_errors_do_not_leave_live_claim_stuck() {
    let h = setup().await;
    let (conn, thread) = with_thread(&h).await;
    let d = h
        .mail
        .create_draft(&me(), my_draft(&conn, Some(&thread), None, "prepare fault"))
        .unwrap();
    h.mail.send_draft(&me(), &d.id).unwrap();
    h.env.clock.advance(10_000);
    h.env.store().with_conn(|c| {
        // The approval and the connection are still valid (so the draft is claimed), but its adapter
        // is gone, so preparing the submission fails after the claim.
        c.execute("UPDATE connections SET adapter = 'gone' WHERE id = ?", [conn.as_str()])?;
        c.execute_batch("CREATE TRIGGER reject_failure BEFORE UPDATE OF status ON mail_drafts WHEN NEW.status = 'failed' BEGIN SELECT RAISE(ABORT, 'test failure record fault'); END")?;
        Ok(())
    }).unwrap();
    assert!(h.mail.execute(&d.id).await.is_err());
    assert_eq!(draft(&h, &d.id).status, "unknown");
    assert!(h.smtp.sent().is_empty());
    assert_eq!(audits(&h, "mail.send.unknown").len(), 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn optional_copy_hang_after_smtp_acceptance_stays_sent() {
    for command in ["APPEND", "LOGOUT"] {
        let h = setup().await;
        let (conn, thread) = with_thread(&h).await;
        let d = h
            .mail
            .create_draft(&me(), my_draft(&conn, Some(&thread), None, "copy timeout"))
            .unwrap();
        h.mail.send_draft(&me(), &d.id).unwrap();
        h.env.clock.advance(10_000);
        h.imap.fail_next(command, "HOLD");
        let mail = h.mail.clone();
        let id = d.id.clone();
        let task = tokio::spawn(async move { mail.execute(&id).await });
        tokio::time::timeout(std::time::Duration::from_secs(4), async {
            while draft(&h, &d.id).status != "sent" {
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("submission is recorded before optional copy returns");
        assert_eq!(h.smtp.sent().len(), 1);
        assert_eq!(h.mail.execute(&d.id).await.unwrap(), SendOutcome::Skipped);
        let out = tokio::time::timeout(
            steve_mail::send::COPY_TIMEOUT + std::time::Duration::from_secs(2),
            task,
        )
        .await
        .expect("whole copy including logout is bounded")
        .unwrap()
        .unwrap();
        assert!(
            matches!(out, SendOutcome::Sent { .. }),
            "{command}: {out:?}"
        );
        assert_eq!(draft(&h, &d.id).status, "sent");
        assert!(audits(&h, "mail.send.unknown").is_empty());
        assert!(audits(&h, "mail.send.failed").is_empty());
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn explicit_smtp_451_rejection_is_failed_not_unknown() {
    for command in ["RCPT", "DATA", "."] {
        let h = setup().await;
        let (conn, thread) = with_thread(&h).await;
        let d = h
            .mail
            .create_draft(
                &me(),
                my_draft(&conn, Some(&thread), None, "temporary refusal"),
            )
            .unwrap();
        h.mail.send_draft(&me(), &d.id).unwrap();
        h.env.clock.advance(10_000);
        h.smtp.fail_next(command, "451 4.3.0 try later");
        let out = h.mail.execute(&d.id).await.unwrap();
        assert!(
            matches!(out, SendOutcome::Failed { .. }),
            "{command}: {out:?}"
        );
        assert_eq!(draft(&h, &d.id).status, "failed");
        assert!(h.smtp.sent().is_empty());
        assert!(audits(&h, "mail.send.unknown").is_empty());
        let decisions = h
            .env
            .store()
            .reader(|c| repo::open_draft_decisions(c, WS, &d.id))
            .unwrap();
        assert_eq!(decisions.len(), 1);
        assert_eq!(decision(&h, &decisions[0]).options, ["retry", "discard"]);
        dispatch(&h.env.modules, &me(), &decisions[0], "retry", "")
            .unwrap()
            .unwrap();
        assert!(matches!(
            h.mail.execute(&d.id).await.unwrap(),
            SendOutcome::Sent { .. }
        ));
        assert_eq!(h.smtp.sent().len(), 1);
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn dismissed_unknown_card_is_recreated_even_without_item_or_thread() {
    let h = setup().await;
    let conn = h.connect().await;
    let d = h
        .mail
        .create_draft(&me(), my_draft(&conn, None, None, "orphan unknown"))
        .unwrap();
    h.mail.send_draft(&me(), &d.id).unwrap();
    h.env.clock.advance(10_000);
    h.smtp.fail_next("AFTER_ACCEPT", "disconnect");
    assert!(matches!(
        h.mail.execute(&d.id).await.unwrap(),
        SendOutcome::Unknown { .. }
    ));
    let row = draft(&h, &d.id);
    assert!(row.item_id.is_none() && row.thread_id.is_none());
    let old = row.decision_id.unwrap();
    // Same persisted effect as Core::dismiss_decision for an orphan card: no undo target.
    h.env
        .store()
        .transaction(|tx| {
            steve_store::base::decisions::settle(
                tx,
                &old,
                &json!({"dismissed": true}),
                "resolved",
                &Value::Null,
            )
        })
        .unwrap();
    assert!(decision(&h, &old).undo.is_null());
    assert!(h.mail.send_due().await.unwrap().is_empty());
    let reopened = draft(&h, &d.id).decision_id.unwrap();
    assert_ne!(reopened, old);
    assert_eq!(decision(&h, &reopened).status, "open");
    assert_eq!(
        decision(&h, &reopened).options,
        ["confirmed_not_sent", "discard"]
    );
    assert_eq!(h.mail.repair_unknown_decisions().unwrap(), 0);
    assert_eq!(audits(&h, "mail.send.reconciliation_reopened").len(), 1);
    assert_eq!(h.smtp.sent().len(), 1);
    // Explicit reconciliation remains final; the repair job never revives abandoned mail.
    dispatch(&h.env.modules, &me(), &reopened, "discard", "已核实发出")
        .unwrap()
        .unwrap();
    assert!(h.mail.send_due().await.unwrap().is_empty());
    assert_eq!(draft(&h, &d.id).status, "discarded");
    assert_eq!(h.mail.repair_unknown_decisions().unwrap(), 0);
    assert!(
        h.env
            .store()
            .reader(|c| repo::open_draft_decisions(c, WS, &d.id))
            .unwrap()
            .is_empty()
    );
    assert_eq!(h.smtp.sent().len(), 1);
}

fn set_connection(h: &H, conn: &str, status: &str, caps: &[&str]) {
    let caps = serde_json::to_string(caps).unwrap();
    h.env
        .store()
        .transaction(|tx| {
            tx.execute(
                "UPDATE connections SET status = ?, caps = ? WHERE id = ?",
                rusqlite::params![status, caps, conn],
            )?;
            Ok::<_, steve_store::StoreError>(())
        })
        .unwrap();
}

/// Y4: the connection is checked when the mail goes out, not when it was approved. Paused: the
/// approved draft waits and goes out once resumed; nothing reaches SMTP meanwhile.
#[tokio::test(flavor = "multi_thread")]
async fn a_paused_connection_holds_an_approved_mail_until_resumed() {
    let h = setup().await;
    let (conn, thread) = with_thread(&h).await;
    let d = h
        .mail
        .create_draft(&me(), my_draft(&conn, Some(&thread), None, "hi"))
        .unwrap();
    h.mail.send_draft(&me(), &d.id).unwrap();
    set_connection(&h, &conn, "paused", &["mail.read", "mail.send"]);
    h.env.clock.advance(10_000);
    let out = h.mail.send_due().await.unwrap();
    assert!(out.is_empty(), "{out:?}");
    // also when it is executed directly
    let o = h.mail.execute(&d.id).await.unwrap();
    assert!(matches!(o, SendOutcome::Skipped), "{o:?}");
    assert_eq!(draft(&h, &d.id).status, "approved");
    assert!(h.smtp.sent().is_empty());
    set_connection(&h, &conn, "ok", &["mail.read", "mail.send"]);
    let out = h.mail.send_due().await.unwrap();
    assert!(matches!(out[0].1, SendOutcome::Sent { .. }), "{out:?}");
    assert_eq!(h.smtp.sent().len(), 1);
}

/// Y4: `mail.send` turned off (or the connection revoked) after the approval: refused with the
/// reason, nothing reaches SMTP.
#[tokio::test(flavor = "multi_thread")]
async fn sending_turned_off_after_approval_refuses_the_mail() {
    for (status, caps) in [
        ("ok", &["mail.read"][..]),
        ("revoked", &["mail.read", "mail.send"][..]),
    ] {
        let h = setup().await;
        let (conn, thread) = with_thread(&h).await;
        let d = h
            .mail
            .create_draft(&me(), my_draft(&conn, Some(&thread), None, "hi"))
            .unwrap();
        h.mail.send_draft(&me(), &d.id).unwrap();
        set_connection(&h, &conn, status, caps);
        h.env.clock.advance(10_000);
        let out = h.mail.send_due().await.unwrap();
        assert!(
            matches!(out[0].1, SendOutcome::Blocked { .. }),
            "{status}: {out:?}"
        );
        let row = draft(&h, &d.id);
        assert_eq!(row.status, "failed", "{status}");
        assert!(row.error.is_some(), "{status}");
        assert_eq!(
            audits(&h, "mail.send.blocked").last().unwrap()["reason"],
            "connection"
        );
        assert!(h.smtp.sent().is_empty(), "{status}");
    }
}
