//! F8 acceptance (feishu-lark-cli.md §2.3) on the fake Feishu: your user token's chats are polled;
//! a new private message lands within one round as `im.dm` to be handled (the “私聊你” rule) with
//! `via=poll`; a chat seen for the first time gets a cursor but no backfill; what the long
//! connection already stored is not processed again, and the bot's groups are not read; a rate
//! limit ends the round and carries the rest over; quiet hours slow the pace down, a new private
//! message speeds it up; topic replies are read through their thread.

use std::sync::Arc;
use std::time::Duration;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, header};
use serde_json::{Value, json};
use steve_channels::adapters::feishu::auth::{FeishuApp, FeishuUser};
use steve_channels::adapters::feishu::client::is_real_host;
use steve_channels::adapters::feishu::self_poll::{
    self, BURST_INTERVAL, DEFAULT_INTERVAL, QUIET_INTERVAL, SelfPoller, SelfPollers,
};
use steve_channels::adapters::feishu::user_routes;
use steve_channels::fake::feishu::model::{Body as MsgBody, Sender as MsgSender};
use steve_channels::fake::feishu::{Chat, Endpoint, FakeFeishu, Member, Message, ids};
use steve_channels::{Channels, HandleRef, InboundMessage, SpaceKind};
use steve_flow::testkit::FakeCore;
use steve_flow::{FlowSetup, LinearExecutor};
use steve_modules::Caller;
use steve_modules::accounts::connections::{self, Connections, ConnectionsConfig};
use steve_modules::clock::Clock;
use steve_modules::testkit::{ME, TestEnv, WS, caller};
use steve_modules::vault::Vault;
use steve_modules::vault::keystore::FileKeyStore;
use tempfile::TempDir;
use tower::ServiceExt;

const ORIGIN: &str = "http://hub.test:8440";
/// Your p2p chat with the bot (the long connection receives what you send there).
const ME_BOT_CHAT: &str = "oc_mebot000000000000000000000001";

