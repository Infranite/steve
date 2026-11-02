//! F0 acceptance: the fake's pushes are recovered by the library's own verify+decrypt, and the
//! fake's REST endpoints behave like Feishu (uuid de-dup, tokens, faults, paging, OAuth).

use std::collections::HashSet;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::body::Bytes;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use serde_json::{Value, json};
use steve_channels::adapters::feishu::crypto;
use steve_channels::fake::feishu::{Endpoint, FakeFeishu, fixture, fixtures_dir, ids};

// ---------------------------------------------------------------------------------------------
// A minimal hook receiver that follows feishu.md §3.1 using only the library crypto.

#[derive(Default)]
struct Hook {
    encrypt_key: String,
    token: String,
    /// Decrypted documents accepted, in order (duplicates included).
    received: Vec<Value>,
    seen: HashSet<String>,
    /// Distinct events handled.
    handled: Vec<String>,
    /// Answer 500 to this many requests first (to exercise redelivery).
    fail_first: usize,
}

type H = Arc<Mutex<Hook>>;

async fn hook(State(h): State<H>, headers: HeaderMap, body: Bytes) -> Response {
    let mut h = h.lock().unwrap();
    if h.fail_first > 0 {
        h.fail_first -= 1;
        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
    }
    let hdr = |k: &str| {
        headers
            .get(k)
            .and_then(|v| v.to_str().ok())
            .map(str::to_string)
    };
    let signed = match (
        hdr(crypto::HEADER_TIMESTAMP),
        hdr(crypto::HEADER_NONCE),
        hdr(crypto::HEADER_SIGNATURE),
    ) {
        (Some(ts), Some(nonce), Some(sig)) => {
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_secs() as i64;
            if !crypto::verify_signature(&ts, &nonce, &h.encrypt_key, &body, &sig)
                || !crypto::timestamp_fresh(&ts, now)
            {
                return StatusCode::UNAUTHORIZED.into_response();
            }
            true
        }
        _ => false,
    };
    let Ok(doc) = crypto::decrypt_body(&h.encrypt_key, &body) else {
        return StatusCode::BAD_REQUEST.into_response();
    };
    if doc["type"] == "url_verification" {
        if doc["token"] != h.token.as_str() {
            return StatusCode::UNAUTHORIZED.into_response();
        }
        h.received.push(doc.clone());
        return axum::Json(json!({"challenge": doc["challenge"]})).into_response();
    }
    if !signed || doc["header"]["token"] != h.token.as_str() {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    h.received.push(doc.clone());
    let id = doc["header"]["event_id"].as_str().unwrap_or("").to_string();
    if h.seen.insert(id.clone()) {
        h.handled.push(id);
    }
    axum::Json(json!({})).into_response()
}

async fn start_hook(fake: &FakeFeishu) -> (String, H) {
    let app = fake.app();
    let h: H = Arc::new(Mutex::new(Hook {
        encrypt_key: app.encrypt_key,
        token: app.verification_token,
        ..Hook::default()
    }));
    let router = axum::Router::new()
        .route("/hooks/con_test", axum::routing::post(hook))
        .with_state(h.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    (format!("http://{addr}/hooks/con_test"), h)
}

fn all_fixtures() -> Vec<(String, Value)> {
    let mut out: Vec<_> = std::fs::read_dir(fixtures_dir())
        .unwrap()
        .filter_map(|e| {
            let p = e.ok()?.path();
            (p.extension()? == "json").then(|| p.file_stem()?.to_str().map(str::to_string))?
        })
        .map(|n| {
            let v = fixture(&n).unwrap();
            (n, v)
        })
        .collect();
    out.sort_by(|a, b| a.0.cmp(&b.0));
    out
}

// ---------------------------------------------------------------------------------------------
// Pushing events.

#[tokio::test]
async fn pushed_fixtures_are_recovered_by_library_crypto() {
    let fake = FakeFeishu::start().await.unwrap();
    let (url, h) = start_hook(&fake).await;
    let p = fake.pusher(url);
    let mut n = 0;
    for (name, v) in all_fixtures() {
        if v.get("schema").is_none() {
            continue; // crypto vectors, not envelopes
        }
        let d = p.push_envelope(v.clone()).await.unwrap();
        assert_eq!(d.status, 200, "{name}: {}", d.body);
        assert_eq!(h.lock().unwrap().received.last(), Some(&v), "{name}");
        n += 1;
    }
    assert_eq!(n, 18);
    assert_eq!(h.lock().unwrap().handled.len(), 18);
}

#[tokio::test]
async fn signed_request_matches_spec_formula() {
    let fake = FakeFeishu::start().await.unwrap();
    let p = fake.pusher("http://unused");
    let env = p.envelope("im.message.receive_v1", json!({"x": 1}));
    let req = p.sign(&env);
    let ts = req.header("X-Lark-Request-Timestamp").unwrap();
    let nonce = req.header("X-Lark-Request-Nonce").unwrap();
    let sig = req.header("X-Lark-Signature").unwrap();
    assert_eq!(
        sig,
        crypto::signature(ts, nonce, ids::ENCRYPT_KEY, &req.body)
    );
    assert_eq!(
        crypto::decrypt_body(ids::ENCRYPT_KEY, &req.body).unwrap(),
        env
    );
    assert_eq!(env["schema"], "2.0");
    assert_eq!(env["header"]["app_id"], ids::APP_ID);
    assert_eq!(env["header"]["tenant_key"], ids::TENANT_KEY);
    assert_eq!(env["header"]["token"], ids::VERIFICATION_TOKEN);
    assert_eq!(env["header"]["event_id"].as_str().unwrap().len(), 32);
}

#[tokio::test]
async fn url_verification_challenge_round_trip() {
    let fake = FakeFeishu::start().await.unwrap();
    let (url, h) = start_hook(&fake).await;
    let p = fake.pusher(url);
    let (challenge, d) = p.push_url_verification().await.unwrap();
    assert_eq!(d.status, 200);
    assert_eq!(d.json(), json!({"challenge": challenge}));
    let (challenge, d) = p.push_url_verification_signed().await.unwrap();
    assert_eq!(d.json(), json!({"challenge": challenge}));
    assert_eq!(h.lock().unwrap().handled.len(), 0);
}

#[tokio::test]
async fn url_verification_fixture_decrypts_and_verifies() {
    let f = fixture("url_verification_encrypted").unwrap();
    let key = f["encrypt_key"].as_str().unwrap();
    let body = f["body"].as_str().unwrap().as_bytes();
    let hd = &f["headers"];
    assert!(crypto::verify_signature(
        hd["X-Lark-Request-Timestamp"].as_str().unwrap(),
        hd["X-Lark-Request-Nonce"].as_str().unwrap(),
        key,
        body,
        hd["X-Lark-Signature"].as_str().unwrap(),
    ));
    assert_eq!(crypto::decrypt_body(key, body).unwrap(), f["plaintext"]);

    let v = fixture("crypto_doc_vector").unwrap();
    let out = crypto::decrypt(
        v["encrypt_key"].as_str().unwrap(),
        v["encrypt"].as_str().unwrap(),
    )
    .unwrap();
    assert_eq!(out, v["plaintext"].as_str().unwrap().as_bytes());
}

#[tokio::test]
async fn redelivery_keeps_event_id_and_is_deduplicated() {
    let fake = FakeFeishu::start().await.unwrap();
    let (url, h) = start_hook(&fake).await;
    let p = fake.pusher(url);
    let d = p
        .push_envelope(fixture("message_group_at_bot").unwrap())
        .await
        .unwrap();
    let id = d.event_id.clone().unwrap();
    let d2 = p.redeliver(&id).await.unwrap();
    assert_eq!(d2.event_id.as_deref(), Some(id.as_str()));
    assert_eq!(d2.status, 200);
    assert!(p.redeliver("nope").await.is_err());
    let h = h.lock().unwrap();
    assert_eq!(h.received.len(), 2);
    assert_eq!(h.handled, vec![id]);
}

#[tokio::test]
async fn retries_until_200_with_the_same_event() {
    let fake = FakeFeishu::start().await.unwrap();
    let (url, h) = start_hook(&fake).await;
    h.lock().unwrap().fail_first = 2;
    let p = fake.pusher(url);
    let tries = p
        .push_with_retries(
            "im.chat.disbanded_v1",
            json!({"chat_id": ids::TOPIC_CHAT}),
            &[Duration::from_millis(10); 4],
        )
        .await
        .unwrap();
    assert_eq!(
        tries.iter().map(|d| d.status).collect::<Vec<_>>(),
        vec![500, 500, 200]
    );
    assert!(tries.iter().all(|d| d.event_id == tries[0].event_id));
    assert_eq!(h.lock().unwrap().handled.len(), 1);
}

#[tokio::test]
async fn bad_signature_stale_timestamp_and_bad_ciphertext_are_refused() {
    let fake = FakeFeishu::start().await.unwrap();
    let (url, h) = start_hook(&fake).await;
    let p = fake.pusher(url);
    let env = p.envelope("im.message.receive_v1", json!({}));
    let good = p.sign(&env);
    let hdrs = |sig: &str| {
        vec![
            (
                "X-Lark-Request-Timestamp",
                good.header("X-Lark-Request-Timestamp").unwrap().to_string(),
            ),
            (
                "X-Lark-Request-Nonce",
                good.header("X-Lark-Request-Nonce").unwrap().to_string(),
            ),
            ("X-Lark-Signature", sig.to_string()),
        ]
    };
    let bad = hdrs(&"0".repeat(64));
    let bad: Vec<(&str, &str)> = bad.iter().map(|(k, v)| (*k, v.as_str())).collect();
    assert_eq!(
        p.push_raw(&bad, good.body.clone()).await.unwrap().status,
        401
    );

    let stale = p.sign_at(&env, "1600000000");
    assert_eq!(p.post(&stale).await.unwrap().status, 401);

    let garbage = br#"{"encrypt":"bm90IGEgY2lwaGVydGV4dA=="}"#.to_vec();
    let ts = (std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs())
    .to_string();
    let sig = crypto::signature(&ts, "n", ids::ENCRYPT_KEY, &garbage);
    let d = p
        .push_raw(
            &[
                ("X-Lark-Request-Timestamp", &ts),
                ("X-Lark-Request-Nonce", "n"),
                ("X-Lark-Signature", &sig),
            ],
            garbage,
        )
        .await
        .unwrap();
    assert_eq!(d.status, 400);
    assert!(h.lock().unwrap().received.is_empty());
}

// ---------------------------------------------------------------------------------------------
// REST endpoints.

struct Api {
    base: String,
    http: reqwest::Client,
    token: String,
}

impl Api {
    async fn new(fake: &FakeFeishu) -> Self {
        let http = reqwest::Client::new();
        let base = fake.base_url().to_string();
        let mut a = Self {
            base,
            http,
            token: String::new(),
        };
        a.token = a.fetch_token().await;
        a
    }
    async fn fetch_token(&self) -> String {
        let v: Value = self
            .http
            .post(format!(
                "{}/open-apis/auth/v3/tenant_access_token/internal",
                self.base
            ))
            .json(&json!({"app_id": ids::APP_ID, "app_secret": ids::APP_SECRET}))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(v["code"], 0);
        assert!(v["expire"].as_u64().unwrap() > 7000);
        v["tenant_access_token"].as_str().unwrap().to_string()
    }
    async fn call(
        &self,
        method: reqwest::Method,
        path: &str,
        body: Option<Value>,
    ) -> (u16, HeaderMap, Value) {
        self.call_as(&self.token, method, path, body).await
    }
    async fn call_as(
        &self,
        token: &str,
        method: reqwest::Method,
        path: &str,
        body: Option<Value>,
    ) -> (u16, HeaderMap, Value) {
        let mut rb = self
            .http
            .request(method, format!("{}{path}", self.base))
            .bearer_auth(token);
        if let Some(b) = body {
            rb = rb.json(&b);
        }
        let r = rb.send().await.unwrap();
        let status = r.status().as_u16();
        let headers = r.headers().clone();
        let v = r.json().await.unwrap_or(Value::Null);
        (status, headers, v)
    }
    async fn get(&self, path: &str) -> Value {
        self.call(reqwest::Method::GET, path, None).await.2
    }
    async fn post(&self, path: &str, body: Value) -> Value {
        self.call(reqwest::Method::POST, path, Some(body)).await.2
    }
}

fn text(t: &str) -> String {
    json!({"text": t}).to_string()
}

#[tokio::test]
async fn token_reuse_expiry_and_bot_info() {
    let fake = FakeFeishu::start().await.unwrap();
    let api = Api::new(&fake).await;
    assert_eq!(
        api.fetch_token().await,
        api.token,
        "reused while >30 min left"
    );
    let v = api.get("/open-apis/bot/v3/info").await;
    assert_eq!(v["bot"]["app_name"], ids::BOT_NAME);
    assert_eq!(v["bot"]["open_id"], ids::BOT_OPEN_ID);

    fake.expire_tokens();
    let v = api.get("/open-apis/bot/v3/info").await;
    assert_eq!(v["code"], 99991663);
    let fresh = api.fetch_token().await;
    assert_ne!(fresh, api.token);
    let (_, _, v) = api
        .call_as(&fresh, reqwest::Method::GET, "/open-apis/bot/v3/info", None)
        .await;
    assert_eq!(v["code"], 0);
    assert_eq!(fake.tokens_issued(), 3);

    let (s, _, v) = api
        .call_as(
            "t-garbage",
            reqwest::Method::GET,
            "/open-apis/bot/v3/info",
            None,
        )
        .await;
    assert_eq!((s, v["code"].as_i64()), (400, Some(99991663)));
    let v: Value = reqwest::Client::new()
        .get(format!("{}/open-apis/bot/v3/info", fake.base_url()))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(v["code"], 99991661);
}

#[tokio::test]
async fn send_dedups_by_uuid_and_records() {
    let fake = FakeFeishu::start().await.unwrap();
    let api = Api::new(&fake).await;
    let body = json!({"receive_id": ids::GROUP_CHAT, "msg_type": "text",
        "content": text(&format!("<at user_id=\"{}\"></at> 已处理", ids::ALICE.open_id)), "uuid": "cm_0001"});
    let a = api
        .post(
            "/open-apis/im/v1/messages?receive_id_type=chat_id",
            body.clone(),
        )
        .await;
    let b = api
        .post("/open-apis/im/v1/messages?receive_id_type=chat_id", body)
        .await;
    assert_eq!(a["code"], 0);
    assert_eq!(a["data"]["message_id"], b["data"]["message_id"]);
    assert_eq!(a["data"]["sender"]["sender_type"], "app");
    assert_eq!(a["data"]["body"]["content"], text("@_user_1 已处理"));
    assert_eq!(a["data"]["mentions"][0]["name"], "王小丽");
    let sent = fake.sent();
    assert_eq!(sent.len(), 1);
    assert_eq!(sent[0].uuid.as_deref(), Some("cm_0001"));
    assert_eq!(sent[0].receive_id.as_deref(), Some(ids::GROUP_CHAT));
    assert!(sent[0].text().unwrap().starts_with("<at user_id="));
    assert_eq!(fake.send_attempts().len(), 2);
    assert!(fake.send_attempts()[1].deduplicated);

    // Different uuid → new message; open_id target → p2p chat.
    let c = api
        .post("/open-apis/im/v1/messages?receive_id_type=union_id",
            json!({"receive_id": ids::ALICE.union_id, "msg_type": "text", "content": text("hi"), "uuid": "cm_0002"}))
        .await;
    assert_eq!(c["data"]["chat_id"], ids::P2P_CHAT);
    assert_eq!(fake.sent().len(), 2);

    // Content must be a JSON string.
    let bad = api
        .post(
            "/open-apis/im/v1/messages?receive_id_type=chat_id",
            json!({"receive_id": ids::GROUP_CHAT, "msg_type": "text", "content": "not json"}),
        )
        .await;
    assert_eq!(bad["code"], 230001);
    // Bot not in chat.
    fake.world(|w| {
        w.chats.iter_mut().for_each(|c| {
            if c.chat_id == ids::GROUP_CHAT {
                c.bot_in_chat = false
            }
        })
    });
    let out = api
        .post(
            "/open-apis/im/v1/messages?receive_id_type=chat_id",
            json!({"receive_id": ids::GROUP_CHAT, "msg_type": "text", "content": text("x")}),
        )
        .await;
    assert_eq!(out["code"], 230002);
}

#[tokio::test]
async fn reply_threads_and_recall() {
    let fake = FakeFeishu::start().await.unwrap();
    let (url, _h) = start_hook(&fake).await;
    fake.pusher(url)
        .push_envelope(fixture("message_group_at_bot").unwrap())
        .await
        .unwrap();
    let api = Api::new(&fake).await;
    let r = api
        .post("/open-apis/im/v1/messages/om_groupat00000000000000000000001/reply",
            json!({"msg_type": "text", "content": text("在查"), "reply_in_thread": true, "uuid": "cm_r1"}))
        .await;
    assert_eq!(r["code"], 0, "{r}");
    assert_eq!(r["data"]["parent_id"], "om_groupat00000000000000000000001");
    assert_eq!(r["data"]["root_id"], "om_groupat00000000000000000000001");
    assert!(r["data"]["thread_id"].as_str().unwrap().starts_with("omt_"));
    let s = &fake.sent()[0];
    assert!(s.reply_in_thread);
    assert_eq!(
        s.reply_to.as_deref(),
        Some("om_groupat00000000000000000000001")
    );

    // Topic group: replies land in the existing thread.
    let t = api
        .post(
            "/open-apis/im/v1/messages/om_topicroot000000000000000000001/reply",
            json!({"msg_type": "text", "content": text("能")}),
        )
        .await;
    assert_eq!(t["data"]["thread_id"], "omt_fake0000000001");

    // Recall: own message ok, twice → already deleted, someone else's → refused.
    let mid = r["data"]["message_id"].as_str().unwrap().to_string();
    let path = format!("/open-apis/im/v1/messages/{mid}");
    assert_eq!(
        api.call(reqwest::Method::DELETE, &path, None).await.2["code"],
        0
    );
    assert_eq!(
        api.call(reqwest::Method::DELETE, &path, None).await.2["code"],
        230110
    );
    let other = api
        .call(
            reqwest::Method::DELETE,
            "/open-apis/im/v1/messages/om_groupat00000000000000000000001",
            None,
        )
        .await;
    assert_eq!(other.2["code"], 230026);
    assert_eq!(fake.deleted(), vec![mid]);
}

#[tokio::test]
async fn rate_limit_then_retry_sends_once() {
    let fake = FakeFeishu::start().await.unwrap();
    let api = Api::new(&fake).await;
    fake.rate_limit_next(Endpoint::SendMessage, 1);
    fake.set_rate_limit_reset(2);
    let body = json!({"receive_id": ids::GROUP_CHAT, "msg_type": "text", "content": text("x"), "uuid": "cm_rl"});
    let (s, h, v) = api
        .call(
            reqwest::Method::POST,
            "/open-apis/im/v1/messages?receive_id_type=chat_id",
            Some(body.clone()),
        )
        .await;
    assert_eq!((s, v["code"].as_i64()), (429, Some(99991400)));
    assert_eq!(h["x-ogw-ratelimit-reset"], "2");
    assert!(h.contains_key("x-ogw-ratelimit-limit"));
    let (s, _, _) = api
        .call(
            reqwest::Method::POST,
            "/open-apis/im/v1/messages?receive_id_type=chat_id",
            Some(body.clone()),
        )
        .await;
    assert_eq!(s, 200);
    let (s, _, _) = api
        .call(
            reqwest::Method::POST,
            "/open-apis/im/v1/messages?receive_id_type=chat_id",
            Some(body),
        )
        .await;
    assert_eq!(s, 200);
    assert_eq!(fake.sent().len(), 1);

    // Global n-th request.
    let next = fake.requests().len() as u64 + 1;
    fake.rate_limit_at(next);
    assert_eq!(api.get("/open-apis/bot/v3/info").await["code"], 99991400);
    assert_eq!(api.get("/open-apis/bot/v3/info").await["code"], 0);
}

#[tokio::test]
async fn denied_scope() {
    let fake = FakeFeishu::start().await.unwrap();
    let api = Api::new(&fake).await;
    fake.deny(Endpoint::ContactScopes);
    assert_eq!(
        api.get("/open-apis/contact/v3/scopes").await["code"],
        99991672
    );
    fake.allow(Endpoint::ContactScopes);
    assert_eq!(api.get("/open-apis/contact/v3/scopes").await["code"], 0);
}

#[tokio::test]
async fn chats_members_paging_and_external() {
    let fake = FakeFeishu::start().await.unwrap();
    let api = Api::new(&fake).await;
    let c = api
        .get(&format!("/open-apis/im/v1/chats/{}", ids::EXTERNAL_CHAT))
        .await;
    assert_eq!(c["data"]["external"], true);
    assert_eq!(c["data"]["chat_mode"], "group");
    assert_eq!(c["data"]["user_count"], "3");
    assert_eq!(
        api.get(&format!("/open-apis/im/v1/chats/{}", ids::TOPIC_CHAT))
            .await["data"]["chat_mode"],
        "topic"
    );
    assert_eq!(
        api.get("/open-apis/im/v1/chats/oc_missing").await["code"],
        232006
    );

    let mut names = Vec::new();
    let mut token = String::new();
    let mut pages = 0;
    loop {
        let v = api
            .get(&format!("/open-apis/im/v1/chats/{}/members?member_id_type=union_id&page_size=2&page_token={token}", ids::EXTERNAL_CHAT))
            .await;
        pages += 1;
        assert_eq!(v["data"]["member_total"], 3);
        for m in v["data"]["items"].as_array().unwrap() {
            assert_eq!(m["member_id_type"], "union_id");
            names.push((
                m["name"].as_str().unwrap().to_string(),
                m["tenant_key"].as_str().unwrap().to_string(),
            ));
        }
        if v["data"]["has_more"] != true {
            break;
        }
        token = v["data"]["page_token"].as_str().unwrap().to_string();
    }
    assert_eq!(pages, 2);
    assert_eq!(names.len(), 3);
    assert!(names.contains(&("赵客户".into(), ids::EXTERNAL_TENANT_KEY.into())));

    let l = api.get("/open-apis/im/v1/chats?page_size=100").await;
    let ids_: Vec<_> = l["data"]["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["chat_id"].clone())
        .collect();
    assert_eq!(ids_.len(), 3, "p2p chats are not listed");
}

#[tokio::test]
async fn history_and_resources() {
    let fake = FakeFeishu::start().await.unwrap();
    let (url, _h) = start_hook(&fake).await;
    let p = fake.pusher(url);
    p.push_envelope(fixture("message_p2p_text").unwrap())
        .await
        .unwrap();
    p.push_envelope(fixture("message_image").unwrap())
        .await
        .unwrap();
    let api = Api::new(&fake).await;
    api.post(
        "/open-apis/im/v1/messages?receive_id_type=chat_id",
        json!({"receive_id": ids::P2P_CHAT, "msg_type": "text", "content": text("按计划")}),
    )
    .await;
    let v = api
        .get(&format!("/open-apis/im/v1/messages?container_id_type=chat&container_id={}&sort_type=ByCreateTimeDesc&page_size=20", ids::P2P_CHAT))
        .await;
    let items = v["data"]["items"].as_array().unwrap();
    assert_eq!(items.len(), 3);
    assert_eq!(items[0]["sender"]["sender_type"], "app");
    assert_eq!(items[2]["message_id"], "om_p2ptext00000000000000000000001");
    assert_eq!(items[2]["msg_type"], "text");

    let r = reqwest::Client::new()
        .get(format!("{}/open-apis/im/v1/messages/om_file000000000000000000000000001/resources/file_v3_fake0000-0000-0000-0000-000000000001g?type=file", fake.base_url()))
        .bearer_auth(&api.token)
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200);
    assert!(
        r.headers()["content-disposition"]
            .to_str()
            .unwrap()
            .contains("UTF-8''")
    );
    assert!(
        String::from_utf8(r.bytes().await.unwrap().to_vec())
            .unwrap()
            .contains("23:10")
    );
    let miss = api.get("/open-apis/im/v1/messages/om_image00000000000000000000000001/resources/nope?type=image").await;
    assert_eq!(miss["code"], 234003);
}

#[tokio::test]
async fn contact_directory() {
    let fake = FakeFeishu::start().await.unwrap();
    let api = Api::new(&fake).await;
    let s = api.get("/open-apis/contact/v3/scopes?user_id_type=union_id&department_id_type=open_department_id").await;
    assert_eq!(s["data"]["department_ids"], json!(["0"]));

    let d = api
        .get("/open-apis/contact/v3/departments/0/children?department_id_type=open_department_id&user_id_type=union_id&fetch_child=true&page_size=50")
        .await;
    let items = d["data"]["items"].as_array().unwrap();
    assert_eq!(items.len(), 3);
    let plat = items
        .iter()
        .find(|x| x["open_department_id"] == ids::DEPT_PLATFORM)
        .unwrap();
    assert_eq!(plat["parent_department_id"], ids::DEPT_ENG);
    assert_eq!(plat["leader_user_id"], ids::ALICE.union_id);
    assert_eq!(plat["status"]["is_deleted"], false);
    let direct = api
        .get("/open-apis/contact/v3/departments/0/children?page_size=50")
        .await;
    assert_eq!(direct["data"]["items"].as_array().unwrap().len(), 2);

    let u = api
        .get(&format!("/open-apis/contact/v3/users/find_by_department?department_id={}&department_id_type=open_department_id&user_id_type=union_id&page_size=50", ids::DEPT_PLATFORM))
        .await;
    let users = u["data"]["items"].as_array().unwrap();
    assert_eq!(users.len(), 2);
    let carol = users
        .iter()
        .find(|x| x["union_id"] == ids::CAROL.union_id)
        .unwrap();
    assert_eq!(carol["status"]["is_resigned"], true);
    let alice = users
        .iter()
        .find(|x| x["union_id"] == ids::ALICE.union_id)
        .unwrap();
    assert_eq!(alice["leader_user_id"], ids::OWNER.union_id);
    assert_eq!(alice["department_ids"], json!([ids::DEPT_PLATFORM]));

    // Narrow the scope to Engineering: Sales becomes invisible.
    fake.world(|w| w.scope_department_ids = vec![ids::DEPT_ENG.into()]);
    let d = api
        .get("/open-apis/contact/v3/departments/0/children?fetch_child=true")
        .await;
    assert_eq!(d["data"]["items"].as_array().unwrap().len(), 2);
    let no = api
        .get(&format!(
            "/open-apis/contact/v3/users/find_by_department?department_id={}",
            ids::DEPT_SALES
        ))
        .await;
    assert_eq!(no["code"], 40004);
}

#[tokio::test]
async fn user_oauth_round_trip_and_send_as_user() {
    let fake = FakeFeishu::start().await.unwrap();
    let base = fake.base_url().to_string();
    let http = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .unwrap();
    let verifier = "verifier-0123456789-0123456789-0123456789";
    let challenge = {
        use base64::Engine as _;
        use sha2::Digest as _;
        base64::engine::general_purpose::URL_SAFE_NO_PAD
            .encode(sha2::Sha256::digest(verifier.as_bytes()))
    };
    let r = http
        .get(format!("{base}/open-apis/authen/v1/authorize"))
        .query(&[
            ("client_id", ids::APP_ID),
            ("redirect_uri", "http://localhost/cb"),
            ("response_type", "code"),
            ("scope", "im:message.send_as_user offline_access"),
            ("state", "st 1"),
            ("code_challenge", &challenge),
            ("code_challenge_method", "S256"),
        ])
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 302);
    let loc = r.headers()["location"].to_str().unwrap().to_string();
    assert!(
        loc.starts_with("http://localhost/cb?code=") && loc.ends_with("&state=st%201"),
        "{loc}"
    );
    let code = loc
        .split("code=")
        .nth(1)
        .unwrap()
        .split('&')
        .next()
        .unwrap();

    let tok = |body: Value| {
        let http = http.clone();
        let base = base.clone();
        async move {
            http.post(format!("{base}/open-apis/authen/v2/oauth/token"))
                .json(&body)
                .send()
                .await
                .unwrap()
                .json::<Value>()
                .await
                .unwrap()
        }
    };
    let bad = tok(json!({"grant_type": "authorization_code", "client_id": ids::APP_ID, "client_secret": ids::APP_SECRET,
        "code": code, "redirect_uri": "http://localhost/cb", "code_verifier": "wrong"})).await;
    assert_eq!(bad["error"], "invalid_grant");
    // The failed exchange consumed the code, as Feishu codes are single-use; start over.
    let r = http
        .get(format!("{base}/open-apis/authen/v1/authorize?client_id={}&redirect_uri=http://localhost/cb&code_challenge={challenge}", ids::APP_ID))
        .send()
        .await
        .unwrap();
    let loc = r.headers()["location"].to_str().unwrap().to_string();
    let code = loc
        .split("code=")
        .nth(1)
        .unwrap()
        .split('&')
        .next()
        .unwrap()
        .to_string();
    let t = tok(json!({"grant_type": "authorization_code", "client_id": ids::APP_ID, "client_secret": ids::APP_SECRET,
        "code": code, "redirect_uri": "http://localhost/cb", "code_verifier": verifier})).await;
    assert_eq!(t["code"], 0);
    assert_eq!(t["token_type"], "Bearer");
    let user_token = t["access_token"].as_str().unwrap().to_string();
    let refreshed = tok(json!({"grant_type": "refresh_token", "client_id": ids::APP_ID, "client_secret": ids::APP_SECRET,
        "refresh_token": t["refresh_token"]})).await;
    assert_eq!(refreshed["code"], 0);

    let info: Value = http
        .get(format!("{base}/open-apis/authen/v1/user_info"))
        .bearer_auth(&user_token)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(info["data"]["union_id"], ids::OWNER.union_id);

    let sent: Value = http
        .post(format!(
            "{base}/open-apis/im/v1/messages?receive_id_type=chat_id"
        ))
        .bearer_auth(&user_token)
        .json(
            &json!({"receive_id": ids::GROUP_CHAT, "msg_type": "text", "content": text("我来看")}),
        )
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(sent["data"]["sender"]["sender_type"], "user");
    assert_eq!(fake.sent()[0].as_user.as_deref(), Some(ids::OWNER.open_id));
}

#[test]
fn fixtures_are_well_formed() {
    let all = all_fixtures();
    assert_eq!(all.len(), 20);
    let mut event_ids = HashSet::new();
    for (name, v) in &all {
        let Some(h) = v.get("header") else { continue };
        for k in [
            "event_id",
            "event_type",
            "create_time",
            "token",
            "app_id",
            "tenant_key",
        ] {
            assert!(h[k].is_string(), "{name}: header.{k}");
        }
        assert_eq!(v["schema"], "2.0");
        assert!(
            event_ids.insert(h["event_id"].as_str().unwrap().to_string()),
            "{name}: duplicate event_id"
        );
        let t = h["event_type"].as_str().unwrap();
        if name.starts_with("message_") {
            assert_eq!(t, "im.message.receive_v1");
            let m = &v["event"]["message"];
            let content: Value = serde_json::from_str(m["content"].as_str().unwrap()).unwrap();
            assert!(content.is_object(), "{name}: content is a JSON string");
            for k in [
                "message_id",
                "chat_id",
                "chat_type",
                "message_type",
                "create_time",
            ] {
                assert!(m[k].is_string(), "{name}: message.{k}");
            }
            assert!(v["event"]["sender"]["sender_id"]["open_id"].is_string());
        }
    }
    assert_eq!(event_ids.len(), 18);
}
