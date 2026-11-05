//! The vault's secret rules for slice A2 (the V1/V2 scan is `vault_secrets.rs`): after
//! OAuth round trips (Gmail, Outlook, Feishu user), refreshes with rotating single-use refresh
//! tokens, a revoked grant (→ reauth), a credential form (Feishu app) with good, wrong and malformed
//! bodies, minted tenant tokens, re-authorization and disconnects, no OAuth client secret, PKCE
//! verifier, access/refresh/tenant token or app secret appears in captured logs (ours, reqwest's,
//! hyper's — everything but the in-process fakes' own), responses and their headers, SSE events,
//! errors and `Debug` text, or the database files (audit included).
//!
//! Its own test binary with a global subscriber, so logs from every thread are captured.

mod conn_support;

use std::io::Write;
use std::sync::{Arc, Mutex};

use conn_support::*;
use serde_json::{Value, json};
use steve_channels::fake::feishu::{FakeFeishu, World, ids};
use steve_mail::fake::{FakeGmail, FakeGraph};
use steve_modules::testkit::{ME, WS, caller};

#[derive(Clone, Default)]
struct Capture(Arc<Mutex<Vec<u8>>>);

impl Write for Capture {
    fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
        self.0
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .extend_from_slice(b);
        Ok(b.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for Capture {
    type Writer = Capture;
    fn make_writer(&'a self) -> Capture {
        self.clone()
    }
}

fn credential(h: &H, item: &str) -> Vec<String> {
    h.vault
        .with_credential(WS, item, "test", "测试", |c| {
            c.labels()
                .filter_map(|l| c.text(l).map(String::from))
                .collect()
        })
        .expect("credential")
}

fn item_of(h: &H, con: &str) -> String {
    h.env
        .store()
        .reader(|c| {
            c.query_row(
                "SELECT credential_item FROM connections WHERE id = ?",
                [con],
                |r| r.get::<_, String>(0),
            )
            .map_err(steve_store::StoreError::from)
        })
        .expect("item")
}

fn con_of(h: &H, acc: &str) -> String {
    h.env
        .store()
        .reader(|c| {
            c.query_row(
                "SELECT id FROM connections WHERE account_id = ?",
                [acc],
                |r| r.get::<_, String>(0),
            )
            .map_err(steve_store::StoreError::from)
        })
        .expect("connection")
}

#[tokio::test(flavor = "multi_thread")]
async fn no_connection_secret_in_logs_responses_errors_or_database() {
    let cap = Capture::default();
    tracing::subscriber::set_global_default(
        tracing_subscriber::fmt()
            .with_max_level(tracing::Level::TRACE)
            .with_writer(cap.clone())
            .with_ansi(false)
            .finish(),
    )
    .expect("subscriber");

    let gmail = FakeGmail::start("peng@example.test").await.expect("gmail");
    let graph = FakeGraph::start("peng@contoso.com").await.expect("graph");
    let feishu = FakeFeishu::start_with(World::sample())
        .await
        .expect("feishu");
    let base = feishu.base_url().to_string();
    let h = setup(vec![
        Arc::new(MailOAuth::gmail(&gmail)),
        Arc::new(MailOAuth::outlook(&graph)),
        Arc::new(FeishuApp::new()),
        Arc::new(FeishuUser::new(&base)),
    ]);
    let me = caller(WS, ME);
    let mut texts: Vec<String> = Vec::new();
    let mut needles: Vec<String> = vec![
        GOOGLE_SECRET.into(),
        MS_SECRET.into(),
        ids::APP_SECRET.into(),
        "PLAINTEXT-encrypt-key".into(),
        "PLAINTEXT-wrong-secret".into(),
    ];
    let push = |texts: &mut Vec<String>, r: (u16, Value, axum::http::HeaderMap)| {
        texts.push(format!("{} {:?} {}", r.0, r.2, r.1));
        r
    };

    // OAuth: Gmail and Outlook
    let g = account_of(&oauth_round_trip(&h, &me, json!({"adapter": "gmail"})).await);
    let o = account_of(&oauth_round_trip(&h, &me, json!({"adapter": "outlook"})).await);
    let (gc, oc) = (con_of(&h, &g), con_of(&h, &o));
    needles.extend(credential(&h, &item_of(&h, &gc)));
    needles.extend(credential(&h, &item_of(&h, &oc)));
    // refresh twice (Microsoft rotates), then the grant is revoked
    h.conns.refresh(WS, &oc, true).await.expect("refresh");
    needles.extend(credential(&h, &item_of(&h, &oc)));
    h.conns.refresh(WS, &oc, true).await.expect("refresh");
    needles.extend(credential(&h, &item_of(&h, &oc)));
    graph.revoke_tokens();
    texts.push(format!("{:?}", h.conns.refresh(WS, &oc, true).await));
    texts.push(format!("{:?}", h.conns.cx(WS, &oc).expect("cx")));
    let gcx = h.conns.cx(WS, &gc).expect("cx");
    texts.push(format!("{:?}", gcx.access_token().await));
    gmail.expire_tokens();
    push(
        &mut texts,
        send(
            &h.api,
            Some(&me),
            "POST",
            &format!("/connections/{gc}/test"),
            None,
            None,
        )
        .await,
    );
    needles.extend(credential(&h, &item_of(&h, &gc)));
    // re-authorize Outlook
    let (_, v, _) = push(
        &mut texts,
        send(
            &h.api,
            Some(&me),
            "POST",
            &format!("/connections/{oc}/reauth"),
            None,
            None,
        )
        .await,
    );
    let back = consent(v["url"].as_str().expect("url")).await;
    push(
        &mut texts,
        send(&h.cb, Some(&me), "GET", &path_of(&back), None, None).await,
    );
    needles.extend(credential(&h, &item_of(&h, &oc)));

    // the credential form, good, wrong, malformed
    let (_, v, _) = push(
        &mut texts,
        send(
            &h.api,
            Some(&me),
            "POST",
            "/connections",
            Some(json!({"adapter": "feishu_app", "fields": {"app_id": ids::APP_ID, "app_secret": ids::APP_SECRET, "encrypt_key": "PLAINTEXT-encrypt-key"}, "config": {"api_base": base}})),
            None,
        )
        .await,
    );
    let app = v["id"].as_str().expect("id").to_string();
    let acx = h.conns.cx(WS, &app).expect("cx");
    let tenant = acx.access_token().await.expect("tenant");
    needles.push(tenant.expose().to_string());
    texts.push(format!("{tenant:?} {acx:?}"));
    push(
        &mut texts,
        send(
            &h.api,
            Some(&me),
            "POST",
            "/connections",
            Some(json!({"adapter": "feishu_app", "fields": {"app_id": "cli_x", "app_secret": "PLAINTEXT-wrong-secret"}, "config": {"api_base": base}})),
            None,
        )
        .await,
    );
    for body in [
        r#"{"adapter": "feishu_app", "fields": {"app_secret": "PLAINTEXT-wrong-secret"}}"#,
        r#"{"adapter": "feishu_app", "fields": {"app_id": 1, "app_secret": "PLAINTEXT-wrong-secret"}}"#,
        r#"{"adapter": "feishu_app", "fields": {"app_secret": ["PLAINTEXT-wrong-secret"]}}"#,
        r#"{"adapter": "feishu_app", "fields": {"app_secret": "PLAINTEXT-wrong-secret""#,
    ] {
        let mut req = axum::http::Request::builder()
            .method("POST")
            .uri("/connections")
            .header("content-type", "application/json")
            .body(axum::body::Body::from(body.to_string()))
            .expect("req");
        req.extensions_mut().insert(me.clone());
        let res = tower::ServiceExt::oneshot(h.api.clone(), req)
            .await
            .expect("infallible");
        let st = res.status();
        let b = axum::body::to_bytes(res.into_body(), 1 << 20)
            .await
            .expect("body");
        assert_eq!(st.as_u16(), 400);
        texts.push(String::from_utf8_lossy(&b).into());
    }
    push(
        &mut texts,
        send(
            &h.api,
            Some(&me),
            "PUT",
            &format!("/connections/{app}/credential"),
            Some(
                json!({"fields": {"app_id": ids::APP_ID, "app_secret": "PLAINTEXT-wrong-secret"}}),
            ),
            None,
        )
        .await,
    );

    // Feishu user OAuth with the app as client, refreshed twice (single-use refresh tokens)
    let u = account_of(
        &oauth_round_trip(
            &h,
            &me,
            json!({"adapter": "feishu_user", "config": {"app_connection": app}}),
        )
        .await,
    );
    let uc = con_of(&h, &u);
    needles.extend(credential(&h, &item_of(&h, &uc)));
    for _ in 0..2 {
        h.conns.refresh(WS, &uc, true).await.expect("refresh");
        needles.extend(credential(&h, &item_of(&h, &uc)));
    }

    // every read the page makes, then disconnect all
    for p in ["/connections", "/connections/adapters"] {
        push(
            &mut texts,
            send(&h.api, Some(&me), "GET", p, None, None).await,
        );
    }
    for c in [&gc, &oc, &app, &uc] {
        push(
            &mut texts,
            send(
                &h.api,
                Some(&me),
                "GET",
                &format!("/connections/{c}"),
                None,
                None,
            )
            .await,
        );
        push(
            &mut texts,
            send(
                &h.api,
                Some(&me),
                "DELETE",
                &format!("/connections/{c}"),
                None,
                None,
            )
            .await,
        );
    }

    // what went over the wire to the token endpoints: verifiers and spent refresh tokens
    for r in gmail
        .token_requests()
        .iter()
        .chain(graph.token_requests().iter())
    {
        for k in ["code_verifier", "refresh_token", "client_secret"] {
            if let Some(v) = r.form.get(k) {
                needles.push(v.clone());
            }
        }
    }
    needles.retain(|n| n.len() >= 8 && !n.chars().all(|c| c.is_ascii_digit()));
    needles.sort();
    needles.dedup();
    assert!(needles.len() > 20, "{} needles", needles.len());

    for e in h.env.take_events() {
        texts.push(serde_json::to_string(&e).expect("event"));
    }
    let logs = String::from_utf8(cap.0.lock().expect("logs").clone()).expect("utf-8");
    let logs: String = logs
        .lines()
        .filter(|l| {
            !(l.contains(" steve_channels::fake")
                || l.contains(" steve_mail::fake")
                || l.contains(" steve_github::fake"))
        })
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        logs.contains("connection connected") && logs.contains("connection tokens refreshed"),
        "logs captured: {logs}"
    );
    let mut db = Vec::new();
    for ext in ["", "-wal", "-shm"] {
        if let Ok(b) = std::fs::read(format!("{}{ext}", h.env.store().path().display())) {
            db.extend_from_slice(&b);
        }
    }
    for n in &needles {
        assert!(!logs.contains(n.as_str()), "a log line leaks a secret");
        assert!(
            !db.windows(n.len()).any(|w| w == n.as_bytes()),
            "the database leaks a secret"
        );
        for t in &texts {
            assert!(
                !t.contains(n.as_str()),
                "response/error/debug leaks a secret: {t}"
            );
        }
    }
}
