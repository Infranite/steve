//! Drives the fake Gmail API with `reqwest`, the HTTP client the adapter uses.

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE;
use reqwest::{Client, StatusCode};
use serde_json::{Value, json};
use steve_mail::fake::{FakeGmail, INITIAL_ACCESS_TOKEN, INITIAL_REFRESH_TOKEN, fixtures};

const ME: &str = "me@example.com";

struct Api {
    gmail: FakeGmail,
    http: Client,
    token: String,
}

impl Api {
    async fn new() -> Self {
        Self {
            gmail: FakeGmail::start(ME).await.unwrap(),
            http: Client::builder()
                .redirect(reqwest::redirect::Policy::none())
                .build()
                .unwrap(),
            token: INITIAL_ACCESS_TOKEN.into(),
        }
    }
    fn url(&self, path: &str) -> String {
        format!("{}users/me/{path}", self.gmail.api_base())
    }
    async fn get(&self, path: &str) -> (StatusCode, Value) {
        let r = self
            .http
            .get(self.url(path))
            .bearer_auth(&self.token)
            .send()
            .await
            .unwrap();
        let s = r.status();
        (s, r.json().await.unwrap_or(Value::Null))
    }
    async fn post(&self, path: &str, body: Value) -> (StatusCode, Value) {
        let r = self
            .http
            .post(self.url(path))
            .bearer_auth(&self.token)
            .json(&body)
            .send()
            .await
            .unwrap();
        let s = r.status();
        (s, r.json().await.unwrap_or(Value::Null))
    }
}

fn ids(v: &Value) -> Vec<String> {
    v["messages"]
        .as_array()
        .map(|a| {
            a.iter()
                .map(|m| m["id"].as_str().unwrap().to_string())
                .collect()
        })
        .unwrap_or_default()
}

#[tokio::test]
async fn auth_profile_and_labels() {
    let api = Api::new().await;
    let r = api.http.get(api.url("profile")).send().await.unwrap();
    assert_eq!(r.status(), 401);
    let body: Value = r.json().await.unwrap();
    assert_eq!(body["error"]["status"], "UNAUTHENTICATED");

    let (s, p) = api.get("profile").await;
    assert_eq!(s, 200);
    assert_eq!(p["emailAddress"], ME);
    assert!(p["historyId"].as_str().unwrap().parse::<u64>().is_ok());

    let work = api.gmail.add_label("工作");
    let (_, l) = api.get("labels").await;
    let labels = l["labels"].as_array().unwrap();
    assert!(
        labels
            .iter()
            .any(|x| x["id"] == "INBOX" && x["type"] == "system")
    );
    assert!(
        labels
            .iter()
            .any(|x| x["id"] == work && x["name"] == "工作" && x["type"] == "user")
    );

    api.gmail
        .inject(fixtures::get("gbk.eml").unwrap(), &["INBOX", "UNREAD"]);
    let (_, inbox) = api.get("labels/INBOX").await;
    assert_eq!(inbox["messagesTotal"], 1);
    assert_eq!(inbox["messagesUnread"], 1);
}

