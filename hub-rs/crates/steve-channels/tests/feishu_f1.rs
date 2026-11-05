//! F1 acceptance on the fake Feishu: the bot connection
//! through the real connect route (credentials ciphertext in the system vault, `verify` with
//! app_name and the bot's open_id, the self-built check, the permission check), tenant tokens
//! cached until 5 minutes before expiry and re-minted once after an invalid-token error, and the
//! user connection by OAuth + PKCE and by the lark-cli device flow.

use std::sync::Arc;
use std::time::Duration;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, header};
use serde_json::{Value, json};
use steve_channels::adapters::feishu::auth::{DeviceFlows, FeishuApp, FeishuUser, device_routes};
use steve_channels::adapters::feishu::client::{FeishuClient, FeishuError};
use steve_channels::adapters::feishu::verify::{self, NOT_SELF_BUILT};
use steve_channels::fake::feishu::{Endpoint, FakeFeishu, World, ids};
use steve_modules::Caller;
use steve_modules::accounts::connections::{
    self, Connections, ConnectionsConfig, Health, ProviderError,
};
use steve_modules::testkit::{ME, OTHER, TestEnv, WS, caller};
use steve_modules::vault::Vault;
use steve_modules::vault::keystore::FileKeyStore;
use tempfile::TempDir;
use tower::ServiceExt;

const ORIGIN: &str = "http://hub.test:8440";
const VTOKEN: &str = "PLAINTEXT-verification-token";
const EKEY: &str = "PLAINTEXT-encrypt-key";

struct H {
    env: TestEnv,
    _keys: TempDir,
    vault: Vault,
    conns: Connections,
    api: Router,
    cb: Router,
    fake: FakeFeishu,
}

async fn setup() -> H {
    setup_with(World::sample()).await
}

async fn setup_with(world: World) -> H {
    let fake = FakeFeishu::start_with(world).await.expect("fake feishu");
    let env = TestEnv::new(connections::register);
    let keys = tempfile::tempdir().expect("tmp");
    let vault = Vault::new(
        env.store().clone(),
        Arc::new(FileKeyStore::open(keys.path()).expect("keys")),
    );
    vault.init(WS, ME).expect("vault init");
    let cfg = ConnectionsConfig::new(vec![ORIGIN.into()], true, |_| None);
    let user = Arc::new(FeishuUser::with_base(fake.base_url()));
    let conns = Connections::builder(env.modules.clone(), vault.clone(), cfg)
        .provider(Arc::new(FeishuApp::new()))
        .provider(user.clone())
        .build();
    let api = connections::routes(conns.clone())
        .merge(device_routes(DeviceFlows::new(conns.clone(), user)))
        .with_state(env.modules.clone());
    let cb = connections::callback_routes(conns.clone()).with_state(env.modules.clone());
    H {
        env,
        _keys: keys,
        vault,
        conns,
        api,
        cb,
        fake,
    }
}

async fn send(
    r: &Router,
    who: &Caller,
    method: &str,
    path: &str,
    body: Option<Value>,
) -> (u16, Value, axum::http::HeaderMap) {
    let mut req = Request::builder()
        .method(method)
        .uri(path)
        .header(header::HOST, "hub.test:8440");
    if body.is_some() {
        req = req.header(header::CONTENT_TYPE, "application/json");
    }
    let mut req = req
        .body(match body {
            Some(b) => Body::from(b.to_string()),
            None => Body::empty(),
        })
        .expect("request");
    req.extensions_mut().insert(who.clone());
    let res = r.clone().oneshot(req).await.expect("infallible");
    let status = res.status().as_u16();
    let headers = res.headers().clone();
    let bytes = axum::body::to_bytes(res.into_body(), 1 << 20)
        .await
        .expect("body");
    let v = serde_json::from_slice(&bytes)
        .unwrap_or_else(|_| Value::String(String::from_utf8_lossy(&bytes).into()));
    (status, v, headers)
}

