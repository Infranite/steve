//! The HTTP control interface used by the `steve-fake-mail` binary.

use serde_json::{Value, json};
use steve_mail::fake::{
    ControlServer, FakeGmail, FakeGraph, FakeImap, FakeSmtp, Fakes, ImapConfig, SmtpConfig,
};

const ME: &str = "me@example.com";

#[tokio::test]
async fn inject_inspect_and_reset_over_http() {
    let imap = FakeImap::start(ImapConfig::default()).await.unwrap();
    let smtp = FakeSmtp::start(SmtpConfig::default()).await.unwrap();
    let gmail = FakeGmail::start(ME).await.unwrap();
    let graph = FakeGraph::start(ME).await.unwrap();
    let control = ControlServer::start(Fakes {
        imap: Some(imap.clone()),
        smtp: Some(smtp),
        gmail: Some(gmail.clone()),
        graph: Some(graph.clone()),
        default_user: ME.into(),
    })
    .await
    .unwrap();
    let http = reqwest::Client::new();
    let post = async |path: &str, body: Value| -> Value {
        http.post(format!("{}/{path}", control.url()))
            .json(&body)
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap()
    };

    let status: Value = http
        .get(control.url())
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(status["imap"]["port"], imap.port());
    assert!(
        status["ca_pem"]
            .as_str()
            .unwrap()
            .starts_with("-----BEGIN CERTIFICATE-----")
    );

    let r = post("inject", json!({"target": "imap", "fixture": "gbk.eml"})).await;
    assert_eq!(r["uid"], 1);
    post("inject", json!({"target": "imap", "folder": "Sent", "eml": "From: me@example.com\nTo: a@b.example\nSubject: hi\n\nbody\n"})).await;
    assert_eq!(
        imap.messages(ME, "Sent").unwrap()[0].raw,
        b"From: me@example.com\r\nTo: a@b.example\r\nSubject: hi\r\n\r\nbody\r\n"
    );
    let r = post(
        "inject",
        json!({"target": "gmail", "fixture": "newsletter.eml", "labels": ["INBOX"]}),
    )
    .await;
    assert_eq!(gmail.messages()[0].id, r["id"].as_str().unwrap());
    post(
        "inject",
        json!({"target": "graph", "fixture": "thread-1.eml"}),
    )
    .await;
    assert_eq!(graph.messages().len(), 1);

    post(
        "set_flags",
        json!({"folder": "INBOX", "uid": 1, "flags": ["\\Seen"]}),
    )
    .await;
    assert_eq!(imap.messages(ME, "INBOX").unwrap()[0].flags, ["\\Seen"]);
    let r = post("bump_uidvalidity", json!({"folder": "INBOX"})).await;
    assert_eq!(r["uidvalidity"], imap.folders(ME).unwrap()[0].uidvalidity);

    let bad = post("inject", json!({"target": "imap", "fixture": "nope.eml"})).await;
    assert!(bad["error"].as_str().unwrap().contains("nope.eml"));

    let sent: Value = http
        .get(format!("{}/sent?target=smtp", control.url()))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(sent, json!([]));

    post("reset", json!({})).await;
    assert!(imap.messages(ME, "INBOX").unwrap().is_empty());
    assert!(gmail.messages().is_empty());
    assert!(graph.messages().is_empty());
}
