//! The bot's app created from Steve (official PersonalAgent registration, re-implemented natively)
//! and the device-code login's cancel / scopes / brand, on the fake Feishu only: begin → poll →
//! credential saved (sealed with the row) → checked → a new bot connection; one active attempt per
//! person; cancel ordered against the binding; a failed check keeps the credential for a retry;
//! another person's attempt is invisible; a stop is resumed within the original deadline; brand
//! switch to Lark.

use std::sync::Arc;
use std::time::Duration;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, header};
use serde_json::{Value, json};
use steve_channels::adapters::feishu::app_registration::{Bases, Registrations, routes};
use steve_channels::adapters::feishu::auth::{DeviceFlows, FeishuApp, FeishuUser, device_routes};
use steve_channels::adapters::feishu::client::is_real_host;
use steve_channels::fake::feishu::{Endpoint, FakeFeishu, World, ids};
use steve_modules::Caller;
use steve_modules::accounts::connections::{self, Connections, ConnectionsConfig};
use steve_modules::testkit::{ME, OTHER, TestEnv, WS, WS2, caller};
use steve_modules::vault::Vault;
use steve_modules::vault::keystore::FileKeyStore;
use tempfile::TempDir;
use tower::ServiceExt;

const ORIGIN: &str = "http://hub.test:8440";
const TICK: Duration = Duration::from_millis(10);

struct H {
    env: TestEnv,
    _keys: TempDir,
    conns: Connections,
    regs: Registrations,
    api: Router,
    fake: FakeFeishu,
}

async fn setup() -> H {
    let fake = FakeFeishu::start_with(World::sample())
        .await
        .expect("fake feishu");
    assert!(!is_real_host(fake.base_url()));
    let env = TestEnv::new(connections::register);
    let keys = tempfile::tempdir().expect("tmp");
    let vault = Vault::new(
        env.store().clone(),
        Arc::new(FileKeyStore::open(keys.path()).expect("keys")),
    );
    vault.init(WS, ME).expect("vault init");
    vault.init(WS, OTHER).ok();
    let cfg = ConnectionsConfig::new(vec![ORIGIN.into()], true, |_| None);
    let user = Arc::new(FeishuUser::with_base(fake.base_url()));
    let conns = Connections::builder(env.modules.clone(), vault.clone(), cfg)
        .provider(Arc::new(FeishuApp::new()))
        .provider(user.clone())
        .build();
    let regs = Registrations::new(conns.clone(), Bases::fake(fake.base_url())).with_tick(TICK);
    let api = connections::routes(conns.clone())
        .merge(routes(regs.clone()))
        .merge(device_routes(DeviceFlows::new(conns.clone(), user.clone())))
        .with_state(env.modules.clone());
    H {
        env,
        _keys: keys,
        conns,
        regs,
        api,
        fake,
    }
}

async fn send(
    r: &Router,
    who: &Caller,
    method: &str,
    path: &str,
    body: Option<Value>,
) -> (u16, Value) {
    let mut req = Request::builder()
        .method(method)
        .uri(path)
        .header(header::HOST, "hub.test:8440");
    if body.is_some() {
        req = req.header(header::CONTENT_TYPE, "application/json");
    }
    let mut req = req
        .body(
            body.map(|b| Body::from(b.to_string()))
                .unwrap_or_else(Body::empty),
        )
        .expect("request");
    req.extensions_mut().insert(who.clone());
    let res = r.clone().oneshot(req).await.expect("infallible");
    let status = res.status().as_u16();
    let bytes = axum::body::to_bytes(res.into_body(), 1 << 20)
        .await
        .expect("body");
    let v = serde_json::from_slice(&bytes)
        .unwrap_or_else(|_| Value::String(String::from_utf8_lossy(&bytes).into()));
    (status, v)
}

fn me() -> Caller {
    caller(WS, ME)
}

fn count(h: &H, sql: &str) -> i64 {
    h.env
        .store()
        .with_conn(|c| Ok(c.query_row(sql, [], |r| r.get(0))?))
        .expect("count")
}

fn s(v: &Value) -> String {
    v.as_str().unwrap_or_default().to_string()
}

async fn begin(h: &H, brand: &str) -> Value {
    let (st, v) = send(
        &h.api,
        &me(),
        "POST",
        "/channels/feishu/registrations",
        Some(json!({"brand": brand})),
    )
    .await;
    assert_eq!(st, 200, "{v}");
    v
}

