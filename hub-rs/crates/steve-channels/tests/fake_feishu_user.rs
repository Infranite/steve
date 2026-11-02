//! The fake's user-identity side (feishu-lark-cli.md §2.3–2.4): OAuth + PKCE, single-use refresh
//! tokens, device flow, user-token chats / messages / search, sending as the user.

use std::time::Duration;

use base64::Engine as _;
use serde_json::{Value, json};
use sha2::Digest as _;
use steve_channels::fake::feishu::{FakeFeishu, codes, ids};

struct U {
    base: String,
    http: reqwest::Client,
}

impl U {
    fn new(f: &FakeFeishu) -> Self {
        Self {
            base: f.base_url().to_string(),
            http: reqwest::Client::builder()
                .redirect(reqwest::redirect::Policy::none())
                .build()
                .unwrap(),
        }
    }

    /// Full authorization-code + PKCE round trip; returns the token response.
    async fn login(&self, scope: Option<&str>) -> Value {
        let verifier = "v-0123456789-0123456789-0123456789-0123456789";
        let challenge = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .encode(sha2::Sha256::digest(verifier.as_bytes()));
        let mut q = vec![
            ("client_id", ids::APP_ID.to_string()),
            ("redirect_uri", "http://127.0.0.1:1/cb".into()),
            ("response_type", "code".into()),
            ("code_challenge", challenge),
            ("code_challenge_method", "S256".into()),
        ];
        if let Some(s) = scope {
            q.push(("scope", s.into()));
        }
        let r = self
            .http
            .get(format!("{}/open-apis/authen/v1/authorize", self.base))
            .query(&q)
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
            .unwrap();
        self.token(json!({"grant_type": "authorization_code", "code": code,
            "redirect_uri": "http://127.0.0.1:1/cb", "code_verifier": verifier}))
            .await
    }

    async fn token(&self, mut body: Value) -> Value {
        body["client_id"] = json!(ids::APP_ID);
        body["client_secret"] = json!(ids::APP_SECRET);
        self.http
            .post(format!("{}/open-apis/authen/v2/oauth/token", self.base))
            .json(&body)
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap()
    }

    async fn refresh(&self, rt: &Value) -> Value {
        self.token(json!({"grant_type": "refresh_token", "refresh_token": rt}))
            .await
    }

    async fn call(
        &self,
        token: &str,
        method: reqwest::Method,
        path: &str,
        body: Option<Value>,
    ) -> Value {
        let mut rb = self
            .http
            .request(method, format!("{}{path}", self.base))
            .bearer_auth(token);
        if let Some(b) = body {
            rb = rb.json(&b);
        }
        rb.send().await.unwrap().json().await.unwrap()
    }

    async fn get(&self, token: &str, path: &str) -> Value {
        self.call(token, reqwest::Method::GET, path, None).await
    }

    async fn post(&self, token: &str, path: &str, body: Value) -> Value {
        self.call(token, reqwest::Method::POST, path, Some(body))
            .await
    }

    async fn tenant(&self) -> String {
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
        v["tenant_access_token"].as_str().unwrap().to_string()
    }

    async fn form(&self, path: &str, form: &[(&str, &str)], basic: bool) -> Value {
        let mut rb = self
            .http
            .post(format!("{}{path}", self.base))
            .header("Content-Type", "application/x-www-form-urlencoded")
            .body(
                form.iter()
                    .map(|(k, v)| format!("{k}={}", v.replace(' ', "+").replace(':', "%3A")))
                    .collect::<Vec<_>>()
                    .join("&"),
            );
        if basic {
            rb = rb.basic_auth(ids::APP_ID, Some(ids::APP_SECRET));
        }
        rb.send().await.unwrap().json().await.unwrap()
    }
}

fn s(v: &Value) -> &str {
    v.as_str().unwrap_or_default()
}

const ALL: &str = "offline_access im:message im:message:readonly im:message.p2p_msg:get_as_user \
    im:message.group_msg:get_as_user im:message.send_as_user im:chat:read search:message";

