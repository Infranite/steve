//! #247: the real C5 rule/model checker and fake SMTP, including the final send boundary.
use steve_store::mail::fixtures as db;
mod support;

use std::sync::Arc;

use serde_json::json;
use steve_channels::disclosure::{CHECK_HEAD, Disclosure};
use steve_mail::compose::QuickDraft;
use steve_mail::drafts::{DraftPatch, NewDraft, RcptIn};
use steve_mail::{DraftHooks, SendOutcome};
use steve_modules::contacts::mcp::McpCx;
use steve_modules::decisions::dispatch;
use steve_modules::outbound::{Audience, CheckCx, CheckResult, KnowledgeUse, OutboundCheck};
use steve_modules::testkit::{ME, WS, caller};
use steve_modules::{GatewayOut, Tier};
use steve_store::mail as repo;
use tokio::sync::Notify;

use support::*;

const PASS_CHECK: &str = r#"{"verdict":"pass","spans":[]}"#;
const COMPOSE: &str = r#"{"body":"原来的正文","reply_all":false,"needs_attention":false,"why":"回复问题","cites":[]}"#;

struct Yolo(bool);
impl DraftHooks for Yolo {
    fn yolo_on(&self, _: &str, _: Option<&str>) -> bool {
        self.0
    }
}

struct NoModelProof;
#[async_trait::async_trait]
impl OutboundCheck for NoModelProof {
    async fn check(&self, cx: &CheckCx, a: &Audience, t: &str, k: &[KnowledgeUse]) -> CheckResult {
        let mut result = PassingCheck.check(cx, a, t, k).await;
        result.check_model = None;
        result
    }
}

async fn real(yolo: bool) -> H {
    let check = Disclosure::with_models(vec!["reviewer".into()]);
    let h = setup_with(Opts {
        check: Some(Arc::new(check.clone())),
        hooks: Some(Arc::new(Yolo(yolo))),
        ..Default::default()
    })
    .await;
    check.bind(h.env.store().clone(), h.env.gateway.clone());
    script(&h, PASS_CHECK, "reviewer");
    h
}

fn script(h: &H, text: &str, model: &str) {
    h.env.gateway.unscript(CHECK_HEAD);
    h.env.gateway.script_out(
        CHECK_HEAD,
        GatewayOut {
            text: text.into(),
            model: model.into(),
            ..Default::default()
        },
    );
}

async fn thread(h: &H) -> (String, String) {
    h.inject("INBOX", steve_mail::fake::fixtures::thread()[0]);
    let conn = h.connect().await;
    h.sync(&conn).await;
    let thread = h.by_subject(&conn, "预算审批流程").thread_id;
    (conn, thread)
}

fn new_draft(conn: &str, body: &str) -> NewDraft {
    serde_json::from_value(json!({
        "connection_id": conn, "mode": "new", "to": ["to@outside.example"],
        "cc": ["cc@outside.example"], "bcc": ["bcc@outside.example"],
        "subject": "Final subject", "body": body
    }))
    .unwrap()
}

fn row(h: &H, id: &str) -> repo::DraftRow {
    h.env.store().read(|c| repo::draft(c, id)).unwrap().unwrap()
}

fn prompts(h: &H) -> Vec<String> {
    h.env
        .gateway
        .prompts
        .lock()
        .unwrap()
        .iter()
        .filter(|p| p.starts_with(CHECK_HEAD))
        .cloned()
        .collect()
}

fn approve(h: &H, id: &str, edit: &str, yolo: bool) {
    let d = row(h, id);
    dispatch(
        &h.env.modules,
        &caller(WS, if yolo { "yolo" } else { ME }),
        d.decision_id.as_deref().unwrap(),
        "send",
        edit,
    )
    .unwrap()
    .unwrap();
    h.env.clock.advance(10_000);
}

async fn direct(h: &H, body: &str) -> String {
    let conn = h.connect().await;
    let d = h.mail.create_draft(&me(), new_draft(&conn, body)).unwrap();
    h.mail.send_draft(&me(), &d.id).unwrap();
    h.env.clock.advance(10_000);
    d.id
}

