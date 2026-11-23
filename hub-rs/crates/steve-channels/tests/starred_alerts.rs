//! A starred contact's message to you on your own account (synthetic chats; names A/B and made-up
//! text): it becomes a high-priority personal notification through your notification policy and its
//! route whatever the space's replies are set to, never once you answered yourself, never a draft of
//! its own; once per stretch of talk; `digest` still sends it, `log` keeps it in the inbox, quiet
//! hours hold it unless you let starred contacts through; a space set aside keeps it out.

#[path = "support/cleanup_backup.rs"]
mod cleanup_backup;
#[path = "support/digest_env.rs"]
mod digest_env;
use digest_env::TestEnv;
use std::sync::Arc;
use steve_store::testkit::channels::inspect;
use steve_store::testkit::channels::seed;
use steve_store::testkit::channels::seed::people::Bot;

use serde_json::{Value, json};
use steve_channels::disclosure::{CHECK_HEAD, Disclosure};
use steve_channels::fake::channel::FakeChannel;
use steve_channels::notifications::{self as n, Mode, Policy};
use steve_channels::outbound::{OutLink, Outbox};
use steve_channels::respond::SUBJECT_MESSAGE;
use steve_channels::starred::{self, HEAD, LABEL_AGAIN, LABEL_POLICY_OFF, LABEL_SENT};
use steve_channels::{Channels, Inbound, InboundMessage, Mention, Replies, SpaceKind, spaces};
use steve_flow::testkit::FakeCore;
use steve_flow::{FlowSetup, LinearExecutor};
use steve_modules::accounts::connections::{self, Connections, ConnectionsConfig};
use steve_modules::testkit::{ME, WS, call, caller};
use steve_modules::vault::Vault;
use steve_modules::vault::keystore::FileKeyStore;
use steve_store::base::Viewer;
use tempfile::TempDir;

const PASS: &str = r#"{"verdict": "pass", "spans": []}"#;

struct H {
    env: TestEnv,
    _keys: TempDir,
    ch: Channels,
    fake: Arc<FakeChannel>,
    outbox: Arc<Outbox>,
    /// Your own account.
    me: String,
    /// The bot whose private chat with you carries your notifications.
    bot: String,
}

/// A bot connection of the workspace (its account is the own organization's).
fn bot_connection(env: &TestEnv, handle: &str, name: &str) -> String {
    FakeChannel::seed_bot_connection(env.store(), &caller(WS, ME), handle, name)
        .expect("bot connection")
        .0
}

async fn setup() -> H {
    let core = Arc::new(FakeCore::default());
    let flow_setup = FlowSetup::new(core.clone());
    let disclosure = Disclosure::with_models(Vec::new());
    let link = OutLink::new();
    let (reg_disclosure, reg_link) = (disclosure.clone(), link.clone());
    let env = TestEnv::new(|r| {
        flow_setup.register(r);
        connections::register(r);
        r.outbound_check(Box::new(reg_disclosure.clone()));
        r.decision(SUBJECT_MESSAGE, reg_link.handler());
    });
    disclosure.bind(env.store().clone(), env.gateway.clone());
    link.bind_clock(env.clock.clone());
    *core.store.lock().unwrap() = Some(env.store().clone());
    steve_store::testkit::channels::seed::people::bots(env.store(), &[Bot::default()]).unwrap();
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
    let outbox = Arc::new(Outbox::new(link));
    let replies = Arc::new(Replies::new().outbound(outbox.clone()));
    let flow = flow_setup
        .connections(conns.clone())
        .build(env.modules.clone(), Arc::new(LinearExecutor));
    let ch = Channels::builder(env.modules.clone(), conns, Arc::new(flow.clone()))
        .adapter(fake.clone())
        .responder(replies)
        .flow(flow)
        .build();
    let (me, _) =
        FakeChannel::seed_connection(env.store(), &caller(WS, ME), Some("upx")).expect("you");
    let bot = bot_connection(&env, "ubot", "通知机器人");
    let people = [("ua", "阿甲"), ("ub", "阿乙")];
    fake.space("C_DEV", "开发群", SpaceKind::Group, false, &people);
    fake.space("D_A", "", SpaceKind::Dm, false, &[("ua", "阿甲")]);
    fake.space("D_B", "", SpaceKind::Dm, false, &[("ub", "阿乙")]);
    fake.space("D_SELF", "", SpaceKind::Dm, false, &[("uself", "主人")]);
    env.gateway.script(CHECK_HEAD, PASS);
    let now = env.modules.clock.now();
    let v = Viewer::bare(WS, ME);
    // nothing is drafted for you anywhere: what the star does is the only thing that happens
    env.store()
        .write(|tx| {
            spaces::set_account_rules(
                tx,
                &v,
                &me,
                &json!([{"when": {"kind": "mention_or_dm"}, "act": "record"}]),
                now,
            )
        })
        .expect("rules");
    H {
        env,
        _keys: keys,
        ch,
        fake,
        outbox,
        me,
        bot,
    }
}