#[tokio::test]
async fn list_filters_pages_and_gets() {
    let api = Api::new().await;
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as i64;
    let day = 86_400_000;
    let mut injected = Vec::new();
    for (i, (name, raw)) in fixtures::ALL.iter().enumerate() {
        let labels: &[&str] = if name.starts_with("newsletter") {
            &["INBOX", "CATEGORY_PROMOTIONS"]
        } else {
            &["INBOX", "UNREAD"]
        };
        injected.push((
            api.gmail.inject_at(raw, labels, now_ms - i as i64 * 1000),
            *name,
            *raw,
        ));
    }
    let old = api.gmail.inject_at(
        fixtures::get("thread-1.eml").unwrap(),
        &["INBOX"],
        now_ms - 90 * day,
    );
    let spam = api
        .gmail
        .inject(fixtures::get("html-tracking.eml").unwrap(), &["SPAM"]);

    // Paging over INBOX, newest first.
    let mut all = Vec::new();
    let mut token: Option<String> = None;
    loop {
        let path = match &token {
            Some(t) => format!("messages?labelIds=INBOX&maxResults=5&pageToken={t}"),
            None => "messages?labelIds=INBOX&maxResults=5".into(),
        };
        let (s, v) = api.get(&path).await;
        assert_eq!(s, 200);
        all.extend(ids(&v));
        match v["nextPageToken"].as_str() {
            Some(t) => token = Some(t.into()),
            None => break,
        }
    }
    assert_eq!(all.len(), fixtures::ALL.len() + 1);
    assert_eq!(all[0], injected[0].0, "newest first");
    assert!(!all.contains(&spam), "SPAM excluded by default");

    let (_, v) = api
        .get("messages?q=newer_than:30d&labelIds=INBOX&maxResults=500")
        .await;
    assert_eq!(ids(&v).len(), fixtures::ALL.len());
    assert!(!ids(&v).contains(&old));
    let (_, v) = api
        .get("messages?labelIds=INBOX&labelIds=CATEGORY_PROMOTIONS")
        .await;
    assert_eq!(ids(&v).len(), 1, "labelIds are ANDed");
    let (_, v) = api.get("messages?q=has:attachment").await;
    assert_eq!(
        ids(&v).len(),
        1,
        "only the invoice has a non-inline attachment"
    );

    // format=raw round-trips the bytes.
    for (id, name, raw) in &injected {
        let (_, m) = api.get(&format!("messages/{id}?format=raw")).await;
        let got = URL_SAFE.decode(m["raw"].as_str().unwrap()).unwrap();
        assert_eq!(&got, raw, "{name}");
        assert_eq!(m["id"], *id);
        assert!(m["internalDate"].as_str().unwrap().parse::<i64>().is_ok());
    }

    // metadata decodes the subject; full exposes the PDF as an attachment.
    let (gbk, _, _) = injected.iter().find(|(_, n, _)| *n == "gbk.eml").unwrap();
    let (_, m) = api
        .get(&format!(
            "messages/{gbk}?format=metadata&metadataHeaders=Subject"
        ))
        .await;
    let headers = m["payload"]["headers"].as_array().unwrap();
    assert_eq!(headers.len(), 1);
    assert_eq!(headers[0]["value"], "回复：报价单（修改版）");

    let (inv, _, _) = injected
        .iter()
        .find(|(_, n, _)| *n == "invoice-pdf.eml")
        .unwrap();
    let (_, m) = api.get(&format!("messages/{inv}?format=full")).await;
    let parts = m["payload"]["parts"].as_array().unwrap();
    assert_eq!(parts[1]["mimeType"], "application/pdf");
    assert_eq!(parts[1]["filename"], "发票-2026-10.pdf");
    let aid = parts[1]["body"]["attachmentId"].as_str().unwrap();
    let (_, a) = api.get(&format!("messages/{inv}/attachments/{aid}")).await;
    let pdf = URL_SAFE.decode(a["data"].as_str().unwrap()).unwrap();
    assert!(pdf.starts_with(b"%PDF-1.4"));

    let (s, e) = api.get("messages/nope?format=raw").await;
    assert_eq!(s, 404);
    assert_eq!(e["error"]["code"], 404);
}

#[tokio::test]
async fn history_increments_and_expires() {
    let api = Api::new().await;
    let first = api
        .gmail
        .inject(fixtures::get("thread-1.eml").unwrap(), &["INBOX", "UNREAD"]);
    let (_, p) = api.get("profile").await;
    let start = p["historyId"].as_str().unwrap().to_string();

    let second = api
        .gmail
        .inject(fixtures::get("thread-3.eml").unwrap(), &["INBOX", "UNREAD"]);
    let (s, m) = api
        .post(
            &format!("messages/{first}/modify"),
            json!({"removeLabelIds": ["UNREAD", "INBOX"]}),
        )
        .await;
    assert_eq!(s, 200);
    assert_eq!(m["labelIds"], json!([]));
    api.gmail.delete(&second).unwrap();

    let types = "historyTypes=messageAdded&historyTypes=labelAdded&historyTypes=labelRemoved&historyTypes=messageDeleted";
    let (s, h) = api
        .get(&format!("history?startHistoryId={start}&{types}"))
        .await;
    assert_eq!(s, 200);
    let recs = h["history"].as_array().unwrap();
    assert_eq!(recs.len(), 3);
    assert_eq!(recs[0]["messagesAdded"][0]["message"]["id"], second);
    assert_eq!(
        recs[1]["labelsRemoved"][0]["labelIds"],
        json!(["UNREAD", "INBOX"])
    );
    assert_eq!(recs[2]["messagesDeleted"][0]["message"]["id"], second);
    let latest = h["historyId"].as_str().unwrap().to_string();

    // Only messageAdded, paged by one.
    let (_, h) = api
        .get(&format!(
            "history?startHistoryId={start}&historyTypes=messageAdded&maxResults=1"
        ))
        .await;
    assert_eq!(h["history"].as_array().unwrap().len(), 1);
    assert!(h["nextPageToken"].is_null());

    // Nothing since the latest id.
    let (_, h) = api.get(&format!("history?startHistoryId={latest}")).await;
    assert!(h["history"].is_null());

    api.gmail.expire_history();
    let (s, e) = api.get(&format!("history?startHistoryId={latest}")).await;
    assert_eq!(s, 404);
    assert_eq!(e["error"]["status"], "NOT_FOUND");
    let (_, p) = api.get("profile").await;
    let fresh = p["historyId"].as_str().unwrap();
    let (s, _) = api.get(&format!("history?startHistoryId={fresh}")).await;
    assert_eq!(s, 200, "a full resync's historyId is valid again");
}

