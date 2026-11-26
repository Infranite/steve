//! C1 acceptance on the [`FakeChannel`]: a pushed message creates its
//! space, identifies the sender and reaches `ext_events`; a new group opens a decision; the bot's
//! own messages are not processed; purged text is NULL; the message is on the contact timeline.
//! Plus the `/spaces` routes, duplicates, `im.*` types, other bots, and the loop guard.

#[path = "support/digest_env.rs"]
mod digest_env;
use digest_env::TestEnv;
use std::sync::Arc;
use steve_store::testkit::channels::inspect;
use steve_store::testkit::channels::seed;

use axum::Router;
use serde_json::{Value, json};
use steve_channels::fake::channel::FakeChannel;
use steve_channels::inbound::{LABEL_BOT, LABEL_OWN};
use steve_channels::messages::{self, MessageTimeline};
use steve_channels::{Channels, Inbound, Next, SpaceKind, api};
use steve_flow::testkit::FakeCore;
use steve_flow::{FlowSetup, Ingested, LinearExecutor};
use steve_modules::accounts::connections::{self, Connections, ConnectionsConfig};
use steve_modules::testkit::{ME, OTHER, WS, WS2, call, caller};
use steve_modules::vault::Vault;
use steve_modules::vault::keystore::FileKeyStore;
use tempfile::TempDir;

struct H {
    env: TestEnv,
    _keys: TempDir,
    fake: Arc<FakeChannel>,
    ch: Channels,
    api: Router,
    con: String,
    bot_acc: String,
}

fn setup() -> H {
    let core = Arc::new(FakeCore::default());
    let flow_setup = FlowSetup::new(core.clone());
    let env = TestEnv::new(|r| {
        flow_setup.register(r);
        connections::register(r);
    });
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
        .build();
    let mut ports = steve_modules::accounts::ports();
    ports.timelines.push(MessageTimeline::arc());
    let api = api::routes(ch.clone())
        .merge(steve_modules::contacts::routes(ports))
        .with_state(env.modules.clone());
    let (con, bot_acc) =
        FakeChannel::seed_connection(env.store(), &caller(WS, ME), None).expect("connection");
    fake.space(
        "G1",
        "运维群",
        SpaceKind::Group,
        false,
        &[("ualice", "王小丽"), ("ubob", "张伟")],
    );
    fake.space("D1", "", SpaceKind::Dm, false, &[("ualice", "王小丽")]);
    fake.space(
        "X1",
        "Acme 客户群",
        SpaceKind::Group,
        true,
        &[("ualice", "王小丽"), ("uext", "Sarah")],
    );
    H {
        env,
        _keys: keys,
        fake,
        ch,
        api,
        con,
        bot_acc,
    }
}

async fn push(h: &H, m: steve_channels::InboundMessage) -> Inbound {
    h.ch.on_inbound(WS, &h.con, m).await.expect("inbound")
}