struct H {
    env: TestEnv,
    _keys: TempDir,
    fake: FakeFeishu,
    ch: Channels,
    user: Arc<FeishuUser>,
    bot_con: String,
    self_con: String,
    self_acc: String,
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

async fn setup() -> H {
    let fake = FakeFeishu::start().await.expect("fake feishu");
    assert!(!is_real_host(fake.base_url()));
    // your p2p chat with the bot
    fake.world(|w| {
        w.chats.push(Chat {
            chat_id: ME_BOT_CHAT.into(),
            name: String::new(),
            description: String::new(),
            chat_mode: "p2p".into(),
            chat_type: "private".into(),
            owner_open_id: String::new(),
            external: false,
            bot_in_chat: true,
            chat_status: "normal".into(),
        });
        let tk = w.app.tenant_key.clone();
        w.members.push((
            ME_BOT_CHAT.into(),
            Member {
                ids: ids::OWNER.into(),
                name: "李鹏翔".into(),
                tenant_key: tk,
            },
        ));
    });
    let core = Arc::new(FakeCore::default());
    let flow_setup = FlowSetup::new(core);
    let env = TestEnv::new(|r| {
        flow_setup.register(r);
        connections::register(r);
    });
    let keys = tempfile::tempdir().expect("tmp");
    let vault = Vault::new(
        env.store().clone(),
        Arc::new(FileKeyStore::open(keys.path()).expect("keys")),
    );
    vault.init(WS, ME).expect("vault init");
    let app = Arc::new(FeishuApp::new());
    let user = Arc::new(
        FeishuUser::with_base(fake.base_url())
            .with_party_hooks(steve_modules::accounts::ports().hooks),
    );
    let conns = Connections::builder(
        env.modules.clone(),
        vault,
        ConnectionsConfig::new(vec![ORIGIN.into()], true, |_| None),
    )
    .provider(app.clone())
    .provider(user.clone())
    .build();
    let flow = flow_setup
        .connections(conns.clone())
        .build(env.modules.clone(), Arc::new(LinearExecutor));
    let ch = Channels::builder(env.modules.clone(), conns.clone(), Arc::new(flow))
        .adapter(app)
        .build();

    let api: Router = connections::routes(conns.clone())
        .merge(user_routes(conns.clone(), user.claims().clone()))
        .with_state(env.modules.clone());
    let cb: Router = connections::callback_routes(conns.clone()).with_state(env.modules.clone());
    let me = caller(WS, ME);
    let (st, v, _) = send(
        &api,
        &me,
        "POST",
        "/connections",
        Some(json!({
            "adapter": "feishu_app",
            "fields": {"App ID": ids::APP_ID, "App Secret": ids::APP_SECRET},
            "config": {"api_base": fake.base_url()},
        })),
    )
    .await;
    assert_eq!(st, 200, "{v}");
    let bot_con = v["id"].as_str().expect("id").to_string();

    // “授权你本人”: start → the fake's consent page → our callback
    let (st, v, _) = send(
        &api,
        &me,
        "POST",
        "/channels/feishu/user/oauth/start",
        Some(json!({"app_connection": bot_con})),
    )
    .await;
    assert_eq!(st, 200, "{v}");
    let url = v["url"].as_str().expect("url").to_string();
    assert!(url.starts_with(fake.base_url()), "fake hosts only: {url}");
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
    let (st, _, hd) = send(&cb, &me, "GET", &p, None).await;
    assert_eq!(st, 302);
    let loc = hd[header::LOCATION].to_str().expect("loc").to_string();
    let self_acc = loc
        .strip_prefix("/#/accounts/")
        .unwrap_or_else(|| panic!("not a success: {loc}"))
        .to_string();
    let self_con = str_of(
        &q(
            &env,
            "SELECT id FROM connections WHERE account_id = ?",
            &self_acc,
        )[0][0],
    );
    let caps = str_of(&q(&env, "SELECT caps FROM connections WHERE id = ?", &self_con)[0][0]);
    assert!(caps.contains("msg.receive"), "{caps}");
    H {
        env,
        _keys: keys,
        fake,
        ch,
        user,
        bot_con,
        self_con,
        self_acc,
    }
}

fn q(env: &TestEnv, sql: &str, p: &str) -> Vec<Vec<Value>> {
    let (sql, p) = (sql.to_string(), p.to_string());
    env.store()
        .reader(move |c| {
            let mut st = c.prepare(&sql)?;
            let n = st.column_count();
            let rows = st
                .query_map([&p], |r| {
                    (0..n)
                        .map(|i| {
                            Ok(match r.get_ref(i)? {
                                rusqlite::types::ValueRef::Null => Value::Null,
                                rusqlite::types::ValueRef::Integer(x) => json!(x),
                                rusqlite::types::ValueRef::Text(t) => {
                                    json!(String::from_utf8_lossy(t))
                                }
                                _ => Value::Null,
                            })
                        })
                        .collect::<rusqlite::Result<Vec<_>>>()
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok::<_, steve_store::StoreError>(rows)
        })
        .expect("query")
}

fn str_of(v: &Value) -> String {
    v.as_str().unwrap_or_default().to_string()
}

fn count(h: &H, sql: &str, p: &str) -> i64 {
    q(&h.env, sql, p)[0][0].as_i64().expect("count")
}

/// Messages stored in the self connection's spaces.
fn self_messages(h: &H) -> Vec<Vec<Value>> {
    q(
        &h.env,
        "SELECT m.external_id, s.external_id, s.kind, m.text FROM chan_messages m JOIN spaces s ON s.id = m.space_id
          WHERE s.connection_id = ? AND m.direction = 'in' ORDER BY m.created, m.external_id",
        &h.self_con,
    )
}

/// A new message in the fake, `secs` seconds after the newest one there.
#[allow(clippy::too_many_arguments)]
fn add(
    h: &H,
    id: &str,
    chat: &str,
    from: &str,
    text: &str,
    secs: i64,
    thread: Option<&str>,
    root: Option<&str>,
) -> i64 {
    let clock = h.env.clock.now();
    h.fake.world(|w| {
        let newest = w
            .messages
            .iter()
            .filter_map(|m| m.create_time.parse::<i64>().ok())
            .max()
            .unwrap_or(0);
        let at = newest.max(clock) + secs * 1000;
        let tk = w.app.tenant_key.clone();
        w.messages.push(Message {
            message_id: id.into(),
            root_id: root.map(String::from),
            parent_id: root.map(String::from),
            thread_id: thread.map(String::from),
            msg_type: "text".into(),
            create_time: at.to_string(),
            update_time: at.to_string(),
            deleted: false,
            updated: false,
            chat_id: chat.into(),
            sender: MsgSender {
                id: from.into(),
                id_type: "open_id".into(),
                sender_type: "user".into(),
                tenant_key: tk,
            },
            body: MsgBody {
                content: json!({"text": text}).to_string(),
            },
            mentions: Vec::new(),
            upper_message_id: None,
        });
        at
    })
}

fn poller(h: &H) -> SelfPoller {
    SelfPoller::new(h.ch.clone(), h.user.clone(), WS, &h.self_con)
}

/// The chats of your user token in the sample world (the bot's p2p with Alice is not yours).
const MY_CHATS: usize = 6;

#[tokio::test]
async fn first_round_sets_cursors_without_backfill_then_a_dm_lands_within_a_round() {
    let h = setup().await;
    set_setting(&h, "quiet", "关闭");
    let lists_before = h.fake.requests_to(Endpoint::ListChats).len();
    let mut p = poller(&h);
    let r = p.round().await;
    assert_eq!(r.error, None, "{r:?}");
    assert_eq!(r.chats, MY_CHATS, "{r:?}");
    assert_eq!(r.first_seen, MY_CHATS);
    assert_eq!(r.new_messages, 0, "no backfill");
    assert_eq!(r.requests, 1 + MY_CHATS as u32);
    assert!(self_messages(&h).is_empty());
    // Bob's DM (already there) is the cursor of that chat
    let k = h
        .env
        .store()
        .reader(|c| self_poll::cursor(c, &h.self_con, ids::USER_P2P_CHAT))
        .expect("read")
        .expect("cursor");
    assert_eq!(k.cursor, Some(1_761_899_600_000));
    assert_eq!(
        k.boundary,
        vec!["om_userp2p0000000000000000000000001".to_string()]
    );
    // the user token was used, never the tenant token for listing
    let lists = h.fake.requests_to(Endpoint::ListChats)[lists_before..].to_vec();
    assert_eq!(lists.len(), 1);
    assert!(
        lists[0].token.as_deref().is_some_and(|t| t.contains("u-")),
        "{lists:?}"
    );
    assert!(
        lists[0].query.contains("types=p2p%2Cgroup") || lists[0].query.contains("types=p2p,group"),
        "{lists:?}"
    );

    // a round with nothing new reads nothing new
    let r = p.round().await;
    assert_eq!(
        (r.first_seen, r.new_messages, r.duplicates),
        (0, 0, 0),
        "{r:?}"
    );

    // Bob writes to you
    add(
        &h,
        "om_f8dm1",
        ids::USER_P2P_CHAT,
        ids::BOB.open_id,
        "合同改好了吗？",
        5,
        None,
        None,
    );
    let r = p.round().await;
    assert_eq!((r.new_messages, r.new_dms), (1, 1), "{r:?}");
    let rows = self_messages(&h);
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0][0], json!("om_f8dm1"));
    assert_eq!(rows[0][2], json!("dm"));
    assert_eq!(rows[0][3], json!("合同改好了吗？"));
    // the signal: im.dm, to be handled (the “私聊你” rule), via=poll
    let ev = q(
        &h.env,
        "SELECT type, payload, outcome_label FROM ext_events WHERE external_id = ?",
        "om_f8dm1",
    );
    assert_eq!(ev.len(), 1, "{ev:?}");
    assert_eq!(ev[0][0], json!("im.dm"));
    let payload: Value = serde_json::from_str(&str_of(&ev[0][1])).expect("payload");
    assert_eq!(payload["via"], json!("poll"));
    // the “私聊你 → 起草” rule (C3): a pending draft as you, waiting for your approval; nothing sent
    assert_eq!(ev[0][2], json!("等你确认回复"));
    let draft = q(
        &h.env,
        "SELECT m.status, m.on_behalf, d.kind, d.question FROM chan_messages m JOIN spaces s ON s.id = m.space_id
           JOIN decisions d ON d.id = m.decision_id WHERE s.connection_id = ? AND m.direction = 'out'",
        &h.self_con,
    );
    assert_eq!(draft.len(), 1, "{draft:?}");
    assert_eq!(draft[0][0], json!("draft"));
    assert_eq!(draft[0][1], json!("none"));
    assert_eq!(draft[0][2], json!("approve"));
    assert!(
        str_of(&draft[0][3]).starts_with("以你本人发到「"),
        "{draft:?}"
    );
    // the sender is Bob, named after the p2p chat
    let sender = q(
        &h.env,
        "SELECT coalesce(nullif(a.display_name, ''), a.handle_display) FROM chan_messages m JOIN accounts a ON a.id = m.sender_account_id WHERE m.external_id = ?",
        "om_f8dm1",
    );
    assert_eq!(sender[0][0], json!("张伟"));

    // the fast pace for five minutes
    let now = h.env.clock.now();
    assert_eq!(p.burst_until(), now + self_poll::BURST_FOR_MS);
    assert_eq!(p.next_delay(&r, now), BURST_INTERVAL);
    h.env.clock.advance(self_poll::BURST_FOR_MS + 1);
    let quiet_off = h.env.clock.now();
    set_setting(&h, "quiet", "关闭");
    assert_eq!(
        p.next_delay(&Default::default(), quiet_off),
        DEFAULT_INTERVAL
    );

    // read again: the same message is not processed twice
    let r = p.round().await;
    assert_eq!((r.new_messages, r.duplicates), (0, 0), "{r:?}");
    assert_eq!(self_messages(&h).len(), 1);
}

#[tokio::test]
async fn your_own_messages_are_recorded_not_handled() {
    let h = setup().await;
    let mut p = poller(&h);
    p.round().await;
    add(
        &h,
        "om_f8mine",
        ids::USER_P2P_CHAT,
        ids::OWNER.open_id,
        "好的，明天签",
        3,
        None,
        None,
    );
    let r = p.round().await;
    assert_eq!((r.new_messages, r.new_dms), (1, 0), "{r:?}");
    let ev = q(
        &h.env,
        "SELECT outcome_label FROM ext_events WHERE external_id = ?",
        "om_f8mine",
    );
    assert_eq!(ev[0][0], json!(steve_channels::inbound::LABEL_OWN));
    let acc = q(
        &h.env,
        "SELECT sender_account_id FROM chan_messages WHERE external_id = ?",
        "om_f8mine",
    );
    assert_eq!(acc[0][0], json!(h.self_acc), "your messages are yours");
    assert_eq!(p.burst_until(), 0, "your own message does not speed up");
}

/// The long connection's view: what the bot connection stored.
async fn bot_received(h: &H, chat: &str, kind: SpaceKind, id: &str, from: &str) {
    let mut m = InboundMessage::text(
        chat,
        id,
        HandleRef::new("feishu", from),
        "x",
        h.env.clock.now(),
    );
    m.space_kind = Some(kind);
    m.via = Some("ws".into());
    let i =
        h.ch.on_inbound(WS, &h.bot_con, m)
            .await
            .expect("bot inbound");
    assert!(!i.duplicate);
}

#[tokio::test]
async fn the_long_connections_part_is_not_processed_twice() {
    let h = setup().await;
    let mut p = poller(&h);
    p.round().await;
    // the bot has the platform group: its long connection covers it, the poller skips it
    bot_received(
        &h,
        ids::GROUP_CHAT,
        SpaceKind::Group,
        "om_f8botgrp",
        ids::ALICE.open_id,
    )
    .await;
    add(
        &h,
        "om_f8botgrp",
        ids::GROUP_CHAT,
        ids::ALICE.open_id,
        "x",
        2,
        None,
        None,
    );
    // you write to the bot: the long connection got it already
    add(
        &h,
        "om_f8tobot",
        ME_BOT_CHAT,
        ids::OWNER.open_id,
        "/bot 帮我查一下",
        3,
        None,
        None,
    );
    bot_received(
        &h,
        ME_BOT_CHAT,
        SpaceKind::Dm,
        "om_f8tobot",
        ids::OWNER.open_id,
    )
    .await;
    let before = h.fake.requests_to(Endpoint::ListMessages).len();
    let r = p.round().await;
    assert_eq!(r.skipped_bot, 1, "{r:?}");
    assert_eq!(r.by_bot, 1, "{r:?}");
    assert_eq!(r.new_messages, 0, "{r:?}");
    assert!(self_messages(&h).is_empty());
    let reads = h.fake.requests_to(Endpoint::ListMessages);
    assert!(
        reads[before..]
            .iter()
            .all(|x| !x.query.contains(ids::GROUP_CHAT)),
        "the bot's group is not read"
    );
    assert_eq!(
        count(
            &h,
            "SELECT count(*) FROM chan_messages WHERE external_id = ?",
            "om_f8tobot"
        ),
        1
    );
}

/// A bot without `im:message.group_msg` only hears the messages that @ it: sitting in a group does not
/// cover it, so the group is still read for the rest, and what the bot stored is not taken twice.
#[tokio::test]
async fn a_group_the_bot_only_hears_mentions_in_is_still_read() {
    let h = setup().await;
    h.env
        .modules
        .store
        .with_conn(|c| {
            let n = c.execute(
                "UPDATE connections SET config = json_set(config, '$.permissions', json('[{\"scope\":\"im:message.group_msg\",\"status\":\"missing\"}]')) WHERE id = ?",
                [&h.bot_con],
            )?;
            assert_eq!(n, 1);
            Ok(())
        })
        .unwrap();
    let mut p = poller(&h);
    p.round().await;
    // someone @-mentions the bot: the long connection gets that one
    bot_received(
        &h,
        ids::GROUP_CHAT,
        SpaceKind::Group,
        "om_f8at",
        ids::ALICE.open_id,
    )
    .await;
    add(
        &h,
        "om_f8at",
        ids::GROUP_CHAT,
        ids::ALICE.open_id,
        "@bot 看看",
        2,
        None,
        None,
    );
    // and writes on without a mention: only the poller can see it
    add(
        &h,
        "om_f8plain",
        ids::GROUP_CHAT,
        ids::ALICE.open_id,
        "顺便说一下",
        3,
        None,
        None,
    );
    let r = p.round().await;
    assert_eq!(r.skipped_bot, 0, "{r:?}");
    assert_eq!((r.by_bot, r.new_messages), (1, 1), "{r:?}");
    for id in ["om_f8at", "om_f8plain"] {
        assert_eq!(
            count(
                &h,
                "SELECT count(*) FROM chan_messages WHERE external_id = ?",
                id
            ),
            1,
            "{id}"
        );
    }
}

#[tokio::test]
async fn a_rate_limit_ends_the_round_and_carries_the_rest_over() {
    let h = setup().await;
    set_setting(&h, "quiet", "关闭");
    let mut p = poller(&h);
    p.round().await;
    add(
        &h,
        "om_f8late",
        ids::USER_P2P_CHAT,
        ids::BOB.open_id,
        "还在吗",
        4,
        None,
        None,
    );
    h.fake.set_rate_limit_reset(90);
    // the first message read of the round is limited
    h.fake.rate_limit_next(Endpoint::ListMessages, 1);
    let r = p.round().await;
    assert_eq!(r.rate_limited, Some(Duration::from_secs(90)), "{r:?}");
    assert_eq!(r.chats, 0);
    assert_eq!(r.carried, MY_CHATS);
    assert_eq!(p.next_delay(&r, h.env.clock.now()), Duration::from_secs(90));
    assert!(self_messages(&h).is_empty());
    // next round: the carried chats first, Bob's message comes in
    let r = p.round().await;
    assert_eq!(r.rate_limited, None, "{r:?}");
    assert_eq!((r.chats, r.carried, r.new_dms), (MY_CHATS, 0, 1), "{r:?}");
    assert_eq!(self_messages(&h).len(), 1);

    // a short reset still waits one interval
    set_setting(&h, "quiet", "关闭");
    h.fake.set_rate_limit_reset(1);
    h.fake.rate_limit_next(Endpoint::ListChats, 1);
    let r = p.round().await;
    assert!(r.rate_limited.is_some());
    h.env.clock.advance(self_poll::BURST_FOR_MS + 1);
    assert_eq!(p.next_delay(&r, h.env.clock.now()), DEFAULT_INTERVAL);
}

fn set_setting(h: &H, key: &str, value: &str) {
    let (k, v) = (key.to_string(), json!(value).to_string());
    h.env
        .store()
        .with_conn(move |c| {
            c.execute(
                "INSERT INTO settings (workspace_id, key, value, updated) VALUES (?1, ?2, ?3, 1)
                 ON CONFLICT (workspace_id, key) DO UPDATE SET value = excluded.value",
                rusqlite::params![WS, k, v],
            )?;
            Ok(())
        })
        .expect("setting");
}

#[tokio::test]
async fn quiet_hours_slow_the_pace() {
    let h = setup().await;
    let p = poller(&h);
    // 02:00 and 12:00 in Asia/Taipei (UTC+8), the default zone; default quiet hours 23:00 – 08:00
    let night = 1_761_933_600_000; // 2025-11-01 02:00 +08:00
    let noon = night + 10 * 3_600_000;
    let r = Default::default();
    assert_eq!(p.next_delay(&r, night), QUIET_INTERVAL);
    assert_eq!(p.next_delay(&r, noon), DEFAULT_INTERVAL);
    set_setting(&h, "quiet", "22:00 – 09:00");
    assert_eq!(
        p.next_delay(&r, night + 6 * 3_600_000 + 30 * 60_000),
        QUIET_INTERVAL,
        "08:30"
    );
    set_setting(&h, "quiet", "关闭");
    assert_eq!(p.next_delay(&r, night), DEFAULT_INTERVAL);
    // the zone setting moves them: 02:00 Taipei is 18:00 UTC the day before
    set_setting(&h, "quiet", "23:00 – 08:00");
    set_setting(&h, "tz", "UTC");
    assert_eq!(p.next_delay(&r, night), DEFAULT_INTERVAL);
    assert_eq!(
        p.next_delay(&r, night + 6 * 3_600_000),
        QUIET_INTERVAL,
        "00:00 UTC"
    );
}

#[tokio::test]
async fn topic_replies_are_read_through_their_thread() {
    let h = setup().await;
    let mut p = poller(&h);
    p.round().await;
    // a new topic in a group without the bot, then a reply inside it
    add(
        &h,
        "om_f8root",
        ids::NO_BOT_GROUP,
        ids::ALICE.open_id,
        "下周换哪本？",
        1,
        Some("omt_f8"),
        None,
    );
    let r = p.round().await;
    assert_eq!(r.new_messages, 1, "{r:?}");
    assert_eq!(
        r.threads, 1,
        "the new thread is read in the same round: {r:?}"
    );
    add(
        &h,
        "om_f8reply",
        ids::NO_BOT_GROUP,
        ids::OWNER.open_id,
        "《三体》",
        1,
        Some("omt_f8"),
        Some("om_f8root"),
    );
    let r = p.round().await;
    assert_eq!(r.threads, 1, "{r:?}");
    assert_eq!(r.new_messages, 1, "{r:?}");
    let rows = self_messages(&h);
    assert_eq!(
        rows.iter().map(|r| str_of(&r[0])).collect::<Vec<_>>(),
        ["om_f8root", "om_f8reply"]
    );
    assert!(
        rows.iter().all(|r| r[1] == json!(ids::NO_BOT_GROUP)),
        "the thread's messages are the chat's"
    );
    let thr = q(
        &h.env,
        "SELECT thread_key, reply_to FROM chan_messages WHERE external_id = ?",
        "om_f8reply",
    );
    assert_eq!(thr[0], vec![json!("omt_f8"), json!("om_f8root")]);
}

#[tokio::test]
async fn an_unreadable_group_is_recorded_and_left_alone() {
    let h = setup().await;
    h.fake.world(|w| w.user_read_requires_bot = true);
    let mut p = poller(&h);
    let r = p.round().await;
    assert_eq!(r.failed, 1, "the group without the bot: {r:?}");
    let k = h
        .env
        .store()
        .reader(|c| self_poll::cursor(c, &h.self_con, ids::NO_BOT_GROUP))
        .expect("read")
        .expect("cursor");
    assert!(k.error.is_some() && k.cursor.is_none(), "{k:?}");
    let before = h.fake.requests_to(Endpoint::ListMessages).len();
    let r = p.round().await;
    assert_eq!(r.failed, 0, "not tried again within the hour");
    assert_eq!(
        h.fake.requests_to(Endpoint::ListMessages).len() - before,
        MY_CHATS - 1
    );
    // once readable and after an hour: a first read, still no backfill
    h.fake.world(|w| w.user_read_requires_bot = false);
    h.env.clock.advance(self_poll::ERROR_RETRY_MS);
    let r = p.round().await;
    assert_eq!((r.failed, r.first_seen, r.new_messages), (0, 1, 0), "{r:?}");
}

#[tokio::test]
async fn pollers_start_for_connections_with_receive_and_stop() {
    let h = setup().await;
    let all = SelfPollers::new(h.ch.clone(), h.user.clone());
    assert_eq!(all.start_all().await, 1);
    assert_eq!(all.start_all().await, 0, "already running");
    // the first round runs at once
    let t = std::time::Instant::now();
    while count(
        &h,
        "SELECT count(*) FROM chan_cursors WHERE connection_id = ?",
        &h.self_con,
    ) < MY_CHATS as i64
    {
        assert!(t.elapsed() < Duration::from_secs(10), "no first round");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert_eq!(all.running().await, vec![h.self_con.clone()]);
    assert!(all.stop(&h.self_con).await);
    assert!(all.running().await.is_empty());
    assert!(!all.stop(&h.self_con).await);
}
