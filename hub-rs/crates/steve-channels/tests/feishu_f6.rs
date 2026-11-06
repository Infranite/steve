//! F6 acceptance on the fake Feishu: the user OAuth
//! round-trip puts your Feishu account on “我” and the bot's view of you (`union_id` / `open_id`)
//! lands on the same contact, in either order; sending and replying as you with the user token
//! (`receive_id_type=chat_id`), the per-user rate limit (230020: wait, retry once, then error); the
//! `reauth` decision when the refresh token is dead, and re-authorizing settles it; capabilities
//! and the permission rows follow the scopes actually granted.

use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::Router;
use axum::body::Body;
use axum::http::{Request, header};
use serde_json::{Value, json};
use steve_channels::adapters::feishu::as_user::UserMessage;
use steve_channels::adapters::feishu::auth::{DeviceFlows, FeishuApp, FeishuUser, device_routes};
use steve_channels::adapters::feishu::client::{FeishuError, is_real_host};
use steve_channels::adapters::feishu::user_oauth::{FeishuIds, identify_sender, user_routes};
use steve_channels::channel::{ActAsUser, ActTarget, MessageRef, OutContent, SendKey};
use steve_channels::fake::feishu::{Endpoint, FakeFeishu, World, ids};
use steve_modules::accounts::connections::{
    self, ConnCx, Connections, ConnectionsConfig, ProviderError,
};
use steve_modules::testkit::{ME, OTHER, TestEnv, WS, caller};
use steve_modules::vault::Vault;
use steve_modules::vault::keystore::FileKeyStore;
use steve_modules::{ApiError, Caller};
use steve_store::Cached;
use tempfile::TempDir;
use tower::ServiceExt;

const ORIGIN: &str = "http://hub.test:8440";

struct H {
    env: TestEnv,
    _keys: TempDir,
    conns: Connections,
    user: Arc<FeishuUser>,
    api: Router,
    cb: Router,
    fake: FakeFeishu,
}

async fn setup_with(world: World) -> H {
    let fake = FakeFeishu::start_with(world).await.expect("fake feishu");
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
    // every endpoint of the user provider on the fake (never the real hosts)
    let user = Arc::new(
        FeishuUser::with_base(fake.base_url())
            .with_party_hooks(steve_modules::accounts::ports().hooks),
    );
    let conns = Connections::builder(env.modules.clone(), vault.clone(), cfg)
        .provider(Arc::new(FeishuApp::new()))
        .provider(user.clone())
        .build();
    let api = connections::routes(conns.clone())
        .merge(device_routes(DeviceFlows::new(conns.clone(), user.clone())))
        .merge(user_routes(conns.clone(), user.claims().clone()))
        .with_state(env.modules.clone());
    let cb = connections::callback_routes(conns.clone()).with_state(env.modules.clone());
    H {
        env,
        _keys: keys,
        conns,
        user,
        api,
        cb,
        fake,
    }
}