#[tokio::test(flavor = "multi_thread")]
async fn pushed_message_creates_space_identifies_sender_and_records_event() {
    let h = setup();
    let r = push(
        &h,
        FakeChannel::message("G1", "m1", "ualice", "昨晚的告警是怎么回事"),
    )
    .await;
    assert!(r.space_created && !r.duplicate);
    assert_eq!(r.type_key, "im.message");
    assert_eq!(r.next, Next::Handle);

    // the space: name and kind from the adapter, settings not yet configured, default disclosure
    let s = inspect::spaces::space_name_kind_member_count_has_external(h.env.store(), &r.space_id)
        .unwrap();
    assert_eq!(
        s,
        vec![
            json!("运维群"),
            json!("group"),
            json!(2),
            json!(0),
            json!(0),
            json!("conclusion"),
            json!("slack")
        ]
    );

    // the sender: identified with the name from the members list, a member of the space
    let acc = r.sender_account_id.clone().expect("sender account");
    let party = r.sender_party_id.clone().expect("sender party");
    let a = inspect::people::account_handle_display_name_party_id(h.env.store(), &acc).unwrap();
    assert_eq!(a, vec![json!("ualice"), json!("王小丽"), json!(party)]);
    assert_eq!(
        inspect::people::party_name_contact_kind(h.env.store(), &party).unwrap(),
        vec![json!("王小丽"), json!("unconfirmed")]
    );
    assert_eq!(
        inspect::spaces::count_space_members_by_space_count(h.env.store(), &r.space_id).unwrap(),
        3,
        "bot + two members"
    );
    assert!(!r.sender_is_ours);

    // the message and its event
    let m = inspect::messages::message_direction_text_status_sender_party_id(
        h.env.store(),
        &r.message_id,
    )
    .unwrap();
    let Some(Ingested::Stored { id: evt, .. }) = r.event.clone() else {
        panic!("event stored: {:?}", r.event);
    };
    assert_eq!(
        m,
        vec![
            json!("in"),
            json!("昨晚的告警是怎么回事"),
            json!("received"),
            json!(party),
            json!(evt),
            json!("m1")
        ]
    );
    let e = inspect::signals::signal_source_kind_space_id_message_id(h.env.store(), &evt).unwrap();
    assert_eq!(
        e,
        vec![
            json!("im"),
            json!("im.message"),
            json!(r.space_id),
            json!(r.message_id),
            json!(party),
            json!(h.con),
            json!("王小丽：昨晚的告警是怎么回事"),
            json!(steve_channels::rules::LABEL_UNADDRESSED),
        ]
    );

    // a second message in the same space: no new space, no new decision, the members not refetched
    let calls = *h.fake.member_calls.lock().unwrap();
    let r2 = push(&h, FakeChannel::message("G1", "m2", "ualice", "还有一件事")).await;
    assert!(!r2.space_created && r2.decision_id.is_none());
    assert_eq!(r2.space_id, r.space_id);
    assert_eq!(r2.sender_party_id, r.sender_party_id);
    assert_eq!(*h.fake.member_calls.lock().unwrap(), calls);
    assert_eq!(inspect::spaces::count_spaces(h.env.store()).unwrap(), 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn new_group_defaults_to_silent_record_with_no_inbox_decision() {
    let h = setup();
    let r = push(&h, FakeChannel::message("G1", "m1", "ualice", "hi")).await;
    assert!(
        r.decision_id.is_none(),
        "new bot groups must not open a 4-choice inbox ask card"
    );
    let set = h
        .env
        .store()
        .read(|db| steve_channels::spaces::settings(db, &r.space_id))
        .unwrap();
    assert!(set.muted, "new bot groups default to silent record");
    assert!(!set.configured);
    push(&h, FakeChannel::message("G1", "m2", "ubob", "hi")).await;

    // One-click rule change via PATCH /spaces/:id enables default rules and marks configured.
    let (status, sp) = call(
        &h.api,
        Some(&caller(WS, ME)),
        "PATCH",
        &format!("/spaces/{}", r.space_id),
        Some(&json!({"muted": false})),
    )
    .await;
    assert_eq!(status, 200, "{sp}");
    assert_eq!(sp["muted"], false);
    assert_eq!(sp["configured"], true);

    // a DM: no decision; named after the other person; DMs stay active (not muted) and default to `reasoning`
    let dm = push(&h, FakeChannel::message("D1", "d1", "ualice", "私下问一句")).await;
    assert!(dm.space_created && dm.decision_id.is_none());
    assert_eq!(dm.type_key, "im.dm");
    let dm_set = h
        .env
        .store()
        .read(|db| steve_channels::spaces::settings(db, &dm.space_id))
        .unwrap();
    assert!(!dm_set.muted, "DMs are not muted by default");
    assert_eq!(
        inspect::spaces::space_name_kind_disclosure(h.env.store(), &dm.space_id).unwrap(),
        vec![json!("王小丽"), json!("dm"), json!("reasoning")]
    );
    // a customer group: `reasoning`, has_external, default silent record, no inbox decision
    let x = push(&h, FakeChannel::message("X1", "x1", "uext", "方案呢")).await;
    assert!(x.decision_id.is_none());
    assert_eq!(
        inspect::spaces::space_has_external_disclosure(h.env.store(), &x.space_id).unwrap(),
        vec![json!(1), json!("reasoning")]
    );
    let x_set = h
        .env
        .store()
        .read(|db| steve_channels::spaces::settings(db, &x.space_id))
        .unwrap();
    assert!(x_set.muted);
    assert_eq!(
        inspect::decisions::count_decisions_by_space(h.env.store()).unwrap(),
        0
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn space_info_failure_falls_back_to_the_payload() {
    let h = setup();
    *h.fake.fail_info.lock().unwrap() = true;
    let mut m = FakeChannel::message("G9", "m1", "ucarol", "hello");
    m.space_name = Some("临时群".into());
    let r = push(&h, m).await;
    assert!(r.space_created);
    assert_eq!(
        inspect::spaces::space_name_kind(h.env.store(), &r.space_id).unwrap(),
        vec![json!("临时群"), json!("group")]
    );
    assert!(r.sender_account_id.is_some(), "still identified by handle");
}

#[tokio::test(flavor = "multi_thread")]
async fn own_bot_messages_are_not_processed_other_bots_only_recorded() {
    let h = setup();
    h.env.clock.advance_to(steve_store::now());
    let message = |space: &str, id: &str, from: &str, text: &str| {
        let mut m = FakeChannel::message(space, id, from, text);
        m.created = h.env.clock.advance(1);
        m
    };
    push(&h, message("G1", "m0", "ualice", "hi")).await;

    // the connection's own account
    let own = push(
        &h,
        message("G1", "m1", FakeChannel::BOT, "我是机器人自己发的"),
    )
    .await;
    assert_eq!(own.next, Next::Own);
    assert_eq!(own.sender_account_id.as_deref(), Some(h.bot_acc.as_str()));
    assert!(own.sender_is_ours);
    let evt = own
        .event
        .as_ref()
        .and_then(|e| e.id())
        .expect("event")
        .to_string();
    assert_eq!(
        inspect::signals::signal_outcome_outcome_label_single_row(h.env.store(), &evt).unwrap(),
        vec![json!("recorded"), json!(LABEL_OWN)]
    );

    // another bot: only recorded …
    let mut m = message("G1", "m2", "umonitor", "【监控】CPU 90%");
    m.sender.is_bot = true;
    let other = push(&h, m).await;
    assert_eq!(other.next, Next::Record(LABEL_BOT));
    let evt = other
        .event
        .as_ref()
        .and_then(|e| e.id())
        .expect("event")
        .to_string();
    assert_eq!(
        inspect::signals::signal_outcome_label_single_row(h.env.store(), &evt).unwrap(),
        vec![json!(LABEL_BOT)]
    );
    assert_eq!(
        inspect::spaces::space_member_role_by_space_and_account(
            h.env.store(),
            &other.space_id,
            other.sender_account_id.as_deref().unwrap()
        )
        .unwrap(),
        vec![json!("bot")]
    );

    // … unless the space collaborates
    let (s, v) = call(
        &h.api,
        Some(&caller(WS, ME)),
        "PATCH",
        &format!("/spaces/{}", other.space_id),
        Some(&json!({"collab": true})),
    )
    .await;
    assert_eq!(s, 200, "{v}");
    let mut m = message("G1", "m3", "umonitor", "【监控】恢复");
    m.sender.is_bot = true;
    assert_eq!(push(&h, m).await.next, Next::Handle);
    // … but between two people's messages a bot gets one answer: once Steve answered it, the bot's next
    // messages (a reply to Steve or a new @) are only recorded, so two bots never answer each other forever
    // a draft waiting for approval is no answer yet
    h.env.clock.advance(5);
    let out = |id: &str, status: &str| {
        let at = h.env.modules.clock.now();
        seed::messages::seed_outbound_state(h.env.store(), id, &other.space_id, status, at)
            .unwrap();
    };
    out("draft1", "draft");
    out("pending1", "pending");
    let mut m = message("G1", "m3b", "umonitor", "【监控】仍在恢复");
    m.sender.is_bot = true;
    assert_eq!(
        push(&h, m).await.next,
        Next::Handle,
        "a draft, or a message not sent yet, is no answer"
    );
    // a message Steve sent (here a flow's, no reply reference) is
    h.env.clock.advance(5);
    out("steve1", "sent");
    let answered = Next::Record(steve_channels::inbound::LABEL_BOT_ANSWERED);
    let mut m = message("G1", "m4", "umonitor", "收到");
    m.sender.is_bot = true;
    m.reply_to = Some("fm_steve1".into());
    assert_eq!(push(&h, m).await.next, answered);
    let mut m = message("G1", "m5", "umonitor", "【监控】又告警了");
    m.sender.is_bot = true;
    assert_eq!(
        push(&h, m).await.next,
        answered,
        "a new message, no reply: still one answer"
    );
    // a person speaks: the bot may be answered again
    h.env.clock.advance(5);
    let p = push(&h, message("G1", "m6", "ualice", "看下")).await;
    assert_eq!(p.next, Next::Handle);
    let mut m = message("G1", "m7", "umonitor", "【监控】恢复");
    m.sender.is_bot = true;
    assert_eq!(push(&h, m).await.next, Next::Handle);
}

#[tokio::test(flavor = "multi_thread")]
async fn redelivery_is_a_duplicate_and_writes_nothing() {
    let h = setup();
    let m = FakeChannel::message("G1", "m1", "ualice", "一次就好");
    let a = push(&h, m.clone()).await;
    let mut again = m.clone();
    again.thread_key = Some("t1".into());
    let b = push(&h, again).await;
    assert!(b.duplicate && b.event.is_none());
    assert_eq!(b.message_id, a.message_id);
    assert_eq!(
        inspect::messages::count_chan_messages(h.env.store()).unwrap(),
        1
    );
    assert_eq!(
        inspect::signals::count_ext_events(h.env.store()).unwrap(),
        1
    );
    // the second path filled what the first lacked
    assert_eq!(
        inspect::messages::message_thread_key_single_row(h.env.store(), &a.message_id).unwrap(),
        vec![json!("t1")]
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn types_mention_reply_and_mentions_column() {
    let h = setup();
    let first = push(&h, FakeChannel::message("G1", "m0", "ualice", "hi")).await;
    let mut m = FakeChannel::message("G1", "m1", "ubob", "@测试机器人 帮我看看 @王小丽");
    m.mentions = vec![
        steve_channels::Mention {
            handle: None,
            name: "测试机器人".into(),
            is_connection: true,
        },
        steve_channels::Mention {
            handle: Some(FakeChannel::handle("ualice")),
            name: "王小丽".into(),
            is_connection: false,
        },
    ];
    let r = push(&h, m).await;
    assert_eq!(r.type_key, "im.mention");
    let mentions: Value = serde_json::from_str(
        inspect::messages::message_mentions(h.env.store(), &r.message_id).unwrap()[0]
            .as_str()
            .unwrap(),
    )
    .unwrap();
    assert_eq!(
        mentions,
        json!([{"account_id": h.bot_acc, "name": "测试机器人"}, {"account_id": first.sender_account_id, "name": "王小丽"}])
    );

    // a reply to a message Steve sent
    seed::messages::seed_persona_reply(h.env.store(), &r.space_id, &h.bot_acc).unwrap();
    let mut m = FakeChannel::message("G1", "m2", "ualice", "谢谢");
    m.reply_to = Some("fm_1".into());
    let r = push(&h, m).await;
    assert_eq!(r.type_key, "im.reply");
    let evt = r.event.as_ref().and_then(|e| e.id()).unwrap().to_string();
    assert_eq!(
        inspect::signals::signal_kind(h.env.store(), &evt).unwrap(),
        vec![json!("im.reply")]
    );
}

/// Many replies in a short time never silence a space by themselves (bots' messages are only recorded,
/// and Steve answers only when addressed, so there is no loop to break).
#[tokio::test(flavor = "multi_thread")]
async fn many_replies_do_not_silence_the_space() {
    let h = setup();
    let r = push(&h, FakeChannel::message("G1", "m0", "ualice", "hi")).await;
    // Enable the new group with one click first (new bot groups default to silent record; #213).
    let (status, _) = call(
        &h.api,
        Some(&caller(WS, ME)),
        "PATCH",
        &format!("/spaces/{}", r.space_id),
        Some(&json!({"muted": false})),
    )
    .await;
    assert_eq!(status, 200);
    let now = h.ch.now();
    seed::messages::seed_recent_outbound(h.env.store(), &r.space_id, now).unwrap();
    push(&h, FakeChannel::message("G1", "m1", "ualice", "再来")).await;
    assert_eq!(
        inspect::spaces::space_coalescemax_by_space(h.env.store(), &r.space_id).unwrap(),
        vec![json!(0)]
    );
    assert_eq!(
        inspect::decisions::decision_count_by_subject_and_fail(h.env.store(), &r.space_id).unwrap(),
        vec![json!(0)]
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn purged_text_is_null_everywhere() {
    let h = setup();
    let r = push(
        &h,
        FakeChannel::message("G1", "m1", "ualice", "密码是什么来着"),
    )
    .await;
    // 7 days (the default keepRaw) and a bit later
    let later = h.env.clock.advance(8 * 24 * 3_600_000);
    assert_eq!(messages::sweep(&h.env.modules, later).unwrap(), 1);
    assert_eq!(
        inspect::messages::message_text_purged(h.env.store(), &r.message_id).unwrap(),
        vec![Value::Null, json!(later)]
    );
    assert_eq!(messages::sweep(&h.env.modules, later).unwrap(), 0, "once");
    let (s, v) = call(
        &h.api,
        Some(&caller(WS, ME)),
        "GET",
        &format!("/spaces/{}/messages", r.space_id),
        None,
    )
    .await;
    assert_eq!(s, 200, "{v}");
    assert_eq!(v[0]["text"], Value::Null);
    assert_eq!(v[0]["purged"], json!(later));

    // keepRaw = 30: a 8-day-old message stays
    let r2 = push(&h, FakeChannel::message("G1", "m2", "ualice", "新的")).await;
    seed::spaces::keep_raw_for_30_days(h.env.store()).unwrap();
    assert_eq!(
        messages::sweep(&h.env.modules, later + 8 * 24 * 3_600_000).unwrap(),
        0
    );
    assert_eq!(
        inspect::messages::message_text_single_row(h.env.store(), &r2.message_id).unwrap(),
        vec![json!("新的")]
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn message_shows_on_the_contact_timeline() {
    let h = setup();
    let r = push(
        &h,
        FakeChannel::message("G1", "m1", "ualice", "下周三上线可以吗"),
    )
    .await;
    let party = r.sender_party_id.clone().unwrap();
    let (s, v) = call(
        &h.api,
        Some(&caller(WS, ME)),
        "GET",
        &format!("/parties/{party}/timeline"),
        None,
    )
    .await;
    assert_eq!(s, 200, "{v}");
    let line = v
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["kind"] == "message")
        .unwrap_or_else(|| panic!("message line in {v}"));
    assert_eq!(line["text"], "在「运维群」说：下周三上线可以吗");
    assert_eq!(line["href"], format!("#/channels/space/{}", r.space_id));

    // after the retention period the line stays, without the text
    let later = h.env.clock.advance(8 * 24 * 3_600_000);
    messages::sweep(&h.env.modules, later).unwrap();
    let (_, v) = call(
        &h.api,
        Some(&caller(WS, ME)),
        "GET",
        &format!("/parties/{party}/timeline"),
        None,
    )
    .await;
    let line = v
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["kind"] == "message")
        .unwrap();
    assert_eq!(line["text"], "在「运维群」发了一条消息（原文已过保留期限）");
}

#[tokio::test(flavor = "multi_thread")]
async fn single_user_keeps_disconnected_history_visible_after_account_disappears() {
    let h = setup();
    let r = push(
        &h,
        FakeChannel::message("G1", "m1", "ualice", "下周三上线可以吗"),
    )
    .await;
    let party = r.sender_party_id.clone().unwrap();
    let lines = |v: &Value| {
        v.as_array()
            .map(|a| a.iter().filter(|e| e["kind"] == "message").count())
            .unwrap_or(0)
    };
    let timeline = |who: &'static str| {
        let (api, path) = (h.api.clone(), format!("/parties/{party}/timeline"));
        async move { call(&api, Some(&caller(WS, who)), "GET", &path, None).await }
    };
    // a bot account's group: everyone sees it, connected or disconnected
    assert_eq!(lines(&timeline(OTHER).await.1), 1);
    seed::spaces::detach_fixture_space(h.env.store(), &r.space_id).unwrap();
    assert_eq!(lines(&timeline(OTHER).await.1), 1);
    let path = format!("/spaces/{}", r.space_id);
    let (st, _) = call(&h.api, Some(&caller(WS, OTHER)), "GET", &path, None).await;
    assert_eq!(st, 200);

    // Single-user reads retain disconnected history even when the account no longer resolves.
    seed::spaces::assign_missing_fixture_account(h.env.store(), &r.space_id).unwrap();
    let (st, _) = call(&h.api, Some(&caller(WS, OTHER)), "GET", &path, None).await;
    assert_eq!(st, 200);
    let (_, v) = call(&h.api, Some(&caller(WS, OTHER)), "GET", "/spaces", None).await;
    assert!(
        v.as_array()
            .unwrap()
            .iter()
            .any(|x| x["id"] == r.space_id.as_str()),
        "{v}"
    );
    assert_eq!(lines(&timeline(OTHER).await.1), 1);
    let (st, _) = call(&h.api, Some(&caller(WS, ME)), "GET", &path, None).await;
    assert_eq!(st, 200);
    assert_eq!(lines(&timeline(ME).await.1), 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn space_routes_list_detail_patch_and_404s() {
    let h = setup();
    let me = caller(WS, ME);
    let r = push(&h, FakeChannel::message("G1", "m1", "ualice", "hi")).await;
    push(&h, FakeChannel::message("D1", "d1", "ualice", "私聊")).await;

    let (s, v) = call(&h.api, Some(&me), "GET", "/spaces", None).await;
    assert_eq!(s, 200, "{v}");
    assert_eq!(v.as_array().unwrap().len(), 2);
    let (_, v) = call(&h.api, Some(&me), "GET", "/spaces?q=运维", None).await;
    assert_eq!(v.as_array().unwrap().len(), 1);
    assert_eq!(v[0]["name"], "运维群");
    assert_eq!(
        v[0]["effective_rules"],
        json!([{"when": {"kind": "mention_or_dm"}, "act": "reply"}])
    );
    assert_eq!(v[0]["rules"], Value::Null);
    assert_eq!(v[0]["configured"], false);
    assert_eq!(v[0]["has_external"], false);
    let (_, v) = call(
        &h.api,
        Some(&me),
        "GET",
        &format!("/spaces?connection={}", "con_nope"),
        None,
    )
    .await;
    assert_eq!(v, json!([]));

    let (s, v) = call(
        &h.api,
        Some(&me),
        "GET",
        &format!("/spaces/{}", r.space_id),
        None,
    )
    .await;
    assert_eq!(s, 200, "{v}");
    assert_eq!(v["recent"][0]["text"], "hi");
    assert_eq!(v["recent"][0]["sender"]["name"], "王小丽");
    assert_eq!(v["recent"][0]["basis"], Value::Null);

    let body = json!({"duty": "负责告警排查", "muted": true, "disclosure": "reasoning", "memory_scope": "persona",
                      "item_defaults": {"tags": [], "project_id": null, "bot_id": null}});
    let (s, v) = call(
        &h.api,
        Some(&me),
        "PATCH",
        &format!("/spaces/{}", r.space_id),
        Some(&body),
    )
    .await;
    assert_eq!(s, 200, "{v}");
    assert_eq!(
        (
            v["duty"].clone(),
            v["muted"].clone(),
            v["disclosure"].clone(),
            v["configured"].clone(),
            v["revision"].clone()
        ),
        (
            json!("负责告警排查"),
            json!(true),
            json!("reasoning"),
            json!(true),
            json!(2)
        )
    );
    assert_eq!(
        inspect::people::count_revisions_by_object(h.env.store(), &r.space_id).unwrap(),
        1
    );
    assert_eq!(
        inspect::audit::count_audit_by_space_patch(h.env.store()).unwrap(),
        1
    );

    // validation and references
    let (s, v) = call(
        &h.api,
        Some(&me),
        "PATCH",
        &format!("/spaces/{}", r.space_id),
        Some(&json!({"disclosure": "secret"})),
    )
    .await;
    assert_eq!(
        (s, v["error"].clone()),
        (400, json!("参数不对：disclosure"))
    );
    let (s, v) = call(
        &h.api,
        Some(&me),
        "PATCH",
        &format!("/spaces/{}", r.space_id),
        Some(&json!({"item_defaults": {"bot_id": "nobot"}})),
    )
    .await;
    assert_eq!((s, v["error"].clone()), (404, json!("没有这个 Bot")));
    let (s, _) = call(
        &h.api,
        Some(&me),
        "GET",
        &format!("/spaces/{}/messages?limit=101", r.space_id),
        None,
    )
    .await;
    assert_eq!(s, 400);

    // another workspace sees nothing
    let w2 = caller(WS2, ME);
    let (s, v) = call(
        &h.api,
        Some(&w2),
        "GET",
        &format!("/spaces/{}", r.space_id),
        None,
    )
    .await;
    assert_eq!((s, v["error"].clone()), (404, json!("没有这个空间")));
    let (_, v) = call(&h.api, Some(&w2), "GET", "/spaces", None).await;
    assert_eq!(v, json!([]));
}

#[tokio::test(flavor = "multi_thread")]
async fn single_user_reads_other_accounts_spaces_with_fixed_bindings() {
    let h = setup();
    let (mine, _) = FakeChannel::seed_connection(h.env.store(), &caller(WS, OTHER), Some("ume"))
        .expect("personal");
    let r =
        h.ch.on_inbound(
            WS,
            &mine,
            FakeChannel::message("G1", "p1", "ualice", "只给我看"),
        )
        .await
        .unwrap();
    // a self connection opens no “新群” decision and gets the self default rules
    assert!(r.decision_id.is_none());
    assert_eq!(
        inspect::connections::channel_settings_mode_by_connection(h.env.store(), &mine).unwrap(),
        vec![json!("self")]
    );
    let (s, _) = call(
        &h.api,
        Some(&caller(WS, ME)),
        "GET",
        &format!("/spaces/{}", r.space_id),
        None,
    )
    .await;
    assert_eq!(s, 200);
    let (s, _) = call(
        &h.api,
        Some(&caller(WS, OTHER)),
        "GET",
        &format!("/spaces/{}", r.space_id),
        None,
    )
    .await;
    assert_eq!(s, 200);
    h.env
        .store()
        .read(|db| -> steve_modules::ApiResult<()> {
            let viewer = steve_store::base::Viewer::bare(WS, ME);
            let context = steve_channels::compose::viewer(db, WS, &mine)?;
            assert_eq!(
                context.user, OTHER,
                "reply identity still follows the connected account"
            );
            use steve_store::channel_spaces::rows;
            assert!(rows::get_in_workspace(db, &viewer.ws, &r.space_id)?.is_some());
            assert!(rows::get_in_workspace(db, WS2, &r.space_id)?.is_none());
            assert!(rows::get_in_workspace(db, &viewer.ws, "missing")?.is_none());
            assert!(rows::visible_ids(db, &viewer.ws)?.contains(&r.space_id));
            Ok(())
        })
        .unwrap();
    // the event is private too (S1: a connection of someone's “我”)
    let evt = r.event.as_ref().and_then(|e| e.id()).unwrap().to_string();
    assert_eq!(
        inspect::signals::signal_visibility(h.env.store(), &evt).unwrap(),
        vec![json!("private")]
    );
    // you writing in the group from your own account: not processed
    let own =
        h.ch.on_inbound(
            WS,
            &mine,
            FakeChannel::message("G1", "p2", "ume", "/bot 帮我回一下"),
        )
        .await
        .unwrap();
    assert_eq!(own.next, Next::Own);
}

/// A message says what kind of chat it came from (Feishu's chat_type on every event): that wins over
/// what its space was stored as, so a private chat is never mistaken for a group by the gate.
#[tokio::test(flavor = "multi_thread")]
async fn the_messages_own_chat_type_decides_a_private_chat() {
    let h = setup();
    let first = push(&h, FakeChannel::message("G1", "k1", "ualice", "hi")).await;
    assert!(
        !first.facts.as_ref().expect("facts").dm,
        "a group by default"
    );
    let mut m = FakeChannel::message("G1", "k2", "ualice", "在吗");
    m.space_kind = Some(steve_channels::SpaceKind::Dm);
    let i = push(&h, m).await;
    assert!(i.facts.as_ref().expect("facts").dm);
    assert_eq!(i.type_key, "im.dm", "the event says so too");
    // and the other way round: a group message in a space stored as a private chat is a group message
    let mut m = FakeChannel::message("D9", "k3", "ualice", "在吗");
    m.space_kind = Some(steve_channels::SpaceKind::Dm);
    push(&h, m).await;
    let mut m = FakeChannel::message("D9", "k4", "ualice", "大家好");
    m.space_kind = Some(steve_channels::SpaceKind::Group);
    let i = push(&h, m).await;
    assert!(!i.facts.as_ref().expect("facts").dm);
    assert_eq!(i.type_key, "im.message");
}

/// A sender the adapter knows by its own id and an email: the contact already holding the email is
/// the sender (no new unconfirmed contact), and the id is attached to it; a stranger's contact gets
/// both.
#[tokio::test(flavor = "multi_thread")]
async fn a_sender_known_by_an_alias_lands_on_the_contact_holding_it() {
    use steve_channels::HandleRef;
    use steve_channels::channel::Sender;
    use steve_modules::accounts::{Hints, identify};
    let h = setup();
    // mail saw Alice first
    let alice = h
        .env
        .store()
        .write(|tx| {
            identify(
                tx,
                WS,
                ME,
                "mail",
                "alice@example.com",
                &Hints {
                    display_name: Some("王小丽".into()),
                    ..Hints::default()
                },
            )
        })
        .expect("identify");
    let contacts = inspect::people::count_parties_by_contact(h.env.store()).unwrap();
    let mut m = FakeChannel::message("G1", "m1", "u_int_1", "你好");
    m.sender = Sender::new(HandleRef::new(FakeChannel::SYSTEM, "u_int_1"))
        .alias(HandleRef::new("mail", "Alice@Example.com"));
    let r = push(&h, m).await;
    assert_eq!(r.sender_party_id.as_deref(), Some(alice.party_id.as_str()));
    assert_eq!(
        inspect::people::account_party_id_by_system(h.env.store(), FakeChannel::SYSTEM).unwrap(),
        vec![json!(alice.party_id)]
    );
    // the members list made contacts for ualice and ubob; the sender added none
    assert_eq!(
        inspect::people::count_parties_by_contact(h.env.store()).unwrap(),
        contacts + 2
    );
    // later, the id alone is enough
    let r = push(&h, FakeChannel::message("G1", "m2", "u_int_1", "在吗")).await;
    assert_eq!(r.sender_party_id.as_deref(), Some(alice.party_id.as_str()));

    // a stranger: one new contact with both handles
    let mut m = FakeChannel::message("G1", "m3", "u_int_2", "我是新来的");
    m.sender = Sender::new(HandleRef::new(FakeChannel::SYSTEM, "u_int_2"))
        .alias(HandleRef::new("mail", "new@example.com"));
    let r = push(&h, m).await;
    let party = r.sender_party_id.expect("sender");
    assert_ne!(party, alice.party_id);
    assert_eq!(
        inspect::people::account_party_id_by_handle_single_row(h.env.store(), "new@example.com")
            .unwrap(),
        vec![json!(party)]
    );
}

/// A sender named first in a handle system an extension declared (not one the Hub seeds), with an
/// email: matched to the contact holding the email, and the declared handle attached to it.
#[tokio::test(flavor = "multi_thread")]
async fn a_sender_in_a_declared_system_lands_on_the_contact_holding_its_alias() {
    use steve_channels::HandleRef;
    use steve_channels::channel::Sender;
    use steve_modules::accounts::{Hints, identify};
    let h = setup();
    h.env
        .store()
        .write(|w| {
            steve_store::Systems(w).upsert_declared(
                "probe_internal",
                "内部账号",
                "probe",
                steve_store::now(),
            )
        })
        .expect("declared");
    let alice = h
        .env
        .store()
        .write(|tx| identify(tx, WS, ME, "mail", "alice@example.com", &Hints::default()))
        .expect("identify");
    let mut m = FakeChannel::message("G1", "m1", "u_int_1", "你好");
    m.sender = Sender::new(HandleRef::new("probe_internal", "U-Int-1"))
        .alias(HandleRef::new("mail", "alice@example.com"));
    let r = push(&h, m).await;
    assert_eq!(r.sender_party_id.as_deref(), Some(alice.party_id.as_str()));
    // kept as given (no normalization), in the system's own family
    assert_eq!(
        inspect::people::account_party_idfamily(h.env.store()).unwrap(),
        vec![json!(format!("{} probe_internal", alice.party_id))]
    );
}
