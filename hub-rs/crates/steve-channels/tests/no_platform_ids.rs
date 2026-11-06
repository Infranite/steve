//! Messages of an adapter without platform message ids: a redelivery is dropped by its delivery id
//! (`source_id`); a copy without an id and the bot's copy with one are one message, whichever comes
//! first (also when the side first named the sender by a handle of another system), and the
//! message takes the provider id; two identical messages stay two, and so does a redelivery
//! without a delivery id; a Hub-made id is never one in use; two messages with provider ids never
//! merge; an edit is no new message when every side times to the ms; the Hub's
//! own ids never leave it.

use std::sync::Arc;

use axum::Router;
use serde_json::{Value, json};
use steve_channels::channel::Sender;
use steve_channels::ext::{ChannelDecl, MessageIds};
use steve_channels::fake::channel::FakeChannel;
use steve_channels::inbound::{LABEL_ELSEWHERE, Next};
use steve_channels::queries::{ConnQueries, Queries};
use steve_channels::{Channels, HandleRef, Inbound, InboundMessage, SpaceKind, api};
use steve_flow::testkit::FakeCore;
use steve_flow::{FlowSetup, LinearExecutor};
use steve_modules::ApiError;
use steve_modules::accounts::connections::{self, Connections, ConnectionsConfig};
use steve_modules::accounts::repo::{self as arepo, NewAccount};
use steve_modules::contacts::service::ensure_base;
use steve_modules::testkit::{ME, TestEnv, WS, call, caller};
use steve_modules::vault::Vault;
use steve_modules::vault::keystore::FileKeyStore;
use steve_store::uid;
use tempfile::TempDir;

/// The adapter without message ids (yours, paired with the fake bot).
const SIDE: &str = "test_self";
const T: i64 = 1_700_000_000_000;

struct H {
    env: TestEnv,
    _keys: TempDir,
    ch: Channels,
    api: Router,
    /// The fake bot's connection (provider ids).
    bot: String,
    /// Your connection without message ids.
    mine: String,
}

/// `precision`: the side's time precision; the bot's is 1 ms.
fn setup(precision: i64) -> H {
    let core = Arc::new(FakeCore::default());
    let flow_setup = FlowSetup::new(core.clone());
    let env = TestEnv::new(|r| {
        flow_setup.register(r);
        connections::register(r);
    });
    // the Hub's clock where the messages are: they are news, not history
    env.clock.set(T);
    let keys = tempfile::tempdir().expect("tmp");
    let vault = Vault::new(
        env.store().clone(),
        Arc::new(FileKeyStore::open(keys.path()).expect("keys")),
    );
    let fake = Arc::new(FakeChannel::new());
    let conns = Connections::builder(
        env.modules.clone(),
        vault,
        ConnectionsConfig::new(Vec::new(), true, |_| None),
    )
    .provider(fake.clone())
    .build();
    let flow = flow_setup
        .connections(conns.clone())
        .build(env.modules.clone(), Arc::new(LinearExecutor));
    let ch = Channels::builder(env.modules.clone(), conns, Arc::new(flow))
        .adapter(fake.clone())
        .decl(
            ChannelDecl::new(FakeChannel::ID)
                .handle_system(FakeChannel::SYSTEM)
                .created_precision_ms(1),
        )
        .decl(
            ChannelDecl::new(SIDE)
                .handle_system(FakeChannel::SYSTEM)
                .message_ids(MessageIds::None)
                .created_precision_ms(precision),
        )
        .build();
    let api = api::routes(ch.clone()).with_state(env.modules.clone());
    let (bot, _) = FakeChannel::seed_connection(env.store(), &caller(WS, ME), None).expect("bot");
    let mine = env
        .store()
        .transaction(|tx| -> steve_modules::ApiResult<String> {
            let me = ensure_base(tx, &caller(WS, ME))?;
            let a = arepo::insert(
                tx,
                WS,
                &NewAccount {
                    system_id: FakeChannel::SYSTEM.into(),
                    family: "slack_user".into(),
                    party_id: me.id,
                    kind: "personal".into(),
                    handle: "upx".into(),
                    handle_display: "upx".into(),
                    display_name: "我".into(),
                    verified: true,
                    confidence: 1.0,
                    source: "connect".into(),
                    source_label: String::new(),
                },
            )?;
            let con = uid("con");
            tx.execute(
                "INSERT INTO connections (id, workspace_id, account_id, adapter, auth, caps, config, created_by, created, updated)
                 VALUES (?, ?, ?, ?, 'none', '[\"msg.receive\",\"msg.send_as_user\"]', ?, ?, 1, 1)",
                rusqlite::params![
                    con,
                    WS,
                    a.id,
                    SIDE,
                    json!({ "app_connection": bot }).to_string(),
                    ME
                ],
            )?;
            Ok(con)
        })
        .expect("your connection");
    fake.space(
        "G1",
        "运维群",
        SpaceKind::Group,
        false,
        &[("ualice", "王小丽"), ("ubob", "张伟")],
    );
    H {
        env,
        _keys: keys,
        ch,
        api,
        bot,
        mine,
    }
}

