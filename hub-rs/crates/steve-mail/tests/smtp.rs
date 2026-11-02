//! Drives the fake SMTP server with `lettre`.

use lettre::address::Envelope;
use lettre::message::{Mailbox, header::ContentType};
use lettre::transport::smtp::authentication::{Credentials, Mechanism};
use lettre::transport::smtp::client::{Certificate, Tls, TlsParameters};
use lettre::{AsyncSmtpTransport, AsyncTransport, Message, Tokio1Executor};
use steve_mail::fake::{FakeImap, FakeSmtp, ImapConfig, SmtpConfig, TestCa, TlsMode, fixtures};

const ME: &str = "me@example.com";
const PASS: &str = "app-password";

fn transport(smtp: &FakeSmtp, pass: &str, mech: Mechanism) -> AsyncSmtpTransport<Tokio1Executor> {
    let params = TlsParameters::builder(smtp.host().to_string())
        .add_root_certificate(Certificate::from_pem(smtp.ca().ca_pem().as_bytes()).unwrap())
        .build_rustls()
        .unwrap();
    let tls = match smtp.tls_mode() {
        TlsMode::Implicit => Tls::Wrapper(params),
        TlsMode::StartTls => Tls::Required(params),
        TlsMode::Plain => Tls::None,
    };
    AsyncSmtpTransport::<Tokio1Executor>::builder_dangerous(smtp.host())
        .port(smtp.port())
        .tls(tls)
        .credentials(Credentials::new(ME.into(), pass.into()))
        .authentication(vec![mech])
        .build()
}

fn reply() -> Message {
    Message::builder()
        .from(Mailbox::new(Some("Me".into()), ME.parse().unwrap()))
        .to("Alice Zhou <alice@acme.example>".parse().unwrap())
        .bcc("audit@example.com".parse().unwrap())
        .subject("Re: 预算审批流程")
        .in_reply_to("<thread-1.alice@acme.example>".into())
        .references("<thread-1.alice@acme.example>".into())
        .header(ContentType::TEXT_PLAIN)
        .body("三步：部门负责人、财务、CFO。\r\n.leading dot line\r\n".to_string())
        .unwrap()
}

#[tokio::test]
async fn implicit_tls_send_is_captured_and_delivered() {
    let ca = TestCa::generate().unwrap();
    let imap = FakeImap::start(ImapConfig {
        ca: Some(ca.clone()),
        accounts: vec![
            (ME.into(), PASS.into()),
            ("alice@acme.example".into(), "x".into()),
        ],
        ..Default::default()
    })
    .await
    .unwrap();
    let smtp = FakeSmtp::start(SmtpConfig {
        ca: Some(ca),
        deliver_to: Some(imap.clone()),
        ..Default::default()
    })
    .await
    .unwrap();

    transport(&smtp, PASS, Mechanism::Plain)
        .send(reply())
        .await
        .unwrap();

    let sent = smtp.sent();
    assert_eq!(sent.len(), 1);
    let m = &sent[0];
    assert_eq!(m.from, ME);
    assert_eq!(m.rcpt, vec!["alice@acme.example", "audit@example.com"]);
    assert_eq!(m.auth_user.as_deref(), Some(ME));
    assert!(m.tls);
    let text = String::from_utf8_lossy(&m.data);
    assert!(
        text.contains("In-Reply-To: <thread-1.alice@acme.example>"),
        "{text}"
    );
    assert!(!text.contains("audit@example.com"), "Bcc is envelope-only");
    // Dot-stuffing removed: the body line starts with a single dot again.
    let parsed = mail_parser_body(&m.data);
    assert!(parsed.contains("\n.leading dot line"), "{parsed:?}");

    // Round trip: Alice has it in her INBOX, byte for byte.
    let inbox = imap.messages("alice@acme.example", "INBOX").unwrap();
    assert_eq!(inbox.len(), 1);
    assert_eq!(inbox[0].raw, m.data);
    // Unknown recipients are relayed (captured) but not delivered anywhere.
    assert!(imap.messages("audit@example.com", "INBOX").is_err());
}

/// lettre always sends CRLF before the final ".", so a message that already
/// ends with CRLF arrives with one more; compare without trailing line ends.
fn trim_crlf(b: &[u8]) -> &[u8] {
    let mut b = b;
    while let Some(x) = b.strip_suffix(b"\r\n") {
        b = x;
    }
    b
}

fn mail_parser_body(raw: &[u8]) -> String {
    // lettre may transfer-encode the body; decode via the same parser the sync uses.
    let msg = mail_parser::MessageParser::new().parse(raw).unwrap();
    msg.body_text(0).unwrap().into_owned()
}

#[tokio::test]
async fn starttls_and_auth_login() {
    let smtp = FakeSmtp::start(SmtpConfig {
        tls: TlsMode::StartTls,
        ..Default::default()
    })
    .await
    .unwrap();
    let raw = fixtures::get("thread-2.eml").unwrap();
    let env = Envelope::new(
        Some(ME.parse().unwrap()),
        vec!["alice@acme.example".parse().unwrap()],
    )
    .unwrap();
    transport(&smtp, PASS, Mechanism::Login)
        .send_raw(&env, raw)
        .await
        .unwrap();
    let sent = smtp.sent();
    assert_eq!(trim_crlf(&sent[0].data), trim_crlf(raw));
    assert!(sent[0].tls);
    let log = smtp.commands();
    assert!(log.iter().any(|l| l == "STARTTLS"));
    assert!(log.iter().any(|l| l == "AUTH LOGIN ***"));
    assert!(log.iter().all(|l| !l.contains(PASS)));
}

#[tokio::test]
async fn wrong_password_and_scripted_rejection() {
    let smtp = FakeSmtp::start(SmtpConfig::default()).await.unwrap();
    let err = transport(&smtp, "nope", Mechanism::Plain)
        .send(reply())
        .await;
    assert!(err.is_err());
    assert!(smtp.sent().is_empty());

    smtp.fail_next("RCPT", "550 5.1.1 <alice@acme.example>: no such user");
    let err = transport(&smtp, PASS, Mechanism::Plain)
        .send(reply())
        .await
        .unwrap_err();
    assert!(err.is_permanent(), "{err}");
    assert!(smtp.sent().is_empty());

    smtp.fail_next(".", "451 4.3.0 try again later");
    let err = transport(&smtp, PASS, Mechanism::Plain)
        .send(reply())
        .await
        .unwrap_err();
    assert!(err.is_transient(), "{err}");
    assert!(smtp.sent().is_empty());

    transport(&smtp, PASS, Mechanism::Plain)
        .send(reply())
        .await
        .unwrap();
    assert_eq!(smtp.sent().len(), 1);
    smtp.reset();
    assert!(smtp.sent().is_empty());
}

#[tokio::test]
async fn large_message_round_trip() {
    let smtp = FakeSmtp::start(SmtpConfig::default()).await.unwrap();
    let big = fixtures::large(28 * 1024 * 1024);
    assert!(big.len() >= 28 * 1024 * 1024);
    let env = Envelope::new(Some(ME.parse().unwrap()), vec![ME.parse().unwrap()]).unwrap();
    transport(&smtp, PASS, Mechanism::Plain)
        .send_raw(&env, &big)
        .await
        .unwrap();
    assert_eq!(trim_crlf(&smtp.sent()[0].data), trim_crlf(&big));
}