fn one(h: &H, sql: &str, p: &str) -> Vec<Value> {
    let (sql, p) = (sql.to_string(), p.to_string());
    h.env
        .store()
        .reader(move |c| {
            let mut st = c.prepare(&sql)?;
            let n = st.column_count();
            let row = st.query_row([&p], |r| {
                (0..n)
                    .map(|i| {
                        r.get::<_, Option<String>>(i)
                            .map(|s| s.map(Value::String).unwrap_or(Value::Null))
                            .or_else(|_| r.get::<_, i64>(i).map(Value::from))
                    })
                    .collect::<Result<Vec<_>, _>>()
            })?;
            Ok::<_, steve_store::StoreError>(row)
        })
        .expect("row")
}

fn count(h: &H, sql: &str) -> i64 {
    let sql = sql.to_string();
    h.env
        .store()
        .reader(move |c| {
            Ok::<_, steve_store::StoreError>(c.query_row(&sql, [], |r| r.get::<_, i64>(0))?)
        })
        .expect("count")
}

fn connect_body(base: &str) -> Value {
    json!({
        "adapter": "feishu_app",
        "fields": {
            "App ID": ids::APP_ID,
            "App Secret": ids::APP_SECRET,
            "Verification Token": VTOKEN,
            "Encrypt Key": EKEY,
        },
        "config": {"api_base": base},
    })
}

/// Connects the bot; returns (connection id, account id).
async fn connect_bot(h: &H) -> (String, String) {
    let me = caller(WS, ME);
    let (s, v, _) = send(
        &h.api,
        &me,
        "POST",
        "/connections",
        Some(connect_body(h.fake.base_url())),
    )
    .await;
    assert_eq!(s, 200, "{v}");
    (
        v["id"].as_str().expect("id").to_string(),
        v["account_id"].as_str().expect("acc").to_string(),
    )
}

fn config_of(h: &H, con: &str) -> Value {
    let c = one(h, "SELECT config FROM connections WHERE id = ?", con);
    serde_json::from_str(c[0].as_str().expect("config")).expect("json")
}

fn status_of(h: &H, con: &str) -> String {
    one(h, "SELECT status FROM connections WHERE id = ?", con)[0]
        .as_str()
        .expect("status")
        .to_string()
}

fn files_contain(dir: &std::path::Path, needle: &str) -> bool {
    std::fs::read_dir(dir).expect("dir").flatten().any(|e| {
        let p = e.path();
        if p.is_dir() {
            return files_contain(&p, needle);
        }
        std::fs::read(&p)
            .map(|b| b.windows(needle.len()).any(|w| w == needle.as_bytes()))
            .unwrap_or(false)
    })
}