fn rows(h: &H, sql: &str, p: &[&str]) -> Vec<Vec<Value>> {
    let (sql, p): (String, Vec<String>) = (sql.into(), p.iter().map(|s| s.to_string()).collect());
    h.env
        .store()
        .reader(move |c| {
            let mut st = c.prepare(&sql)?;
            let n = st.column_count();
            let out = st
                .query_map(rusqlite::params_from_iter(p.iter()), |r| {
                    (0..n)
                        .map(|i| {
                            Ok(match r.get_ref(i)? {
                                rusqlite::types::ValueRef::Null => Value::Null,
                                rusqlite::types::ValueRef::Integer(x) => json!(x),
                                rusqlite::types::ValueRef::Real(x) => json!(x),
                                rusqlite::types::ValueRef::Text(t) => {
                                    json!(String::from_utf8_lossy(t))
                                }
                                rusqlite::types::ValueRef::Blob(_) => Value::Null,
                            })
                        })
                        .collect::<rusqlite::Result<Vec<_>>>()
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok::<_, steve_store::StoreError>(out)
        })
        .expect("rows")
}

fn count(h: &H, sql: &str, p: &[&str]) -> i64 {
    rows(h, sql, p)[0][0].as_i64().expect("count")
}

/// The external ids of the stored messages, oldest first.
fn stored(h: &H) -> Vec<String> {
    rows(
        h,
        "SELECT external_id FROM chan_messages WHERE direction = 'in' ORDER BY created, rowid",
        &[],
    )
    .into_iter()
    .map(|r| r[0].as_str().unwrap_or_default().to_string())
    .collect()
}

/// Alice on the bot, with a provider id.
fn bot_copy(id: &str, text: &str, created: i64) -> InboundMessage {
    let mut m = FakeChannel::message("G1", id, "ualice", text);
    m.created = created;
    m
}

/// Alice on your side, without one: known by her email there, with her chat handle as an alias.
fn own_copy(text: &str, created: i64, source: Option<&str>) -> InboundMessage {
    let sender = Sender::new(HandleRef::new("mail", "alice@example.com"))
        .alias(FakeChannel::handle("ualice"));
    let m = InboundMessage::without_id("G1", sender, text, created);
    match source {
        Some(s) => m.source_id(s),
        None => m,
    }
}

async fn push(h: &H, conn: &str, m: InboundMessage) -> Inbound {
    h.ch.on_inbound(WS, conn, m).await.expect("inbound")
}

fn queries(h: &H, conn: &str) -> ConnQueries {
    let cx = h.ch.connections().cx(WS, conn).expect("cx");
    Queries::new(h.ch.clone()).for_cx(&cx)
}

#[tokio::test(flavor = "multi_thread")]
async fn a_redelivery_with_the_same_delivery_id_is_stored_once() {
    let h = setup(1000);
    let first = push(&h, &h.mine, own_copy("今天发布吗", T, Some("ev1"))).await;
    assert!(!first.duplicate);
    assert_eq!(first.next, Next::Handle);
    let ext = stored(&h);
    assert_eq!(ext.len(), 1);
    assert!(ext[0].starts_with("fp:v1:"), "{ext:?}");
    // the same delivery again, even read a minute later with another time
    let again = push(&h, &h.mine, own_copy("今天发布吗", T + 60_000, Some("ev1"))).await;
    assert!(again.duplicate);
    assert_eq!(again.message_id, first.message_id);
    assert_eq!(stored(&h).len(), 1);
    assert_eq!(
        count(
            &h,
            "SELECT count(*) FROM ext_events WHERE source = 'im'",
            &[]
        ),
        1
    );
    // the connection knows it by that id, and acts on it through it
    let q = queries(&h, &h.mine);
    assert!(
        q.known(&own_copy("今天发布吗", T, Some("ev1")))
            .expect("known")
    );
    assert!(
        !q.known(&own_copy("今天发布吗", T, Some("ev2")))
            .expect("known")
    );
    let r = q
        .message_ref_by_source("G1", "ev1")
        .expect("ref")
        .expect("found");
    assert_eq!((r.id(), r.source_id.as_deref()), (None, Some("ev1")));
    assert_eq!((r.created, r.created_exact), (T, true));
    assert_eq!(r.text.as_deref(), Some("今天发布吗"));
    assert_eq!(q.message_ref("G1", "ev1").expect("ref"), Some(r));
    // the Hub's own id is no handle on it
    assert_eq!(q.message_ref("G1", &ext[0]).expect("ref"), None);
    // the bot does not know it by your delivery id
    assert_eq!(
        queries(&h, &h.bot)
            .message_ref_by_source("G1", "ev1")
            .expect("ref"),
        None
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn your_copy_and_the_bots_are_one_message_in_either_order() {
    // yours first: the bot's copy upgrades it to the provider id
    let h = setup(1000);
    let a = push(&h, &h.mine, own_copy("今天发布吗", T + 400, Some("ev1"))).await;
    assert!(!a.duplicate);
    let b = push(&h, &h.bot, bot_copy("om_1", "今天发布吗", T + 123)).await;
    assert!(b.duplicate, "{b:?}");
    assert_eq!(b.message_id, a.message_id);
    assert_eq!(b.next, Next::Record(LABEL_ELSEWHERE));
    assert_eq!(stored(&h), ["om_1"]);
    // the redelivery of yours is still the same message
    let c = push(&h, &h.mine, own_copy("今天发布吗", T + 400, Some("ev1"))).await;
    assert_eq!((c.duplicate, c.message_id), (true, a.message_id.clone()));
    assert_eq!(stored(&h), ["om_1"]);
    // your side acts on it by the provider id now
    let r = queries(&h, &h.mine)
        .message_ref_by_source("G1", "ev1")
        .expect("ref")
        .expect("found");
    assert_eq!(r.id(), Some("om_1"));

    // the bot's first: yours finds it, the provider id stays
    let h = setup(1000);
    let b = push(&h, &h.bot, bot_copy("om_1", "今天发布吗", T + 123)).await;
    // too far apart for the side's precision: another message
    let late = push(&h, &h.mine, own_copy("今天发布吗", T + 5_000, None)).await;
    assert!(!late.duplicate);
    let a = push(&h, &h.mine, own_copy("今天发布吗", T, None)).await;
    assert!(a.duplicate, "{a:?}");
    assert_eq!(a.message_id, b.message_id);
    assert_eq!(stored(&h).len(), 2);
    assert_eq!(stored(&h)[0], "om_1");
}

#[tokio::test(flavor = "multi_thread")]
async fn two_identical_messages_stay_two_whichever_copy_comes_first() {
    // yours first, then the bot's
    let h = setup(1000);
    let a1 = push(&h, &h.mine, own_copy("好的", T, Some("ev1"))).await;
    let a2 = push(&h, &h.mine, own_copy("好的", T, Some("ev2"))).await;
    assert!(!a1.duplicate && !a2.duplicate);
    assert_ne!(a1.message_id, a2.message_id);
    let ext = stored(&h);
    assert_eq!(ext.len(), 2);
    assert!(ext[0].ends_with(":0") && ext[1].ends_with(":1"), "{ext:?}");
    let b1 = push(&h, &h.bot, bot_copy("om_1", "好的", T + 100)).await;
    let b2 = push(&h, &h.bot, bot_copy("om_2", "好的", T + 300)).await;
    assert!(b1.duplicate && b2.duplicate);
    assert_ne!(b1.message_id, b2.message_id);
    let mut ext = stored(&h);
    ext.sort();
    assert_eq!(ext, ["om_1", "om_2"]);

    // the bot's first, then yours (without delivery ids)
    let h = setup(1000);
    let b1 = push(&h, &h.bot, bot_copy("om_1", "好的", T + 100)).await;
    let b2 = push(&h, &h.bot, bot_copy("om_2", "好的", T + 300)).await;
    assert_ne!(b1.message_id, b2.message_id);
    let a1 = push(&h, &h.mine, own_copy("好的", T, None)).await;
    let a2 = push(&h, &h.mine, own_copy("好的", T, None)).await;
    assert!(a1.duplicate && a2.duplicate);
    assert_eq!(a1.message_id, b1.message_id, "the nearest first");
    assert_eq!(a2.message_id, b2.message_id, "never the one it supplied");
    assert_eq!(stored(&h).len(), 2);
}

#[tokio::test(flavor = "multi_thread")]
async fn two_messages_with_provider_ids_never_merge() {
    let h = setup(1000);
    push(&h, &h.bot, bot_copy("om_1", "好的", T)).await;
    let b = push(&h, &h.bot, bot_copy("om_2", "好的", T)).await;
    assert!(!b.duplicate);
    assert_eq!(stored(&h), ["om_1", "om_2"]);
}

#[tokio::test(flavor = "multi_thread")]
async fn an_edit_is_no_new_message_when_every_side_times_to_the_ms() {
    let h = setup(1);
    let b = push(&h, &h.bot, bot_copy("om_1", "明天上线", T + 7)).await;
    // your side read it after it was edited: other text, the same time
    let a = push(&h, &h.mine, own_copy("明天下午上线", T + 7, Some("ev1"))).await;
    assert!(a.duplicate, "{a:?}");
    assert_eq!(a.message_id, b.message_id);
    assert_eq!(stored(&h), ["om_1"]);
    // the same text a moment later is another message: the first is yours already
    let c = push(&h, &h.mine, own_copy("明天上线", T + 8, Some("ev2"))).await;
    assert!(!c.duplicate);
    // at this precision a ms apart is not the same time
    let h = setup(1);
    push(&h, &h.bot, bot_copy("om_1", "明天上线", T + 7)).await;
    let a = push(&h, &h.mine, own_copy("明天下午上线", T + 9, Some("ev1"))).await;
    assert!(!a.duplicate);
}

#[tokio::test(flavor = "multi_thread")]
async fn ids_the_hub_makes_never_leave_it_and_are_never_an_adapters() {
    let h = setup(1000);
    let a = push(&h, &h.mine, own_copy("今天发布吗", T, Some("ev1"))).await;
    let ext = stored(&h).remove(0);
    let (s, v) = call(
        &h.api,
        Some(&caller(WS, ME)),
        "GET",
        &format!("/spaces/{}", a.space_id),
        None,
    )
    .await;
    assert_eq!(s, 200, "{v}");
    assert!(!v.to_string().contains("fp:"), "{v}");
    // an adapter with provider ids must give one; nobody may give one of the Hub's
    let mut m = bot_copy("x", "你好", T);
    m.external_id = String::new();
    let e = h.ch.on_inbound(WS, &h.bot, m).await.expect_err("no id");
    assert!(matches!(e, ApiError::Invalid(_)), "{e:?}");
    let e =
        h.ch.on_inbound(WS, &h.bot, bot_copy(&ext, "你好", T))
            .await
            .expect_err("ours");
    assert!(matches!(e, ApiError::Rule(_)), "{e:?}");
    assert_eq!(stored(&h).len(), 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_hub_made_id_is_never_one_in_use_after_an_upgrade() {
    let h = setup(1000);
    let a1 = push(&h, &h.mine, own_copy("好的", T, Some("ev1"))).await;
    let a2 = push(&h, &h.mine, own_copy("好的", T, Some("ev2"))).await;
    assert_ne!(a1.message_id, a2.message_id);
    // the bot's copy of the first takes its Hub-made id away
    let b1 = push(&h, &h.bot, bot_copy("om_1", "好的", T)).await;
    assert_eq!((b1.duplicate, b1.message_id), (true, a1.message_id));
    // a third identical one at the same time gets an id of its own
    let a3 = push(&h, &h.mine, own_copy("好的", T, Some("ev3"))).await;
    assert!(!a3.duplicate, "{a3:?}");
    assert_ne!(a3.message_id, a2.message_id);
    let ext = stored(&h);
    assert_eq!(ext.len(), 3);
    assert_eq!(ext[0], "om_1");
    assert!(ext[1].ends_with(":1") && ext[2].ends_with(":2"), "{ext:?}");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_redelivery_without_a_delivery_id_is_another_message() {
    // a connection never matches its own copies: without a source_id the adapter must not
    // deliver a message twice
    let h = setup(1000);
    let a = push(&h, &h.mine, own_copy("今天发布吗", T, None)).await;
    let b = push(&h, &h.mine, own_copy("今天发布吗", T, None)).await;
    assert!(!a.duplicate && !b.duplicate);
    assert_ne!(a.message_id, b.message_id);
    assert_eq!(stored(&h).len(), 2);
    assert!(
        !queries(&h, &h.mine)
            .known(&own_copy("今天发布吗", T, None))
            .expect("known")
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_copy_from_a_sender_first_known_by_another_system_is_still_one_message() {
    let h = setup(1000);
    // your side sees her first, by an id of another system and her email: she has no handle in
    // the side's handle system yet
    let sender = Sender::new(HandleRef::new("github", "carol-gh"))
        .alias(HandleRef::new("mail", "carol@example.com"));
    let a = push(
        &h,
        &h.mine,
        InboundMessage::without_id("G1", sender, "今天发布吗", T + 400).source_id("ev1"),
    )
    .await;
    assert!(!a.duplicate);
    // the bot knows her (new to it) by her chat handle and her email: the same person, the same message
    let mut m = bot_copy("om_1", "今天发布吗", T + 123);
    m.sender = Sender::new(FakeChannel::handle("ucarol"))
        .alias(HandleRef::new("mail", "carol@example.com"));
    let b = push(&h, &h.bot, m).await;
    assert!(b.duplicate, "{b:?}");
    assert_eq!(b.message_id, a.message_id);
    assert_eq!(b.sender_party_id, a.sender_party_id);
    assert_eq!(stored(&h), ["om_1"]);
}