#[tokio::test]
async fn grants_and_single_use_refresh_tokens() {
    let f = FakeFeishu::start().await.unwrap();
    let u = U::new(&f);

    // Requested ∩ enabled; unknown scopes are dropped.
    let t = u
        .login(Some("offline_access im:chat:read not:a:scope"))
        .await;
    assert_eq!(t["code"], 0);
    assert_eq!(t["scope"], "offline_access im:chat:read");
    assert_eq!(t["refresh_token_expires_in"], 604800);

    let r1 = u.refresh(&t["refresh_token"]).await;
    assert_eq!(r1["code"], 0, "{r1}");
    assert_eq!(r1["scope"], t["scope"]);
    assert_ne!(r1["refresh_token"], t["refresh_token"]);
    // The old refresh token is spent; a second use fails.
    let again = u.refresh(&t["refresh_token"]).await;
    assert_eq!(again["code"], codes::REFRESH_TOKEN_REUSED);
    assert_eq!(again["error"], "invalid_grant");
    // The rotated one still works.
    let r2 = u.refresh(&r1["refresh_token"]).await;
    assert_eq!(r2["code"], 0);
    assert_eq!(
        u.refresh(&json!("garbage")).await["code"],
        codes::REFRESH_TOKEN_INVALID
    );

    f.expire_refresh_tokens();
    assert_eq!(
        u.refresh(&r2["refresh_token"]).await["code"],
        codes::REFRESH_TOKEN_EXPIRED
    );

    let t = u.login(None).await;
    f.revoke_user_grant();
    assert_eq!(
        u.refresh(&t["refresh_token"]).await["code"],
        codes::REFRESH_TOKEN_REVOKED
    );
    let info = u
        .get(s(&t["access_token"]), "/open-apis/authen/v1/user_info")
        .await;
    assert_ne!(info["code"], 0);

    // No offline_access, or the console switch off: no refresh token at all.
    let t = u.login(Some("im:chat:read")).await;
    assert!(t.get("refresh_token").is_none());
    f.world(|w| w.app.refresh_enabled = false);
    let t = u.login(Some(ALL)).await;
    assert!(t.get("refresh_token").is_none());
    assert!(s(&t["scope"]).contains("offline_access"));
}

#[tokio::test]
async fn device_flow() {
    let f = FakeFeishu::start().await.unwrap();
    let u = U::new(&f);
    f.set_device_interval(0);
    let bad = u
        .form(
            "/oauth/v1/device_authorization",
            &[("client_id", ids::APP_ID)],
            false,
        )
        .await;
    assert_eq!(bad["error"], "invalid_client");

    let d = u
        .form(
            "/oauth/v1/device_authorization",
            &[
                ("client_id", ids::APP_ID),
                ("scope", "im:chat:read offline_access"),
            ],
            true,
        )
        .await;
    assert_eq!(d["interval"], 0);
    assert!(s(&d["verification_uri_complete"]).contains(s(&d["user_code"])));
    let poll = |code: String| {
        let u = &u;
        async move {
            u.form(
                "/oauth/v3/token",
                &[
                    ("grant_type", "urn:ietf:params:oauth:grant-type:device_code"),
                    ("device_code", &code),
                    ("client_id", ids::APP_ID),
                    ("client_secret", ids::APP_SECRET),
                ],
                false,
            )
            .await
        }
    };
    let dc = s(&d["device_code"]).to_string();
    assert_eq!(poll(dc.clone()).await["error"], "authorization_pending");
    assert!(f.device_approve(s(&d["user_code"])));
    let t = poll(dc.clone()).await;
    assert_eq!(t["scope"], "im:chat:read offline_access");
    assert!(t["refresh_token"].is_string());
    let info = u
        .get(s(&t["access_token"]), "/open-apis/authen/v1/user_info")
        .await;
    assert_eq!(info["data"]["open_id"], ids::OWNER.open_id);
    assert_eq!(poll(dc).await["error"], "invalid_grant");

    // Polling faster than `interval` → slow_down; denial; expiry.
    f.set_device_interval(60);
    let d = u
        .form(
            "/oauth/v1/device_authorization",
            &[("client_id", ids::APP_ID)],
            true,
        )
        .await;
    let dc = s(&d["device_code"]).to_string();
    assert_eq!(poll(dc.clone()).await["error"], "authorization_pending");
    assert_eq!(poll(dc.clone()).await["error"], "slow_down");
    assert!(f.device_deny(s(&d["user_code"])));
    assert_eq!(poll(dc).await["error"], "access_denied");

    f.set_device_ttl(Duration::from_millis(1));
    let d = u
        .form(
            "/oauth/v1/device_authorization",
            &[("client_id", ids::APP_ID)],
            true,
        )
        .await;
    tokio::time::sleep(Duration::from_millis(5)).await;
    assert_eq!(
        poll(s(&d["device_code"]).to_string()).await["error"],
        "expired_token"
    );
}

