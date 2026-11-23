//! One space per platform chat (migration 88): your account through a channel service that names
//! the bot's platform joins the bot's space as one more identity, whichever came first; a message
//! both received is stored once and carries both identities (`seen_by`); another member of the
//! workspace can read all stored copies in single-user mode, with identity records preserved. A generic service
//! that names no platform keeps its own space.

#[path = "support/digest_env.rs"]
mod digest_env;
use digest_env::TestEnv;
use std::sync::Arc;

use serde_json::{Value, json};
use steve_channels::channel::Sender;
use steve_channels::ext::{ChannelDecl, MessageIds};
use steve_channels::fake::channel::FakeChannel;
use steve_channels::{Channels, HandleRef, Inbound, InboundMessage, SpaceKind, messages};
use steve_flow::testkit::FakeCore;
use steve_flow::{FlowSetup, LinearExecutor};
use steve_modules::accounts::connections::{self, Connections, ConnectionsConfig};
use steve_modules::accounts::repo::{self as arepo, NewAccount};
use steve_modules::contacts::service::ensure_base;
use steve_modules::testkit::{ME, WS, caller};
use steve_modules::vault::Vault;
use steve_modules::vault::keystore::FileKeyStore;
use steve_store::uid;
use tempfile::TempDir;

/// Your account through a channel service: no message ids, no bot app of its own.
const SIDE: &str = "test_self";
const T: i64 = 1_700_000_000_000;

struct H {
    env: TestEnv,
    _keys: TempDir,
    ch: Channels,
    /// The fake bot's connection (provider ids).
    bot: String,
    /// Your connection through the service.
    mine: String,
}

/// `precision`: the side's time precision; the bot's is 1 ms.
fn setup(precision: i64, proxy_config: Value) -> H {
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
                    proxy_config.to_string(),
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

fn spaces_of_g1(h: &H) -> Vec<String> {
    rows(
        h,
        "SELECT id FROM spaces WHERE external_id = 'G1' ORDER BY created",
        &[],
    )
    .into_iter()
    .map(|r| r[0].as_str().unwrap().to_string())
    .collect()
}

/// `(text, seen_by)` of the stored messages, oldest first.
fn seen(h: &H) -> Vec<(String, i64)> {
    rows(
        h,
        "SELECT text, seen_by FROM chan_messages WHERE direction = 'in' ORDER BY created, rowid",
        &[],
    )
    .into_iter()
    .map(|r| (r[0].as_str().unwrap().to_string(), r[1].as_i64().unwrap()))
    .collect()
}

fn page_as(h: &H, space: &str, user: &str) -> Vec<String> {
    let (space, user) = (space.to_string(), user.to_string());
    messages::page(h.env.store(), &space, &user, None, 50)
        .expect("page")
        .into_iter()
        .rev()
        .map(|m| m.text.unwrap_or_default())
        .collect()
}

#[tokio::test(flavor = "multi_thread")]
async fn the_bot_and_your_account_through_a_service_share_one_space_and_one_copy() {
    let h = setup(1000, json!({ "platform": FakeChannel::SYSTEM }));
    push(&h, &h.bot, bot_copy("om_1", "今天发布吗", T)).await;
    // your copy of the same message, and one only you received
    let dup = push(&h, &h.mine, own_copy("今天发布吗", T + 200, Some("ev1"))).await;
    assert!(dup.duplicate);
    push(&h, &h.mine, own_copy("我先看一下", T + 5_000, Some("ev2"))).await;
    let spaces = spaces_of_g1(&h);
    assert_eq!(spaces.len(), 1, "one space for the chat");
    let ids = rows(
        &h,
        "SELECT connection_id, ordinal, owner_user FROM space_identities WHERE space_id = ? ORDER BY ordinal",
        &[&spaces[0]],
    );
    assert_eq!(ids.len(), 2);
    assert_eq!(
        (ids[0][0].as_str(), ids[0][1].as_i64(), &ids[0][2]),
        (Some(h.bot.as_str()), Some(0), &Value::Null)
    );
    assert_eq!(
        (ids[1][0].as_str(), ids[1][1].as_i64(), ids[1][2].as_str()),
        (Some(h.mine.as_str()), Some(1), Some(ME))
    );
    assert_eq!(
        seen(&h),
        [
            ("今天发布吗".to_string(), 0b11),
            ("我先看一下".to_string(), 0b10)
        ]
    );
    // the bot speaks there; your account only listens
    let space = spaces[0].clone();
    let voice = h
        .env
        .store()
        .reader(move |c| steve_channels::spaces::voice(c, &space))
        .expect("voice");
    assert_eq!(voice.as_deref(), Some(h.bot.as_str()));
    // Single-user reads include both identities; receipt and outbound voice identity stay intact.
    assert_eq!(
        page_as(&h, &spaces[0], "someone_else"),
        ["今天发布吗", "我先看一下"]
    );
    assert_eq!(page_as(&h, &spaces[0], ME), ["今天发布吗", "我先看一下"]);
    let before_reads = seen(&h);
    h.env
        .store()
        .read(|_| -> steve_modules::ApiResult<()> {
            let page = messages::page_before(h.env.store(), &spaces[0], "someone_else", None, 50)?;
            assert_eq!(page.len(), 2);
            let older = messages::page_before(
                h.env.store(),
                &spaces[0],
                "someone_else",
                Some((page[0].created, &page[0].id)),
                50,
            )?;
            assert_eq!(older.len(), 1);
            assert_eq!(older[0].text.as_deref(), Some("今天发布吗"));
            Ok(())
        })
        .unwrap();
    assert_eq!(
        seen(&h),
        before_reads,
        "reads must not rewrite receiving identity bits"
    );
    // A historical bit with no remaining identity row must not hide the message either.
    h.env
        .store()
        .with_conn(|c| {
            c.execute(
                "UPDATE chan_messages SET seen_by = 4 WHERE text = '我先看一下'",
                [],
            )?;
            Ok(())
        })
        .unwrap();
    assert_eq!(page_as(&h, &spaces[0], "someone_else").len(), 2);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_bot_that_comes_later_joins_the_space_your_account_made() {
    let h = setup(1000, json!({ "platform": FakeChannel::SYSTEM }));
    push(&h, &h.mine, own_copy("只有我看到的", T, Some("ev0"))).await;
    push(&h, &h.bot, bot_copy("om_2", "大家好", T + 10_000)).await;
    let spaces = spaces_of_g1(&h);
    assert_eq!(spaces.len(), 1, "{spaces:?}");
    // Joining a bot does not restrict single-user reads of the earlier personal copy.
    assert_eq!(
        page_as(&h, &spaces[0], "someone_else"),
        ["只有我看到的", "大家好"]
    );
    assert_eq!(page_as(&h, &spaces[0], ME), ["只有我看到的", "大家好"]);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_service_that_names_no_platform_keeps_its_own_space() {
    let h = setup(1000, json!({}));
    push(&h, &h.bot, bot_copy("om_1", "今天发布吗", T)).await;
    push(&h, &h.mine, own_copy("我先看一下", T + 5_000, Some("ev2"))).await;
    assert_eq!(spaces_of_g1(&h).len(), 2);
}