#[tokio::test]
async fn send_threads_with_reply_headers() {
    let api = Api::new().await;
    let [t1, t2, t3] = fixtures::thread();
    let a = api.gmail.inject(t1, &["INBOX"]);
    let c = api.gmail.inject(t3, &["INBOX"]);
    let msgs = api.gmail.messages();
    assert_eq!(msgs[0].thread_id, a);
    assert_ne!(
        msgs[1].thread_id, msgs[1].id,
        "thread-3 joins via References"
    );
    let _ = c;

    let (s, r) = api
        .post(
            "messages/send",
            json!({"raw": URL_SAFE.encode(t2), "threadId": a}),
        )
        .await;
    assert_eq!(s, 200);
    assert_eq!(r["threadId"], a);
    assert_eq!(r["labelIds"], json!(["SENT"]));
    let sent = api.gmail.sent();
    assert_eq!(sent.len(), 1);
    assert_eq!(sent[0].raw, t2);
    assert_eq!(sent[0].requested_thread_id.as_deref(), Some(a.as_str()));

    let (_, th) = api.get(&format!("threads/{a}?format=minimal")).await;
    assert_eq!(th["messages"].as_array().unwrap().len(), 3);

    let (s, _) = api
        .post(
            "messages/send",
            json!({"raw": URL_SAFE.encode(b"Subject: x\r\n\r\nno recipients\r\n")}),
        )
        .await;
    assert_eq!(s, 400);

    api.gmail.fail_next(
        "/messages/send",
        429,
        json!({"error": {"code": 429, "status": "RESOURCE_EXHAUSTED"}}),
    );
    let (s, _) = api
        .post("messages/send", json!({"raw": URL_SAFE.encode(t2)}))
        .await;
    assert_eq!(s, 429);
    assert_eq!(api.gmail.sent().len(), 1);
}

#[tokio::test]
async fn oauth_refresh_and_code_flow() {
    let mut api = Api::new().await;
    api.gmail.expire_tokens();
    let (s, _) = api.get("profile").await;
    assert_eq!(s, 401);

    let r: Value = api
        .http
        .post(api.gmail.token_url())
        .form(&[
            ("grant_type", "refresh_token"),
            ("refresh_token", INITIAL_REFRESH_TOKEN),
            ("client_id", "cid"),
            ("client_secret", "secret"),
        ])
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(
        r["refresh_token"].is_null(),
        "Google keeps the refresh token"
    );
    api.token = r["access_token"].as_str().unwrap().into();
    assert_eq!(api.get("profile").await.0, 200);

    // Consent screen redirect → code → tokens.
    let resp = api
        .http
        .get(format!(
            "{}?redirect_uri=http://localhost/cb&state=xyz&scope=email",
            api.gmail.authorize_url()
        ))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 303);
    let loc = resp.headers()["location"].to_str().unwrap().to_string();
    assert!(
        loc.starts_with("http://localhost/cb?code=") && loc.ends_with("&state=xyz"),
        "{loc}"
    );
    let code = loc
        .split("code=")
        .nth(1)
        .unwrap()
        .split('&')
        .next()
        .unwrap();
    let r: Value = api
        .http
        .post(api.gmail.token_url())
        .form(&[("grant_type", "authorization_code"), ("code", code)])
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(r["refresh_token"].as_str().is_some());
    // A code works once.
    let again = api
        .http
        .post(api.gmail.token_url())
        .form(&[("grant_type", "authorization_code"), ("code", code)])
        .send()
        .await
        .unwrap();
    assert_eq!(again.status(), 400);
    assert_eq!(api.gmail.token_requests().len(), 3);
}