fn chat_ids(v: &Value) -> Vec<String> {
    v["data"]["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| s(&c["chat_id"]).to_string())
        .collect()
}

#[tokio::test]
async fn user_token_lists_p2p_chats_by_activity() {
    let f = FakeFeishu::start().await.unwrap();
    let u = U::new(&f);
    let tok = u.login(Some(ALL)).await;
    let ut = s(&tok["access_token"]).to_string();
    let path = "/open-apis/im/v1/chats?types=p2p,group&sort_type=ByActiveTimeDesc&page_size=20";

    let v = u.get(&ut, path).await;
    let got = chat_ids(&v);
    // Bob's DM is the most recent activity; the bot-less group is there too.
    assert_eq!(got[0], ids::USER_P2P_CHAT, "{v}");
    assert!(got.contains(&ids::NO_BOT_GROUP.to_string()));
    assert!(
        !got.contains(&ids::P2P_CHAT.to_string()),
        "the bot's DM with Alice isn't yours"
    );
    let dm = &v["data"]["items"][0];
    assert_eq!(dm["chat_mode"], "p2p");
    assert_eq!(dm["name"], "张伟");
    // Groups only by default.
    let groups = chat_ids(&u.get(&ut, "/open-apis/im/v1/chats").await);
    assert!(!groups.contains(&ids::USER_P2P_CHAT.to_string()));

    // The bot never sees p2p chats or groups it isn't in.
    let tt = u.tenant().await;
    let bot = chat_ids(&u.get(&tt, path).await);
    assert!(!bot.contains(&ids::USER_P2P_CHAT.to_string()));
    assert!(!bot.contains(&ids::NO_BOT_GROUP.to_string()));
    assert!(!bot.contains(&ids::P2P_CHAT.to_string()));

    let narrow = u.login(Some("im:message")).await;
    let v = u.get(s(&narrow["access_token"]), path).await;
    assert_eq!(v["code"], codes::USER_SCOPE_MISSING);
}

#[tokio::test]
async fn user_token_reads_history_with_restrictions() {
    let f = FakeFeishu::start().await.unwrap();
    let u = U::new(&f);
    let ut = s(&u.login(Some(ALL)).await["access_token"]).to_string();
    let tt = u.tenant().await;
    let list = |chat: &str, extra: &str| {
        format!(
            "/open-apis/im/v1/messages?container_id_type=chat&container_id={chat}&sort_type=ByCreateTimeAsc&page_size=50{extra}"
        )
    };

    let v = u.get(&ut, &list(ids::USER_P2P_CHAT, "")).await;
    assert_eq!(v["code"], 0, "{v}");
    assert_eq!(v["data"]["items"][0]["sender"]["id"], ids::BOB.open_id);
    // start_time cursor (seconds) excludes older messages.
    let v = u
        .get(&ut, &list(ids::USER_P2P_CHAT, "&start_time=1761899601"))
        .await;
    assert_eq!(v["data"]["items"].as_array().unwrap().len(), 0);
    // The bot can't read your DM.
    let v = u.get(&tt, &list(ids::USER_P2P_CHAT, "")).await;
    assert_eq!(v["code"], codes::IM_BOT_NOT_IN_CHAT);

    // Group without the bot: readable by default; the stricter reading is a switch.
    assert_eq!(u.get(&ut, &list(ids::NO_BOT_GROUP, "")).await["code"], 0);
    f.world(|w| w.user_read_requires_bot = true);
    assert_eq!(
        u.get(&ut, &list(ids::NO_BOT_GROUP, "")).await["code"],
        codes::IM_BOT_NOT_IN_CHAT
    );
    assert_eq!(u.get(&ut, &list(ids::USER_P2P_CHAT, "")).await["code"], 0);

    // Threads with a user token.
    let v = u
        .get(
            &ut,
            "/open-apis/im/v1/messages?container_id_type=thread&container_id=omt_fake0000000001",
        )
        .await;
    assert_eq!(v["data"]["items"].as_array().unwrap().len(), 1);

    // Missing get_as_user scope for p2p.
    let narrow = u
        .login(Some("im:message im:message.group_msg:get_as_user"))
        .await;
    let nt = s(&narrow["access_token"]).to_string();
    assert_eq!(
        u.get(&nt, &list(ids::USER_P2P_CHAT, "")).await["code"],
        codes::USER_SCOPE_MISSING
    );
    assert_eq!(u.get(&nt, &list(ids::GROUP_CHAT, "")).await["code"], 0);

    // External sharing switches user-identity history off entirely; the bot is unaffected.
    f.world(|w| w.app.external_sharing = true);
    assert_eq!(
        u.get(&ut, &list(ids::USER_P2P_CHAT, "")).await["code"],
        codes::USER_IDENTITY_SHARED_APP
    );
    assert_eq!(u.get(&tt, &list(ids::GROUP_CHAT, "")).await["code"], 0);
}

#[tokio::test]
async fn send_as_user_needs_scope_and_is_rate_limited() {
    let f = FakeFeishu::start().await.unwrap();
    let u = U::new(&f);
    let text = |t: &str| json!({"text": t}).to_string();
    let send = "/open-apis/im/v1/messages?receive_id_type=chat_id";

    let no_send = u.login(Some("im:message im:chat:read")).await;
    let v = u
        .post(
            s(&no_send["access_token"]),
            send,
            json!({"receive_id": ids::USER_P2P_CHAT, "msg_type": "text", "content": text("x")}),
        )
        .await;
    assert_eq!(v["code"], codes::USER_SCOPE_MISSING);

    let ut = s(&u.login(Some(ALL)).await["access_token"]).to_string();
    let v = u
        .post(
            &ut,
            send,
            json!({"receive_id": ids::USER_P2P_CHAT, "msg_type": "text", "content": text("在的")}),
        )
        .await;
    assert_eq!(v["code"], 0, "{v}");
    assert_eq!(v["data"]["sender"]["id"], ids::OWNER.open_id);
    // By open_id: lands in your existing DM with Bob, not the bot's.
    let v = u
        .post(
            &ut,
            "/open-apis/im/v1/messages?receive_id_type=open_id",
            json!({"receive_id": ids::BOB.open_id, "msg_type": "text", "content": text("好")}),
        )
        .await;
    assert_eq!(v["data"]["chat_id"], ids::USER_P2P_CHAT);
    let sent = f.sent();
    assert!(
        sent.iter()
            .all(|m| m.as_user.as_deref() == Some(ids::OWNER.open_id))
    );
    // The bot can't post into your DM.
    let tt = u.tenant().await;
    let v = u
        .post(
            &tt,
            send,
            json!({"receive_id": ids::USER_P2P_CHAT, "msg_type": "text", "content": text("x")}),
        )
        .await;
    assert_eq!(v["code"], codes::IM_BOT_NOT_IN_CHAT);

    // 5 per second per user: the 6th in the window fails, then it recovers.
    tokio::time::sleep(Duration::from_millis(1100)).await;
    let mut codes_seen = Vec::new();
    for i in 0..6 {
        let v = u
            .post(&ut, send, json!({"receive_id": ids::NO_BOT_GROUP, "msg_type": "text", "content": text(&i.to_string())}))
            .await;
        codes_seen.push(v["code"].as_i64().unwrap());
    }
    assert_eq!(codes_seen, [0, 0, 0, 0, 0, codes::IM_SEND_RATE_LIMITED]);
    tokio::time::sleep(Duration::from_millis(1100)).await;
    let v = u
        .post(
            &ut,
            send,
            json!({"receive_id": ids::NO_BOT_GROUP, "msg_type": "text", "content": text("ok")}),
        )
        .await;
    assert_eq!(v["code"], 0);
    // Replies count too and go through membership, not the bot.
    let v = u
        .post(
            &ut,
            "/open-apis/im/v1/messages/om_nobot00000000000000000000000001/reply",
            json!({"msg_type": "text", "content": text("收到")}),
        )
        .await;
    assert_eq!(v["code"], 0, "{v}");
}

#[tokio::test]
async fn search_and_mget_with_user_token() {
    let f = FakeFeishu::start().await.unwrap();
    let u = U::new(&f);
    let ut = s(&u.login(Some(ALL)).await["access_token"]).to_string();
    let v = u
        .post(
            &ut,
            "/open-apis/im/v1/messages/search?page_size=20",
            json!({"query": "合同"}),
        )
        .await;
    assert_eq!(v["code"], 0, "{v}");
    let id = s(&v["data"]["items"][0]["meta_data"]["message_id"]).to_string();
    assert_eq!(id, "om_userp2p0000000000000000000000001");
    let m = u
        .get(
            &ut,
            &format!("/open-apis/im/v1/messages/mget?message_ids={id}"),
        )
        .await;
    assert_eq!(m["data"]["items"][0]["chat_id"], ids::USER_P2P_CHAT);

    // Filters, the bot-less-group switch, missing scope, bot identity.
    let v = u
        .post(
            &ut,
            "/open-apis/im/v1/messages/search",
            json!({"query": "第三章", "filter": {"chat_type": "group"}}),
        )
        .await;
    assert_eq!(v["data"]["items"].as_array().unwrap().len(), 1);
    f.world(|w| w.search_sees_no_bot_groups = false);
    let v = u
        .post(
            &ut,
            "/open-apis/im/v1/messages/search",
            json!({"query": "第三章"}),
        )
        .await;
    assert_eq!(v["data"]["items"].as_array().unwrap().len(), 0);
    let narrow = u.login(Some("im:message")).await;
    let v = u
        .post(
            s(&narrow["access_token"]),
            "/open-apis/im/v1/messages/search",
            json!({"query": "合同"}),
        )
        .await;
    assert_eq!(v["code"], codes::USER_SCOPE_MISSING);
    let tt = u.tenant().await;
    let v = u
        .post(
            &tt,
            "/open-apis/im/v1/messages/search",
            json!({"query": "合同"}),
        )
        .await;
    assert_eq!(v["data"]["items"].as_array().unwrap().len(), 0);
    let m = u
        .get(
            &tt,
            &format!("/open-apis/im/v1/messages/mget?message_ids={id}"),
        )
        .await;
    assert_eq!(m["data"]["items"].as_array().unwrap().len(), 0);
}