#[tokio::test(flavor = "multi_thread")]
async fn bot_connects_with_verify_and_secrets_only_as_ciphertext() {
    let h = setup().await;
    let (con, acc) = connect_bot(&h).await;

    // verify: handle app_id, display name app_name, the bot's open_id under this connection
    let a = one(
        &h,
        "SELECT a.handle, a.display_name, a.kind, a.system_id FROM accounts a WHERE a.id = ?",
        &acc,
    );
    assert_eq!(
        a,
        vec![
            json!(ids::APP_ID),
            json!(ids::BOT_NAME),
            json!("app_bot"),
            json!("feishu_app")
        ]
    );
    let handles = one(
        &h,
        "SELECT group_concat(handle) FROM account_handles WHERE connection_id = ?",
        &con,
    );
    assert!(
        handles[0]
            .as_str()
            .unwrap_or_default()
            .contains(ids::BOT_OPEN_ID),
        "{handles:?}"
    );
    let cfg = config_of(&h, &con);
    assert_eq!(cfg["app_id"], ids::APP_ID);
    assert_eq!(cfg["app_name"], ids::BOT_NAME);
    assert_eq!(cfg["bot_open_id"], ids::BOT_OPEN_ID);
    assert_eq!(cfg["self_built"], true);
    assert_eq!(cfg["missing_permissions"], json!([]));
    let perms = cfg["permissions"].as_array().expect("permissions");
    let st = |scope: &str| {
        perms
            .iter()
            .find(|p| p["scope"] == scope)
            .map(|p| p["status"].clone())
    };
    assert_eq!(st("im:chat:readonly"), Some(json!("granted")));
    assert_eq!(st("im:message.group_msg"), Some(json!("granted")));
    assert_eq!(st("contact:contact.base:readonly"), Some(json!("granted")));
    assert_eq!(st("im:resource"), Some(json!("unknown")));

    // verify minted exactly one tenant token for all its calls
    assert_eq!(h.fake.tokens_issued(), 1);

    // the three secrets are in the system vault, readable only through it…
    let item = one(
        &h,
        "SELECT credential_item FROM connections WHERE id = ?",
        &con,
    )[0]
    .as_str()
    .expect("item")
    .to_string();
    let vals = h
        .vault
        .with_credential(WS, &item, "test", "测试", |c| {
            ["app_secret", "verification_token", "encrypt_key"].map(|k| c.text(k).map(String::from))
        })
        .expect("credential");
    assert_eq!(
        vals,
        [
            Some(ids::APP_SECRET.to_string()),
            Some(VTOKEN.to_string()),
            Some(EKEY.to_string())
        ]
    );
    // …and nowhere in plain text: not in config, not in any database file (WAL included)
    let raw_cfg = one(&h, "SELECT config FROM connections WHERE id = ?", &con)[0].to_string();
    for s in [ids::APP_SECRET, VTOKEN, EKEY] {
        assert!(!raw_cfg.contains(s));
        assert!(
            !files_contain(h.env.dir.path(), s),
            "{s} in plain text on disk"
        );
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn tenant_token_cached_until_five_minutes_before_expiry() {
    let h = setup().await;
    let (con, _) = connect_bot(&h).await;
    let cx = h.conns.cx(WS, &con).expect("cx");
    let n0 = h.fake.tokens_issued();
    // verify's token is kept for the new connection
    let t1 = cx.access_token().await.expect("token");
    assert_eq!(h.fake.tokens_issued(), n0);
    let t2 = cx.access_token().await.expect("token");
    assert_eq!(t1.expose(), t2.expose());
    // the fake hands out tokens for 7200 s: 5 min + 1 s before expiry still cached…
    h.env.clock.advance((7200 - 301) * 1000);
    let _ = cx.access_token().await.expect("token");
    assert_eq!(h.fake.tokens_issued(), n0);
    // …from 5 min before, re-minted
    h.env.clock.advance(2000);
    let _ = cx.access_token().await.expect("token");
    assert_eq!(h.fake.tokens_issued(), n0 + 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn invalid_token_is_reminted_once_then_reauth() {
    let h = setup().await;
    let (con, _) = connect_bot(&h).await;
    let cx = h.conns.cx(WS, &con).expect("cx");
    let client = FeishuClient::for_cx(&cx);
    verify::bot_info(&client, &cx).await.expect("bot info");
    let n0 = h.fake.tokens_issued();
    let calls0 = h.fake.requests_to(Endpoint::BotInfo).len();

    // the cached token dies early: one invalid-token answer, one re-mint, one retry
    h.fake.expire_tokens();
    let b = verify::bot_info(&client, &cx).await.expect("retried");
    assert_eq!(b.open_id, ids::BOT_OPEN_ID);
    assert_eq!(h.fake.tokens_issued(), n0 + 1);
    let calls = h.fake.requests_to(Endpoint::BotInfo);
    assert_eq!(calls.len(), calls0 + 2);
    assert_eq!(calls[calls.len() - 2].code, 99991663);
    assert_eq!(calls[calls.len() - 1].code, 0);
    assert_eq!(status_of(&h, &con), "ok");

    // the secret was rotated in the console: re-mint fails → reauth, no second retry loop
    h.fake.expire_tokens();
    h.fake.world(|w| w.app.app_secret = "rotated".into());
    let e = verify::bot_info(&client, &cx).await.expect_err("refused");
    assert!(
        matches!(e, FeishuError::Token(ProviderError::Auth(_))),
        "{e:?}"
    );
    assert_eq!(h.fake.requests_to(Endpoint::BotInfo).len(), calls0 + 3);
    assert_eq!(status_of(&h, &con), "reauth");

    // health says the same
    let p = h.conns.provider("feishu_app").expect("provider");
    assert!(matches!(p.health(&cx).await, Health::Auth(_)));
    h.fake.world(|w| w.app.app_secret = ids::APP_SECRET.into());
    assert_eq!(p.health(&cx).await, Health::Ok);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_token_refused_twice_reports_reauth() {
    let h = setup().await;
    let (con, _) = connect_bot(&h).await;
    let cx = h.conns.cx(WS, &con).expect("cx");
    let client = FeishuClient::for_cx(&cx);
    // every new token comes back already expired
    h.fake.set_token_ttl(Duration::from_secs(0));
    h.fake.expire_tokens();
    let e = client
        .get(&cx, "/open-apis/bot/v3/info", &[])
        .await
        .expect_err("refused twice");
    assert!(matches!(e, FeishuError::TokenInvalid { .. }), "{e:?}");
    assert_eq!(status_of(&h, &con), "reauth");
}

#[tokio::test(flavor = "multi_thread")]
async fn permissions_not_enabled_are_flagged() {
    let h = setup().await;
    h.fake.deny(Endpoint::ContactScopes);
    h.fake.deny(Endpoint::ListMessages);
    let (con, _) = connect_bot(&h).await;
    let cfg = config_of(&h, &con);
    assert_eq!(
        cfg["missing_permissions"],
        json!(["im:message.group_msg", "contact:contact.base:readonly"])
    );

    let h = setup().await;
    h.fake.deny(Endpoint::ListChats);
    let (con, _) = connect_bot(&h).await;
    let cfg = config_of(&h, &con);
    assert_eq!(cfg["missing_permissions"], json!(["im:chat:readonly"]));
    let gm = cfg["permissions"]
        .as_array()
        .expect("p")
        .iter()
        .find(|p| p["scope"] == "im:message.group_msg")
        .expect("row")
        .clone();
    assert_eq!(gm["status"], "unknown", "no group to look at");
}

#[tokio::test(flavor = "multi_thread")]
async fn store_apps_are_rejected() {
    let me = caller(WS, ME);
    // app info says scene_type 1
    let mut w = World::sample();
    w.app.self_built = false;
    let h = setup_with(w).await;
    let (s, v, _) = send(
        &h.api,
        &me,
        "POST",
        "/connections",
        Some(connect_body(h.fake.base_url())),
    )
    .await;
    assert_eq!(s, 400);
    assert_eq!(v["error"], format!("验证没有通过：{NOT_SELF_BUILT}"));
    assert_eq!(count(&h, "SELECT COUNT(*) FROM connections"), 0);
    assert_eq!(
        count(
            &h,
            "SELECT COUNT(*) FROM vault_items WHERE template = 'connection_credential' AND status = 'active'"
        ),
        0
    );

    // app info not readable: the long-connection endpoint tells (403 for store apps)
    h.fake.deny(Endpoint::AppInfo);
    let (s, v, _) = send(
        &h.api,
        &me,
        "POST",
        "/connections",
        Some(connect_body(h.fake.base_url())),
    )
    .await;
    assert_eq!(s, 400, "{v}");
    assert!(h.fake.requests_to(Endpoint::WsEndpoint).len() == 1);

    // …and lets a self-built one through
    h.fake.world(|w| w.app.self_built = true);
    let (s, v, _) = send(
        &h.api,
        &me,
        "POST",
        "/connections",
        Some(connect_body(h.fake.base_url())),
    )
    .await;
    assert_eq!(s, 200, "{v}");
}

#[tokio::test(flavor = "multi_thread")]
async fn wrong_secret_is_refused_without_leaking_it() {
    let h = setup().await;
    let me = caller(WS, ME);
    let mut b = connect_body(h.fake.base_url());
    b["fields"]["App Secret"] = json!("PLAINTEXT-wrong-secret");
    let (s, v, _) = send(&h.api, &me, "POST", "/connections", Some(b)).await;
    assert_eq!(s, 400);
    assert!(
        v["error"]
            .as_str()
            .expect("e")
            .contains("App ID 或 App Secret 不对"),
        "{v}"
    );
    assert!(!v.to_string().contains("PLAINTEXT-wrong-secret"));
}

/// Start → the fake's consent page (302) → our callback; returns the callback's `Location`.
async fn oauth_round_trip(h: &H, who: &Caller, start: Value) -> String {
    let (s, v, _) = send(&h.api, who, "POST", "/connections/oauth/start", Some(start)).await;
    assert_eq!(s, 200, "{v}");
    let url = v["url"].as_str().expect("url");
    assert!(url.contains("code_challenge_method=S256"));
    assert!(url.contains(&format!("client_id={}", ids::APP_ID)));
    let res = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .expect("client")
        .get(url)
        .send()
        .await
        .expect("authorize");
    assert!(res.status().is_redirection());
    let back =
        url::Url::parse(res.headers()[header::LOCATION].to_str().expect("loc")).expect("url");
    let path = format!("{}?{}", back.path(), back.query().unwrap_or_default());
    let (s, _, hd) = send(&h.cb, who, "GET", &path, None).await;
    assert_eq!(s, 302);
    hd[header::LOCATION].to_str().expect("loc").to_string()
}

#[tokio::test(flavor = "multi_thread")]
async fn user_connects_by_oauth_with_the_apps_client() {
    let h = setup().await;
    let me = caller(WS, ME);
    let (app_con, _) = connect_bot(&h).await;
    let loc = oauth_round_trip(
        &h,
        &me,
        json!({"adapter": "feishu_user", "config": {"app_connection": app_con}}),
    )
    .await;
    let acc = loc
        .strip_prefix("/#/accounts/")
        .unwrap_or_else(|| panic!("not a success: {loc}"))
        .to_string();
    let a = one(
        &h,
        "SELECT a.handle, a.system_id, p.user_id FROM accounts a JOIN parties p ON p.id = a.party_id WHERE a.id = ?",
        &acc,
    );
    assert_eq!(
        a,
        vec![json!(ids::OWNER.union_id), json!("feishu"), json!(ME)]
    );
    let c = one(
        &h,
        "SELECT id, caps, credential_item FROM connections WHERE account_id = ?",
        &acc,
    );
    let ucon = c[0].as_str().expect("id").to_string();
    let caps: Value = serde_json::from_str(c[1].as_str().expect("caps")).expect("json");
    assert_eq!(caps, json!(["msg.receive", "msg.send_as_user"]));
    let cfg = config_of(&h, &ucon);
    assert_eq!(cfg["open_id"], ids::OWNER.open_id);
    assert_eq!(cfg["missing_permissions"], json!([]));
    assert!(cfg.get("warning").is_none());
    // tokens in the vault, not in config or on disk in plain text
    let item = c[2].as_str().expect("item").to_string();
    let rt = h
        .vault
        .with_credential(WS, &item, "test", "测试", |c| {
            c.text("refresh_token").map(String::from)
        })
        .expect("cred")
        .expect("refresh token");
    assert!(!files_contain(h.env.dir.path(), &rt));
    // the user token works through the client (`user_info`), refreshed when needed
    let ucx = h.conns.cx(WS, &ucon).expect("cx");
    let p = h.conns.provider("feishu_user").expect("p");
    assert_eq!(p.health(&ucx).await, Health::Ok);
    h.conns.refresh(WS, &ucon, true).await.expect("refresh");
    assert_eq!(p.health(&ucx).await, Health::Ok);
}

#[tokio::test(flavor = "multi_thread")]
async fn user_without_some_scopes_or_refresh_is_flagged() {
    let mut w = World::sample();
    w.app.refresh_enabled = false;
    w.app
        .user_scopes
        .retain(|s| s != "im:message.send_as_user" && s != "im:message.p2p_msg:get_as_user");
    let h = setup_with(w).await;
    let me = caller(WS, ME);
    let (app_con, _) = connect_bot(&h).await;
    let loc = oauth_round_trip(
        &h,
        &me,
        json!({"adapter": "feishu_user", "config": {"app_connection": app_con}}),
    )
    .await;
    let acc = loc.strip_prefix("/#/accounts/").expect("ok").to_string();
    let c = one(
        &h,
        "SELECT id, caps FROM connections WHERE account_id = ?",
        &acc,
    );
    let caps: Value = serde_json::from_str(c[1].as_str().expect("caps")).expect("json");
    assert_eq!(
        caps,
        json!(["msg.receive"]),
        "group get_as_user still allows polling"
    );
    let cfg = config_of(&h, c[0].as_str().expect("id"));
    assert_eq!(
        cfg["missing_permissions"],
        json!(["im:message.p2p_msg:get_as_user", "im:message.send_as_user"])
    );
    assert_eq!(cfg["warning"], verify::NO_REFRESH_HINT);
}

#[tokio::test(flavor = "multi_thread")]
async fn user_connects_by_device_code() {
    let h = setup().await;
    h.fake.set_device_interval(1);
    let me = caller(WS, ME);
    let (app_con, _) = connect_bot(&h).await;

    // someone else can't use my app connection's flow state, unknown connection is 404
    let (s, _, _) = send(
        &h.api,
        &me,
        "POST",
        "/channels/feishu/device/start",
        Some(json!({"app_connection": "con_nope"})),
    )
    .await;
    assert_eq!(s, 404);

    let (s, v, _) = send(
        &h.api,
        &me,
        "POST",
        "/channels/feishu/device/start",
        Some(json!({"app_connection": app_con})),
    )
    .await;
    assert_eq!(s, 200, "{v}");
    let id = v["id"].as_str().expect("id").to_string();
    let code = v["user_code"].as_str().expect("code").to_string();
    assert!(v.get("device_code").is_none());
    let path = format!("/channels/feishu/device/{id}");
    let (_, st, _) = send(&h.api, &me, "GET", &path, None).await;
    assert_eq!(st["status"], "pending");
    let (s, _, _) = send(&h.api, &caller(WS, OTHER), "GET", &path, None).await;
    assert_eq!(s, 404);

    assert!(h.fake.device_approve(&code));
    let mut last = Value::Null;
    for _ in 0..40 {
        tokio::time::sleep(Duration::from_millis(250)).await;
        last = send(&h.api, &me, "GET", &path, None).await.1;
        if last["status"] != "pending" {
            break;
        }
    }
    assert_eq!(last["status"], "ok", "{last}");
    let acc = last["account_id"].as_str().expect("acc");
    let a = one(
        &h,
        "SELECT handle, system_id FROM accounts WHERE id = ?",
        acc,
    );
    assert_eq!(a, vec![json!(ids::OWNER.union_id), json!("feishu")]);
    let c = one(
        &h,
        "SELECT id, config FROM connections WHERE account_id = ?",
        acc,
    );
    let cfg: Value = serde_json::from_str(c[1].as_str().expect("cfg")).expect("json");
    assert_eq!(cfg["app_connection"], app_con);
    // the device-code tokens refresh like the OAuth ones (the app's client from config)
    h.conns
        .refresh(WS, c[0].as_str().expect("id"), true)
        .await
        .expect("refresh");
}

#[tokio::test(flavor = "multi_thread")]
async fn user_oauth_needs_an_app_connection() {
    let h = setup().await;
    let me = caller(WS, ME);
    let (s, v, _) = send(
        &h.api,
        &me,
        "POST",
        "/connections/oauth/start",
        Some(json!({"adapter": "feishu_user"})),
    )
    .await;
    assert_eq!(
        (s, v["error"].as_str()),
        (400, Some("还没有配置 飞书 的 OAuth 应用"))
    );
}

// ---------------------------------------------------------------------------------------------
// Permissions learned at runtime, the one-click links, installations.

fn integrations(h: &H) -> Value {
    serde_json::to_value(h.conns.integrations(&caller(WS, ME)).expect("integrations"))
        .expect("json")
}

#[tokio::test(flavor = "multi_thread")]
async fn a_runtime_refusal_is_recorded_and_offered_as_one_link() {
    let h = setup().await;
    let (con, _) = connect_bot(&h).await;
    assert_eq!(config_of(&h, &con)["missing_permissions"], json!([]));
    let me = caller(WS, ME);
    let x = integrations(&h);
    assert_eq!(x["installations"][0]["missing"], 0);
    assert_eq!(
        x["installations"][0]["identities"][0]["connection"]["detail"]["grant"],
        Value::Null
    );

    // the members call of the inbound pipeline is refused: Feishu names the scopes and the link
    let before = one(&h, "SELECT revision FROM connections WHERE id = ?", &con);
    h.fake.deny(Endpoint::ChatMembers);
    let cx = h.conns.cx(WS, &con).expect("cx");
    let path = format!("/open-apis/im/v1/chats/{}/members", ids::GROUP_CHAT);
    let e = FeishuClient::for_cx(&cx)
        .get(&cx, &path, &[("member_id_type", "union_id")])
        .await
        .expect_err("refused");
    let FeishuError::NoScope {
        code,
        scopes,
        help_url,
        ..
    } = &e
    else {
        panic!("{e:?}")
    };
    assert_eq!(*code, 99991672);
    assert_eq!(scopes, &["im:chat:readonly", "im:chat"]);
    let link = format!(
        "https://open.feishu.cn/app/{}/auth?q=im:chat:readonly,im:chat&op_from=openapi&token_type=tenant",
        ids::APP_ID
    );
    assert_eq!(help_url.as_deref(), Some(link.as_str()));
    assert!(e.to_string().contains("im:chat:readonly"), "{e}");

    // recorded on the connection without a manual check: the scope of the table Feishu named
    let cfg = config_of(&h, &con);
    assert_eq!(cfg["missing_permissions"], json!(["im:chat:readonly"]));
    let row = cfg["permissions"]
        .as_array()
        .expect("rows")
        .iter()
        .find(|r| r["scope"] == "im:chat:readonly")
        .expect("row")
        .clone();
    assert_eq!(row["status"], "missing");
    assert!(row["refused_at"].as_i64().is_some());
    assert_eq!(cfg["permission_help"]["url"], link);
    // learned, not edited: no revision bump, so what acts on real connection changes (names,
    // the directory sync) doesn't run again because of a refusal
    let revision = one(&h, "SELECT revision FROM connections WHERE id = ?", &con);
    assert_eq!(revision, before);

    // the same refusal again changes nothing
    let _ = FeishuClient::for_cx(&cx).get(&cx, &path, &[]).await;
    assert_eq!(
        one(&h, "SELECT revision FROM connections WHERE id = ?", &con),
        revision
    );

    // the 接入 page: one installation, the channel area 缺权限, Feishu's own link preferred
    let x = integrations(&h);
    let inst = &x["installations"][0];
    assert_eq!(
        (inst["id"].as_str(), inst["missing"].as_i64()),
        (Some(con.as_str()), Some(1))
    );
    let c = &inst["identities"][0]["connection"];
    assert_eq!(inst["identities"][0]["kind"], "steve");
    let channel = c["capabilities"]
        .as_array()
        .expect("caps")
        .iter()
        .find(|a| a["area"] == "channel")
        .expect("channel")
        .clone();
    assert_eq!(channel["state"], "missing");
    assert_eq!(channel["missing_scopes"], json!(["im:chat:readonly"]));
    let d = &c["detail"];
    assert_eq!(d["grant"], json!({"url": link, "from": "feishu"}));
    assert_eq!(
        d["publish_url"],
        format!("https://open.feishu.cn/app/{}/version", ids::APP_ID)
    );
    assert_eq!(d["missing"][0]["scope"], "im:chat:readonly");

    // a second missing scope Feishu's link doesn't cover: Steve builds one link for both
    h.fake.deny(Endpoint::GetResource);
    let _ = FeishuClient::for_cx(&cx)
        .get(
            &cx,
            "/open-apis/im/v1/messages/om_x/resources/k",
            &[("type", "file")],
        )
        .await;
    let x = integrations(&h);
    let d = &x["installations"][0]["identities"][0]["connection"]["detail"];
    assert_eq!(d["grant"]["from"], "steve");
    assert_eq!(
        d["grant"]["url"],
        format!(
            "https://open.feishu.cn/app/{}/auth?q=im:chat:readonly,im:resource&op_from=openapi&token_type=tenant",
            ids::APP_ID
        )
    );
    assert_eq!(x["installations"][0]["missing"], 2);

    // enabled in the console → 我已开通，重新检查 runs the permission check again
    h.fake.allow(Endpoint::ChatMembers);
    h.fake.allow(Endpoint::GetResource);
    let (s, v, _) = send(
        &h.api,
        &me,
        "POST",
        &format!("/connections/{con}/test"),
        None,
    )
    .await;
    assert_eq!(s, 200, "{v}");
    let cfg = config_of(&h, &con);
    assert_eq!(cfg["missing_permissions"], json!([]));
    assert_eq!(cfg["permission_help"], Value::Null);
    let x = integrations(&h);
    assert_eq!(x["installations"][0]["missing"], 0);
}

#[tokio::test(flavor = "multi_thread")]
async fn you_join_the_apps_installation_and_reauth_asks_for_the_missing_scopes() {
    let mut w = World::sample();
    w.app.user_scopes.retain(|s| s != "im:message.send_as_user");
    let h = setup_with(w).await;
    let me = caller(WS, ME);
    let (app_con, _) = connect_bot(&h).await;
    let loc = oauth_round_trip(
        &h,
        &me,
        json!({"adapter": "feishu_user", "config": {"app_connection": app_con}}),
    )
    .await;
    assert!(loc.starts_with("/#/accounts/"), "{loc}");

    let x = integrations(&h);
    let insts = x["installations"].as_array().expect("installations");
    assert_eq!(insts.len(), 1, "one app, two identities: {x}");
    let kinds: Vec<_> = insts[0]["identities"]
        .as_array()
        .expect("ids")
        .iter()
        .map(|i| i["kind"].as_str().unwrap_or_default().to_string())
        .collect();
    assert_eq!(kinds, ["steve", "you"]);
    assert_eq!(insts[0]["name"], ids::BOT_NAME);
    assert_eq!(
        insts[0]["missing"], 1,
        "an optional user scope not granted counts too"
    );
    let you = &insts[0]["identities"][1]["connection"];
    assert_eq!(you["detail"]["app_connection"], app_con);
    assert_eq!(
        you["detail"]["missing"][0]["scope"],
        "im:message.send_as_user"
    );

    // re-authorizing asks for extra scopes; only scope-shaped ones
    let (s, v, _) = send(
        &h.api,
        &me,
        "POST",
        "/connections/oauth/start",
        Some(json!({"adapter": "feishu_user", "config": {
            "app_connection": app_con,
            "extra_scopes": ["search:message", "im:message", "bad scope&x=1"],
        }})),
    )
    .await;
    assert_eq!(s, 200, "{v}");
    let url = url::Url::parse(v["url"].as_str().expect("url")).expect("url");
    let scope = url
        .query_pairs()
        .find(|(k, _)| k == "scope")
        .map(|(_, v)| v.to_string())
        .expect("scope");
    let scopes: Vec<&str> = scope.split(' ').collect();
    assert!(scopes.contains(&"search:message"), "{scope}");
    assert_eq!(scopes.iter().filter(|s| **s == "im:message").count(), 1);
    assert!(!scope.contains("bad"), "{scope}");
}