/// A message `secs` from now (the fake clock).
fn msg(h: &H, space: &str, id: &str, from: &str, text: &str, secs: i64) -> InboundMessage {
    let mut m = FakeChannel::message(space, id, from, text);
    m.created = h.ch.now() + secs * 1000;
    m
}

fn at_you(mut m: InboundMessage) -> InboundMessage {
    m.mentions.push(Mention {
        handle: None,
        name: "你".into(),
        is_connection: true,
    });
    m
}

async fn push(h: &H, m: InboundMessage) -> Inbound {
    h.ch.on_message(WS, &h.me.clone(), m)
        .await
        .expect("message")
}

fn party_of(h: &H, handle: &str) -> String {
    inspect::people::account_party_id_by_handle(h.env.store(), handle).unwrap()[0]
        .as_str()
        .unwrap()
        .to_string()
}

fn space_of(h: &H, ext: &str) -> String {
    inspect::spaces::space_id_by_external(h.env.store(), ext).unwrap()[0]
        .as_str()
        .unwrap()
        .to_string()
}

fn star(h: &H, handle: &str) {
    seed::people::set_party_important_value_1(h.env.store(), &party_of(h, handle)).unwrap();
}

/// What went out to you: the texts sent anywhere.
fn sent(h: &H) -> Vec<String> {
    h.fake
        .sent
        .lock()
        .unwrap()
        .iter()
        .map(|(_, c, _)| c.text.clone())
        .collect()
}

fn alerts(h: &H) -> Vec<String> {
    sent(h)
        .into_iter()
        .filter(|t| t.starts_with(HEAD))
        .collect()
}

/// Your notification policy, turned on in `mode`, routed to your private chat with the bot.
async fn policy(h: &H, mode: Mode) -> Policy {
    h.ch.on_message(
        WS,
        &h.bot.clone(),
        FakeChannel::message("D_SELF", "self0", "uself", "在"),
    )
    .await
    .expect("self dm");
    let me_party = inspect::people::party_id_by_workspace_and_user(h.env.store(), WS, ME).unwrap()
        [0]
    .as_str()
    .unwrap()
    .to_string();
    // your account on the bot's side is yours (a verified link, not a guess)
    seed::people::set_account_party_id(h.env.store(), &me_party).unwrap();
    let self_dm = inspect::spaces::space_id_by_d_self_and_connection(h.env.store(), &h.bot)
        .unwrap()[0]
        .as_str()
        .unwrap()
        .to_string();
    let p = Policy {
        enabled: true,
        mode,
        party_id: Some(me_party),
        space_id: Some(self_dm),
        timezone: "UTC".into(),
        quiet_start: None,
        quiet_end: None,
        starred_in_quiet: false,
    };
    n::put_policy(&h.ch, &caller(WS, ME), &p).expect("policy");
    p
}

async fn pump(h: &H) {
    n::pump(&h.ch, &h.outbox).await.expect("pump");
}

/// A known A, starred, and a quiet hour later.
async fn starred_a(h: &H) {
    push(h, msg(h, "D_A", "k0", "ua", "早", 0)).await;
    push(h, msg(h, "D_B", "k1", "ub", "早", 0)).await;
    star(h, "ua");
    h.env.clock.advance(3_600_000);
}

// ---------------------------------------------------------------- what reaches you