/// Polls `GET :id` until the state is not in `while_in`.
async fn settle(h: &H, id: &str, while_in: &[&str]) -> Value {
    let path = format!("/channels/feishu/registrations/{id}");
    let mut last = Value::Null;
    for _ in 0..300 {
        last = send(&h.api, &me(), "GET", &path, None).await.1;
        if !while_in.contains(&last["state"].as_str().unwrap_or_default()) {
            return last;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("still {last}");
}

const ACTIVE: &[&str] = &["pending", "credentials_saved", "verifying"];

/// Waits until the saved credential has been checked: the row reaches `credentials_saved` without an
/// error before the check claims it, so that alone is not the end of the check.
async fn checked(h: &H, id: &str) -> Value {
    let path = format!("/channels/feishu/registrations/{id}");
    let mut last = Value::Null;
    for _ in 0..300 {
        last = send(&h.api, &me(), "GET", &path, None).await.1;
        let st = last["state"].as_str().unwrap_or_default();
        let unchecked = st == "credentials_saved" && last["error_kind"].is_null();
        if !["pending", "verifying"].contains(&st) && !unchecked {
            return last;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("still {last}");
}

fn no_secrets(v: &Value) {
    let t = v.to_string();
    for bad in [
        "device_code",
        "client_secret",
        "dc-",
        ids::APP_SECRET,
        "app_secret",
    ] {
        assert!(!t.contains(bad), "{bad} leaked: {t}");
    }
}

fn begins(h: &H) -> usize {
    h.fake
        .reg_calls()
        .iter()
        .filter(|(_, a)| a == "begin")
        .count()
}

#[tokio::test(flavor = "multi_thread")]
async fn scan_confirm_and_the_bot_is_connected() {
    let h = setup().await;
    h.fake.reg_polls(vec![
        json!({"error": "authorization_pending"}),
        json!({"error": "slow_down"}),
        h.fake.reg_issued("feishu"),
    ]);
    let v = begin(&h, "feishu").await;
    no_secrets(&v);
    assert_eq!(v["state"], "pending");
    assert!(
        s(&v["verification_uri_complete"]).starts_with(h.fake.base_url()),
        "{v}"
    );
    assert!(!s(&v["user_code"]).is_empty());
    let done = settle(&h, &s(&v["id"]), ACTIVE).await;
    no_secrets(&done);
    assert_eq!(done["state"], "succeeded", "{done}");
    let conn = s(&done["connection_id"]);
    assert_eq!(
        count(
            &h,
            "SELECT count(*) FROM connections WHERE adapter = 'feishu_app'"
        ),
        1
    );
    let out = h.conns.get(&me(), &conn).expect("visible connection");
    assert_eq!(out.adapter, "feishu_app");
    // the protocol as the official clients send it
    let forms = h.fake.reg_forms();
    assert_eq!(forms[0]["action"], "begin");
    assert_eq!(forms[0]["archetype"], "PersonalAgent");
    assert_eq!(forms[0]["auth_method"], "client_secret");
    assert_eq!(forms[0]["request_user_info"], "open_id tenant_brand");
    assert!(
        forms[1..]
            .iter()
            .all(|f| f["action"] == "poll" && f["device_code"] == "dc-1")
    );
    assert_eq!(begins(&h), 1);
    // the ticket is gone, the app secret lives in the connection's credential only
    assert_eq!(
        count(
            &h,
            "SELECT count(*) FROM chan_app_registrations WHERE device_item IS NOT NULL OR vault_item IS NOT NULL"
        ),
        0
    );
    assert_eq!(
        count(
            &h,
            "SELECT count(*) FROM vault_items WHERE title = '飞书应用注册确认码' AND status = 'active'"
        ),
        0
    );
    // nothing active any more
    assert_eq!(
        send(
            &h.api,
            &me(),
            "GET",
            "/channels/feishu/registrations/current",
            None
        )
        .await
        .1,
        Value::Null
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn asking_twice_is_one_begin_and_others_see_nothing() {
    let h = setup().await;
    let (a, b) = tokio::join!(begin(&h, "feishu"), begin(&h, "feishu"));
    assert_eq!(a["id"], b["id"], "one attempt per person");
    assert_eq!(begins(&h), 1, "the provider was asked once");
    let id = s(&a["id"]);
    let cur = send(
        &h.api,
        &me(),
        "GET",
        "/channels/feishu/registrations/current",
        None,
    )
    .await
    .1;
    assert_eq!(cur["id"], a["id"]);
    // another person, another workspace: nothing to see or touch
    for who in [caller(WS, OTHER), caller(WS2, ME)] {
        assert_eq!(
            send(
                &h.api,
                &who,
                "GET",
                "/channels/feishu/registrations/current",
                None
            )
            .await
            .1,
            Value::Null
        );
        for (m, p) in [
            ("GET", format!("/channels/feishu/registrations/{id}")),
            (
                "POST",
                format!("/channels/feishu/registrations/{id}/cancel"),
            ),
            ("POST", format!("/channels/feishu/registrations/{id}/retry")),
        ] {
            let (st, v) = send(&h.api, &who, m, &p, None).await;
            assert_eq!(st, 404, "{m} {p}: {v}");
            assert!(!v.to_string().contains(&s(&a["user_code"])));
        }
    }
    // the other person gets their own attempt
    let (st, theirs) = send(
        &h.api,
        &caller(WS, OTHER),
        "POST",
        "/channels/feishu/registrations",
        Some(json!({"brand": "feishu"})),
    )
    .await;
    assert_eq!(st, 200, "{theirs}");
    assert_ne!(theirs["id"], a["id"]);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_refused_begin_is_an_error_and_keeps_nothing() {
    let h = setup().await;
    h.fake
        .reg_begin(500, json!({"device_code": "dc-x", "user_code": "U"}));
    let (st, v) = send(
        &h.api,
        &me(),
        "POST",
        "/channels/feishu/registrations",
        Some(json!({"brand": "feishu"})),
    )
    .await;
    assert_eq!(st, 400, "HTTP ≥ 400 is never a begin: {v}");
    no_secrets(&v);
    h.fake.reg_begin(
        200,
        json!({"error": "invalid_client", "error_description": "secret dc-zz"}),
    );
    let (st, v) = send(
        &h.api,
        &me(),
        "POST",
        "/channels/feishu/registrations",
        Some(json!({"brand": "feishu"})),
    )
    .await;
    assert_eq!(st, 400);
    assert!(
        !v.to_string().contains("dc-zz"),
        "the provider's text is not passed on: {v}"
    );
    assert_eq!(count(&h, "SELECT count(*) FROM chan_app_registrations"), 0);
    let (st, _) = send(
        &h.api,
        &me(),
        "POST",
        "/channels/feishu/registrations",
        Some(json!({"brand": "nope"})),
    )
    .await;
    assert_eq!(st, 400);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_lark_tenant_switches_the_polled_host_once() {
    let h = setup().await;
    h.fake.reg_polls(vec![
        json!({"error": "authorization_pending", "user_info": {"tenant_brand": "lark"}}),
        json!({"error": "authorization_pending", "user_info": {"tenant_brand": "feishu"}}),
        h.fake.reg_issued("lark"),
    ]);
    let v = begin(&h, "lark").await;
    assert!(
        s(&v["verification_uri_complete"]).starts_with(&format!("{}/lark/", h.fake.base_url())),
        "{v}"
    );
    let done = settle(&h, &s(&v["id"]), ACTIVE).await;
    assert_eq!(
        (done["state"].as_str(), done["brand"].as_str()),
        (Some("succeeded"), Some("lark")),
        "{done}"
    );
    let calls = h.fake.reg_calls();
    assert_eq!(
        calls[0],
        ("feishu".into(), "begin".into()),
        "begin always bootstraps on Feishu"
    );
    assert_eq!(calls[1], ("feishu".into(), "poll".into()));
    assert!(
        calls[2..].iter().all(|(b, _)| b == "lark"),
        "switched once, not back: {calls:?}"
    );
    let domain: String = h.env.store().with_conn(|c| Ok(c.query_row("SELECT json_extract(config, '$.domain') FROM connections WHERE adapter = 'feishu_app'", [], |r| r.get(0))?)).expect("domain");
    assert_eq!(domain, "lark");
}

#[tokio::test(flavor = "multi_thread")]
async fn denied_and_expired_end_and_destroy_the_ticket() {
    let h = setup().await;
    h.fake.reg_polls(vec![
        json!({"error": "access_denied", "error_description": "dc-leak"}),
    ]);
    let v = begin(&h, "feishu").await;
    let done = settle(&h, &s(&v["id"]), ACTIVE).await;
    assert_eq!(
        (done["state"].as_str(), done["error_kind"].as_str()),
        (Some("denied"), Some("denied"))
    );
    no_secrets(&done);
    assert!(!done.to_string().contains("dc-leak"));
    h.fake.reg_polls(vec![json!({"error": "expired_token"})]);
    let v = begin(&h, "feishu").await;
    let done = settle(&h, &s(&v["id"]), ACTIVE).await;
    assert_eq!(done["state"], "expired");
    assert_eq!(
        count(
            &h,
            "SELECT count(*) FROM chan_app_registrations WHERE device_item IS NOT NULL"
        ),
        0
    );
    assert_eq!(
        count(
            &h,
            "SELECT count(*) FROM vault_items WHERE title = '飞书应用注册确认码' AND status = 'active'"
        ),
        0
    );
    assert_eq!(count(&h, "SELECT count(*) FROM connections"), 0);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_cancel_first_means_never_bound() {
    let h = setup().await;
    h.fake.reg_poll_delay(Duration::from_millis(150));
    let v = begin(&h, "feishu").await;
    let id = s(&v["id"]);
    let (st, c) = send(
        &h.api,
        &me(),
        "POST",
        &format!("/channels/feishu/registrations/{id}/cancel"),
        None,
    )
    .await;
    assert_eq!(st, 200);
    assert_eq!(
        (c["state"].as_str(), c["app_may_exist"].as_bool()),
        (Some("cancelled"), Some(true))
    );
    // the provider confirms afterwards: nothing is kept or bound
    h.fake.reg_polls(vec![h.fake.reg_issued("feishu")]);
    tokio::time::sleep(Duration::from_millis(600)).await;
    let after = settle(&h, &id, ACTIVE).await;
    assert_eq!(after["state"], "cancelled");
    assert_eq!(count(&h, "SELECT count(*) FROM connections"), 0);
    assert_eq!(
        count(
            &h,
            "SELECT count(*) FROM vault_items WHERE status = 'active' AND title LIKE '飞书应用%'"
        ),
        0
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_failed_check_keeps_the_credential_and_retry_binds_once() {
    let h = setup().await;
    h.fake.deny(Endpoint::BotInfo);
    h.fake.reg_polls(vec![h.fake.reg_issued("feishu")]);
    let v = begin(&h, "feishu").await;
    let id = s(&v["id"]);
    let saved = checked(&h, &id).await;
    assert_eq!(saved["state"], "credentials_saved", "{saved}");
    assert!(
        saved["error"].is_string() && saved["error_kind"].is_string(),
        "{saved}"
    );
    no_secrets(&saved);
    assert_eq!(count(&h, "SELECT count(*) FROM connections"), 0);
    assert_eq!(
        count(
            &h,
            "SELECT count(*) FROM chan_app_registrations WHERE vault_item IS NOT NULL"
        ),
        1,
        "the credential is kept"
    );
    // current still offers it (retryable); asking again does not begin anew
    let cur = send(
        &h.api,
        &me(),
        "GET",
        "/channels/feishu/registrations/current",
        None,
    )
    .await
    .1;
    assert_eq!(cur["id"], json!(id));
    assert_eq!(begin(&h, "feishu").await["id"], json!(id));
    assert_eq!(begins(&h), 1);
    h.fake.allow(Endpoint::BotInfo);
    let (st, _) = send(
        &h.api,
        &me(),
        "POST",
        &format!("/channels/feishu/registrations/{id}/retry"),
        None,
    )
    .await;
    assert_eq!(st, 200);
    let done = settle(&h, &id, ACTIVE).await;
    assert_eq!(done["state"], "succeeded", "{done}");
    assert_eq!(count(&h, "SELECT count(*) FROM connections"), 1);
    assert_eq!(begins(&h), 1, "a retry never begins again");
    let (st, _) = send(
        &h.api,
        &me(),
        "POST",
        &format!("/channels/feishu/registrations/{id}/retry"),
        None,
    )
    .await;
    assert_eq!(st, 409, "nothing left to retry");
}

#[tokio::test(flavor = "multi_thread")]
async fn cancelling_a_saved_credential_destroys_it() {
    let h = setup().await;
    h.fake.deny(Endpoint::BotInfo);
    h.fake.reg_polls(vec![h.fake.reg_issued("feishu")]);
    let id = s(&begin(&h, "feishu").await["id"]);
    checked(&h, &id).await;
    let (_, c) = send(
        &h.api,
        &me(),
        "POST",
        &format!("/channels/feishu/registrations/{id}/cancel"),
        None,
    )
    .await;
    assert_eq!(c["state"], "cancelled");
    assert_eq!(
        count(
            &h,
            "SELECT count(*) FROM vault_items WHERE status = 'active' AND title LIKE '飞书应用%'"
        ),
        0
    );
    h.fake.allow(Endpoint::BotInfo);
    let (st, _) = send(
        &h.api,
        &me(),
        "POST",
        &format!("/channels/feishu/registrations/{id}/retry"),
        None,
    )
    .await;
    assert_eq!(st, 409);
    assert_eq!(count(&h, "SELECT count(*) FROM connections"), 0);
}

#[tokio::test(flavor = "multi_thread")]
async fn an_app_already_connected_is_not_replaced() {
    let h = setup().await;
    // the same app connected by hand first
    let (st, v) = send(
        &h.api,
        &me(),
        "POST",
        "/connections",
        Some(json!({
            "adapter": "feishu_app",
            "fields": {"App ID": ids::APP_ID, "App Secret": ids::APP_SECRET},
            "config": {"api_base": h.fake.base_url()},
        })),
    )
    .await;
    assert_eq!(st, 200, "{v}");
    let before: (String, String) = h
        .env
        .store()
        .with_conn(|c| {
            Ok(c.query_row(
                "SELECT credential_item, updated FROM connections",
                [],
                |r| Ok((r.get(0)?, r.get::<_, i64>(1)?.to_string())),
            )?)
        })
        .expect("row");
    h.fake.reg_polls(vec![h.fake.reg_issued("feishu")]);
    let id = s(&begin(&h, "feishu").await["id"]);
    let r = checked(&h, &id).await;
    assert_eq!(
        (r["state"].as_str(), r["error_kind"].as_str()),
        (Some("credentials_saved"), Some("conflict")),
        "{r}"
    );
    let after: (String, String) = h
        .env
        .store()
        .with_conn(|c| {
            Ok(c.query_row(
                "SELECT credential_item, updated FROM connections",
                [],
                |r| Ok((r.get(0)?, r.get::<_, i64>(1)?.to_string())),
            )?)
        })
        .expect("row");
    assert_eq!(before, after, "the existing connection is untouched");
    assert_eq!(count(&h, "SELECT count(*) FROM connections"), 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_stop_is_resumed_within_the_original_deadline() {
    let h = setup().await;
    // the first Hub polls once, then sleeps “forever” (it stopped)
    let stopped = Registrations::new(h.conns.clone(), Bases::fake(h.fake.base_url()))
        .with_tick(Duration::from_secs(3600));
    let v = stopped
        .begin(
            &me(),
            steve_channels::adapters::feishu::app_registration::Brand::Feishu,
        )
        .await
        .expect("begin");
    let id = s(&v["id"]);
    let expires = v["expires_at"].as_i64().expect("deadline");
    tokio::time::sleep(Duration::from_millis(100)).await;
    // the next Hub resumes it: same row, same deadline, no new begin
    h.fake.reg_polls(vec![h.fake.reg_issued("feishu")]);
    assert_eq!(h.regs.resume().expect("resume"), 1);
    let done = settle(&h, &id, ACTIVE).await;
    assert_eq!(done["state"], "succeeded", "{done}");
    assert_eq!(done["expires_at"].as_i64(), Some(expires));
    assert_eq!(begins(&h), 1);
    // an attempt whose deadline passed while stopped is not polled again
    let v = stopped
        .begin(
            &me(),
            steve_channels::adapters::feishu::app_registration::Brand::Feishu,
        )
        .await
        .expect("begin");
    let id2 = s(&v["id"]);
    // its first (immediate) poll is done; then the deadline passes while “stopped”
    tokio::time::sleep(Duration::from_millis(150)).await;
    h.env
        .store()
        .with_conn(|c| {
            Ok(c.execute(
                "UPDATE chan_app_registrations SET expires_at = 1 WHERE id = ?",
                [&id2],
            )?)
        })
        .expect("age");
    let polls = h.fake.reg_calls().len();
    assert_eq!(h.regs.resume().expect("resume"), 0);
    let r = send(
        &h.api,
        &me(),
        "GET",
        &format!("/channels/feishu/registrations/{id2}"),
        None,
    )
    .await
    .1;
    assert_eq!(r["state"], "expired");
    assert_eq!(
        h.fake.reg_calls().len(),
        polls,
        "no poll after the deadline"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_member_who_left_gets_nothing_bound() {
    let h = setup().await;
    h.fake.reg_poll_delay(Duration::from_millis(100));
    h.fake.reg_polls(vec![h.fake.reg_issued("feishu")]);
    let id = s(&begin(&h, "feishu").await["id"]);
    h.env
        .store()
        .with_conn(|c| {
            Ok(c.execute(
                "DELETE FROM workspace_members WHERE workspace_id = ? AND user_id = ?",
                [WS, ME],
            )?)
        })
        .expect("leave");
    let mut last = Value::Null;
    for _ in 0..200 {
        last = h.env.store().with_conn(|c| Ok(c.query_row("SELECT state, coalesce(error_kind, '') FROM chan_app_registrations WHERE id = ?", [&id], |r| Ok(json!([r.get::<_, String>(0)?, r.get::<_, String>(1)?])))?)).expect("row");
        if last[1] == "member_gone" {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert_eq!(last, json!(["credentials_saved", "member_gone"]));
    assert_eq!(count(&h, "SELECT count(*) FROM connections"), 0);
}

// ------------------------------------------------------------------ the device-code login

async fn bot(h: &H) -> String {
    let (st, v) = send(
        &h.api,
        &me(),
        "POST",
        "/connections",
        Some(json!({
            "adapter": "feishu_app",
            "fields": {"App ID": ids::APP_ID, "App Secret": ids::APP_SECRET},
            "config": {"api_base": h.fake.base_url()},
        })),
    )
    .await;
    assert_eq!(st, 200, "{v}");
    s(&v["id"])
}

async fn device(h: &H, body: Value) -> (u16, Value) {
    send(
        &h.api,
        &me(),
        "POST",
        "/channels/feishu/device/start",
        Some(body),
    )
    .await
}

async fn device_done(h: &H, id: &str) -> Value {
    let path = format!("/channels/feishu/device/{id}");
    for _ in 0..80 {
        let v = send(&h.api, &me(), "GET", &path, None).await.1;
        if v["status"] != "pending" {
            return v;
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
    panic!("device login still pending");
}

fn granted(h: &H, account: &str) -> Vec<String> {
    let v: String = h.env.store().with_conn(|c| Ok(c.query_row("SELECT granted_scopes FROM connections WHERE account_id = ? AND adapter = 'feishu_user'", [account], |r| r.get(0))?)).expect("scopes");
    serde_json::from_str(&v).expect("json")
}

#[tokio::test(flavor = "multi_thread")]
async fn a_device_login_cancelled_first_is_never_bound() {
    let h = setup().await;
    h.fake.set_device_interval(1);
    let app = bot(&h).await;
    let (st, v) = device(&h, json!({"app_connection": app})).await;
    assert_eq!(st, 200, "{v}");
    let id = s(&v["id"]);
    let (_, c) = send(
        &h.api,
        &me(),
        "POST",
        &format!("/channels/feishu/device/{id}/cancel"),
        None,
    )
    .await;
    assert_eq!(c["status"], "cancelled");
    assert!(h.fake.device_approve(&s(&v["user_code"])));
    tokio::time::sleep(Duration::from_millis(2500)).await;
    assert_eq!(
        send(
            &h.api,
            &me(),
            "GET",
            &format!("/channels/feishu/device/{id}"),
            None
        )
        .await
        .1["status"],
        "cancelled"
    );
    assert_eq!(
        count(
            &h,
            "SELECT count(*) FROM connections WHERE adapter = 'feishu_user'"
        ),
        0
    );
    // someone else cannot cancel (or see) it
    let (st, _) = send(
        &h.api,
        &caller(WS, OTHER),
        "POST",
        &format!("/channels/feishu/device/{id}/cancel"),
        None,
    )
    .await;
    assert_eq!(st, 404);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_read_only_login_refreshed_stays_read_only() {
    let h = setup().await;
    h.fake.set_device_interval(1);
    let app = bot(&h).await;
    let read: Vec<&str> = steve_channels::adapters::feishu::auth::USER_SCOPES
        .iter()
        .copied()
        .filter(|x| *x != "im:message.send_as_user")
        .collect();
    let (st, v) = device(&h, json!({"app_connection": app, "scopes": read})).await;
    assert_eq!(st, 200, "{v}");
    assert!(h.fake.device_approve(&s(&v["user_code"])));
    let ok = device_done(&h, &s(&v["id"])).await;
    assert_eq!(ok["status"], "ok", "{ok}");
    let account = s(&ok["account_id"]);
    let g = granted(&h, &account);
    assert!(!g.iter().any(|x| x == "im:message.send_as_user"), "{g:?}");
    let conn: String = h
        .env
        .store()
        .with_conn(|c| {
            Ok(c.query_row(
                "SELECT id FROM connections WHERE adapter = 'feishu_user'",
                [],
                |r| r.get(0),
            )?)
        })
        .expect("id");
    let caps = h.conns.get(&me(), &conn).expect("conn").available_caps;
    assert!(!caps.iter().any(|c| c == "msg.send_as_user"), "{caps:?}");
    // re-authorizing it without scopes asks only for what it had (+ offline_access)
    let (st, v) = device(&h, json!({"app_connection": app, "reauth_of": conn})).await;
    assert_eq!(st, 200, "{v}");
    assert!(h.fake.device_approve(&s(&v["user_code"])));
    assert_eq!(device_done(&h, &s(&v["id"])).await["status"], "ok");
    let g2 = granted(&h, &account);
    assert!(
        !g2.iter().any(|x| x == "im:message.send_as_user"),
        "never widened: {g2:?}"
    );
    // adding the missing one keeps the rest
    let (st, v) = device(
        &h,
        json!({"app_connection": app, "reauth_of": conn, "scopes": ["im:message.send_as_user"]}),
    )
    .await;
    assert_eq!(st, 200, "{v}");
    assert!(h.fake.device_approve(&s(&v["user_code"])));
    assert_eq!(device_done(&h, &s(&v["id"])).await["status"], "ok");
    let g3 = granted(&h, &account);
    assert!(g3.iter().any(|x| x == "im:message.send_as_user"), "{g3:?}");
    for x in &g2 {
        assert!(g3.contains(x), "kept {x}: {g3:?}");
    }
    // an undeclared scope is refused
    let (st, _) = device(
        &h,
        json!({"app_connection": app, "scopes": ["admin:everything"]}),
    )
    .await;
    assert_eq!(st, 400);
}

#[tokio::test(flavor = "multi_thread")]
async fn re_authorizing_through_another_app_is_refused() {
    let h = setup().await;
    h.fake.set_device_interval(1);
    let app = bot(&h).await;
    let (_, v) = device(&h, json!({"app_connection": app})).await;
    assert!(h.fake.device_approve(&s(&v["user_code"])));
    assert_eq!(device_done(&h, &s(&v["id"])).await["status"], "ok");
    let conn: String = h
        .env
        .store()
        .with_conn(|c| {
            Ok(c.query_row(
                "SELECT id FROM connections WHERE adapter = 'feishu_user'",
                [],
                |r| r.get(0),
            )?)
        })
        .expect("id");
    // the same app under another connection id stands in for “another app”
    h.env.store().with_conn(|c| Ok(c.execute("UPDATE connections SET config = json_set(config, '$.app_connection', 'con_other') WHERE id = ?", [&conn])?)).expect("move");
    let (st, v) = device(&h, json!({"app_connection": app, "reauth_of": conn})).await;
    assert_eq!(st, 400, "{v}");
    // someone else's connection is not theirs to re-authorize
    let (st, _) = send(
        &h.api,
        &caller(WS, OTHER),
        "POST",
        "/channels/feishu/device/start",
        Some(json!({"app_connection": app, "reauth_of": conn})),
    )
    .await;
    assert!(st == 404 || st == 400, "{st}");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_lark_bot_logs_you_in_on_lark_hosts() {
    let h = setup().await;
    h.fake.set_device_interval(1);
    let (st, v) = send(
        &h.api,
        &me(),
        "POST",
        "/connections",
        Some(json!({
            "adapter": "feishu_app",
            "fields": {"App ID": ids::APP_ID, "App Secret": ids::APP_SECRET, "域名": "lark"},
            "config": {"api_base": format!("{}/lark", h.fake.base_url()), "domain": "lark"},
        })),
    )
    .await;
    assert_eq!(st, 200, "{v}");
    let app = s(&v["id"]);
    let (st, v) = device(&h, json!({"app_connection": app})).await;
    assert_eq!(st, 200, "{v}");
    assert!(h.fake.device_approve(&s(&v["user_code"])));
    let ok = device_done(&h, &s(&v["id"])).await;
    assert_eq!(ok["status"], "ok", "{ok}");
    // every user-side call went to Lark's hosts (the fake's /lark), none to Feishu's
    let user_side: Vec<String> = h
        .fake
        .requests()
        .into_iter()
        .filter(|r| {
            matches!(
                r.endpoint,
                Endpoint::DeviceAuthorization | Endpoint::DeviceToken | Endpoint::UserInfo
            )
        })
        .map(|r| r.path)
        .collect();
    assert!(!user_side.is_empty());
    assert!(
        user_side.iter().all(|p| p.starts_with("/lark/")),
        "{user_side:?}"
    );
    let domain: String = h.env.store().with_conn(|c| Ok(c.query_row("SELECT json_extract(config, '$.domain') FROM connections WHERE adapter = 'feishu_user'", [], |r| r.get(0))?)).expect("domain");
    assert_eq!(domain, "lark");
    // the browser code flow is refused for a Lark app (its credential never goes to Feishu)
    let (st, v) = send(
        &h.api,
        &me(),
        "POST",
        "/connections/oauth/start",
        Some(json!({"adapter": "feishu_user", "config": {"app_connection": app}})),
    )
    .await;
    assert_ne!(st, 200, "{v}");
    assert!(v.to_string().contains("Lark"), "{v}");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_device_login_never_replaces_another_members_own_connection() {
    let h = setup().await;
    h.fake.set_device_interval(1);
    let app = bot(&h).await;
    let (_, v) = device(&h, json!({"app_connection": app})).await;
    assert!(h.fake.device_approve(&s(&v["user_code"])));
    assert_eq!(device_done(&h, &s(&v["id"])).await["status"], "ok");
    let before: (String, String) = h
        .env
        .store()
        .with_conn(|c| {
            Ok(c.query_row(
                "SELECT created_by, credential_item FROM connections WHERE adapter = 'feishu_user'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )?)
        })
        .expect("row");
    // another member logs in with the same Feishu account (no reauth_of)
    let other = caller(WS, OTHER);
    let (st, v) = send(
        &h.api,
        &other,
        "POST",
        "/channels/feishu/device/start",
        Some(json!({"app_connection": app})),
    )
    .await;
    assert_eq!(st, 200, "{v}");
    assert!(h.fake.device_approve(&s(&v["user_code"])));
    let path = format!("/channels/feishu/device/{}", s(&v["id"]));
    let mut last = Value::Null;
    for _ in 0..80 {
        last = send(&h.api, &other, "GET", &path, None).await.1;
        if last["status"] != "pending" {
            break;
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
    assert_eq!(last["status"], "error", "{last}");
    let after: (String, String) = h
        .env
        .store()
        .with_conn(|c| {
            Ok(c.query_row(
                "SELECT created_by, credential_item FROM connections WHERE adapter = 'feishu_user'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )?)
        })
        .expect("row");
    assert_eq!(before, after, "the first member's connection is untouched");
    assert_eq!(
        count(
            &h,
            "SELECT count(*) FROM connections WHERE adapter = 'feishu_user'"
        ),
        1
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_check_that_died_is_picked_up_again() {
    let h = setup().await;
    h.fake.deny(Endpoint::BotInfo);
    h.fake.reg_polls(vec![h.fake.reg_issued("feishu")]);
    let id = s(&begin(&h, "feishu").await["id"]);
    checked(&h, &id).await;
    // as if a check had stopped half-way: verifying, no worker
    h.env.store().with_conn(|c| Ok(c.execute("UPDATE chan_app_registrations SET state = 'verifying', error = NULL, error_kind = NULL WHERE id = ?", [&id])?)).expect("stuck");
    h.fake.allow(Endpoint::BotInfo);
    let done = settle(&h, &id, ACTIVE).await;
    assert_eq!(done["state"], "succeeded", "{done}");
    assert_eq!(count(&h, "SELECT count(*) FROM connections"), 1);
}
