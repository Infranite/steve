//! Characterization before SG17's production extraction. Reuses the existing fake IMAP/SMTP.
mod support;
use serde_json::json;
use steve_mail::{
    AdapterError, FetchedMessage, FolderState, MailError, MailSink, ScopeMode, SyncState,
    compose::QuickDraft,
    drafts::{NewDraft, RcptIn},
    fake::fixtures,
    repo,
};
use steve_modules::testkit::WS;
use support::*;
const NOW: i64 = 1_800_000_000_000;

fn golden(name: &str, bytes: &[u8]) {
    let file = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join(format!("tests/fixtures/sg17/{name}.json"));
    if std::env::var_os("STEVE_SG17_GENERATE_GOLDENS").is_some() {
        assert!(!file.exists(), "do not overwrite a baseline");
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(
            file,
            serde_json::to_vec_pretty(&String::from_utf8(bytes.to_vec()).unwrap()).unwrap(),
        )
        .unwrap();
    } else {
        let expected: String = serde_json::from_slice(&std::fs::read(file).unwrap()).unwrap();
        assert_eq!(bytes, expected.as_bytes(), "{name}");
    }
}
fn incoming(conn: &str) -> repo::MessageRow {
    repo::MessageRow {
        id: "sg17-original".into(),
        workspace_id: WS.into(),
        connection_id: conn.into(),
        thread_id: "sg17-thread".into(),
        ext_id: "INBOX:1:1".into(),
        message_id: Some("<sg17-original@example.com>".into()),
        in_reply_to: None,
        refs: vec![],
        folders: vec!["INBOX".into()],
        flags: vec![],
        direction: "in".into(),
        from_addr: "alice@example.com".into(),
        from_name: "Alice".into(),
        from_account_id: None,
        from_party_id: None,
        reply_to: None,
        to_addrs: json!([{"addr":ADDR,"name":"Me"}]),
        cc_addrs: json!([{"addr":"bob@example.com","name":"Bob"}]),
        subject: "Budget review".into(),
        snippet: "Can we approve it?".into(),
        date: NOW - 1000,
        size: 30,
        has_attachments: false,
        bulk: false,
        body_text: Some("Can we approve it?\n> Previous message".into()),
        body_new: Some("Can we approve it?".into()),
        has_html: false,
        raw_path: None,
        purged: None,
        event_id: None,
        status: "received".into(),
        created: NOW - 1000,
    }
}
#[tokio::test(flavor = "multi_thread")]
async fn quick_draft_prompt_and_persisted_result_table() {
    for (name, model_all, override_all, wanted_cc) in [
        ("prompt-reply", false, None, 0),
        ("prompt-all", true, None, 1),
        ("prompt-override", true, Some(false), 0),
    ] {
        let h = setup().await;
        h.env.clock.set(NOW);
        let conn = h.connect().await;
        h.env
            .store()
            .transaction(|tx| -> steve_mail::MailResult<()> {
                repo::insert_thread(
                    tx,
                    &repo::ThreadRow {
                        id: "sg17-thread".into(),
                        workspace_id: WS.into(),
                        connection_id: conn.clone(),
                        thread_key: "sg17-thread-key".into(),
                        subject: "Thread fallback".into(),
                        last_at: NOW,
                        count: 1,
                        unread: 1,
                        participants: vec![],
                        state: "active".into(),
                        visibility: "private".into(),
                        created_by: steve_modules::testkit::ME.into(),
                    },
                    NOW,
                )?;
                repo::insert_message(tx, &incoming(&conn))?;
                repo::ensure_settings(tx, WS, &conn, NOW)?;
                let mut settings = repo::settings(tx, &conn)?.unwrap_or_default();
                settings.signature = "Regards,\nMe".into();
                repo::update_settings(tx, &conn, &settings, NOW)?;
                Ok(())
            })
            .unwrap();
        h.env.gateway.reply(json!({"subject":"MODEL MUST NOT CHOOSE THE SUBJECT","body":"  Reviewed.  ","reply_all":model_all,"to":["outside@example.invalid"],"needs_attention":true,"why":"Needs a decision","cites":[]}).to_string());
        let out = h
            .mail
            .quick_draft(
                &me(),
                QuickDraft {
                    thread_id: "sg17-thread".into(),
                    message_id: Some("sg17-original".into()),
                    instruction: Some("Keep this reply brief.".into()),
                    reply_all: override_all,
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        let calls = h.env.gateway.calls.lock().unwrap().clone();
        assert_eq!(calls.len(), 1);
        golden(name, calls[0].1.as_bytes());
        let d = h
            .env
            .store()
            .reader(|c| repo::draft(c, &out.id))
            .unwrap()
            .unwrap();
        assert_eq!(d.subject, "Re: Budget review");
        assert_eq!(d.to.len(), 1);
        assert_eq!(d.to[0].addr, "alice@example.com");
        assert_eq!(d.cc.len(), wanted_cc);
        if wanted_cc == 1 {
            assert_eq!(d.cc[0].addr, "bob@example.com");
        }
        assert!(d.bcc.is_empty());
        assert!(
            !serde_json::to_string(&d.to)
                .unwrap()
                .contains("outside@example.invalid")
        );
        assert_eq!(d.body, "Reviewed.\n\nRegards,\nMe");
        assert_eq!(d.mode, if wanted_cc == 1 { "reply_all" } else { "reply" });
        assert_eq!(d.author, "steve");
        let basis = d.basis.as_ref().unwrap();
        assert_eq!(basis["why"], "Needs a decision");
        assert_eq!(basis["needs_attention"], true);
        assert_eq!(basis["cites"], json!([]));
        assert!(d.decision_id.is_some());
        assert!(h.smtp.sent().is_empty());
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn smtp_stage_and_failure_classification_table() {
    for (stage, response, temporary) in [
        ("RCPT", "451 4.3.0 later", false),
        ("RCPT", "550 5.1.1 no such user", false),
        (".", "451 4.3.0 later", false),
        (".", "550 5.7.1 refused", false),
        ("AFTER_ACCEPT", "disconnect", true),
    ] {
        let h = setup().await;
        let conn = h.connect().await;
        let adapter = h.mail.adapter("imap").unwrap();
        let cx = h.conns.cx(WS, &conn).unwrap();
        // Connecting verifies SMTP with NOOP; isolate the submission session below.
        h.smtp.reset();
        h.smtp.fail_next(stage, response);
        let meta = steve_mail::SendMeta {
            from: ADDR.into(),
            rcpt: vec!["alice@example.com".into()],
            thread: None,
            append_sent: Some(false),
        };
        let error = adapter
            .send(
                &cx,
                b"From: me@example.com\r\nTo: alice@example.com\r\n\r\nbody\r\n",
                &meta,
            )
            .await
            .unwrap_err();
        assert!(
            if temporary {
                matches!(error, AdapterError::Transient(_))
            } else {
                matches!(error, AdapterError::Rejected(_))
            },
            "{stage}/{response}: {error:?}"
        );
        let stages: Vec<_> = h
            .smtp
            .commands()
            .into_iter()
            .filter_map(|c| {
                if c.starts_with("EHLO ") {
                    Some("EHLO")
                } else if c.starts_with("AUTH ") {
                    Some("AUTH")
                } else if c.starts_with("MAIL ") {
                    Some("MAIL")
                } else if c.starts_with("RCPT ") {
                    Some("RCPT")
                } else if c == "DATA" {
                    Some("DATA")
                } else {
                    None
                }
            })
            .collect();
        assert_eq!(
            stages,
            if stage == "RCPT" {
                vec!["EHLO", "AUTH", "MAIL", "RCPT"]
            } else {
                vec!["EHLO", "AUTH", "MAIL", "RCPT", "DATA"]
            },
            "{stage}: {:?}",
            h.smtp.commands()
        );
        assert_eq!(h.smtp.sent().len(), usize::from(stage == "AFTER_ACCEPT"));
    }
}

#[derive(Default)]
struct Sink {
    messages: Vec<FetchedMessage>,
    updates: Vec<(String, Vec<String>)>,
}
#[async_trait::async_trait]
impl MailSink for Sink {
    async fn put_batch(&mut self, m: Vec<FetchedMessage>) -> Result<(), MailError> {
        self.messages.extend(m);
        Ok(())
    }
    async fn flags_batch(&mut self, m: Vec<(String, Vec<String>)>) -> Result<(), MailError> {
        self.updates.extend(m);
        Ok(())
    }
    async fn removed(&mut self, _: &str) -> Result<(), MailError> {
        Ok(())
    }
}
#[tokio::test(flavor = "multi_thread")]
async fn imap_first_incremental_and_validity_reset_table() {
    for condstore in [false, true] {
        let h = setup_with(Opts {
            imap: steve_mail::fake::ImapConfig {
                condstore,
                ..Default::default()
            },
            ..Default::default()
        })
        .await;
        h.env.clock.set(NOW);
        h.inject("INBOX", fixtures::get("gbk.eml").unwrap());
        let conn = h.connect().await;
        let adapter = h.mail.adapter("imap").unwrap();
        let cx = h.conns.cx(WS, &conn).unwrap();
        let mut state = SyncState {
            folders: vec![FolderState {
                ext_id: "INBOX".into(),
                name: "INBOX".into(),
                role: "inbox".into(),
                mode: ScopeMode::Receive,
                uidvalidity: None,
                last_uid: None,
                modseq: None,
                cursor: None,
                synced: None,
                flags_checked: None,
            }],
            cursor: None,
            sync_days: 365,
            now: NOW,
        };
        for phase in ["first", "incremental", "validity-reset"] {
            let old = state.folders[0].uidvalidity;
            if phase == "incremental" {
                h.imap.set_flags(ADDR, "INBOX", 1, &["\\Seen"]).unwrap();
                state.now += steve_mail::adapters::imap::FLAG_RECHECK_MS;
            }
            if phase == "validity-reset" {
                h.imap.bump_uidvalidity(ADDR, "INBOX").unwrap();
            }
            let start = h.imap.commands().len();
            let mut sink = Sink::default();
            let report = adapter.sync(&cx, &mut state, &mut sink).await.unwrap();
            let commands = &h.imap.commands()[start..];
            assert!(commands.iter().any(|c| c.contains(" EXAMINE ")));
            assert_eq!(state.folders[0].synced, Some(state.now));
            if phase == "incremental" {
                assert_eq!(report.fetched, 0);
                assert!(sink.messages.is_empty());
                assert!(commands.iter().all(|c| !c.contains(" SEARCH ")));
                assert_eq!(report.flag_updates, 1);
                assert_eq!(sink.updates.len(), 1);
                assert_eq!(sink.updates[0].1, vec!["seen".to_string()]);
                assert_eq!(
                    commands.iter().any(|c| c.contains("CHANGEDSINCE")),
                    condstore
                );
                if !condstore {
                    assert_eq!(state.folders[0].flags_checked, Some(state.now));
                }
            } else {
                assert_eq!(report.fetched, 1);
                assert_eq!(sink.messages.len(), 1);
                assert!(sink.messages[0].backfill);
                assert!(commands.iter().any(|c| c.contains(" UID SEARCH ")));
            }
            if phase == "validity-reset" {
                assert_ne!(state.folders[0].uidvalidity, old);
                assert_eq!(report.reset, vec!["INBOX"]);
            }
            assert_eq!(state.folders[0].last_uid, Some(1));
            assert!(state.folders[0].cursor.is_none());
        }
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn execute_authorization_and_transport_result_table() {
    for scenario in [
        "unapproved",
        "not-due",
        "uncertain",
        "temporary-rejection",
        "permanent",
        "sent",
    ] {
        let h = setup().await;
        let conn = h.connect().await;
        h.smtp.reset();
        let d = h
            .mail
            .create_draft(
                &me(),
                NewDraft {
                    connection_id: conn,
                    mode: "new".into(),
                    thread_id: None,
                    reply_to: None,
                    to: vec![RcptIn::Addr("alice@example.com".into())],
                    cc: None,
                    bcc: None,
                    subject: "SG17 send".into(),
                    body: "fixed body".into(),
                    files: None,
                },
            )
            .unwrap();
        if scenario != "unapproved" {
            h.mail.send_draft(&me(), &d.id).unwrap();
        }
        if scenario != "not-due" {
            h.env.clock.advance(10_000);
        }
        if scenario == "uncertain" {
            h.smtp.fail_next("AFTER_ACCEPT", "disconnect");
        }
        if scenario == "temporary-rejection" {
            h.smtp.fail_next("RCPT", "451 4.3.0 later");
        }
        if scenario == "permanent" {
            h.smtp.fail_next("RCPT", "550 5.1.1 no such user");
        }
        let out = h.mail.execute(&d.id).await.unwrap();
        let row = h
            .env
            .store()
            .reader(|c| repo::draft(c, &d.id))
            .unwrap()
            .unwrap();
        match scenario {
            "unapproved" => {
                assert_eq!(
                    out,
                    steve_mail::SendOutcome::Blocked {
                        reason: steve_mail::send::ERR_NO_APPROVAL.into()
                    }
                );
                assert_eq!(row.status, "draft");
            }
            "not-due" => {
                assert_eq!(out, steve_mail::SendOutcome::Skipped);
                assert_eq!(row.status, "approved");
            }
            "uncertain" => {
                assert!(
                    matches!(out, steve_mail::SendOutcome::Unknown { .. }),
                    "{out:?}"
                );
                assert_eq!(row.status, "unknown");
                assert_eq!(
                    row.error,
                    Some(format!(
                        "等待服务器确认时超时、断连或记录失败。{}",
                        steve_mail::send::ERR_UNKNOWN
                    ))
                );
            }
            "permanent" | "temporary-rejection" => {
                assert!(
                    matches!(out, steve_mail::SendOutcome::Failed { .. }),
                    "{out:?}"
                );
                assert_eq!(row.status, "failed");
            }
            _ => {
                assert!(
                    matches!(out, steve_mail::SendOutcome::Sent { .. }),
                    "{out:?}"
                );
                assert_eq!(row.status, "sent");
            }
        }
        let commands = h.smtp.commands();
        if matches!(scenario, "unapproved" | "not-due") {
            assert!(
                commands.is_empty(),
                "authorization must precede SMTP: {commands:?}"
            );
        } else {
            let names: Vec<_> = commands
                .iter()
                .filter_map(|c| {
                    if c.starts_with("MAIL ") {
                        Some("MAIL")
                    } else if c.starts_with("RCPT ") {
                        Some("RCPT")
                    } else if c == "DATA" {
                        Some("DATA")
                    } else {
                        None
                    }
                })
                .collect();
            assert_eq!(
                names,
                if matches!(scenario, "sent" | "uncertain") {
                    vec!["MAIL", "RCPT", "DATA"]
                } else {
                    vec!["MAIL", "RCPT"]
                }
            );
        }
        assert_eq!(
            h.smtp.sent().len(),
            usize::from(matches!(scenario, "sent" | "uncertain"))
        );
    }
}