#[tokio::test(flavor = "multi_thread")]
async fn a_starred_contacts_message_reaches_you_where_nothing_is_drafted() {
    let h = setup().await;
    policy(&h, Mode::Now).await;
    starred_a(&h).await;

    let r = push(&h, msg(&h, "D_A", "s1", "ua", "晚上一起吃饭吗", 0))
        .await
        .response
        .unwrap();
    assert!(r.label.ends_with(LABEL_SENT), "{}", r.label);
    pump(&h).await;
    let out = alerts(&h);
    assert_eq!(out.len(), 1, "{out:?}");
    assert_eq!(out[0], format!("{HEAD} 阿甲 私聊你\n晚上一起吃饭吗"));
    // urgent, of its own kind in your inbox and feed
    let intent: Value = serde_json::from_str(
        inspect::delivery::output_delivery_intent(h.env.store()).unwrap()[0]
            .as_str()
            .unwrap(),
    )
    .unwrap();
    assert_eq!(intent["urgency"], "high");
    assert_eq!(
        inspect::feed_entries::feed_entrie_kind(h.env.store()).unwrap()[0],
        json!("starred")
    );
    // never a draft in your name because of the star
    assert_eq!(
        inspect::messages::count_chan_messages_by_outgoing_and_space(
            h.env.store(),
            &space_of(&h, "D_A")
        )
        .unwrap(),
        0
    );

    // someone not starred: nothing
    h.env.clock.advance(3_600_000);
    let r = push(&h, msg(&h, "D_B", "b1", "ub", "晚上一起吃饭吗", 0))
        .await
        .response
        .unwrap();
    assert!(!r.label.contains(HEAD), "{}", r.label);
    pump(&h).await;
    assert_eq!(alerts(&h).len(), 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn in_a_group_only_what_is_meant_for_you() {
    let h = setup().await;
    policy(&h, Mode::Now).await;
    push(&h, msg(&h, "C_DEV", "k0", "ua", "早", 0)).await;
    star(&h, "ua");
    h.env.clock.advance(3_600_000);

    // said to the group: not for you
    push(&h, msg(&h, "C_DEV", "g1", "ua", "今天发版", 0)).await;
    pump(&h).await;
    assert!(alerts(&h).is_empty());
    // an @ of you
    push(&h, at_you(msg(&h, "C_DEV", "g2", "ua", "这个你看下", 0))).await;
    pump(&h).await;
    assert_eq!(
        alerts(&h),
        vec![format!("{HEAD} 阿甲 在「开发群」@ 了你\n这个你看下")]
    );
}

// ---------------------------------------------------------------- you answered yourself

#[tokio::test(flavor = "multi_thread")]
async fn nothing_once_you_answered_yourself() {
    let h = setup().await;
    let mut p = policy(&h, Mode::Now).await;
    starred_a(&h).await;

    // you are chatting there already
    push(&h, msg(&h, "D_A", "y0", "upx", "在吗", 0)).await;
    let r = push(&h, msg(&h, "D_A", "s1", "ua", "在的", 20))
        .await
        .response
        .unwrap();
    assert!(r.label.ends_with(starred::LABEL_YOU_THERE), "{}", r.label);
    pump(&h).await;
    assert!(alerts(&h).is_empty());

    // notified, but you answer before it goes out (quiet hours hold it meanwhile)
    h.env.clock.advance(3_600_000);
    let minute = (h.ch.now().rem_euclid(86_400_000) / 60_000) as u16;
    p.quiet_start = Some(minute);
    p.quiet_end = Some((minute + 10) % 1440);
    n::put_policy(&h.ch, &caller(WS, ME), &p).unwrap();
    push(&h, msg(&h, "D_A", "s2", "ua", "明天几点", 0)).await;
    pump(&h).await;
    assert!(alerts(&h).is_empty());
    push(&h, msg(&h, "D_A", "y1", "upx", "十点", 60)).await;
    h.env.clock.advance(11 * 60_000);
    pump(&h).await;
    assert!(alerts(&h).is_empty(), "{:?}", alerts(&h));
    assert_eq!(
        inspect::notifications::count_personal_notifications_by_cancelled_count(h.env.store())
            .unwrap(),
        1
    );
}

// ---------------------------------------------------------------- once per stretch of talk

#[tokio::test(flavor = "multi_thread")]
async fn once_until_you_speak_there() {
    let h = setup().await;
    policy(&h, Mode::Now).await;
    starred_a(&h).await;

    push(&h, msg(&h, "D_A", "s1", "ua", "在吗", 0)).await;
    let r = push(&h, msg(&h, "D_A", "s2", "ua", "有个事", 30))
        .await
        .response
        .unwrap();
    assert!(r.label.ends_with(LABEL_AGAIN), "{}", r.label);
    pump(&h).await;
    assert_eq!(alerts(&h).len(), 1);

    // you answered; a while later A writes again: a new one
    h.env.clock.advance(60_000);
    push(&h, msg(&h, "D_A", "y1", "upx", "说", 0)).await;
    h.env.clock.advance(20 * 60_000);
    push(&h, msg(&h, "D_A", "s3", "ua", "算了 明天再说", 0)).await;
    pump(&h).await;
    assert_eq!(alerts(&h).len(), 2, "{:?}", alerts(&h));
}

// ---------------------------------------------------------------- your notification policy

#[tokio::test(flavor = "multi_thread")]
async fn your_policy_decides_how_it_reaches_you() {
    let h = setup().await;
    starred_a(&h).await;

    // no policy turned on: nothing queued, and the message says so
    let r = push(&h, msg(&h, "D_A", "s1", "ua", "在吗", 0))
        .await
        .response
        .unwrap();
    assert!(r.label.ends_with(LABEL_POLICY_OFF), "{}", r.label);
    assert_eq!(
        inspect::notifications::count_personal_notifications(h.env.store()).unwrap(),
        0
    );

    // digest, saved as the settings page does with no conversation chosen: no route is looked up
    // for you, it waits in your inbox
    let mut p = policy(&h, Mode::Digest).await;
    let chosen = p.space_id.take();
    n::put_policy(&h.ch, &caller(WS, ME), &p).unwrap();
    h.env.clock.advance(3_600_000);
    let r = push(&h, msg(&h, "D_A", "s2", "ua", "急 帮我看下", 0))
        .await
        .response
        .unwrap();
    assert!(r.label.ends_with(starred::LABEL_INBOX), "{}", r.label);
    pump(&h).await;
    assert!(alerts(&h).is_empty(), "{:?}", alerts(&h));
    // a conversation that is not a private chat with you and the bot is refused under digest too
    p.space_id = Some(space_of(&h, "D_A"));
    assert!(n::put_policy(&h.ch, &caller(WS, ME), &p).is_err());
    // digest with the private chat you chose: still sent now (the urgent kind)
    p.space_id = chosen;
    n::put_policy(&h.ch, &caller(WS, ME), &p).unwrap();
    h.env.clock.advance(3_600_000);
    push(&h, msg(&h, "D_A", "s2b", "ua", "急 帮我看下", 0)).await;
    pump(&h).await;
    assert_eq!(alerts(&h).len(), 1);

    // log: kept in your inbox only
    p.mode = Mode::Log;
    n::put_policy(&h.ch, &caller(WS, ME), &p).unwrap();
    h.env.clock.advance(3_600_000);
    push(&h, msg(&h, "D_A", "s3", "ua", "还在吗", 0)).await;
    pump(&h).await;
    assert_eq!(alerts(&h).len(), 1);
    assert_eq!(
        inspect::notifications::count_personal_notifications_by_held_count(h.env.store()).unwrap(),
        2
    );

    // quiet hours hold it, unless you let starred contacts through
    p.mode = Mode::Now;
    h.env.clock.advance(3_600_000);
    let minute = (h.ch.now().rem_euclid(86_400_000) / 60_000) as u16;
    p.quiet_start = Some(minute);
    p.quiet_end = Some((minute + 30) % 1440);
    n::put_policy(&h.ch, &caller(WS, ME), &p).unwrap();
    push(&h, msg(&h, "D_A", "s4", "ua", "睡了吗", 0)).await;
    pump(&h).await;
    assert_eq!(alerts(&h).len(), 1);
    p.starred_in_quiet = true;
    n::put_policy(&h.ch, &caller(WS, ME), &p).unwrap();
    h.env.clock.advance(11_000); // the next recovery pass
    pump(&h).await;
    assert_eq!(alerts(&h).len(), 2, "{:?}", alerts(&h));
}

// ---------------------------------------------------------------- spaces set aside

#[tokio::test(flavor = "multi_thread")]
async fn a_space_set_aside_keeps_it_out() {
    let h = setup().await;
    policy(&h, Mode::Now).await;
    starred_a(&h).await;
    let space = space_of(&h, "D_A");

    // replies set to record there: still told (the setting is about replies)
    seed::spaces::seed_space_preference(h.env.store(), &space).unwrap();
    push(&h, msg(&h, "D_A", "s1", "ua", "在吗", 0)).await;
    pump(&h).await;
    assert_eq!(alerts(&h).len(), 1);

    // Steve leaves the space alone (off): nothing, and a waiting one is cancelled
    h.env.clock.advance(3_600_000);
    seed::spaces::set_space_preference_reply_as_you_off(h.env.store(), &space).unwrap();
    let r = push(&h, msg(&h, "D_A", "s2", "ua", "在吗", 0))
        .await
        .response
        .unwrap();
    assert!(!r.label.contains(HEAD), "{}", r.label);
    pump(&h).await;
    assert_eq!(alerts(&h).len(), 1);

    // 不提醒: nothing either
    seed::spaces::set_space_preference_reply_as_you_draft(h.env.store(), &space).unwrap();
    seed::spaces::set_space_preference_attention_quiet(h.env.store(), &space).unwrap();
    h.env.clock.advance(3_600_000);
    push(&h, msg(&h, "D_A", "s3", "ua", "在吗", 0)).await;
    pump(&h).await;
    assert_eq!(alerts(&h).len(), 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_retry_after_you_answered_is_let_go() {
    let h = setup().await;
    policy(&h, Mode::Now).await;
    starred_a(&h).await;

    // the first attempt fails: a card offers to retry it
    *h.fake.fail_send.lock().unwrap() = Some("发不出去".into());
    push(&h, msg(&h, "D_A", "s1", "ua", "在吗", 0)).await;
    pump(&h).await;
    *h.fake.fail_send.lock().unwrap() = None;
    let out = inspect::delivery::output_delivery_message_id(h.env.store()).unwrap()[0]
        .as_str()
        .unwrap()
        .to_string();
    let fail = inspect::decisions::decision_id_by_subject_and_fail_and_open(h.env.store(), &out)
        .unwrap()[0]
        .as_str()
        .unwrap()
        .to_string();

    // you answered A yourself meanwhile; the retry sends nothing
    h.env.clock.advance(30_000);
    push(&h, msg(&h, "D_A", "y1", "upx", "在", 0)).await;
    h.env.clock.advance(30_000);
    steve_modules::decisions::dispatch(&h.env.modules, &caller(WS, ME), &fail, "retry", "")
        .expect("ours")
        .expect("retry");
    h.outbox.send_due(&h.ch).await;
    assert!(alerts(&h).is_empty(), "{:?}", alerts(&h));
    assert_eq!(
        inspect::messages::message_status(h.env.store(), &out).unwrap()[0],
        json!("discarded")
    );
    assert_eq!(
        inspect::notifications::notice_state(h.env.store()).unwrap()[0],
        json!("cancelled")
    );
}

// ---------------------------------------------------------------- your own account and a bot in one group

#[tokio::test(flavor = "multi_thread")]
async fn the_bots_copy_first_still_tells_you() {
    let h = setup().await;
    policy(&h, Mode::Now).await;
    push(&h, msg(&h, "C_DEV", "k0", "ua", "早", 0)).await;
    star(&h, "ua");
    h.env.clock.advance(3_600_000);

    // the bot's copy arrives first: it @-ed nobody the bot knows as you
    let first =
        h.ch.on_message(
            WS,
            &h.bot.clone(),
            msg(&h, "C_DEV", "g2", "ua", "这个你看下", 0),
        )
        .await
        .expect("bot copy");
    assert!(!first.duplicate);
    pump(&h).await;
    assert!(alerts(&h).is_empty());
    // your own account's copy, with the @ of you, is a duplicate there: it still tells you, once
    let mine = push(&h, at_you(msg(&h, "C_DEV", "g2", "ua", "这个你看下", 0))).await;
    assert!(mine.duplicate);
    pump(&h).await;
    assert_eq!(
        alerts(&h),
        vec![format!("{HEAD} 阿甲 在「开发群」@ 了你\n这个你看下")]
    );
    assert!(
        inspect::signals::signal_outcome_label(h.env.store(), &first.message_id).unwrap()[0]
            .as_str()
            .unwrap()
            .ends_with(LABEL_SENT)
    );
    // the same copy again: nothing more
    push(&h, at_you(msg(&h, "C_DEV", "g2", "ua", "这个你看下", 0))).await;
    pump(&h).await;
    assert_eq!(alerts(&h).len(), 1);

    // the bot's copy of a reply to your message, first: told by what is stored, before your copy
    h.env.clock.advance(3_600_000);
    push(&h, msg(&h, "C_DEV", "y1", "upx", "发版时间定了吗", 0)).await;
    h.env.clock.advance(20 * 60_000);
    let mut reply = msg(&h, "C_DEV", "g3", "ua", "定了 周四", 0);
    reply.reply_to = Some("y1".into());
    h.ch.on_message(WS, &h.bot.clone(), reply.clone())
        .await
        .expect("bot copy");
    pump(&h).await;
    assert_eq!(alerts(&h).len(), 2, "{:?}", alerts(&h));
    assert_eq!(
        alerts(&h)[1],
        format!("{HEAD} 阿甲 在「开发群」回复了你\n定了 周四")
    );
    push(&h, reply).await;
    pump(&h).await;
    assert_eq!(alerts(&h).len(), 2);
}

// ---------------------------------------------------------------- an interrupted send

#[tokio::test(flavor = "multi_thread")]
async fn under_digest_an_interrupted_send_is_resumed() {
    let h = setup().await;
    policy(&h, Mode::Digest).await;
    starred_a(&h).await;

    // the Hub is in maintenance when the notification is submitted: nothing goes out yet
    h.env.store().maintenance_set(true).unwrap();
    push(&h, msg(&h, "D_A", "s1", "ua", "在吗", 0)).await;
    pump(&h).await;
    assert!(alerts(&h).is_empty());
    assert_eq!(
        inspect::notifications::notice_state(h.env.store()).unwrap()[0],
        json!("submitted")
    );
    h.env.store().maintenance_set(false).unwrap();
    h.env
        .clock
        .advance(steve_channels::outbound::UNCLAIMED_MS + 1);
    h.outbox.send_due(&h.ch).await;
    assert_eq!(alerts(&h).len(), 1, "{:?}", alerts(&h));
}

// ---------------------------------------------------------------- purging the message

/// The starred contacts' messages, one sent to you and one kept in your inbox (log).
async fn two_notified(h: &H) {
    let mut p = policy(h, Mode::Now).await;
    starred_a(h).await;
    push(h, msg(h, "D_A", "s1", "ua", "暗号是青蛙", 0)).await;
    pump(h).await;
    assert_eq!(alerts(h).len(), 1);
    p.mode = Mode::Log;
    n::put_policy(&h.ch, &caller(WS, ME), &p).unwrap();
    star(h, "ub");
    push(h, msg(h, "D_B", "s2", "ub", "门禁口令是松鼠", 0)).await;
    // the copies there are to purge
    assert_eq!(
        inspect::delivery::count_output_deliveries_count_3(h.env.store()).unwrap(),
        1
    );
    assert_eq!(
        inspect::messages::count_chan_messages_by_outgoing_count_3(h.env.store()).unwrap(),
        1
    );
    assert_eq!(
        inspect::notifications::count_personal_notifications_count_2(h.env.store()).unwrap(),
        2
    );
}

/// No copy of what they said is left: not in the notification (nor the feed that shows it), the
/// message that went out to you, or its delivery.
fn no_copy_left(h: &H, how: &str) {
    assert_eq!(
        inspect::notifications::count_personal_notifications_count_3(h.env.store()).unwrap(),
        0,
        "{how}"
    );
    assert_eq!(
        inspect::messages::count_chan_messages_by_outgoing_count_4(h.env.store()).unwrap(),
        0,
        "{how}"
    );
    assert_eq!(
        inspect::delivery::count_output_deliveries_count_4(h.env.store()).unwrap(),
        0,
        "{how}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn purging_the_message_purges_every_copy_of_it() {
    // the text purged (删除消息正文, or retention): the same update of the message as both do
    let h = setup().await;
    two_notified(&h).await;
    seed::messages::clear_message_text_for_purging_the_message_purges_every_co(h.env.store())
        .unwrap();
    no_copy_left(&h, "text");

    // 清理记录: the spaces, their messages and the cards on them deleted
    let h = setup().await;
    two_notified(&h).await;
    let cleanup = steve_channels::cleanup::routes(h.ch.clone())
        .layer(axum::Extension(cleanup_backup::port(
            h.env.store().path().to_owned(),
            h.env.dir.path().join("backups"),
            false,
        )))
        .with_state(h.env.modules.clone());
    let who = caller(WS, ME);
    let (st, plan) = call(
        &cleanup,
        Some(&who),
        "POST",
        "/channels/cleanup/preview",
        Some(&json!({"connection_id": h.me, "level": "all"})),
    )
    .await;
    assert_eq!(st, 200, "{plan}");
    let (st, result) = call(
        &cleanup,
        Some(&who),
        "POST",
        "/channels/cleanup",
        Some(&json!({"connection_id": h.me, "level": "all", "confirm": true, "decision_ids": plan["decision_ids"]})),
    )
    .await;
    assert_eq!(st, 200, "{result}");
    assert_eq!(
        inspect::messages::count_chan_messages_by_external_id_s1_s2(h.env.store()).unwrap(),
        0,
        "{result}"
    );
    no_copy_left(&h, "all");
}