async fn setup() -> H {
    setup_with(World::sample()).await
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

fn row(h: &H, sql: &str, p: &str) -> Vec<Value> {
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

fn s(v: &Value) -> String {
    v.as_str().unwrap_or_default().to_string()
}

async fn connect_bot(h: &H) -> String {
    let (st, v, _) = send(
        &h.api,
        &caller(WS, ME),
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

/// Start (`path`, `body`) → the fake's consent page (302) → our callback; the callback's `Location`.
async fn oauth_via(h: &H, who: &Caller, path: &str, body: Value) -> String {
    let (st, v, _) = send(&h.api, who, "POST", path, Some(body)).await;
    assert_eq!(st, 200, "{v}");
    let url = s(&v["url"]);
    assert!(url.starts_with(h.fake.base_url()), "fake hosts only: {url}");
    let res = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .expect("client")
        .get(&url)
        .send()
        .await
        .expect("authorize");
    assert!(res.status().is_redirection());
    let back =
        url::Url::parse(res.headers()[header::LOCATION].to_str().expect("loc")).expect("url");
    let p = format!("{}?{}", back.path(), back.query().unwrap_or_default());
    let (st, _, hd) = send(&h.cb, who, "GET", &p, None).await;
    assert_eq!(st, 302);
    hd[header::LOCATION].to_str().expect("loc").to_string()
}

/// “授权你本人” (the F6 route); returns (account id, user connection id).
async fn authorize_me(h: &H, app_con: &str) -> (String, String) {
    let loc = oauth_via(
        h,
        &caller(WS, ME),
        "/channels/feishu/user/oauth/start",
        json!({"app_connection": app_con}),
    )
    .await;
    let acc = loc
        .strip_prefix("/#/accounts/")
        .unwrap_or_else(|| panic!("not a success: {loc}"))
        .to_string();
    let con = s(&row(h, "SELECT id FROM connections WHERE account_id = ?", &acc)[0]);
    (acc, con)
}

fn me_party(h: &H) -> String {
    s(&row(
        h,
        "SELECT id FROM parties WHERE user_id = ? AND type = 'contact'",
        ME,
    )[0])
}

fn owner_ids() -> FeishuIds {
    FeishuIds {
        union_id: ids::OWNER.union_id.into(),
        open_id: ids::OWNER.open_id.into(),
    }
}

/// What the bot connection `bot` makes of a message from `ids`.
fn bot_sees(h: &H, bot: &str, ids: &FeishuIds) -> steve_modules::accounts::Identified {
    h.env
        .store()
        .transaction(|tx| identify_sender(tx, WS, ME, ids, Some(bot), Some("李鹏翔")))
        .expect("identify")
}

fn user_cx(h: &H, con: &str) -> ConnCx {
    h.conns.cx(WS, con).expect("cx")
}

// ---------------------------------------------------------------------------------------------
// Identity.

#[tokio::test(flavor = "multi_thread")]
async fn oauth_first_then_the_bot_finds_you_on_me() {
    let h = setup().await;
    let bot = connect_bot(&h).await;
    let (acc, ucon) = authorize_me(&h, &bot).await;
    let me = me_party(&h);
    let a = row(
        &h,
        "SELECT handle, system_id, party_id FROM accounts WHERE id = ?",
        &acc,
    );
    assert_eq!(
        a,
        vec![json!(ids::OWNER.union_id), json!("feishu"), json!(me)]
    );
    // the spent ticket is not kept in the connection's config
    let cfg: Value = serde_json::from_str(&s(&row(
        &h,
        "SELECT config FROM connections WHERE id = ?",
        &ucon,
    )[0]))
    .expect("json");
    assert!(cfg["self_claim"].is_null(), "{cfg}");

    // the bot sees your union_id (and the same app's open_id): the same account, the same contact
    let i = bot_sees(&h, &bot, &owner_ids());
    assert_eq!(
        (i.account_id.as_str(), i.party_id.as_str()),
        (acc.as_str(), me.as_str())
    );
    assert!(!i.created && !i.is_ours);
    // an event carrying only the open_id matches too
    let only_open = FeishuIds {
        union_id: String::new(),
        open_id: ids::OWNER.open_id.into(),
    };
    assert_eq!(bot_sees(&h, &bot, &only_open).party_id, me);
    // someone else is someone else
    let alice = FeishuIds {
        union_id: ids::ALICE.union_id.into(),
        open_id: ids::ALICE.open_id.into(),
    };
    let a = bot_sees(&h, &bot, &alice);
    assert!(a.created);
    assert_ne!(a.party_id, me);
}

#[tokio::test(flavor = "multi_thread")]
async fn bot_first_then_oauth_merges_the_unconfirmed_contact_into_me() {
    let h = setup().await;
    let bot = connect_bot(&h).await;
    // the bot saw you before you authorized: identify made an unconfirmed contact
    let first = bot_sees(&h, &bot, &owner_ids());
    assert!(first.created);
    let me = me_party(&h);
    assert_ne!(first.party_id, me);
    let k = row(
        &h,
        "SELECT contact_kind, status FROM parties WHERE id = ?",
        &first.party_id,
    );
    assert_eq!(k, vec![json!("unconfirmed"), json!("active")]);

    let (acc, _) = authorize_me(&h, &bot).await;
    // the same account (the bot's), now on 我; the old contact merged into 我
    assert_eq!(acc, first.account_id);
    assert_eq!(
        s(&row(&h, "SELECT party_id FROM accounts WHERE id = ?", &acc)[0]),
        me
    );
    let k = row(
        &h,
        "SELECT status, merged_into FROM parties WHERE id = ?",
        &first.party_id,
    );
    assert_eq!(k, vec![json!("merged"), json!(me)]);
    assert_eq!(
        row(
            &h,
            "SELECT COUNT(*) FROM merges WHERE from_id = ?",
            &first.party_id
        ),
        vec![json!(1)],
        "a normal merge: it can be split again"
    );
    let again = bot_sees(&h, &bot, &owner_ids());
    assert_eq!(
        (again.party_id.as_str(), again.created),
        (me.as_str(), false)
    );
}

/// The names run promotes the contact the bot made (it has your Feishu name) some 20 s after your first
/// message, well before you authorize: promoted by nobody, it is still yours to claim. A kind a person
/// set is not.
#[tokio::test(flavor = "multi_thread")]
async fn a_contact_the_names_run_made_real_is_still_merged_but_not_one_you_confirmed() {
    let h = setup().await;
    let bot = connect_bot(&h).await;
    let first = bot_sees(&h, &bot, &owner_ids());
    let promoted = h
        .env
        .store()
        .transaction(|tx| {
            steve_modules::accounts::enrich(
                tx,
                &[],
                WS,
                ME,
                &first.account_id,
                &steve_modules::accounts::Enrichment {
                    name: Some("李鹏翔".into()),
                    ..Default::default()
                },
            )
        })
        .expect("enrich");
    assert!(promoted.promoted);
    let k = row(
        &h,
        "SELECT contact_kind, source FROM parties WHERE id = ?",
        &first.party_id,
    );
    assert_eq!(k, vec![json!("real"), json!("chat")]);
    let (acc, _) = authorize_me(&h, &bot).await;
    assert_eq!(acc, first.account_id);
    assert_eq!(
        s(&row(&h, "SELECT party_id FROM accounts WHERE id = ?", &acc)[0]),
        me_party(&h)
    );
    assert_eq!(
        s(&row(
            &h,
            "SELECT status FROM parties WHERE id = ?",
            &first.party_id
        )[0]),
        "merged"
    );

    // the same contact, its kind set by a person (a revision that is not a sync): it stays
    let h = setup().await;
    let bot = connect_bot(&h).await;
    let first = bot_sees(&h, &bot, &owner_ids());
    // confirmed by a person the way the contacts page does it: a revision of `kind` that is no sync
    h.env
        .store()
        .transaction(|tx| {
            use steve_modules::contacts::{repo, service};
            let p = repo::get(tx, &first.party_id)?.expect("party");
            service::revise_as(
                tx,
                &p,
                &repo::PartyPatch {
                    contact_kind: Some("real".into()),
                    ..Default::default()
                },
                json!({"kind": "unconfirmed"}),
                json!({"kind": "real"}),
                ME,
            )?;
            tx.run(
                "UPDATE parties SET source = 'chat' WHERE id = ?",
                [&first.party_id],
            )?;
            Ok::<_, ApiError>(())
        })
        .expect("confirm");
    let loc = oauth_via(
        &h,
        &caller(WS, ME),
        "/channels/feishu/user/oauth/start",
        json!({"app_connection": bot}),
    )
    .await;
    assert!(loc.starts_with("/#/accounts?error="), "{loc}");
    assert_eq!(
        s(&row(
            &h,
            "SELECT status FROM parties WHERE id = ?",
            &first.party_id
        )[0]),
        "active"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn bot_first_then_device_code_merges_too() {
    let h = setup().await;
    h.fake.set_device_interval(1);
    let bot = connect_bot(&h).await;
    let first = bot_sees(&h, &bot, &owner_ids());
    let me_c = caller(WS, ME);
    let (st, v, _) = send(
        &h.api,
        &me_c,
        "POST",
        "/channels/feishu/device/start",
        Some(json!({"app_connection": bot})),
    )
    .await;
    assert_eq!(st, 200, "{v}");
    assert!(h.fake.device_approve(&s(&v["user_code"])));
    let path = format!("/channels/feishu/device/{}", s(&v["id"]));
    let mut last = Value::Null;
    for _ in 0..40 {
        tokio::time::sleep(Duration::from_millis(250)).await;
        last = send(&h.api, &me_c, "GET", &path, None).await.1;
        if last["status"] != "pending" {
            break;
        }
    }
    assert_eq!(last["status"], "ok", "{last}");
    assert_eq!(s(&last["account_id"]), first.account_id);
    assert_eq!(
        s(&row(
            &h,
            "SELECT status FROM parties WHERE id = ?",
            &first.party_id
        )[0]),
        "merged"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn without_a_ticket_or_for_a_confirmed_contact_nothing_is_merged() {
    // the generic start route carries no ticket: no merge, the connect says who holds the account
    let h = setup().await;
    let bot = connect_bot(&h).await;
    let first = bot_sees(&h, &bot, &owner_ids());
    let loc = oauth_via(
        &h,
        &caller(WS, ME),
        "/connections/oauth/start",
        json!({"adapter": "feishu_user", "config": {"app_connection": bot, "self_claim": "scl_forged"}}),
    )
    .await;
    assert!(loc.starts_with("/#/accounts?error="), "{loc}");
    assert_eq!(
        s(&row(
            &h,
            "SELECT status FROM parties WHERE id = ?",
            &first.party_id
        )[0]),
        "active"
    );

    // a contact someone confirmed by hand is never merged automatically
    h.env
        .store()
        .transaction(|tx| {
            tx.run(
                "UPDATE parties SET contact_kind = 'real' WHERE id = ?",
                [&first.party_id],
            )?;
            Ok::<_, ApiError>(())
        })
        .expect("confirm");
    let loc = oauth_via(
        &h,
        &caller(WS, ME),
        "/channels/feishu/user/oauth/start",
        json!({"app_connection": bot}),
    )
    .await;
    assert!(loc.starts_with("/#/accounts?error="), "{loc}");
    assert_eq!(
        s(&row(
            &h,
            "SELECT status FROM parties WHERE id = ?",
            &first.party_id
        )[0]),
        "active"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn the_start_route_needs_a_visible_app_connection() {
    let h = setup().await;
    let (st, _, _) = send(
        &h.api,
        &caller(WS, ME),
        "POST",
        "/channels/feishu/user/oauth/start",
        Some(json!({"app_connection": "con_nope"})),
    )
    .await;
    assert_eq!(st, 404);
}

// ---------------------------------------------------------------------------------------------
// Sending as you.

#[tokio::test(flavor = "multi_thread")]
async fn sends_and_replies_as_you_with_the_user_token() {
    let h = setup().await;
    let bot = connect_bot(&h).await;
    let (_, ucon) = authorize_me(&h, &bot).await;
    let cx = user_cx(&h, &ucon);

    let m = h
        .user
        .send_as_user(&cx, ids::GROUP_CHAT, &UserMessage::text("我来看一下"))
        .await
        .expect("sent");
    assert!(m.message_id.starts_with("om_"));
    assert_eq!(m.chat_id, ids::GROUP_CHAT);
    assert!(!m.retried);
    let last = h.fake.sent().pop().expect("sent");
    assert_eq!(last.as_user.as_deref(), Some(ids::OWNER.open_id));
    assert_eq!(last.receive_id_type.as_deref(), Some("chat_id"));
    assert_eq!(last.receive_id.as_deref(), Some(ids::GROUP_CHAT));
    assert_eq!(last.msg_type, "text");
    assert_eq!(
        serde_json::from_str::<Value>(&last.content).expect("json"),
        json!({"text": "我来看一下"})
    );
    assert_eq!(last.uuid.as_deref(), Some(m.uuid.as_str()));
    // with the user token, not the tenant token
    let req = h
        .fake
        .requests_to(Endpoint::SendMessage)
        .pop()
        .expect("request");
    assert!(
        req.token.as_deref().is_some_and(|t| t.starts_with("u-")),
        "{:?}",
        req.token
    );

    let r = h
        .user
        .reply_as_user(&cx, &m.message_id, &UserMessage::text("好了"), true)
        .await
        .expect("replied");
    let last = h.fake.sent().pop().expect("reply");
    assert_eq!(last.reply_to.as_deref(), Some(m.message_id.as_str()));
    assert_eq!(last.as_user.as_deref(), Some(ids::OWNER.open_id));
    assert!(last.reply_in_thread);
    assert_eq!(r.chat_id, ids::GROUP_CHAT);
    assert!(r.thread_id.is_some());

    // the same uuid twice is one message (Feishu's de-duplication)
    let mut once = UserMessage::text("只发一次");
    once.uuid = Some("steve-out-1".into());
    let a = h
        .user
        .send_as_user(&cx, ids::GROUP_CHAT, &once)
        .await
        .expect("a");
    let b = h
        .user
        .send_as_user(&cx, ids::GROUP_CHAT, &once)
        .await
        .expect("b");
    assert_eq!(a.message_id, b.message_id);

    // a chat you're not in is Feishu's refusal, passed on
    let e = h
        .user
        .send_as_user(&cx, ids::P2P_CHAT, &UserMessage::text("x"))
        .await
        .expect_err("not a member");
    assert!(matches!(e, FeishuError::Api { .. }), "{e:?}");
}

#[tokio::test(flavor = "multi_thread")]
async fn reacts_as_you_with_the_user_token() {
    let h = setup().await;
    let bot = connect_bot(&h).await;
    let (_, ucon) = authorize_me(&h, &bot).await;
    let cx = user_cx(&h, &ucon);
    let m = h
        .user
        .send_as_user(&cx, ids::GROUP_CHAT, &UserMessage::text("/bot 帮我回一下"))
        .await
        .expect("sent");
    let id = h
        .user
        .react_as_user(&cx, &m.message_id, "OnIt")
        .await
        .expect("reacted");
    let r = h.fake.reactions().pop().expect("reaction");
    assert_eq!(r.as_user.as_deref(), Some(ids::OWNER.open_id));
    assert_eq!(r.emoji_type, "OnIt");
    h.user
        .unreact_as_user(&cx, &m.message_id, &id)
        .await
        .expect("removed");
    assert!(h.fake.reactions_on(&m.message_id).is_empty());
}

/// Acting as you through the extension seam: sending with the outbound key as Feishu's `uuid`,
/// and recalling what you sent with your user token. The bot is refused that recall (only the
/// sender can), which is why a message sent as you must not be recalled through the bot.
#[tokio::test(flavor = "multi_thread")]
async fn recalls_what_you_sent_with_the_user_token() {
    let h = setup().await;
    let bot = connect_bot(&h).await;
    let (_, ucon) = authorize_me(&h, &bot).await;
    let cx = user_cx(&h, &ucon);
    let user: &dyn ActAsUser = h.user.as_ref();
    assert!(user.supports_recall() && user.supports_reactions());

    let to = ActTarget::Space {
        ext_id: ids::GROUP_CHAT.into(),
    };
    let sent = user
        .send(
            &cx,
            &to,
            &OutContent::text("发错了"),
            &SendKey("cm_1".into()),
        )
        .await
        .expect("sent");
    let id = sent.ext_id.clone().expect("a message id");
    let last = h.fake.sent().pop().expect("sent");
    assert_eq!(last.uuid.as_deref(), Some("cm_1"));
    assert_eq!(last.as_user.as_deref(), Some(ids::OWNER.open_id));

    // as the bot: Feishu refuses, it did not send it
    let e = steve_channels::adapters::feishu::send::recall(&user_cx(&h, &bot), &id)
        .await
        .expect_err("only the sender");
    assert!(matches!(e, FeishuError::Api { .. }), "{e:?}");
    assert!(h.fake.deleted().is_empty());

    // as you, with the user token
    let msg = MessageRef::platform(ids::GROUP_CHAT, &id);
    user.recall(&cx, &msg).await.expect("recalled");
    assert_eq!(h.fake.deleted(), vec![id.clone()]);
    let req = h
        .fake
        .requests_to(Endpoint::DeleteMessage)
        .pop()
        .expect("request");
    assert!(
        req.token.as_deref().is_some_and(|t| t.starts_with("u-")),
        "{:?}",
        req.token
    );
    // again: already recalled, Feishu's refusal passed on
    assert!(user.recall(&cx, &msg).await.is_err());
    // a message without a Feishu id: nothing is called
    let n = h.fake.requests_to(Endpoint::DeleteMessage).len();
    let mut none = MessageRef::platform(ids::GROUP_CHAT, "");
    none.ext_id = None;
    assert!(user.recall(&cx, &none).await.is_err());
    assert_eq!(h.fake.requests_to(Endpoint::DeleteMessage).len(), n);
}

#[tokio::test(flavor = "multi_thread")]
async fn the_per_user_rate_limit_waits_and_retries_once() {
    let h = setup().await;
    let bot = connect_bot(&h).await;
    let (_, ucon) = authorize_me(&h, &bot).await;
    let cx = user_cx(&h, &ucon);

    // one send per second: the second one is limited, waits, goes through on the retry
    h.fake.set_user_send_qps(1);
    h.user
        .send_as_user(&cx, ids::GROUP_CHAT, &UserMessage::text("1"))
        .await
        .expect("first");
    let t = Instant::now();
    let m = h
        .user
        .send_as_user(&cx, ids::GROUP_CHAT, &UserMessage::text("2"))
        .await
        .expect("retried");
    assert!(m.retried);
    assert!(
        t.elapsed() >= Duration::from_millis(900),
        "{:?}",
        t.elapsed()
    );
    let codes: Vec<i64> = h
        .fake
        .requests_to(Endpoint::SendMessage)
        .iter()
        .map(|r| r.code)
        .collect();
    assert_eq!(codes, vec![0, 230020, 0]);
    // the retry carried the same uuid
    let attempts = h.fake.send_attempts();
    assert_eq!(attempts.last().and_then(|a| a.uuid.clone()), Some(m.uuid));

    // never allowed: one retry, then the error
    h.fake.set_user_send_qps(0);
    h.fake.clear_log();
    let e = h
        .user
        .reply_as_user(&cx, &attempts[0].message_id, &UserMessage::text("3"), false)
        .await
        .expect_err("limited twice");
    assert!(
        matches!(e, FeishuError::RateLimited { code: 230020, .. }),
        "{e:?}"
    );
    assert_eq!(h.fake.requests_to(Endpoint::ReplyMessage).len(), 2);
    // a rate limit is not an auth problem
    assert_eq!(
        s(&row(&h, "SELECT status FROM connections WHERE id = ?", &ucon)[0]),
        "ok"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn without_the_send_scope_nothing_is_sent() {
    let mut w = World::sample();
    w.app.user_scopes.retain(|s| s != "im:message.send_as_user");
    let h = setup_with(w).await;
    let bot = connect_bot(&h).await;
    let (_, ucon) = authorize_me(&h, &bot).await;
    let caps: Value = serde_json::from_str(&s(&row(
        &h,
        "SELECT caps FROM connections WHERE id = ?",
        &ucon,
    )[0]))
    .expect("json");
    assert_eq!(caps, json!(["msg.receive"]), "ticked by what was granted");
    let e = h
        .user
        .send_as_user(
            &user_cx(&h, &ucon),
            ids::GROUP_CHAT,
            &UserMessage::text("x"),
        )
        .await
        .expect_err("no scope");
    assert!(e.is_no_scope(), "{e:?}");
    assert!(h.fake.requests_to(Endpoint::SendMessage).is_empty());

    // the bot connection can't send as you
    let e = h
        .user
        .send_as_user(&user_cx(&h, &bot), ids::GROUP_CHAT, &UserMessage::text("x"))
        .await
        .expect_err("not the user connection");
    assert!(
        matches!(e, FeishuError::Token(ProviderError::Config(_))),
        "{e:?}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn granted_scopes_tick_caps_and_permission_rows() {
    let mut w = World::sample();
    // only group reading is enabled for users, no p2p, no sending
    w.app
        .user_scopes
        .retain(|s| s != "im:message.send_as_user" && s != "im:message.p2p_msg:get_as_user");
    let h = setup_with(w).await;
    let bot = connect_bot(&h).await;
    let (_, ucon) = authorize_me(&h, &bot).await;
    let r = row(
        &h,
        "SELECT caps, granted_scopes, config FROM connections WHERE id = ?",
        &ucon,
    );
    let caps: Value = serde_json::from_str(&s(&r[0])).expect("caps");
    let granted: Value = serde_json::from_str(&s(&r[1])).expect("scopes");
    let cfg: Value = serde_json::from_str(&s(&r[2])).expect("config");
    assert_eq!(caps, json!(["msg.receive"]));
    let granted: Vec<String> = serde_json::from_value(granted).expect("list");
    assert!(granted.contains(&"im:message.group_msg:get_as_user".to_string()));
    assert!(!granted.contains(&"im:message.send_as_user".to_string()));
    let status = |scope: &str| {
        cfg["permissions"]
            .as_array()
            .and_then(|ps| ps.iter().find(|p| p["scope"] == scope))
            .map(|p| s(&p["status"]))
    };
    assert_eq!(
        status("im:message.group_msg:get_as_user").as_deref(),
        Some("granted")
    );
    assert_eq!(
        status("im:message.p2p_msg:get_as_user").as_deref(),
        Some("missing")
    );
    assert_eq!(
        status("im:message.send_as_user").as_deref(),
        Some("missing")
    );
    assert_eq!(status("search:message").as_deref(), Some("not_requested"));
    // asking for a capability the grant doesn't allow doesn't get it either
    let (st, v, _) = send(
        &h.api,
        &caller(WS, ME),
        "PATCH",
        &format!("/connections/{ucon}"),
        Some(json!({"caps": ["msg.receive", "msg.send_as_user"]})),
    )
    .await;
    assert_eq!((st, v), (400, json!({"error": "参数不对：caps.1"})));
}

// ---------------------------------------------------------------------------------------------
// reauth.

fn decision_of(h: &H, con: &str) -> Option<Vec<Value>> {
    let (sql, p) = (
        "SELECT question, options, status FROM decisions WHERE subject_type = 'connection' AND subject_id = ? ORDER BY created DESC LIMIT 1".to_string(),
        con.to_string(),
    );
    h.env
        .store()
        .reader(move |c| {
            let mut st = c.prepare(&sql)?;
            let r = st.query_map([&p], |r| {
                Ok(vec![
                    Value::String(r.get(0)?),
                    Value::String(r.get(1)?),
                    Value::String(r.get(2)?),
                ])
            })?;
            Ok::<_, steve_store::StoreError>(r.flatten().next())
        })
        .expect("decision")
}

#[tokio::test(flavor = "multi_thread")]
async fn dead_refresh_token_means_reauth_with_a_decision_and_reauthorizing_settles_it() {
    let h = setup().await;
    let bot = connect_bot(&h).await;
    let (acc, ucon) = authorize_me(&h, &bot).await;
    assert_eq!(decision_of(&h, &ucon), None);

    // offline for too long: the refresh is refused, the connection goes to reauth, one decision
    h.fake.expire_refresh_tokens();
    let e = h.conns.refresh(WS, &ucon, true).await.expect_err("refused");
    assert!(matches!(e, ProviderError::Auth(_)), "{e:?}");
    assert_eq!(
        s(&row(&h, "SELECT status FROM connections WHERE id = ?", &ucon)[0]),
        "reauth"
    );
    let d = decision_of(&h, &ucon).expect("decision");
    assert_eq!(d[0], "飞书需要重新授权");
    assert_eq!(
        serde_json::from_str::<Value>(&s(&d[1])).expect("options"),
        json!(["reauth", "pause"])
    );
    let open_status = s(&d[2]);
    assert_ne!(open_status, "resolved");
    // a second failure doesn't open another one
    let _ = h.conns.refresh(WS, &ucon, true).await;
    assert_eq!(
        row(
            &h,
            "SELECT COUNT(*) FROM decisions WHERE subject_type = 'connection' AND subject_id = ?",
            &ucon
        ),
        vec![json!(1)]
    );

    // re-authorize through the connection's reauth: same account, back to ok, decision settled
    let loc = {
        let (st, v, _) = send(
            &h.api,
            &caller(WS, ME),
            "POST",
            &format!("/connections/{ucon}/reauth"),
            None,
        )
        .await;
        assert_eq!(st, 200, "{v}");
        let url = s(&v["url"]);
        assert!(url.starts_with(h.fake.base_url()));
        let res = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .expect("client")
            .get(&url)
            .send()
            .await
            .expect("authorize");
        let back =
            url::Url::parse(res.headers()[header::LOCATION].to_str().expect("loc")).expect("url");
        let p = format!("{}?{}", back.path(), back.query().unwrap_or_default());
        let (_, _, hd) = send(&h.cb, &caller(WS, ME), "GET", &p, None).await;
        hd[header::LOCATION].to_str().expect("loc").to_string()
    };
    assert_eq!(loc, format!("/#/accounts/{acc}"));
    assert_eq!(
        s(&row(&h, "SELECT status FROM connections WHERE id = ?", &ucon)[0]),
        "ok"
    );
    assert_eq!(s(&decision_of(&h, &ucon).expect("decision")[2]), "resolved");
    // and sending works again
    h.user
        .send_as_user(
            &user_cx(&h, &ucon),
            ids::GROUP_CHAT,
            &UserMessage::text("回来了"),
        )
        .await
        .expect("sent");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_withdrawn_grant_fails_the_send_and_asks_for_reauth() {
    let h = setup().await;
    let bot = connect_bot(&h).await;
    let (_, ucon) = authorize_me(&h, &bot).await;
    h.fake.revoke_user_grant();
    let e = h
        .user
        .send_as_user(
            &user_cx(&h, &ucon),
            ids::GROUP_CHAT,
            &UserMessage::text("x"),
        )
        .await
        .expect_err("revoked");
    assert!(
        matches!(e, FeishuError::Token(ProviderError::Auth(_))),
        "{e:?}"
    );
    assert!(h.fake.sent().is_empty());
    assert_eq!(
        s(&row(&h, "SELECT status FROM connections WHERE id = ?", &ucon)[0]),
        "reauth"
    );
    assert_eq!(
        decision_of(&h, &ucon).map(|d| d[0].clone()),
        Some(json!("飞书需要重新授权"))
    );
}

// ---------------------------------------------------------------------------------------------
// The guard.

/// Once the fake runs, a client left on the default (real) base fails the test instead of
/// reaching open.feishu.cn.
#[tokio::test(flavor = "multi_thread")]
#[should_panic(expected = "测试里不许连真实飞书")]
async fn a_client_on_the_real_base_fails_the_test() {
    let h = setup().await;
    let bot = connect_bot(&h).await;
    let cx = h.conns.cx(WS, &bot).expect("cx");
    let real = steve_channels::adapters::feishu::FeishuClient::new(
        reqwest::Client::new(),
        steve_channels::adapters::feishu::Domain::Feishu.open_base(),
    );
    let _ = real.get(&cx, "/open-apis/bot/v3/info", &[]).await;
}

#[tokio::test(flavor = "multi_thread")]
#[should_panic(expected = "测试里不许连真实飞书")]
async fn the_user_provider_on_real_hosts_fails_the_test() {
    let h = setup().await;
    let bot = connect_bot(&h).await;
    // a provider built for production, used in a test: the token exchange is stopped
    let real = FeishuUser::new(steve_channels::adapters::feishu::Domain::Feishu);
    let cx = h.conns.cx(WS, &bot).expect("cx");
    let client = steve_modules::accounts::connections::OAuthClient {
        id: ids::APP_ID.into(),
        secret: zeroize::Zeroizing::new(ids::APP_SECRET.into()),
    };
    use steve_modules::accounts::connections::ConnectionProvider;
    let _ = real.refresh_tokens(cx.http(), &client, "ur-x").await;
}