#[tokio::test(flavor = "multi_thread")]
async fn compose_approval_and_edited_approval_check_the_wire_body_with_yolo_on_or_off() {
    for yolo in [false, true] {
        for edit in ["", "编辑后要发送的正文"] {
            let h = real(yolo).await;
            let (_, thread) = thread(&h).await;
            h.env.gateway.reply(COMPOSE);
            let d = h
                .mail
                .quick_draft(
                    &me(),
                    QuickDraft {
                        thread_id: thread,
                        ..Default::default()
                    },
                )
                .await
                .unwrap();
            assert_eq!(
                prompts(&h).len(),
                usize::from(yolo),
                "only previews depend on YOLO"
            );
            // Even a successful explicit preview is never permission to reuse it at send time.
            h.mail.check_draft(&me(), &d.id).await.unwrap();
            let before = prompts(&h).len();
            approve(&h, &d.id, edit, yolo);
            assert!(matches!(
                h.mail.execute(&d.id).await.unwrap(),
                SendOutcome::Sent { .. }
            ));
            assert_eq!(prompts(&h).len(), before + 1);
            let sent = h.smtp.sent();
            let parsed = mail_parser::MessageParser::default()
                .parse(&sent[0].data)
                .unwrap();
            let body = parsed.body_text(0).unwrap();
            let expected = if edit.is_empty() {
                "原来的正文"
            } else {
                edit
            };
            assert!(body.contains(expected), "{body}");
            assert_eq!(body.starts_with("[AI] "), yolo);
            let prompt = prompts(&h).pop().unwrap();
            assert!(prompt.contains(expected), "{prompt}");
            assert_eq!(prompt.contains("[AI] "), yolo, "check the final mark too");
            let basis = row(&h, &d.id).basis.unwrap();
            assert_eq!(basis["verdict"], "pass");
            assert_eq!(basis["check_model"], "reviewer");
            assert!(!basis["checked_content_hash"].as_str().unwrap().is_empty());
        }
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn direct_send_checks_edited_to_cc_bcc_and_body_and_never_sends_twice() {
    let h = real(false).await;
    let conn = h.connect().await;
    let d = h.mail.create_draft(&me(), new_draft(&conn, "old")).unwrap();
    h.mail
        .update_draft(
            &me(),
            &d.id,
            DraftPatch {
                to: Some(vec![RcptIn::Full {
                    addr: "final-to@outside.example".into(),
                    name: "Same name".into(),
                }]),
                cc: Some(vec![RcptIn::Addr("final-cc@outside.example".into())]),
                bcc: Some(vec![RcptIn::Addr("final-bcc@outside.example".into())]),
                body: Some("[AI] final body".into()),
                ..Default::default()
            },
        )
        .unwrap();
    let d = h.mail.send_draft(&me(), &d.id).unwrap();
    assert!(h.mail.send_draft(&me(), &d.id).is_err());
    let replay = dispatch(
        &h.env.modules,
        &me(),
        d.decision_id.as_deref().unwrap(),
        "send",
        "",
    )
    .unwrap()
    .unwrap();
    assert_eq!(replay.status, steve_types::DecisionStatus::Resolved);
    h.env.clock.advance(10_000);
    let (a, b) = tokio::join!(h.mail.execute(&d.id), h.mail.execute(&d.id));
    assert!(matches!(a.unwrap(), SendOutcome::Sent { .. }));
    assert_eq!(b.unwrap(), SendOutcome::Skipped);
    assert_eq!(h.mail.execute(&d.id).await.unwrap(), SendOutcome::Skipped);
    let sent = h.smtp.sent();
    assert_eq!(sent.len(), 1);
    for addr in [
        "final-to@outside.example",
        "final-cc@outside.example",
        "final-bcc@outside.example",
    ] {
        assert!(sent[0].rcpt.iter().any(|a| a == addr));
        assert!(prompts(&h)[0].contains(addr));
    }
    assert!(prompts(&h)[0].contains("Same name"));
    assert!(prompts(&h)[0].contains("Final subject\n\nfinal body"));
    assert!(!prompts(&h)[0].contains("[AI] final body"));
    assert_eq!(prompts(&h).len(), 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn mcp_draft_and_revision_use_the_same_final_send_check() {
    let h = real(false).await;
    let (_, thread) = thread(&h).await;
    db::link_agent_thread(h.env.store(), &thread).unwrap();
    let cx = McpCx {
        ws: WS.into(),
        user: ME.into(),
        bot_id: "mail".into(),
        item_id: Some("ST-1".into()),
        session_id: Some("ses_mail".into()),
    };
    for body in ["MCP initial", "MCP revision"] {
        steve_mail::mcp::call(&h.mail, &cx, steve_mail::mcp::DRAFT, &json!({"body":body}))
            .await
            .unwrap();
    }
    assert!(prompts(&h).is_empty());
    let id = db::item_drafts(h.env.store(), "ST-1").remove(0);
    approve(&h, &id, "MCP final owner edit", false);
    assert!(matches!(
        h.mail.execute(&id).await.unwrap(),
        SendOutcome::Sent { .. }
    ));
    assert_eq!(prompts(&h).len(), 1);
    assert!(prompts(&h)[0].contains("MCP final owner edit"));
    assert_eq!(h.smtp.sent().len(), 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn model_failure_unreadable_leak_and_missing_second_model_all_hold_mail() {
    for case in [
        "error",
        "unreadable",
        "leak",
        "same_model",
        "missing_check",
        "local_pass",
    ] {
        let h = if case == "missing_check" {
            setup_with(Opts {
                check: None,
                ..Default::default()
            })
            .await
        } else if case == "local_pass" {
            setup_with(Opts {
                check: Some(Arc::new(NoModelProof)),
                ..Default::default()
            })
            .await
        } else {
            real(false).await
        };
        let id = direct(&h, "final body").await;
        match case {
            "error" => {
                h.env.gateway.unscript(CHECK_HEAD);
                h.env
                    .gateway
                    .script_fail(CHECK_HEAD, "fixture model unavailable");
            }
            "unreadable" => script(&h, "no readable verdict", "reviewer"),
            "leak" => script(&h, r#"{"verdict":"leak","spans":[]}"#, "reviewer"),
            "same_model" => {
                db::set_reviewer_model(h.env.store(), &id).unwrap();
            }
            _ => {}
        }
        assert!(
            matches!(
                h.mail.execute(&id).await.unwrap(),
                SendOutcome::Blocked { .. }
            ),
            "{case}"
        );
        let d = row(&h, &id);
        assert_eq!(d.status, "failed", "{case}");
        assert!(d.error.is_some(), "{case}");
        assert_ne!(d.basis.unwrap()["verdict"], "pass", "{case}");
        assert!(h.smtp.sent().is_empty(), "{case}");
        assert!(
            h.env
                .gateway
                .calls
                .lock()
                .unwrap()
                .iter()
                .filter(|(_, p)| p.starts_with(CHECK_HEAD))
                .all(|(t, _)| *t == Tier::Fast)
        );
    }
}

fn source(h: &H, id: &str) {
    db::give_restricted_basis(h.env.store(), &id).unwrap();
}

fn retry(h: &H, id: &str) {
    let dec = db::open_failure(h.env.store(), id);
    dispatch(&h.env.modules, &me(), &dec, "retry", "")
        .unwrap()
        .unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn a_passing_preview_cannot_bypass_a_failed_final_check_and_retry_checks_again() {
    let h = real(false).await;
    let conn = h.connect().await;
    let d = h
        .mail
        .create_draft(&me(), new_draft(&conn, "body"))
        .unwrap();
    assert_eq!(
        h.mail
            .check_draft(&me(), &d.id)
            .await
            .unwrap()
            .basis
            .unwrap()["verdict"],
        "pass"
    );
    script(&h, "unreadable after a successful preview", "reviewer");
    h.mail.send_draft(&me(), &d.id).unwrap();
    h.env.clock.advance(10_000);
    assert!(matches!(
        h.mail.execute(&d.id).await.unwrap(),
        SendOutcome::Blocked { .. }
    ));
    assert!(h.smtp.sent().is_empty());
    assert_eq!(prompts(&h).len(), 2);
    script(&h, PASS_CHECK, "reviewer");
    retry(&h, &d.id);
    assert!(matches!(
        h.mail.execute(&d.id).await.unwrap(),
        SendOutcome::Sent { .. }
    ));
    assert_eq!(h.smtp.sent().len(), 1);
    assert_eq!(prompts(&h).len(), 3);
}

#[tokio::test(flavor = "multi_thread")]
async fn rules_and_missing_knowledge_stay_blocked_even_after_repeated_checks_and_retries() {
    for missing in [false, true] {
        let h = real(false).await;
        let id = direct(&h, "预算为 420万元").await;
        source(&h, &id);
        if missing {
            db::delete_restricted_note(h.env.store()).unwrap();
        }
        for _ in 0..2 {
            assert!(matches!(
                h.mail.execute(&id).await.unwrap(),
                SendOutcome::Blocked { .. }
            ));
            let b = row(&h, &id).basis.unwrap();
            assert_eq!(b["verdict"], "leak");
            assert_eq!(b["entries"][0]["source_id"], "restricted");
            assert!(!b["spans"].as_array().unwrap().is_empty());
            retry(&h, &id);
        }
        assert!(h.smtp.sent().is_empty());
    }
}

#[derive(Default)]
struct Waiting {
    entered: Notify,
    release: Notify,
}
#[async_trait::async_trait]
impl OutboundCheck for Waiting {
    async fn check(&self, cx: &CheckCx, a: &Audience, t: &str, k: &[KnowledgeUse]) -> CheckResult {
        self.entered.notify_one();
        self.release.notified().await;
        PassingCheck.check(cx, a, t, k).await
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn changes_during_the_check_cannot_reuse_a_passing_verdict() {
    for mutation in [
        "body",
        "to_addrs",
        "cc_addrs",
        "bcc_addrs",
        "approval",
        "caps",
        "paused",
        "knowledge",
    ] {
        let waiting = Arc::new(Waiting::default());
        let h = setup_with(Opts {
            check: Some(waiting.clone()),
            ..Default::default()
        })
        .await;
        let id = direct(&h, "body before check").await;
        if mutation == "knowledge" {
            source(&h, &id);
        }
        let m = h.mail.clone();
        let d = id.clone();
        let task = tokio::spawn(async move { m.execute(&d).await });
        waiting.entered.notified().await;
        assert!(h.smtp.sent().is_empty());
        let connection = row(&h, &id).connection_id;
        db::change_disclosure_inputs(h.env.store(), mutation, &id, &connection).unwrap();
        waiting.release.notify_one();
        let outcome = task.await.unwrap().unwrap();
        assert!(
            !matches!(outcome, SendOutcome::Sent { .. }),
            "{mutation}: {outcome:?}"
        );
        assert!(h.smtp.sent().is_empty(), "{mutation}");
        assert_eq!(
            row(&h, &id).status,
            if mutation == "paused" {
                "approved"
            } else {
                "failed"
            },
            "{mutation}"
        );
    }
}

#[tokio::test]
async fn a_hung_checker_times_out_without_submitting_and_retry_checks_again() {
    let waiting = Arc::new(Waiting::default());
    let h = setup_with(Opts {
        check: Some(waiting.clone()),
        ..Default::default()
    })
    .await;
    let id = direct(&h, "body").await;
    tokio::time::pause();
    assert!(matches!(
        h.mail.execute(&id).await.unwrap(),
        SendOutcome::Blocked { .. }
    ));
    assert_eq!(row(&h, &id).status, "failed");
    assert_eq!(row(&h, &id).basis.unwrap()["verdict"], "unchecked");
    assert!(h.smtp.sent().is_empty());
    retry(&h, &id);
    waiting.release.notify_one();
    tokio::time::resume();
    assert!(matches!(
        h.mail.execute(&id).await.unwrap(),
        SendOutcome::Sent { .. }
    ));
    assert_eq!(h.smtp.sent().len(), 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn cancelling_a_check_is_a_definite_hold_and_cannot_leave_a_sending_claim() {
    let waiting = Arc::new(Waiting::default());
    let h = setup_with(Opts {
        check: Some(waiting.clone()),
        ..Default::default()
    })
    .await;
    let id = direct(&h, "body").await;
    let m = h.mail.clone();
    let d = id.clone();
    let task = tokio::spawn(async move { m.execute(&d).await });
    waiting.entered.notified().await;
    task.abort();
    assert!(task.await.unwrap_err().is_cancelled());
    assert_eq!(row(&h, &id).status, "failed");
    assert!(h.smtp.sent().is_empty());
}
