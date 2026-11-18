//! IM conversations:
//! - the reaction lifecycle: “收到” before anything is generated, “完成” once the answer is out,
//!   “失败” when it could not go out, the acknowledgement taken off for a discarded draft; an item
//!   keeps it until it ends; reactions are best-effort (a provider refusing them changes nothing);
//! - one response per provider message: only the voice answers (the other identity observes and
//!   never drafts), your `/bot` is answered by the voice, a redelivery or the other identity's copy is
//!   a duplicate with one reaction lifecycle, Steve's own messages read back and other assistants'
//!   automatic replies are not instructions;
//! - every space Steve answers in has one conversation (`chan:<space>`): turns go there, Steve's
//!   reply there goes out to the space, an item made there is linked back to the chat message, a
//!   failure sends a short note; a composer skip or question there is written on the message's
//!   event once the answer is known (`recorded` with the composer's reason, or `need` with the
//!   decision), respond()'s hand-off writes nothing before it;
//! - once the space's connection was removed: the reaction ends here without a provider call, and
//!   Steve's reply there is recorded as not sent (`chan_out.status = "disconnected"`).

use std::sync::{Arc, Mutex};

use serde_json::{Value, json};
use steve_channels::compose::H_SAFETY;
use steve_channels::conversations::{self, Thread, Turn};
use steve_channels::deliver::Delivered;
use steve_channels::disclosure::{CHECK_HEAD, Disclosure};
use steve_channels::fake::channel::FakeChannel;
use steve_channels::flow_actions::LateChannels;
use steve_channels::outbound::{OutLink, Outbox};
use steve_channels::respond::SUBJECT_MESSAGE;
use steve_channels::{
    Channels, Inbound, InboundMessage, Mention, ReactionKind, Replies, SpaceKind, arbiter, spaces,
};
use steve_flow::testkit::FakeCore;
use steve_flow::{FlowSetup, LinearExecutor};
use steve_modules::ApiResult;
use steve_modules::accounts::connections::{self, Connections, ConnectionsConfig};
use steve_modules::accounts::repo::{self as arepo, NewAccount};
use steve_modules::accounts::service::ensure_seeds;
use steve_modules::testkit::{ME, TestEnv, WS, caller};
use steve_modules::vault::Vault;
use steve_modules::vault::keystore::FileKeyStore;
use steve_store::base::Viewer;
use steve_store::uid;
use tempfile::TempDir;

const REPLY: &str =
    r#"{"text": "先看部署日志，再按回滚手册处理。", "needs_approval": false, "why": "按手册"}"#;
const PASS: &str = r#"{"verdict": "pass", "spans": []}"#;

/// steve-core's thread as the test sees it: turns are stored in their conversation and recorded.
struct FakeThread {
    store: steve_store::Store,
    turns: Mutex<Vec<(String, Turn)>>,
    /// The turn is stored, then taking it fails with this.
    fail: Mutex<Option<String>>,
}

impl Thread for FakeThread {
    fn say(&self, ws: &str, turn: Turn) -> Result<String, String> {
        let id = self
            .store
            .messages()
            .add(ws, "me", &turn.text, None, &turn.meta, &turn.space)
            .map_err(|e| e.to_string())?;
        self.turns.lock().unwrap().push((id.clone(), turn));
        if let Some(e) = self.fail.lock().unwrap().clone() {
            return Err(e);
        }
        Ok(id)
    }
}

struct H {
    env: TestEnv,
    _keys: TempDir,
    ch: Channels,
    fake: Arc<FakeChannel>,
    outbox: Arc<Outbox>,
    thread: Option<Arc<FakeThread>>,
    me: String,
    bot: String,
}

fn bot_connection(h: &TestEnv, handle: &str, name: &str) -> (String, String) {
    h.store()
        .transaction(|tx| -> ApiResult<(String, String)> {
            ensure_seeds(tx, WS)?;
            steve_modules::contacts::service::ensure_base(tx, &caller(WS, ME))?;
            let org = steve_modules::contacts::repo::self_org(tx, WS)?.expect("self org");
            let a = arepo::insert(
                tx,
                WS,
                &NewAccount {
                    system_id: FakeChannel::SYSTEM.into(),
                    family: "slack_user".into(),
                    party_id: org.id,
                    kind: "app_bot".into(),
                    handle: handle.into(),
                    handle_display: handle.into(),
                    display_name: name.into(),
                    verified: true,
                    confidence: 1.0,
                    source: "connect".into(),
                    source_label: String::new(),
                },
            )?;
            let con = uid("con");
            tx.execute(
                "INSERT INTO connections (id, workspace_id, account_id, adapter, auth, caps, created_by, created, updated)
                 VALUES (?, ?, ?, ?, 'none', '[\"msg.receive\",\"msg.send\",\"msg.send_as_user\"]', ?, 1, 1)",
                rusqlite::params![con, WS, a.id, FakeChannel::ID, ME],
            )?;
            Ok((con, a.id))
        })
        .expect("bot connection")
}

fn set_rules(h: &H, conn: &str, rules: Value) {
    let now = h.ch.now();
    h.env
        .store()
        .transaction(|tx| spaces::set_account_rules(tx, &Viewer::bare(WS, ME), conn, &rules, now))
        .expect("rules");
}

fn exec(h: &H, sql: &str, p: &[&str]) {
    let (sql, p): (String, Vec<String>) = (sql.into(), p.iter().map(|s| s.to_string()).collect());
    h.env
        .store()
        .with_conn(move |c| {
            c.execute(&sql, rusqlite::params_from_iter(p.iter()))?;
            Ok(())
        })
        .expect("sql");
}

fn one(h: &H, sql: &str, p: &[&str]) -> Value {
    let (sql, p): (String, Vec<String>) = (sql.into(), p.iter().map(|s| s.to_string()).collect());
    h.env
        .store()
        .reader(move |c| {
            let v = c.query_row(&sql, rusqlite::params_from_iter(p.iter()), |r| {
                Ok(match r.get_ref(0)? {
                    rusqlite::types::ValueRef::Null => Value::Null,
                    rusqlite::types::ValueRef::Integer(x) => json!(x),
                    rusqlite::types::ValueRef::Text(t) => json!(String::from_utf8_lossy(t)),
                    _ => Value::Null,
                })
            })?;
            Ok::<_, steve_store::StoreError>(v)
        })
        .expect("row")
}

/// A message from `from`; `at` @-mentions the receiving connection, `bot_at` the bot by handle.
fn msg(space: &str, id: &str, from: &str, text: &str, at: bool, bot_at: bool) -> InboundMessage {
    let mut m = FakeChannel::message(space, id, from, text);
    if at {
        m.mentions.push(Mention {
            handle: None,
            name: "值班小运".into(),
            is_connection: true,
        });
    }
    if bot_at {
        m.mentions.push(Mention {
            handle: Some(FakeChannel::handle("uoncall")),
            name: "值班小运".into(),
            is_connection: false,
        });
    }
    m
}

async fn push(h: &H, conn: &str, m: InboundMessage) -> Inbound {
    h.ch.on_message(WS, conn, m).await.expect("message")
}

fn sent(h: &H) -> Vec<String> {
    h.fake
        .sent
        .lock()
        .unwrap()
        .iter()
        .map(|(_, c, _)| c.text.clone())
        .collect()
}

fn log(h: &H) -> Vec<String> {
    h.fake
        .reaction_log
        .lock()
        .unwrap()
        .iter()
        .map(|l| {
            l.split(' ')
                .map(|x| if x.starts_with("-r") { "-r" } else { x })
                .collect::<Vec<_>>()
                .join(" ")
        })
        .collect()
}

fn state(h: &H, message: &str) -> Value {
    one(
        h,
        "SELECT state FROM chan_reactions WHERE message_id = ?",
        &[message],
    )
}

async fn setup(with_thread: bool) -> H {
    setup_answering(with_thread, REPLY).await
}

/// [`setup`] whose composer answers `composed` (the fake gateway keeps its first script per prefix).
async fn setup_answering(with_thread: bool, composed: &str) -> H {
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
    env.store()
        .with_conn(|c| {
            c.execute_batch(
                "INSERT INTO bots (id, workspace_id, name, main, harness, persona, role, rules, updated) VALUES
                    ('steve', 'default', 'Steve', 1, 'claude', '', '', '', 1);
                 INSERT INTO bot_workspaces (bot_id, workspace_id, created) VALUES ('steve', 'default', 1);",
            )?;
            Ok(())
        })
        .expect("bots");
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
    .usage(Arc::new(steve_channels::usage::ChannelUsage))
    .build();
    let outbox = Arc::new(Outbox::new(link));
    let replies = Arc::new(Replies::new().outbound(outbox.clone()));
    let late = LateChannels::new();
    let flow = flow_setup
        .connections(conns.clone())
        .build(env.modules.clone(), Arc::new(LinearExecutor));
    let thread = with_thread.then(|| {
        Arc::new(FakeThread {
            store: env.store().clone(),
            turns: Mutex::new(vec![]),
            fail: Mutex::new(None),
        })
    });
    let mut b = Channels::builder(env.modules.clone(), conns, Arc::new(flow.clone()))
        .adapter(fake.clone())
        .responder(replies.clone())
        .flow(flow.clone())
        .reactor(outbox.clone());
    if let Some(t) = &thread {
        b = b.thread(t.clone());
    }
    let ch = b.build();
    late.set(&ch);
    let (me, _) =
        FakeChannel::seed_connection(env.store(), &caller(WS, ME), Some("upx")).expect("you");
    let (bot, _) = bot_connection(&env, "uoncall", "值班小运");
    // you joined the bot's app (one installation, two identities: one space per chat)
    env.store()
        .with_conn(|c| {
            c.execute(
                "UPDATE connections SET config = json_object('app_connection', ?) WHERE id = ?",
                rusqlite::params![bot, me],
            )?;
            Ok(())
        })
        .expect("installation");
    let people = [("ulilei", "李雷"), ("uhan", "韩梅梅")];
    fake.space("C_OPS", "运维群", SpaceKind::Group, false, &people);
    fake.space("C_ACME", "Acme 客户群", SpaceKind::Group, true, &people);
    fake.space("D_LI", "", SpaceKind::Dm, false, &[("ulilei", "李雷")]);
    let h = H {
        env,
        _keys: keys,
        ch,
        fake,
        outbox,
        thread,
        me,
        bot,
    };
    set_rules(
        &h,
        &h.me.clone(),
        json!([{"when": {"kind": "mention_or_dm"}, "act": "draft"}]),
    );
    set_rules(
        &h,
        &h.bot.clone(),
        json!([{"when": {"kind": "mention_or_dm"}, "act": "reply"}]),
    );
    h.env.gateway.script(CHECK_HEAD, PASS);
    h.env.gateway.script(H_SAFETY, composed);
    h
}

// ---------------------------------------------------------------- the reaction lifecycle

#[tokio::test(flavor = "multi_thread")]
async fn ack_first_then_done_once_the_reply_is_out() {
    let h = setup(false).await;
    let i = push(
        &h,
        &h.bot.clone(),
        msg("C_OPS", "o1", "ulilei", "部署挂了怎么办", true, false),
    )
    .await;
    assert_eq!(sent(&h).len(), 1);
    assert_eq!(
        log(&h),
        ["+ack o1", "-r o1", "+done o1"],
        "ack before the reply, swapped after"
    );
    assert_eq!(h.fake.reactions_on("o1"), [ReactionKind::Done]);
    assert_eq!(state(&h, &i.message_id), "done");
    // the answer that completed it
    let out = i.response.unwrap().out_message_id.unwrap();
    assert_eq!(
        one(
            &h,
            "SELECT answer_id FROM chan_reactions WHERE message_id = ?",
            &[&i.message_id]
        ),
        json!(out)
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_failed_send_turns_failed_and_a_retry_turns_done() {
    let h = setup(false).await;
    *h.fake.fail_send.lock().unwrap() = Some("飞书暂时不可用".into());
    let i = push(
        &h,
        &h.bot.clone(),
        msg("C_OPS", "o1", "ulilei", "部署挂了怎么办", true, false),
    )
    .await;
    assert_eq!(h.fake.reactions_on("o1"), [ReactionKind::Failed]);
    assert_eq!(state(&h, &i.message_id), "failed");
    // you retry once the provider is back: done replaces failed
    *h.fake.fail_send.lock().unwrap() = None;
    let out = i.response.unwrap().out_message_id.unwrap();
    let d = one(
        &h,
        "SELECT decision_id FROM chan_messages WHERE id = ?",
        &[&out],
    );
    steve_modules::decisions::dispatch(
        &h.env.modules,
        &caller(WS, ME),
        d.as_str().unwrap(),
        "retry",
        "",
    )
    .expect("ours")
    .expect("retry");
    h.env.clock.advance(11_000);
    assert_eq!(h.outbox.send_due(&h.ch).await, 1);
    assert_eq!(h.fake.reactions_on("o1"), [ReactionKind::Done]);
    assert_eq!(state(&h, &i.message_id), "done");
}

#[tokio::test(flavor = "multi_thread")]
async fn reactions_are_best_effort() {
    let h = setup(false).await;
    *h.fake.fail_reactions.lock().unwrap() = true;
    let i = push(
        &h,
        &h.bot.clone(),
        msg("C_OPS", "o1", "ulilei", "部署挂了怎么办", true, false),
    )
    .await;
    assert_eq!(sent(&h).len(), 1, "the reply goes out regardless");
    assert_eq!(i.response.unwrap().label, "已回复");
    // the state is kept (with the provider's error), so the UI and the ending still know
    assert_eq!(state(&h, &i.message_id), "done");
    assert!(
        one(
            &h,
            "SELECT error FROM chan_reactions WHERE message_id = ?",
            &[&i.message_id]
        )
        .is_string()
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_discarded_draft_takes_the_ack_off() {
    let h = setup(false).await;
    // the bot's rules draft here
    drafting_bot(&h);
    let i = push(
        &h,
        &h.bot.clone(),
        msg("C_OPS", "d1", "ulilei", "明天几点开会？", true, false),
    )
    .await;
    let r = i.response.unwrap();
    assert_eq!(r.label, "等你确认回复");
    // waiting for your approval is not “working on it”: no acknowledgement meanwhile
    assert!(h.fake.reactions_on("d1").is_empty(), "{:?}", log(&h));
    assert_eq!(state(&h, &i.message_id), "cleared");
    steve_modules::decisions::dispatch(
        &h.env.modules,
        &caller(WS, ME),
        r.decision_id.as_deref().unwrap(),
        "no",
        "",
    )
    .expect("ours")
    .expect("no");
    h.outbox.send_due(&h.ch).await;
    assert!(h.fake.reactions_on("d1").is_empty(), "{:?}", log(&h));
    assert_eq!(state(&h, &i.message_id), "cleared");
}

/// The bot's rules draft (for your approval) instead of replying.
fn drafting_bot(h: &H) {
    set_rules(
        h,
        &h.bot.clone(),
        json!([{"when": {"kind": "mention_or_dm"}, "act": "draft"}]),
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn an_item_keeps_the_ack_until_it_ends() {
    let h = setup(false).await;
    set_rules(
        &h,
        &h.bot.clone(),
        json!([{"when": {"kind": "mention_or_dm"}, "act": "create_item", "mode": "start"}]),
    );
    let i = push(
        &h,
        &h.bot.clone(),
        msg("C_OPS", "o1", "ulilei", "告警脚本坏了，修一下", true, false),
    )
    .await;
    let item = i.response.unwrap().item_id.expect("item");
    assert_eq!(sent(&h), [format!("收到，已记为 {item}。")]);
    h.ch.react_settled(&i.message_id).await;
    assert_eq!(
        h.fake.reactions_on("o1"),
        [ReactionKind::Ack],
        "still working on it"
    );
    // the item ends: done
    h.ch.react_item_ended(WS, &item, true).await;
    assert_eq!(h.fake.reactions_on("o1"), [ReactionKind::Done]);

    // a dropped item takes the ack off instead
    let j = push(
        &h,
        &h.bot.clone(),
        msg("C_OPS", "o2", "ulilei", "再加个告警", true, false),
    )
    .await;
    let item2 = j.response.unwrap().item_id.expect("item");
    h.ch.react_item_ended(WS, &item2, false).await;
    assert!(h.fake.reactions_on("o2").is_empty());
    assert_eq!(state(&h, &j.message_id), "cleared");
    // the conversation of the space has both turns and Steve's answers
    let conv = conversations::conv_space(&i.space_id);
    let msgs = h.env.store().messages().list(WS, &conv, 50).unwrap();
    assert_eq!(msgs.len(), 4);
    assert_eq!(msgs[1].item_id.as_deref(), Some(item.as_str()));
}

/// Removes the bot's connection the way 接入 does (`DELETE /connections/:id`): its spaces are left,
/// `spaces.connection_id` becomes NULL.
async fn remove_bot(h: &H) {
    h.ch.connections()
        .disconnect(&caller(WS, ME), &h.bot)
        .await
        .expect("disconnect");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_reaction_ends_locally_once_the_connection_is_removed() {
    let h = setup(false).await;
    set_rules(
        &h,
        &h.bot.clone(),
        json!([{"when": {"kind": "mention_or_dm"}, "act": "create_item", "mode": "start"}]),
    );
    let i = push(
        &h,
        &h.bot.clone(),
        msg("C_OPS", "o1", "ulilei", "告警脚本坏了，修一下", true, false),
    )
    .await;
    let item = i.response.unwrap().item_id.expect("item");
    h.ch.react_settled(&i.message_id).await;
    assert_eq!(h.fake.reactions_on("o1"), [ReactionKind::Ack]);
    remove_bot(&h).await;
    assert_eq!(
        one(
            &h,
            "SELECT connection_id FROM spaces WHERE id = ?",
            &[&i.space_id]
        ),
        Value::Null,
        "the space keeps no connection"
    );
    let before = log(&h);
    // the item ends after the connection is gone: done, recorded here, no provider call
    h.ch.react_item_ended(WS, &item, true).await;
    assert_eq!(state(&h, &i.message_id), "done");
    assert_eq!(log(&h), before, "nothing asked of the provider");
    assert_eq!(
        one(
            &h,
            "SELECT error FROM chan_reactions WHERE message_id = ?",
            &[&i.message_id]
        ),
        Value::Null
    );
    // and nothing to acknowledge any more
    h.ch.react_ack(&i.message_id).await;
    assert_eq!(state(&h, &i.message_id), "done");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_reply_to_a_space_whose_connection_was_removed_is_recorded_as_not_sent() {
    let h = setup(true).await;
    let a = push(
        &h,
        &h.bot.clone(),
        msg("C_OPS", "o1", "ulilei", "部署挂了怎么办", true, false),
    )
    .await;
    let (turn, _) = h.thread.as_ref().unwrap().turns.lock().unwrap()[0].clone();
    let text = h.ch.conv_answer(WS, &turn).await.expect("answer");
    remove_bot(&h).await;
    let ops = conversations::conv_space(&a.space_id);
    let reply = h
        .env
        .store()
        .messages()
        .add(WS, "steve", &text, None, &json!({"origin": turn}), &ops)
        .unwrap();
    let before = log(&h);
    let d = h.ch.conv_deliver(WS, &reply).await.expect("no error");
    assert!(d.is_none(), "{d:?}");
    assert!(sent(&h).is_empty());
    let m = h.env.store().messages().get(&reply).unwrap().unwrap();
    assert_eq!(
        m.meta["chan_out"]["status"],
        json!(conversations::DISCONNECTED)
    );
    assert_eq!(state(&h, &a.message_id), "failed");
    assert_eq!(log(&h), before, "nothing asked of the provider");
    // recorded once
    assert!(h.ch.conv_deliver(WS, &reply).await.unwrap().is_none());
}

// ---------------------------------------------------------------- one response per message

/// The bot and your own account both see 运维群 (the bot is a member: it is the voice).
async fn both_see_ops(h: &H) -> (String, String) {
    let b = push(
        h,
        &h.bot.clone(),
        msg("C_OPS", "o0", "uhan", "早", false, false),
    )
    .await;
    let s = push(
        h,
        &h.me.clone(),
        msg("C_OPS", "o0", "uhan", "早", false, false),
    )
    .await;
    (b.space_id, s.space_id)
}

#[tokio::test(flavor = "multi_thread")]
async fn only_the_voice_answers_whoever_sees_the_message_first() {
    let h = setup(false).await;
    let (bot_space, _) = both_see_ops(&h).await;
    // your poller sees “@值班小运 …” first: the bot answers it, you do not draft
    let s = push(
        &h,
        &h.me.clone(),
        msg("C_OPS", "o5", "ulilei", "@值班小运 部署挂了", false, true),
    )
    .await;
    let r = s.response.unwrap();
    assert!(r.out_message_id.is_some(), "{r:?}");
    assert_eq!(sent(&h).len(), 1);
    assert_eq!(
        one(
            &h,
            "SELECT count(*) FROM decisions WHERE status = 'open' AND kind = 'approve'",
            &[]
        ),
        json!(0),
        "your account does not draft"
    );
    let out = r.out_message_id.unwrap();
    assert_eq!(
        one(
            &h,
            "SELECT space_id FROM chan_messages WHERE id = ?",
            &[&out]
        ),
        json!(bot_space)
    );
    // then the bot's own copy arrives: a duplicate, nothing more
    let b = push(
        &h,
        &h.bot.clone(),
        msg("C_OPS", "o5", "ulilei", "@值班小运 部署挂了", true, false),
    )
    .await;
    assert!(b.duplicate);
    assert_eq!(sent(&h).len(), 1);
    assert_eq!(
        log(&h),
        ["+ack o5", "-r o5", "+done o5"],
        "one reaction lifecycle"
    );

    // the other way round: the bot first, your copy later is a duplicate
    push(
        &h,
        &h.bot.clone(),
        msg("C_OPS", "o6", "ulilei", "@值班小运 回滚了吗", true, false),
    )
    .await;
    let s = push(
        &h,
        &h.me.clone(),
        msg("C_OPS", "o6", "ulilei", "@值班小运 回滚了吗", false, true),
    )
    .await;
    assert!(s.duplicate);
    assert_eq!(sent(&h).len(), 2);
}

#[tokio::test(flavor = "multi_thread")]
async fn your_account_only_observes_where_the_bot_speaks() {
    let h = setup(false).await;
    both_see_ops(&h).await;
    // “@你” in a group the bot is in: your draft rule does not run there
    let s = push(
        &h,
        &h.me.clone(),
        msg("C_OPS", "o7", "ulilei", "@你 这个怎么看", true, false),
    )
    .await;
    let r = s.response.unwrap();
    assert!(r.decision_id.is_none(), "{r:?}");
    assert!(sent(&h).is_empty());
    assert!(h.fake.reactions_on("o7").is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn your_command_is_answered_as_you_once_where_the_bot_speaks() {
    let h = setup(false).await;
    let (_, self_space) = both_see_ops(&h).await;
    exec(
        &h,
        "UPDATE channel_settings SET trigger = ? WHERE connection_id = ?",
        &[
            &json!({"on": true, "words": ["/bot"], "where": [self_space], "mark": true})
                .to_string(),
            &h.me,
        ],
    );
    // the bot's connection sees your command first (it has the group's messages): its own rules
    let b = push(
        &h,
        &h.bot.clone(),
        msg("C_OPS", "o8", "upx", "/bot 告诉李雷先回滚", false, false),
    )
    .await;
    let r = b.response.unwrap();
    assert!(!matches!(r.by, steve_channels::By::Trigger { .. }), "{r:?}");
    assert!(sent(&h).is_empty());
    // your poller's copy: stored already, but the trigger word is yours: answered as you, once
    let s = push(
        &h,
        &h.me.clone(),
        msg("C_OPS", "o8", "upx", "/bot 告诉李雷先回滚", false, false),
    )
    .await;
    assert!(s.duplicate);
    let r = s.response.expect("answered");
    assert!(matches!(r.by, steve_channels::By::Trigger { .. }), "{r:?}");
    assert_eq!(sent(&h).len(), 1, "your own account answers your command");
    assert!(
        sent(&h)[0].starts_with("[AI] "),
        "as you, marked: {:?}",
        sent(&h)
    );
    // the same copy again is not a second answer
    push(
        &h,
        &h.me.clone(),
        msg("C_OPS", "o8", "upx", "/bot 告诉李雷先回滚", false, false),
    )
    .await;
    assert_eq!(sent(&h).len(), 1);
    assert_eq!(log(&h), ["+ack o8", "-r o8", "+done o8"]);
}

#[tokio::test(flavor = "multi_thread")]
async fn the_same_message_twice_is_answered_once() {
    let h = setup(false).await;
    let m = msg("C_OPS", "o1", "ulilei", "部署挂了怎么办", true, false);
    let a = push(&h, &h.bot.clone(), m.clone()).await;
    let b = push(&h, &h.bot.clone(), m).await;
    assert!(!a.duplicate && b.duplicate);
    assert_eq!(sent(&h).len(), 1);
    assert_eq!(log(&h), ["+ack o1", "-r o1", "+done o1"]);
}

#[tokio::test(flavor = "multi_thread")]
async fn echoes_and_automatic_replies_are_not_instructions() {
    let h = setup(false).await;
    let (_, self_space) = both_see_ops(&h).await;
    exec(
        &h,
        "UPDATE channel_settings SET trigger = ? WHERE connection_id = ?",
        &[
            &json!({"on": true, "words": ["/bot"], "where": [self_space], "mark": true})
                .to_string(),
            &h.me,
        ],
    );
    // the bot's answer read back by your poller before its send result is stored
    let i = push(
        &h,
        &h.bot.clone(),
        msg("C_OPS", "o1", "ulilei", "部署挂了怎么办", true, false),
    )
    .await;
    let out = i.response.unwrap().out_message_id.unwrap();
    exec(
        &h,
        "UPDATE chan_messages SET external_id = NULL, status = 'pending' WHERE id = ?",
        &[&out],
    );
    let e = push(
        &h,
        &h.me.clone(),
        msg(
            "C_OPS",
            "fm_echo",
            "upx",
            "先看部署日志，再按回滚手册处理。",
            false,
            false,
        ),
    )
    .await;
    assert_eq!(
        e.next,
        steve_channels::Next::Record(steve_channels::inbound::LABEL_ECHO)
    );
    // another assistant posting on your account, even with your trigger word in it
    let a = push(
        &h,
        &h.me.clone(),
        msg(
            "C_OPS",
            "o9",
            "upx",
            "/bot 没能生成回答…🤖 AI 代答，仅供参考",
            false,
            false,
        ),
    )
    .await;
    assert_eq!(
        a.next,
        steve_channels::Next::Record(arbiter::LABEL_AUTOMATED)
    );
    let b = push(
        &h,
        &h.bot.clone(),
        msg(
            "C_OPS",
            "o10",
            "upx",
            "没能生成回答…🤖 AI 代答，仅供参考",
            true,
            false,
        ),
    )
    .await;
    assert_eq!(
        b.next,
        steve_channels::Next::Record(arbiter::LABEL_AUTOMATED)
    );
    assert_eq!(sent(&h).len(), 1, "only the first answer");
}

// ---------------------------------------------------------------- conversations

#[tokio::test(flavor = "multi_thread")]
async fn each_space_has_one_conversation_that_continues() {
    let h = setup(true).await;
    let t = h.thread.clone().unwrap();
    let a = push(
        &h,
        &h.bot.clone(),
        msg("C_OPS", "o1", "ulilei", "部署挂了怎么办", true, false),
    )
    .await;
    assert_eq!(a.response.as_ref().unwrap().label, "交给对话回答");
    assert!(
        sent(&h).is_empty(),
        "the conversation answers, not the responder"
    );
    assert_eq!(
        state(&h, &a.message_id),
        "ack",
        "收到 recorded before anything is generated"
    );
    h.ch.react_settled(&a.message_id).await;
    assert_eq!(
        h.fake.reactions_on("o1"),
        [ReactionKind::Ack],
        "收到 before anything is generated"
    );
    let b = push(
        &h,
        &h.bot.clone(),
        msg("C_OPS", "o2", "uhan", "那回滚呢", true, false),
    )
    .await;
    push(
        &h,
        &h.bot.clone(),
        msg("D_LI", "d1", "ulilei", "在吗", false, false),
    )
    .await;
    let c = push(
        &h,
        &h.bot.clone(),
        msg(
            "C_ACME",
            "x1",
            "ulilei",
            "你们的部署流程是怎样的",
            true,
            false,
        ),
    )
    .await;
    let turns = t.turns.lock().unwrap().clone();
    assert_eq!(turns.len(), 4);
    let ops = conversations::conv_space(&a.space_id);
    assert_eq!(turns[0].1.space, ops);
    assert_eq!(
        turns[1].1.space, ops,
        "the same group continues the same conversation"
    );
    assert_eq!(b.space_id, a.space_id);
    assert_ne!(turns[2].1.space, ops, "a DM is a conversation of its own");
    assert_eq!(turns[0].1.meta["chan"]["message"], json!(a.message_id));
    assert_eq!(turns[0].1.meta["chan"]["space_name"], json!("运维群"));
    assert_eq!(turns[0].1.meta["chan"]["sender"], json!("李雷"));
    assert!(turns[0].1.research && turns[0].1.items);
    assert!(
        !turns[3].1.research && !turns[3].1.items,
        "outside members: answered in place only"
    );
    assert!(
        turns.iter().all(|(_, t)| !t.owner_only),
        "colleagues read these"
    );
    assert_eq!(c.space_id, turns[3].1.space.trim_start_matches("chan:"));

    // steve-core answers the first turn with the channel's composer and stores the reply there
    let (turn_id, _) = turns[0].clone();
    let text = h.ch.conv_answer(WS, &turn_id).await.expect("answer");
    assert_eq!(text, "先看部署日志，再按回滚手册处理。");
    let reply = h
        .env
        .store()
        .messages()
        .add(
            WS,
            "steve",
            &text,
            None,
            &json!({"origin": turn_id, "chat": "部署挂了怎么办"}),
            &ops,
        )
        .unwrap();
    let d =
        h.ch.conv_deliver(WS, &reply)
            .await
            .unwrap()
            .expect("delivered");
    assert!(matches!(d, Delivered::Sent { .. }), "{d:?}");
    assert_eq!(sent(&h), ["先看部署日志，再按回滚手册处理。"]);
    assert_eq!(h.fake.reactions_on("o1"), [ReactionKind::Done]);
    let m = h.env.store().messages().get(&reply).unwrap().unwrap();
    assert_eq!(m.meta["chan_out"]["status"], json!("sent"));
    // delivered once
    assert!(h.ch.conv_deliver(WS, &reply).await.unwrap().is_none());

    // an item made from the second turn: linked to the chat message, “收到，已记为”, ack kept
    exec(
        &h,
        "INSERT INTO items (id, workspace_id, no, title, status, stage, source, created, updated) VALUES ('ST-7', 'default', 7, '回滚', 'run', '', '[]', 1, 1)",
        &[],
    );
    let (turn2, _) = turns[1].clone();
    let ann = h
        .env
        .store()
        .messages()
        .add(
            WS,
            "steve",
            "好，交给它了，做完请你验收。",
            Some("ST-7"),
            &json!({"origin": turn2}),
            &ops,
        )
        .unwrap();
    h.ch.conv_deliver(WS, &ann).await.unwrap();
    assert_eq!(sent(&h)[1], "收到，已记为 ST-7。");
    h.ch.react_settled(&b.message_id).await;
    assert_eq!(h.fake.reactions_on("o2"), [ReactionKind::Ack]);
    assert_eq!(
        one(
            &h,
            "SELECT count(*) FROM links WHERE subject_id = 'ST-7' AND predicate = 'from' AND object_id = ?",
            &[&b.message_id]
        ),
        json!(1)
    );
    h.ch.react_item_ended(WS, "ST-7", true).await;
    assert_eq!(h.fake.reactions_on("o2"), [ReactionKind::Done]);

    // the conversation list: three conversations with their badges
    let list = h
        .env
        .store()
        .reader(|c| conversations::list(c, WS, &Viewer::bare(WS, ME)))
        .unwrap();
    assert_eq!(list.len(), 3);
    let opsb = list.iter().find(|x| x["space"] == json!(ops)).unwrap();
    assert_eq!(opsb["name"], json!("运维群"));
    assert_eq!(opsb["dm"], json!(false));
    let v = conversations::messages_of(&h.ch, WS, &Viewer::bare(WS, ME), &ops).unwrap();
    assert_eq!(v["messages"][0]["reaction"]["state"], json!("done"));
}

#[tokio::test(flavor = "multi_thread")]
async fn a_failed_answer_turns_failed_and_sends_a_short_note() {
    let h = setup(true).await;
    let a = push(
        &h,
        &h.bot.clone(),
        msg("C_OPS", "o1", "ulilei", "部署挂了怎么办", true, false),
    )
    .await;
    let (turn, _) = h.thread.as_ref().unwrap().turns.lock().unwrap()[0].clone();
    let ops = conversations::conv_space(&a.space_id);
    let err = h
        .env
        .store()
        .messages()
        .add(
            WS,
            "steve",
            "（出错了：模型超时）",
            None,
            &json!({"origin": turn, "error": true}),
            &ops,
        )
        .unwrap();
    h.ch.conv_deliver(WS, &err).await.unwrap();
    assert_eq!(h.fake.reactions_on("o1"), [ReactionKind::Failed]);
    let s = sent(&h);
    assert_eq!(s.len(), 1);
    assert!(s[0].starts_with("抱歉，这次没能答上来"), "{s:?}");
    assert!(
        !s[0].contains("模型超时"),
        "the details stay in the conversation"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn your_command_is_not_answered_once_you_left_the_chat() {
    let h = setup(true).await;
    let i = push(
        &h,
        &h.me.clone(),
        msg("D_LI", "d0", "ulilei", "在吗", false, false),
    )
    .await;
    exec(
        &h,
        "UPDATE channel_settings SET trigger = ? WHERE connection_id = ?",
        &[
            &json!({"on": true, "words": ["/bot"], "where": ["dm:*"], "mark": true}).to_string(),
            &h.me,
        ],
    );
    let c =
        h.ch.on_inbound(
            WS,
            &h.me.clone(),
            msg(
                "D_LI",
                "d2",
                "upx",
                "/bot 记一下明天给李雷回电话",
                false,
                false,
            ),
        )
        .await
        .expect("stored");
    // no identity speaks there any more (every identity left the space)
    exec(
        &h,
        "UPDATE space_identities SET left_at = 1 WHERE space_id = ?",
        &[&i.space_id],
    );
    exec(
        &h,
        "UPDATE spaces SET connection_id = NULL, status = 'left' WHERE id = ?",
        &[&i.space_id],
    );
    let before = h.thread.as_ref().unwrap().turns.lock().unwrap().len();
    let r = h.ch.respond(WS, &h.me.clone(), &c).await.expect("respond");
    // your account is not in the chat any more: nobody answers, nothing starts
    assert!(!matches!(r.by, steve_channels::By::Trigger { .. }), "{r:?}");
    let turns = h.thread.as_ref().unwrap().turns.lock().unwrap().clone();
    assert_eq!(turns.len(), before, "{turns:?}");
    assert!(sent(&h).is_empty());
}

/// An inbound turn carries its chat message (`channel_message`), read from the
/// same space only; purged sources show nothing; the turn says what Steve did not read.
#[tokio::test(flavor = "multi_thread")]
async fn a_turn_carries_its_chat_message_with_media() {
    let h = setup(true).await;
    let mut m = msg("C_OPS", "o1", "ulilei", "看下这张图", true, false);
    m.attachments = vec![
        json!({"type": "image", "source": "message", "key": "img_1"}),
        json!({"type": "file", "kind": "folder", "source": "post", "key": "f1", "name": "资料"}),
    ];
    let a = push(&h, &h.bot.clone(), m).await;
    let turns = h.thread.as_ref().unwrap().turns.lock().unwrap().clone();
    assert_eq!(
        turns[0].1.text, "看下这张图\n[图片：未读取内容] [文件夹 资料：无法读取]",
        "nothing pretends the image was seen"
    );
    let ops = conversations::conv_space(&a.space_id);
    let v = conversations::messages_of(&h.ch, WS, &Viewer::bare(WS, ME), &ops).unwrap();
    let cm = &v["messages"][0]["channel_message"];
    assert_eq!(cm["id"], json!(a.message_id));
    assert_eq!(cm["text"], json!("看下这张图"));
    // the fake channel has no resource API: said, not hidden; no keys either way
    assert_eq!(cm["attachments"][0]["unavailable"], json!("unsupported"));
    assert_eq!(cm["attachments"][1]["unavailable"], json!("folder"));
    assert!(!cm.to_string().contains("img_1"));
    assert!(v["messages"][0]["meta"].get("channel_message").is_none());

    // a turn pointing at a message of another space gets null
    let d = push(
        &h,
        &h.bot.clone(),
        msg("D_LI", "d1", "ulilei", "在吗", false, false),
    )
    .await;
    h.env
        .store()
        .messages()
        .add(
            WS,
            "me",
            "借来的",
            None,
            &json!({"chan": {"message": d.message_id}}),
            &ops,
        )
        .unwrap();
    let v = conversations::messages_of(&h.ch, WS, &Viewer::bare(WS, ME), &ops).unwrap();
    let last = v["messages"].as_array().unwrap().last().unwrap().clone();
    assert_eq!(last["text"], json!("借来的"));
    assert_eq!(last["channel_message"], Value::Null);

    // purged: text null, attachments empty, purged set
    exec(
        &h,
        "UPDATE chan_messages SET text = NULL, purged = 9 WHERE id = ?",
        &[&a.message_id],
    );
    let v = conversations::messages_of(&h.ch, WS, &Viewer::bare(WS, ME), &ops).unwrap();
    let cm = &v["messages"][0]["channel_message"];
    assert_eq!(
        (
            cm["text"].clone(),
            cm["attachments"].clone(),
            cm["purged"].clone()
        ),
        (Value::Null, json!([]), json!(9))
    );
}

// ---------------------------------------------------------------- approvals and the reaction (disclosure-01)

fn decide(h: &H, id: &str, choice: &str) {
    steve_modules::decisions::dispatch(&h.env.modules, &caller(WS, ME), id, choice, "")
        .expect("ours")
        .expect("decided");
}

/// What steve-core's `undoDecision` does around the handler.
fn undo(h: &H, id: &str) {
    h.env
        .store()
        .transaction(|tx| -> ApiResult<()> {
            let d = steve_store::base::decisions::get(tx, id)?.expect("decision");
            h.env.modules.registry.before_undo(tx, &d)?;
            tx.execute("UPDATE decisions SET status = 'open' WHERE id = ?", [id])?;
            Ok(())
        })
        .expect("undo");
}

/// A draft for your approval: no acknowledgement while it waits; approved and sent → done; approved
/// and failing → failed; discarded, taken back and approved → done after all.
#[tokio::test(flavor = "multi_thread")]
async fn an_approved_draft_ends_done_or_failed_even_after_a_discard_was_undone() {
    let h = setup(false).await;
    drafting_bot(&h);
    let draft = |id: &'static str| {
        let h = &h;
        async move {
            let i = push(
                h,
                &h.bot.clone(),
                msg("C_OPS", id, "ulilei", "明天几点开会？", true, false),
            )
            .await;
            let r = i.response.clone().unwrap();
            assert!(h.fake.reactions_on(id).is_empty(), "{:?}", log(h));
            (i.message_id, r.decision_id.unwrap())
        }
    };

    // approved and sent: done
    let (m1, d1) = draft("d1").await;
    decide(&h, &d1, "yes");
    h.env.clock.advance(11_000);
    assert_eq!(h.outbox.send_due(&h.ch).await, 1);
    assert_eq!(h.fake.reactions_on("d1"), [ReactionKind::Done]);
    assert_eq!(state(&h, &m1), "done");

    // discarded, the discard undone, approved: done after all
    let (m2, d2) = draft("d2").await;
    decide(&h, &d2, "no");
    h.outbox.send_due(&h.ch).await;
    assert_eq!(state(&h, &m2), "cleared");
    undo(&h, &d2);
    decide(&h, &d2, "yes");
    h.env.clock.advance(11_000);
    assert_eq!(h.outbox.send_due(&h.ch).await, 1);
    assert_eq!(
        h.fake.reactions_on("d2"),
        [ReactionKind::Done],
        "{:?}",
        log(&h)
    );
    assert_eq!(state(&h, &m2), "done");

    // approved but the provider refuses: failed (fault injected)
    let (m3, d3) = draft("d3").await;
    *h.fake.fail_send.lock().unwrap() = Some("飞书暂时不可用".into());
    decide(&h, &d3, "yes");
    h.env.clock.advance(11_000);
    h.outbox.send_due(&h.ch).await;
    assert_eq!(h.fake.reactions_on("d3"), [ReactionKind::Failed]);
    assert_eq!(state(&h, &m3), "failed");
}

/// An approval nobody answered expires: the draft is discarded (never sent) and the message keeps
/// no acknowledgement.
#[tokio::test(flavor = "multi_thread")]
async fn an_expired_approval_discards_the_draft() {
    let h = setup(false).await;
    drafting_bot(&h);
    let i = push(
        &h,
        &h.bot.clone(),
        msg("C_OPS", "d1", "ulilei", "明天几点开会？", true, false),
    )
    .await;
    let r = i.response.unwrap();
    let (out, d) = (r.out_message_id.unwrap(), r.decision_id.unwrap());
    assert!(steve_modules::decisions::expire(&h.env.modules, &d).expect("expire"));
    h.outbox.send_due(&h.ch).await;
    assert_eq!(
        one(&h, "SELECT status FROM chan_messages WHERE id = ?", &[&out]),
        json!("discarded")
    );
    assert_eq!(
        one(&h, "SELECT status FROM decisions WHERE id = ?", &[&d]),
        json!("expired")
    );
    assert!(h.fake.reactions_on("d1").is_empty());
    assert_eq!(state(&h, &i.message_id), "cleared");
    h.env.clock.advance(60_000);
    assert_eq!(h.outbox.send_due(&h.ch).await, 0, "never sent");
    assert!(sent(&h).is_empty());
}

/// Your private chat with the bot: only you read it, the turn says so (Steve answers there with its tools).
#[tokio::test(flavor = "multi_thread")]
async fn your_private_chat_with_the_bot_is_owner_only() {
    let h = setup(true).await;
    let t = h.thread.clone().unwrap();
    h.env
        .store()
        .transaction(|tx| -> ApiResult<()> {
            let me = steve_modules::contacts::service::ensure_base(tx, &caller(WS, ME))?;
            for (key, value) in [
                ("call", "Private nickname"),
                ("call_external", "External name"),
            ] {
                steve_modules::contacts::repo::value_insert(
                    tx,
                    WS,
                    &steve_modules::contacts::repo::NewValue {
                        party_id: me.id.clone(),
                        key: key.into(),
                        label: key.into(),
                        value: value.into(),
                        source: "me".into(),
                        source_label: "Manual".into(),
                        source_ref: None,
                        level: steve_store::base::Level::Public,
                        status: "confirmed".into(),
                        decision_id: None,
                    },
                )?;
            }
            Ok(())
        })
        .unwrap();
    h.fake
        .space("D_ME", "", SpaceKind::Dm, false, &[("upx", "我")]);
    push(
        &h,
        &h.bot.clone(),
        msg("D_ME", "m1", "upx", "你能看见我电脑在干嘛吗", false, false),
    )
    .await;
    let turns = t.turns.lock().unwrap().clone();
    assert_eq!(turns.len(), 1, "{turns:?}");
    assert!(turns[0].1.owner_only && turns[0].1.research);
    let context =
        h.ch.conv_research_context(WS, &turns[0].1.space, &turns[0].0);
    assert!(context.contains("Private nickname"), "{context}");
    assert!(!context.contains("External name"), "{context}");
    h.env
        .store()
        .reader(|c| {
            assert_eq!(
                steve_channels::personas::owner_name(c, WS)?,
                "External name"
            );
            Ok::<_, steve_modules::ApiError>(())
        })
        .unwrap();
    // a one-to-one chat whose member count the provider never gave is still complete: one person
    exec(
        &h,
        "UPDATE spaces SET member_count = 0 WHERE external_id = 'D_ME'",
        &[],
    );
    push(
        &h,
        &h.bot.clone(),
        msg("D_ME", "m2", "upx", "现在呢", false, false),
    )
    .await;
    let turns = t.turns.lock().unwrap().clone();
    assert_eq!(turns.len(), 2, "{turns:?}");
    assert!(turns[1].1.owner_only, "still only you");
}

/// A group whose member list could not be read looks like "the bot and you": it is still not your private
/// chat (only a one-to-one chat is).
#[tokio::test(flavor = "multi_thread")]
async fn a_group_with_only_you_recorded_is_not_owner_only() {
    let h = setup(true).await;
    let t = h.thread.clone().unwrap();
    h.fake
        .space("G_ME", "新群", SpaceKind::Group, false, &[("upx", "我")]);
    push(
        &h,
        &h.bot.clone(),
        msg("G_ME", "g1", "upx", "大家看下进度", true, false),
    )
    .await;
    let turns = t.turns.lock().unwrap().clone();
    assert_eq!(turns.len(), 1, "{turns:?}");
    assert!(!turns[0].1.owner_only);
}

/// "收到" goes on in the background: a slow reaction API never keeps the message from reaching the
/// conversation, and the acknowledgement still lands (in order, before anything takes it off).
#[tokio::test(flavor = "multi_thread")]
async fn a_slow_reaction_api_does_not_hold_the_answer() {
    let h = setup(true).await;
    let t = h.thread.clone().unwrap();
    *h.fake.reaction_delay.lock().unwrap() = Some(std::time::Duration::from_secs(2));
    let t0 = std::time::Instant::now();
    push(
        &h,
        &h.bot.clone(),
        msg("C_OPS", "s1", "ulilei", "部署挂了怎么办", true, false),
    )
    .await;
    assert!(
        t0.elapsed() < std::time::Duration::from_secs(1),
        "{:?}",
        t0.elapsed()
    );
    assert_eq!(
        t.turns.lock().unwrap().len(),
        1,
        "handed to the conversation at once"
    );
    for _ in 0..50 {
        if h.fake.reactions_on("s1") == [ReactionKind::Ack] {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    assert_eq!(h.fake.reactions_on("s1"), [ReactionKind::Ack]);
}

/// The answer may go out before the provider has put "收到" on: it is still bound to the message, and
/// once the acknowledgement is on it is swapped for "done" (never left at "收到").
#[tokio::test(flavor = "multi_thread")]
async fn an_answer_before_the_slow_ack_still_ends_done() {
    let h = setup(false).await;
    *h.fake.reaction_delay.lock().unwrap() = Some(std::time::Duration::from_millis(1500));
    let i = push(
        &h,
        &h.bot.clone(),
        msg("C_OPS", "s2", "ulilei", "部署挂了怎么办", true, false),
    )
    .await;
    assert_eq!(
        sent(&h).len(),
        1,
        "answered without waiting for the reaction"
    );
    for _ in 0..60 {
        if h.fake.reactions_on("s2") == [ReactionKind::Done] {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    assert_eq!(h.fake.reactions_on("s2"), [ReactionKind::Done]);
    assert_eq!(state(&h, &i.message_id), "done");
}

/// Work starts only when the owner asks: the owner's turn may start, anyone else's (a workspace member
/// too) waits as a proposal. Decided from the sender's contact, not from the chat being internal.
#[tokio::test(flavor = "multi_thread")]
async fn only_the_owner_starts_work_from_a_chat() {
    let h = setup(true).await;
    let t = h.thread.clone().unwrap();
    h.fake.space(
        "G_W",
        "工作群",
        SpaceKind::Group,
        false,
        &[("upx", "我"), ("uli", "李雷")],
    );
    push(
        &h,
        &h.bot.clone(),
        msg("G_W", "w1", "upx", "把脚本改一下", true, false),
    )
    .await;
    push(
        &h,
        &h.bot.clone(),
        msg("G_W", "w2", "uli", "把脚本改一下", true, false),
    )
    .await;
    let turns = t.turns.lock().unwrap().clone();
    assert_eq!(turns.len(), 2, "{turns:?}");
    assert!(turns[0].1.start, "the owner");
    assert!(!turns[1].1.start, "someone else");
    assert_eq!(turns[1].1.meta["chan"]["start"], json!(false));
    // 李雷 becomes a member of the workspace (not its owner): still a proposal
    exec(
        &h,
        "INSERT INTO users (id, workspace_id, subject, name, role, created) VALUES ('u_li', ?, 'li', '李雷', 'member', 1)",
        &[WS],
    );
    exec(
        &h,
        "INSERT INTO workspace_members (workspace_id, user_id, role, created) VALUES (?, 'u_li', 'member', 1)",
        &[WS],
    );
    exec(
        &h,
        "UPDATE parties SET user_id = 'u_li' WHERE id = (SELECT sender_party_id FROM chan_messages WHERE external_id = 'w2')",
        &[],
    );
    assert_eq!(
        one(
            &h,
            "SELECT count(*) FROM parties WHERE user_id = ?",
            &["u_li"]
        ),
        json!(1),
        "李雷's contact is linked to the member"
    );
    push(
        &h,
        &h.bot.clone(),
        msg("G_W", "w3", "uli", "把脚本改一下", true, false),
    )
    .await;
    let turns = t.turns.lock().unwrap().clone();
    assert_eq!(turns.len(), 3, "{turns:?}");
    assert!(!turns[2].1.start, "a member is not the owner");
}

/// A `/bot` command from a workspace member who is not the owner: answered in the chat like any turn,
/// but it doesn't start work (it may become a proposal), and it never reaches your web conversation.
#[tokio::test(flavor = "multi_thread")]
async fn a_trigger_word_a_bot_receives_is_no_command() {
    let h = setup(true).await;
    let t = h.thread.clone().unwrap();
    let (_, self_space) = both_see_ops(&h).await;
    exec(
        &h,
        "UPDATE channel_settings SET trigger = ? WHERE connection_id = ?",
        &[
            &json!({"on": true, "words": ["/bot"], "where": [self_space], "mark": true})
                .to_string(),
            &h.me,
        ],
    );
    // 李雷 has said something before (Steve knows their contact) and is a member of the workspace
    push(
        &h,
        &h.bot.clone(),
        msg("C_OPS", "o1", "ulilei", "大家好", false, false),
    )
    .await;
    exec(
        &h,
        "INSERT INTO users (id, workspace_id, subject, name, role, created) VALUES ('u_li', ?, 'li', '李雷', 'member', 1)",
        &[WS],
    );
    exec(
        &h,
        "INSERT INTO workspace_members (workspace_id, user_id, role, created) VALUES (?, 'u_li', 'member', 1)",
        &[WS],
    );
    exec(
        &h,
        "UPDATE parties SET user_id = 'u_li' WHERE id = (SELECT sender_party_id FROM chan_messages WHERE external_id = 'o1')",
        &[],
    );
    assert_eq!(
        one(
            &h,
            "SELECT count(*) FROM parties WHERE user_id = ?",
            &["u_li"]
        ),
        json!(1)
    );
    let before = t.turns.lock().unwrap().len();
    push(
        &h,
        &h.bot.clone(),
        msg("C_OPS", "o2", "ulilei", "/bot 把脚本改一下", false, false),
    )
    .await;
    let turns = t.turns.lock().unwrap().clone();
    assert!(
        turns.iter().all(|(_, x)| x.space != "steve"),
        "not your web conversation: {turns:?}"
    );
    // what a bot receives keeps the bot's own rules: no trigger word there
    assert!(
        turns[before..]
            .iter()
            .all(|(_, x)| x.meta["chan"]["command"] != true),
        "{turns:?}"
    );
    assert!(turns[before..].iter().all(|(_, x)| !x.start), "{turns:?}");
}

/// The conversation card comes from records: who asks and how they relate, who else reads the answer,
/// how complete that list is, what the asker may make Steve do. Nothing unrecorded is guessed.
#[tokio::test(flavor = "multi_thread")]
async fn the_conversation_card_says_who_asks_and_who_listens() {
    let h = setup(true).await;
    let t = h.thread.clone().unwrap();
    h.fake.space(
        "G_C",
        "客户群",
        SpaceKind::Group,
        false,
        &[("upx", "我"), ("uli", "李雷")],
    );
    push(
        &h,
        &h.bot.clone(),
        msg("G_C", "c1", "upx", "进展如何", true, false),
    )
    .await;
    push(
        &h,
        &h.bot.clone(),
        msg("G_C", "c2", "uli", "进展如何", true, false),
    )
    .await;
    let turns = t.turns.lock().unwrap().clone();
    assert_eq!(turns.len(), 2, "{turns:?}");
    let space = turns[0].1.space.clone();
    let card = |origin: &str| h.ch.conv_research_context(WS, &space, origin);
    // the owner asks: may start; 李雷 is unknown to Steve's records
    let mine = card(&turns[0].0);
    assert!(mine.contains("【对话卡】"), "{mine}");
    assert!(mine.contains("提问的人：Dev（所有者本人）"), "{mine}");
    assert!(mine.contains("李雷（身份未知）"), "{mine}");
    assert!(mine.contains("可以直接做成事项开工"), "{mine}");
    // 李雷 asks: only a proposal; 李雷 works at a customer, recorded as a job
    let party = one(
        &h,
        "SELECT sender_party_id FROM chan_messages WHERE external_id = ?",
        &["c2"],
    );
    let party = party.as_str().unwrap().to_string();
    exec(
        &h,
        "INSERT INTO parties (id, workspace_id, type, name, org_kind, created_by, created, updated) VALUES ('org_acme', ?, 'org', 'ACME 客户', 'customer', 'u', 1, 1)",
        &[WS],
    );
    exec(
        &h,
        "INSERT INTO links (workspace_id, subject_type, subject_id, predicate, object_type, object_id, source, created) VALUES (?, 'party', ?, 'works_at', 'party', 'org_acme', 'manual', 1)",
        &[WS, &party],
    );
    let theirs = card(&turns[1].0);
    assert!(
        theirs.contains("提问的人：李雷（外部，ACME 客户）"),
        "{theirs}"
    );
    assert!(
        theirs.contains("其他会看到回答的人：") && theirs.contains("（所有者本人）"),
        "{theirs}"
    );
    assert!(theirs.contains("只能作为提议"), "{theirs}");
    // an organization not everyone in the workspace may see is not named
    exec(
        &h,
        "UPDATE parties SET visibility = 'private' WHERE id = 'org_acme'",
        &[],
    );
    let hidden = card(&turns[1].0);
    assert!(
        hidden.contains("提问的人：李雷（外部）") && !hidden.contains("ACME"),
        "{hidden}"
    );
    // a job that ended says nothing about now
    exec(
        &h,
        "UPDATE links SET props = '{\"to\":\"2025-01\"}' WHERE object_id = 'org_acme'",
        &[],
    );
    let ended = card(&turns[1].0);
    assert!(ended.contains("提问的人：李雷（身份未知）"), "{ended}");
    // the provider counts more members than are recorded: they are said to be unknown
    exec(
        &h,
        "UPDATE spaces SET member_count = member_count + 3 WHERE external_id = 'G_C'",
        &[],
    );
    let wider = card(&turns[1].0);
    assert!(wider.contains("另有 3 位没有记录，身份未知"), "{wider}");
}

/// "Everyone here is one of us" needs everyone to be recorded: while the provider counts members nobody
/// recorded, the chat isn't taken for an internal one (they could be anyone).
#[tokio::test(flavor = "multi_thread")]
async fn an_incomplete_member_list_widens_nothing() {
    let h = setup(true).await;
    h.fake.space(
        "G_I",
        "内部群",
        SpaceKind::Group,
        false,
        &[("upx", "我"), ("uli", "李雷")],
    );
    push(
        &h,
        &h.bot.clone(),
        msg("G_I", "i1", "uli", "大家好", false, false),
    )
    .await;
    exec(
        &h,
        "INSERT INTO users (id, workspace_id, subject, name, role, created) VALUES ('u_li', ?, 'li', '李雷', 'member', 1)",
        &[WS],
    );
    exec(
        &h,
        "INSERT INTO workspace_members (workspace_id, user_id, role, created) VALUES (?, 'u_li', 'member', 1)",
        &[WS],
    );
    exec(
        &h,
        "UPDATE parties SET user_id = 'u_li' WHERE id = (SELECT sender_party_id FROM chan_messages WHERE external_id = 'i1')",
        &[],
    );
    let id = one(&h, "SELECT id FROM spaces WHERE external_id = ?", &["G_I"]);
    let id = id.as_str().unwrap().to_string();
    let scopes = |h: &H| -> Vec<String> {
        h.ch.store()
            .reader(|c| -> steve_channels::ChannelResult<Vec<String>> {
                let space = spaces::get(c, &id)?.expect("space");
                let (bot, _) = steve_channels::bindings::connection_account(c, WS, &h.bot)?;
                Ok(steve_channels::disclosure::space_audience(c, &space, &bot, ME)?.scopes)
            })
            .unwrap()
    };
    assert!(
        scopes(&h).contains(&"workspace".to_string()),
        "everyone recorded, all of us"
    );
    exec(
        &h,
        "UPDATE spaces SET member_count = member_count + 2 WHERE external_id = 'G_I'",
        &[],
    );
    assert!(
        !scopes(&h).contains(&"workspace".to_string()),
        "two members nobody recorded"
    );
}

/// A group a chat-list sync found has no member count; the first message there fills it in (and its
/// members), so the chat can be judged complete.
#[tokio::test(flavor = "multi_thread")]
async fn a_group_without_a_member_count_gets_one_from_its_next_message() {
    let h = setup(true).await;
    h.fake.space(
        "G_S",
        "同步来的群",
        SpaceKind::Group,
        false,
        &[("upx", "我"), ("uli", "李雷")],
    );
    push(
        &h,
        &h.bot.clone(),
        msg("G_S", "s1", "upx", "在吗", false, false),
    )
    .await;
    exec(
        &h,
        "UPDATE spaces SET member_count = 0 WHERE external_id = 'G_S'",
        &[],
    );
    exec(
        &h,
        "DELETE FROM space_members WHERE space_id = (SELECT id FROM spaces WHERE external_id = 'G_S') AND account_id <> (SELECT account_id FROM connections WHERE id = ?)",
        &[&h.bot],
    );
    push(
        &h,
        &h.bot.clone(),
        msg("G_S", "s2", "upx", "在吗", false, false),
    )
    .await;
    assert_eq!(
        one(
            &h,
            "SELECT member_count FROM spaces WHERE external_id = ?",
            &["G_S"]
        ),
        json!(2)
    );
    let n = one(
        &h,
        "SELECT count(*) FROM space_members WHERE space_id = (SELECT id FROM spaces WHERE external_id = ?)",
        &["G_S"],
    );
    assert!(n.as_i64().unwrap() >= 3, "the bot and both people: {n}");
}

/// A reply drafted for your approval waits in its own conversation with the approval to answer
/// there; 不发出 closes it for good (a reload still reads it discarded), and a draft whose approval
/// was closed without discarding it (dismissed before) no longer reads as waiting.
#[tokio::test(flavor = "multi_thread")]
async fn a_drafted_answer_is_closed_in_its_own_conversation() {
    let h = setup(true).await;
    set_rules(
        &h,
        &h.bot.clone(),
        json!([{"when": {"kind": "mention_or_dm"}, "act": "draft"}]),
    );
    let t = h.thread.clone().unwrap();
    let draft = |id: &'static str| {
        let (h, t) = (&h, t.clone());
        async move {
            let a = push(
                h,
                &h.bot.clone(),
                msg("C_OPS", id, "ulilei", "@林知远 明天", true, false),
            )
            .await;
            let ops = conversations::conv_space(&a.space_id);
            let (turn, _) = t.turns.lock().unwrap().last().cloned().expect("a turn");
            let reply = h
                .env
                .store()
                .messages()
                .add(
                    WS,
                    "steve",
                    "这条看起来是发给林知远的，要我处理吗？",
                    None,
                    &json!({"origin": turn, "ask": {"options": ["不用处理", "请补充完整"]}}),
                    &ops,
                )
                .unwrap();
            let d =
                h.ch.conv_deliver(WS, &reply)
                    .await
                    .unwrap()
                    .expect("drafted");
            let Delivered::Drafted { decision_id, .. } = d else {
                panic!("{d:?}")
            };
            (ops, reply, decision_id)
        }
    };
    let out_of = |ops: &str, reply: &str| {
        let v = conversations::messages_of(&h.ch, WS, &Viewer::bare(WS, ME), ops).unwrap();
        v["messages"]
            .as_array()
            .unwrap()
            .iter()
            .find(|m| m["id"] == json!(reply))
            .expect("the reply")["meta"]["chan_out"]
            .clone()
    };

    let (ops, r1, d1) = draft("q1").await;
    let o = out_of(&ops, &r1);
    assert_eq!(
        (o["status"].clone(), o["decision"].clone()),
        (json!("draft"), json!(d1))
    );
    decide(&h, &d1, "no");
    h.outbox.send_due(&h.ch).await;
    let o = out_of(&ops, &r1);
    assert_eq!(
        (o["status"].clone(), o["decision"].clone()),
        (json!("discarded"), Value::Null)
    );
    h.env.clock.advance(60_000);
    assert_eq!(h.outbox.send_due(&h.ch).await, 0, "never sent");
    assert!(sent(&h).is_empty());
    // answered once: a second 不发出 is refused, nothing changes
    assert!(
        steve_modules::decisions::dispatch(&h.env.modules, &caller(WS, ME), &d1, "no", "")
            .expect("ours")
            .is_err()
    );

    // a draft whose approval was taken out of 待处理 without discarding it
    let (ops, r2, d2) = draft("q2").await;
    exec(
        &h,
        "UPDATE decisions SET status = 'resolved', result = '{\"dismissed\":true}' WHERE id = ?",
        &[&d2],
    );
    let o = out_of(&ops, &r2);
    assert_eq!(
        (o["status"].clone(), o["decision"].clone()),
        (json!("discarded"), Value::Null)
    );
}

/// `push` returns as soon as `state = "ack"` and the per-message reaction lock are held, before
/// the detached provider call finishes; `react_settled` awaits that exact lock without polling.
#[tokio::test(flavor = "multi_thread")]
async fn react_settled_synchronizes_with_detached_ack_provider_without_polling() {
    let h = setup(true).await;
    *h.fake.reaction_delay.lock().unwrap() = Some(std::time::Duration::from_millis(60));
    let m = push(
        &h,
        &h.bot.clone(),
        msg("C_OPS", "r1", "ulilei", "部署挂了怎么办", true, false),
    )
    .await;
    assert_eq!(state(&h, &m.message_id), "ack");
    assert!(
        h.fake.reactions_on("r1").is_empty(),
        "detached provider call is still in flight when synchronous FakeThread::say returns"
    );
    h.ch.react_settled(&m.message_id).await;
    assert_eq!(h.fake.reactions_on("r1"), [ReactionKind::Ack]);
}

// ---------------------------------------------------------------- a trigger word that failed

/// Your trigger word on your own account, its turn in the conversation; the id of that turn.
async fn your_trigger(h: &H, id: &str, text: &str) -> String {
    let t = h.thread.clone().unwrap();
    let before = t.turns.lock().unwrap().len();
    push(h, &h.me.clone(), msg("D_LI", id, "upx", text, false, false)).await;
    let turns = t.turns.lock().unwrap().clone();
    assert_eq!(turns.len(), before + 1, "{turns:?}");
    let (turn, tt) = turns.last().unwrap().clone();
    assert_eq!(tt.meta["chan"]["command"], json!(true));
    turn
}

/// What the conversation said for turn `turn`: the failure steve-core records (`meta.error`).
fn failed_answer(h: &H, turn: &str, text: &str) -> String {
    let conv = h.env.store().messages().get(turn).unwrap().unwrap().space;
    h.env
        .store()
        .messages()
        .add(
            WS,
            "steve",
            text,
            None,
            &json!({"origin": turn, "error": true}),
            &conv,
        )
        .unwrap()
}

fn unsendable_notices(h: &H) -> i64 {
    one(
        h,
        "SELECT count(*) FROM messages WHERE space = 'notify' AND json_extract(meta, '$.notice.kind') = ?",
        &[steve_channels::unsendable::NOTICE_KIND],
    )
    .as_i64()
    .unwrap()
}

/// The owner sent `/ai 看下我现在磁盘情况` on their own account; the answer's model reached for a tool,
/// the turn failed, and before this nothing reached the chat and the failure was in the log only.
/// Now the failure is said in its thread as you (the reason stays in Steve), the reaction ends
/// failed; when the account cannot send (a read-only connection) the conversation records it and you
/// are told once in Steve, not for every message, until the account sends again.
#[tokio::test(flavor = "multi_thread")]
async fn a_failed_trigger_word_is_visible_in_its_thread_and_in_steve() {
    let h = setup(true).await;
    push(
        &h,
        &h.me.clone(),
        msg("D_LI", "d0", "ulilei", "在吗", false, false),
    )
    .await;
    exec(
        &h,
        "UPDATE channel_settings SET trigger = ? WHERE connection_id = ?",
        &[
            &json!({"on": true, "words": ["/ai"], "where": ["dm:*"], "who": ["me"], "reply": "thread", "mark": true}).to_string(),
            &h.me,
        ],
    );
    let turn = your_trigger(&h, "d1", "/ai 看下我现在磁盘情况").await;
    let err = failed_answer(&h, &turn, "（这件事要在机器上动手……）");
    let d = h.ch.conv_deliver(WS, &err).await.unwrap();
    assert!(matches!(d, Some(Delivered::Sent { .. })), "{d:?}");
    let s = sent(&h);
    assert_eq!(
        s.last().unwrap(),
        &format!("[AI] {}", conversations::TRIGGER_FAILED_NOTE),
        "said in the chat as you, marked; the details stay in Steve"
    );
    assert!(h.fake.reactions_on("d1").contains(&ReactionKind::Failed));
    let m = h.env.store().messages().get(&err).unwrap().unwrap();
    assert_eq!(m.meta["chan_out"]["status"], json!("sent"));
    assert_eq!(unsendable_notices(&h), 0);

    // the account cannot send (the channel service forbids writes): recorded, told once
    *h.fake.fail_send.lock().unwrap() = Some("连接只读：渠道服务禁止写入".into());
    for (id, n) in [("d2", 1), ("d3", 1)] {
        let turn = your_trigger(&h, id, "/ai 看下我现在磁盘情况").await;
        let err = failed_answer(&h, &turn, "（没能生成回答）");
        let d = h.ch.conv_deliver(WS, &err).await.unwrap();
        assert!(matches!(d, Some(Delivered::Failed { .. })), "{d:?}");
        let m = h.env.store().messages().get(&err).unwrap().unwrap();
        assert_eq!(m.meta["chan_out"]["status"], json!("failed"));
        assert_eq!(unsendable_notices(&h), n, "one notice, not one per message");
    }
    let notice = one(
        &h,
        "SELECT text FROM messages WHERE space = 'notify' AND json_extract(meta, '$.notice.kind') = ?",
        &[steve_channels::unsendable::NOTICE_KIND],
    );
    let notice = notice.as_str().unwrap();
    assert!(
        notice.contains("连接只读") && notice.contains("接入"),
        "why and where to fix it: {notice}"
    );
    assert_eq!(
        one(
            &h,
            "SELECT count(*) FROM messages WHERE space = 'steve' AND json_extract(meta, '$.notice.kind') = ?",
            &[steve_channels::unsendable::NOTICE_KIND],
        ),
        json!(1),
        "a line in your conversation too"
    );

    // it sends again: a later failure is told again
    *h.fake.fail_send.lock().unwrap() = None;
    let turn = your_trigger(&h, "d4", "/ai 看下我现在磁盘情况").await;
    let err = failed_answer(&h, &turn, "（没能生成回答）");
    assert!(matches!(
        h.ch.conv_deliver(WS, &err).await.unwrap(),
        Some(Delivered::Sent { .. })
    ));
    *h.fake.fail_send.lock().unwrap() = Some("连接只读：渠道服务禁止写入".into());
    let turn = your_trigger(&h, "d5", "/ai 看下我现在磁盘情况").await;
    let err = failed_answer(&h, &turn, "（没能生成回答）");
    h.ch.conv_deliver(WS, &err).await.unwrap();
    assert_eq!(unsendable_notices(&h), 2);
}

/// Someone else may use your trigger word; their turn failed. The note goes out from your own
/// account, so it speaks as you: never “我请 <你> 来看” (you asking yourself).
#[tokio::test(flavor = "multi_thread")]
async fn a_failed_trigger_word_of_someone_else_is_answered_in_your_voice() {
    let h = setup(true).await;
    push(
        &h,
        &h.me.clone(),
        msg("D_LI", "o0", "ulilei", "在吗", false, false),
    )
    .await;
    exec(
        &h,
        "UPDATE channel_settings SET trigger = ? WHERE connection_id = ?",
        &[
            &json!({"on": true, "words": ["/ai"], "where": ["dm:*"], "who": ["*"], "reply": "thread", "mark": true}).to_string(),
            &h.me,
        ],
    );
    // the privacy guard lets an ordinary request through
    h.env.gateway.script(
        "你是一道隐私关卡",
        r#"{"verdict": "safe", "why": "工作问题"}"#,
    );
    let t = h.thread.clone().unwrap();
    let i = push(
        &h,
        &h.me.clone(),
        msg("D_LI", "o1", "ulilei", "/ai 帮我看看磁盘", false, false),
    )
    .await;
    assert_eq!(
        i.response.as_ref().map(|r| r.label.as_str()),
        Some("你的指令，交给对话回答")
    );
    let (turn, tt) = t.turns.lock().unwrap().last().unwrap().clone();
    assert_eq!(tt.meta["chan"]["command"], json!(true));
    assert!(
        !tt.start,
        "not the owner: what it asks for waits for the owner"
    );
    assert!(
        !tt.hands_on,
        "only the owner's own trigger word is handed to an item"
    );
    let err = failed_answer(&h, &turn, "（没能生成回答）");
    let d = h.ch.conv_deliver(WS, &err).await.unwrap();
    assert!(matches!(d, Some(Delivered::Sent { .. })), "{d:?}");
    let s = sent(&h);
    let note = s.last().unwrap();
    assert_eq!(
        note,
        &format!("[AI] {}", conversations::TRIGGER_FAILED_FOR_OTHERS)
    );
    assert!(!note.contains("我请"), "{note}");
}

/// Your trigger word the conversation stored and then failed to take, and the reply could not be
/// written either: what is recorded is the real error, on the turn already stored (not a second
/// one), and the thread hears of it as for any other failure.
#[tokio::test(flavor = "multi_thread")]
async fn a_trigger_word_no_one_took_records_the_real_reason_once() {
    let h = setup(true).await;
    push(
        &h,
        &h.me.clone(),
        msg("D_LI", "n0", "ulilei", "在吗", false, false),
    )
    .await;
    exec(
        &h,
        "UPDATE channel_settings SET trigger = ? WHERE connection_id = ?",
        &[
            &json!({"on": true, "words": ["/ai"], "where": ["dm:*"], "who": ["me"], "reply": "thread", "mark": true}).to_string(),
            &h.me,
        ],
    );
    let t = h.thread.clone().unwrap();
    *t.fail.lock().unwrap() = Some("对话队列已满".into());
    h.env.gateway.unscript(H_SAFETY);
    h.env.gateway.script_fail(H_SAFETY, "模型不可用");
    let before = sent(&h).len();
    let i = push(
        &h,
        &h.me.clone(),
        msg("D_LI", "n1", "upx", "/ai 看下我现在磁盘情况", false, false),
    )
    .await;
    assert_eq!(
        i.response.as_ref().map(|r| r.label.as_str()),
        Some("触发语未接入，只记录")
    );
    let conv = conversations::conv_space(&i.space_id);
    let said = h.env.store().messages().list(WS, &conv, 50).unwrap();
    let turns: Vec<_> = said
        .iter()
        .filter(|m| m.author == "me" && m.meta["chan"]["message"] == json!(i.message_id))
        .collect();
    assert_eq!(turns.len(), 1, "the turn once: {said:#?}");
    assert_eq!(
        turns[0].text, "看下我现在磁盘情况",
        "the instruction, as any turn"
    );
    let why = said
        .iter()
        .find(|m| m.author == "steve" && m.meta["origin"] == json!(turns[0].id))
        .expect("why");
    assert!(why.text.contains("对话队列已满"), "{}", why.text);
    assert!(!why.text.contains("没有接上"), "{}", why.text);
    assert_eq!(why.meta["error"], json!(true));
    // said in its thread as you, like any failed trigger word
    let s = sent(&h);
    assert_eq!(s.len(), before + 1, "{s:?}");
    assert_eq!(
        s.last().unwrap(),
        &format!("[AI] {}", conversations::TRIGGER_FAILED_NOTE)
    );
    assert!(h.fake.reactions_on("n1").contains(&ReactionKind::Failed));
}

/// An account the channel refuses writes for good (read-only, remembered as a write block): its own
/// card is the one notice about it, so a failed trigger word adds no second one, however often.
#[tokio::test(flavor = "multi_thread")]
async fn a_read_only_account_gets_the_write_block_notice_only() {
    let h = setup(true).await;
    push(
        &h,
        &h.me.clone(),
        msg("D_LI", "b0", "ulilei", "在吗", false, false),
    )
    .await;
    exec(
        &h,
        "UPDATE channel_settings SET trigger = ? WHERE connection_id = ?",
        &[
            &json!({"on": true, "words": ["/ai"], "where": ["dm:*"], "who": ["me"], "reply": "thread", "mark": true}).to_string(),
            &h.me,
        ],
    );
    *h.fake.block_send.lock().unwrap() = Some("write_policy forbidden".into());
    for id in ["b1", "b2"] {
        let turn = your_trigger(&h, id, "/ai 看下我现在磁盘情况").await;
        let err = failed_answer(&h, &turn, "（没能生成回答）");
        let d = h.ch.conv_deliver(WS, &err).await.unwrap();
        assert!(
            matches!(
                d,
                Some(Delivered::Failed { .. } | Delivered::Blocked { .. })
            ),
            "{d:?}"
        );
    }
    assert_eq!(unsendable_notices(&h), 0, "the write block's card says it");
}

// ---------------------------------------------------------------- the record in 对话 (#243)

fn record_of(
    h: &H,
    user: &str,
    space: &str,
    q: &steve_channels::conv_record::RecordQuery,
) -> ApiResult<Value> {
    let conv = conversations::conv_space(space);
    h.env
        .store()
        .reader(|c| steve_channels::conv_record::record(c, WS, &Viewer::bare(WS, user), &conv, q))
}

/// The handling of the received message `id` and the out state of Steve's message `id` in a record.
fn rec<'a>(r: &'a Value, id: &str) -> &'a Value {
    r["records"]
        .as_array()
        .unwrap()
        .iter()
        .find(|x| x["id"] == id)
        .unwrap_or_else(|| panic!("{id} not in {r}"))
}

#[tokio::test(flavor = "multi_thread")]
async fn the_record_shows_every_message_with_what_steve_did() {
    let h = setup(false).await;
    let bot = h.bot.clone();
    // answered
    let a = push(
        &h,
        &bot,
        msg("C_OPS", "o1", "ulilei", "部署挂了怎么办", true, false),
    )
    .await;
    // no rule for a message nobody @-mentioned: only recorded
    let b = push(
        &h,
        &bot,
        msg("C_OPS", "o2", "uhan", "我先去吃饭", false, false),
    )
    .await;
    // the answer could not go out
    *h.fake.fail_send.lock().unwrap() = Some("飞书暂时不可用".into());
    let c = push(
        &h,
        &bot,
        msg("C_OPS", "o3", "uhan", "那回滚呢", true, false),
    )
    .await;
    *h.fake.fail_send.lock().unwrap() = None;
    let space = one(
        &h,
        "SELECT space_id FROM chan_messages WHERE id = ?",
        &[&a.message_id],
    );
    let space = space.as_str().unwrap().to_string();

    let r = record_of(&h, ME, &space, &Default::default()).unwrap();
    let recs = r["records"].as_array().unwrap();
    assert_eq!(recs.len(), 5, "three received, two answers: {r}");
    assert!(
        recs.windows(2)
            .all(|w| w[0]["created"].as_i64() <= w[1]["created"].as_i64()),
        "oldest first"
    );
    assert_eq!(r["has_more"], false);
    assert_eq!(rec(&r, &a.message_id)["handling"]["state"], "answered");
    let answer = a.response.unwrap().out_message_id.unwrap();
    assert_eq!(rec(&r, &answer)["out"]["status"], "sent");
    let skipped = &rec(&r, &b.message_id)["handling"];
    assert!(
        matches!(skipped["state"].as_str(), Some("recorded" | "none")),
        "{skipped}"
    );
    let failed = &rec(&r, &c.message_id)["handling"];
    assert_eq!(failed["state"], "failed", "{failed}");
    assert!(
        failed["error"].as_str().unwrap().contains("飞书暂时不可用"),
        "{failed}"
    );
    let failed_out = c.response.unwrap().out_message_id.unwrap();
    assert_eq!(rec(&r, &failed_out)["out"]["status"], "failed");
    let out = &rec(&r, &failed_out)["out"];
    assert!(
        out["error"].as_str().unwrap().contains("飞书暂时不可用"),
        "{out}"
    );
    assert!(out["decision"].is_string(), "its retry card: {out}");

    // a page at a time, without losing or repeating a message
    let q = |before: Option<&Value>| steve_channels::conv_record::RecordQuery {
        before: before.and_then(|n| n["before"].as_i64()),
        before_id: before.and_then(|n| n["before_id"].as_str().map(String::from)),
        limit: Some(2),
    };
    let mut seen = Vec::new();
    let mut next: Option<Value> = None;
    loop {
        let p = record_of(&h, ME, &space, &q(next.as_ref())).unwrap();
        let ids: Vec<String> = p["records"]
            .as_array()
            .unwrap()
            .iter()
            .map(|x| x["id"].as_str().unwrap().into())
            .collect();
        seen.splice(0..0, ids);
        if p["has_more"] == false {
            assert_eq!(p["next"], Value::Null);
            break;
        }
        next = Some(p["next"].clone());
    }
    let all: Vec<String> = recs
        .iter()
        .map(|x| x["id"].as_str().unwrap().into())
        .collect();
    assert_eq!(seen, all);
    let bad = steve_channels::conv_record::RecordQuery {
        before: Some(1),
        ..Default::default()
    };
    assert!(
        record_of(&h, ME, &space, &bad).is_err(),
        "a cursor needs both halves"
    );
    let big = steve_channels::conv_record::RecordQuery {
        limit: Some(steve_channels::conv_record::RECORD_MAX + 1),
        ..Default::default()
    };
    assert!(record_of(&h, ME, &space, &big).is_err());

    // a space Steve never answered in has no conversation, and is listed all the same
    let quiet = push(
        &h,
        &bot,
        msg("C_ACME", "q1", "ulilei", "周五见", false, false),
    )
    .await;
    let quiet_space = one(
        &h,
        "SELECT space_id FROM chan_messages WHERE id = ?",
        &[&quiet.message_id],
    );
    // in the list, with what is new for you; reading it clears that
    let now =
        h.ch.now()
            .max(recs.last().unwrap()["created"].as_i64().unwrap());
    let list = |user: &str, connection: Option<&str>| {
        h.env
            .store()
            .reader(|c| {
                conversations::list_page(
                    c,
                    WS,
                    &Viewer::bare(WS, user),
                    &conversations::ListQuery {
                        connection: connection.map(String::from),
                        ..Default::default()
                    },
                    now,
                )
            })
            .unwrap()
    };
    let l = list(ME, None);
    let row = l
        .conversations
        .iter()
        .find(|x| x["space_id"] == space.as_str())
        .expect("listed");
    assert_eq!(row["unread"], 3);
    assert_eq!(row["connection"], json!(bot));
    let q = l
        .conversations
        .iter()
        .find(|x| x["space_id"] == quiet_space)
        .expect("a space without a conversation");
    assert_eq!(
        (
            q["count"].clone(),
            q["last_text"].clone(),
            q["unread"].clone()
        ),
        (json!(0), json!("周五见"), json!(1))
    );
    assert_eq!(list(ME, Some(&bot)).total, l.total);
    assert_eq!(list(ME, Some("con_missing")).total, 0);
    let entries = h
        .env
        .store()
        .reader(|c| steve_channels::conv_record::channels(&h.ch, c, &Viewer::bare(WS, ME), now))
        .unwrap();
    let e = entries
        .iter()
        .find(|e| e.connection == bot)
        .expect("the bot");
    assert_eq!((e.conversations, e.unread), (2, 2));
    let conv = conversations::conv_space(&space);
    let last = recs[2]["created"].as_i64().unwrap();
    h.env
        .store()
        .transaction(|tx| {
            steve_channels::conv_record::mark_read(tx, WS, &Viewer::bare(WS, ME), &conv, last, now)
        })
        .unwrap();
    let unread_after = |l: conversations::ListPage| {
        l.conversations
            .iter()
            .find(|x| x["space_id"] == space.as_str())
            .unwrap()["unread"]
            .clone()
    };
    let left = recs[3..]
        .iter()
        .filter(|x| x["direction"] == "in" && x["created"].as_i64() > Some(last))
        .count();
    assert_eq!(unread_after(list(ME, None)), json!(left));
    // the marker never moves back
    h.env
        .store()
        .transaction(|tx| {
            steve_channels::conv_record::mark_read(tx, WS, &Viewer::bare(WS, ME), &conv, 0, now)
        })
        .unwrap();
    assert_eq!(unread_after(list(ME, None)), json!(left));
    h.env
        .store()
        .transaction(|tx| {
            steve_channels::conv_record::mark_read(tx, WS, &Viewer::bare(WS, ME), &conv, now, now)
        })
        .unwrap();
    assert_eq!(unread_after(list(ME, None)), json!(0));
    // another workspace member keeps their own marker
    assert_eq!(
        unread_after(list(steve_modules::testkit::OTHER, None)),
        json!(3)
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_record_of_your_own_account_is_yours_alone() {
    let h = setup(false).await;
    let me = h.me.clone();
    let i = push(&h, &me, msg("D_LI", "d1", "ulilei", "在吗", false, false)).await;
    let space = one(
        &h,
        "SELECT space_id FROM chan_messages WHERE id = ?",
        &[&i.message_id],
    );
    let space = space.as_str().unwrap().to_string();
    let r = record_of(&h, ME, &space, &Default::default()).unwrap();
    assert_eq!(rec(&r, &i.message_id)["direction"], "in");
    let other = steve_modules::testkit::OTHER;
    assert!(record_of(&h, other, &space, &Default::default()).is_err());
    let conv = conversations::conv_space(&space);
    assert!(
        h.env
            .store()
            .transaction(|tx| steve_channels::conv_record::mark_read(
                tx,
                WS,
                &Viewer::bare(WS, other),
                &conv,
                1,
                2
            ))
            .is_err()
    );
    let theirs = h
        .env
        .store()
        .reader(|c| steve_channels::conv_record::channels(&h.ch, c, &Viewer::bare(WS, other), 0))
        .unwrap();
    assert!(
        theirs.iter().all(|e| e.connection != me),
        "your account is not in their switcher"
    );
    let l = h
        .env
        .store()
        .reader(|c| {
            conversations::list_page(c, WS, &Viewer::bare(WS, other), &Default::default(), 0)
        })
        .unwrap();
    assert!(
        l.conversations
            .iter()
            .all(|x| x["space_id"] != space.as_str())
    );
}

/// A turn whose answer was an error (`meta.error`, as steve-core writes a failed turn) shows failed
/// with that error as the reason, under its turn, before and after its reaction is ended.
#[tokio::test(flavor = "multi_thread")]
async fn a_turn_whose_answer_failed_shows_failed_with_the_reason() {
    let h = setup(true).await;
    let a = push(
        &h,
        &h.bot.clone(),
        msg("C_OPS", "o1", "ulilei", "部署挂了怎么办", true, false),
    )
    .await;
    let (turn, _) = h.thread.as_ref().unwrap().turns.lock().unwrap()[0].clone();
    let r = record_of(&h, ME, &a.space_id, &Default::default()).unwrap();
    assert_eq!(rec(&r, &a.message_id)["turn"], json!(turn));
    assert_eq!(rec(&r, &a.message_id)["handling"]["state"], "working");
    let ops = conversations::conv_space(&a.space_id);
    let err = h
        .env
        .store()
        .messages()
        .add(
            WS,
            "steve",
            "（出错了：模型超时）",
            None,
            &json!({"origin": turn, "error": true}),
            &ops,
        )
        .unwrap();
    let failed = |h: &H| {
        let r = record_of(h, ME, &a.space_id, &Default::default()).unwrap();
        let x = rec(&r, &a.message_id).clone();
        assert_eq!(x["turn"], json!(turn));
        let hd = &x["handling"];
        assert_eq!(hd["state"], "failed", "{hd}");
        assert!(
            hd["error"].as_str().unwrap().contains("模型超时"),
            "the turn's error is the reason: {hd}"
        );
        assert_eq!(hd["main_turn"], Value::Null);
    };
    // the reaction is still “处理中”: the turn's error alone says it failed
    assert_eq!(state(&h, &a.message_id), json!("ack"));
    failed(&h);
    h.ch.conv_deliver(WS, &err).await.unwrap();
    assert_eq!(state(&h, &a.message_id), json!("failed"));
    failed(&h);
}

/// Your `/bot` command where your account may not send as you is answered in your web conversation:
/// the record shows what Steve said there (answered, or failed with the reason) and links that turn,
/// though the space's “处理中” reaction is never taken off.
#[tokio::test(flavor = "multi_thread")]
async fn a_command_answered_in_your_web_conversation_shows_that_answer() {
    let h = setup(true).await;
    exec(
        &h,
        "UPDATE connections SET caps = '[\"msg.receive\"]' WHERE id = ?",
        &[&h.me],
    );
    exec(
        &h,
        "UPDATE channel_settings SET trigger = ? WHERE connection_id = ?",
        &[
            &json!({"on": true, "words": ["/bot"], "where": ["dm:*"], "mark": true}).to_string(),
            &h.me,
        ],
    );
    let me = h.me.clone();
    let main = |h: &H, n: usize| -> String {
        let turns = h.thread.as_ref().unwrap().turns.lock().unwrap().clone();
        let (id, t) = turns[n].clone();
        assert_eq!(t.space, conversations::MAIN_CONV, "{t:?}");
        id
    };
    let a = push(
        &h,
        &me,
        msg("D_LI", "d1", "upx", "/bot 看下磁盘", false, false),
    )
    .await;
    assert_eq!(
        a.response.as_ref().unwrap().label,
        steve_channels::respond::NO_SEND_AS_YOU_COMMAND
    );
    let t1 = main(&h, 0);
    let handling = |h: &H, id: &str| {
        let r = record_of(h, ME, &a.space_id, &Default::default()).unwrap();
        let x = rec(&r, id).clone();
        assert_eq!(x["turn"], Value::Null, "no turn in the chat's conversation");
        x["handling"].clone()
    };
    let x = handling(&h, &a.message_id);
    assert_eq!(
        (x["state"].clone(), x["main_turn"].clone()),
        (json!("working"), json!(t1))
    );
    let msgs = h.env.store().messages();
    msgs.add(
        WS,
        "steve",
        "还剩 20G。",
        None,
        &json!({"origin": t1}),
        conversations::MAIN_CONV,
    )
    .unwrap();
    assert_eq!(state(&h, &a.message_id), json!("ack"));
    let x = handling(&h, &a.message_id);
    assert_eq!(
        (x["state"].clone(), x["main_turn"].clone()),
        (json!("answered"), json!(t1))
    );

    let b = push(
        &h,
        &me,
        msg("D_LI", "d2", "upx", "/bot 清一下缓存", false, false),
    )
    .await;
    let t2 = main(&h, 1);
    msgs.add(
        WS,
        "steve",
        "（出错了：只读策略拒绝了工具）",
        None,
        &json!({"origin": t2, "error": true}),
        conversations::MAIN_CONV,
    )
    .unwrap();
    let x = handling(&h, &b.message_id);
    assert_eq!(
        (x["state"].clone(), x["main_turn"].clone()),
        (json!("failed"), json!(t2))
    );
    assert!(
        x["error"].as_str().unwrap().contains("只读策略拒绝了工具"),
        "{x}"
    );
    // another space's command in your web conversation is not this one's
    assert_eq!(handling(&h, &a.message_id)["state"], "answered");
}

/// The event of a message answered on the conversation path (a bot's) says what became of it once
/// the composer read it: respond() hands it to the conversation and writes nothing (the answer is
/// not known yet), then `conv_deliver` records a skip with the composer's reason, or a question to
/// you as `need` with the decision it opened. Nothing goes out to the space either way.
async fn conversation_trail(composed: &str, id: &str, text: &str) -> (H, String, Value) {
    let h = setup_answering(true, composed).await;
    let t = h.thread.clone().unwrap();
    // a message to the bot: handed to the conversation
    let a = push(
        &h,
        &h.bot.clone(),
        msg("C_OPS", id, "ulilei", text, true, false),
    )
    .await;
    assert_eq!(a.response.as_ref().unwrap().label, "交给对话回答");
    assert_eq!(
        event_of(&h, &a.message_id),
        (
            json!("recorded"),
            json!(steve_flow::ingest::LABEL_NO_FLOW),
            json!(""),
            Value::Null
        ),
        "respond()'s hand-off writes nothing on the event: it is as ingest left it"
    );
    let (turn, _) = t.turns.lock().unwrap().last().cloned().expect("a turn");
    let said = h.ch.conv_answer(WS, &turn).await.expect("answer");
    let reply = h
        .env
        .store()
        .messages()
        .add(
            WS,
            "steve",
            &said,
            None,
            &json!({"origin": turn}),
            &conversations::conv_space(&a.space_id),
        )
        .unwrap();
    let d = h.ch.conv_deliver(WS, &reply).await.unwrap();
    assert!(d.is_none(), "not delivered: {d:?}");
    assert!(sent(&h).is_empty(), "nothing said in the space");
    let opened = one(
        &h,
        "SELECT count(*) FROM decisions WHERE subject_id = ? AND kind = 'approve'",
        &[&a.message_id],
    );
    assert_eq!(opened, json!(0), "no draft");
    (h, a.message_id, json!(said))
}

/// The message's event: outcome, label, reason, decision.
fn event_of(h: &H, message: &str) -> (Value, Value, Value, Value) {
    let q = |col: &str| {
        one(
            h,
            &format!(
                "SELECT e.{col} FROM ext_events e JOIN chan_messages m ON m.event_id = e.id WHERE m.id = ?"
            ),
            &[message],
        )
    };
    (
        q("outcome"),
        q("outcome_label"),
        q("reason"),
        q("decision_id"),
    )
}

#[tokio::test(flavor = "multi_thread")]
async fn a_composer_skip_on_the_conversation_path_is_recorded_with_its_reason() {
    let (h, mid, _) = conversation_trail(
        r#"{"outcome": "skip", "text": "玩笑，不需要回", "needs_approval": false, "why": "闲聊"}"#,
        "t1",
        "@你 哥，你去年也是这么说的",
    )
    .await;
    assert_eq!(
        event_of(&h, &mid),
        (
            json!("recorded"),
            json!(format!("{}玩笑，不需要回", conversations::NO_REPLY)),
            json!(format!("{}：闲聊", steve_channels::respond::TRAIL_COMPOSER)),
            Value::Null
        )
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_question_on_the_conversation_path_marks_the_event_as_needing_you() {
    let (h, mid, said) = conversation_trail(
        r#"{"outcome": "ask_owner", "text": "这条要回吗？", "needs_approval": false, "why": "不确定"}"#,
        "t2",
        "@你 明天",
    )
    .await;
    assert_eq!(said, json!("这条要回吗？"));
    let opened = one(
        &h,
        "SELECT id FROM decisions WHERE subject_id = ? AND status = 'open'",
        &[&mid],
    );
    assert!(opened.is_string(), "a question opened: {opened:?}");
    assert!(
        one(
            &h,
            "SELECT question FROM decisions WHERE id = ?",
            &[opened.as_str().unwrap()]
        )
        .as_str()
        .unwrap()
        .contains("这条要回吗？"),
        "the question carries what Steve asks"
    );
    assert_eq!(
        event_of(&h, &mid),
        (
            json!("need"),
            json!(steve_channels::replies::ASKED),
            json!(""),
            opened
        )
    );
}

// ---------------------------------------------------------------- owner voice: only the composer speaks as you

/// `/bot` works for you everywhere on your account.
fn bot_command_everywhere(h: &H) {
    exec(
        h,
        "UPDATE channel_settings SET trigger = ? WHERE connection_id = ?",
        &[
            &json!({"on": true, "words": ["/bot"], "where": ["*"], "mark": true}).to_string(),
            &h.me,
        ],
    );
}

/// Steve's message `text` about turn `origin` in the conversation of `space`, as steve-core's thread
/// posts it (`meta` and the turn as its origin).
fn steve_says(
    h: &H,
    space: &str,
    origin: &str,
    text: &str,
    item: Option<&str>,
    meta: Value,
) -> String {
    let mut meta = meta;
    meta["origin"] = json!(origin);
    h.env
        .store()
        .messages()
        .add(
            WS,
            "steve",
            text,
            item,
            &meta,
            &conversations::conv_space(space),
        )
        .unwrap()
}

/// Everything drafted or sent in any space, oldest first.
fn out_texts(h: &H) -> Vec<String> {
    h.env
        .store()
        .with_conn(|c| {
            let mut st = c.prepare(
                "SELECT coalesce(text, '') FROM chan_messages WHERE direction = 'out' ORDER BY created, rowid",
            )?;
            let v = st
                .query_map([], |r| r.get::<_, String>(0))?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(v)
        })
        .expect("out")
}

/// On your account a message is the composer's alone: the router never sees it (no turn), the
/// composer's reply is a draft for your approval, and nothing reacts as you on the sender's message.
#[tokio::test(flavor = "multi_thread")]
async fn on_your_account_a_message_never_reaches_the_router() {
    let h = setup(true).await;
    let t = h.thread.clone().unwrap();
    let i = push(
        &h,
        &h.me.clone(),
        msg("C_OPS", "v1", "ulilei", "@你 周四能上线吗", true, false),
    )
    .await;
    let r = i.response.unwrap();
    assert_eq!(r.label, "等你确认回复");
    assert!(t.turns.lock().unwrap().is_empty(), "no turn for the router");
    assert_eq!(out_texts(&h), ["先看部署日志，再按回滚手册处理。"]);
    assert!(sent(&h).is_empty());
    // the space's conversation still has the turn and the answer, shown as the draft it is
    let page = conversations::messages_of(
        &h.ch,
        WS,
        &Viewer::bare(WS, ME),
        &conversations::conv_space(&i.space_id),
    )
    .expect("page");
    let answer = page["messages"]
        .as_array()
        .unwrap()
        .last()
        .cloned()
        .unwrap();
    assert_eq!(answer["author"], json!("steve"));
    assert_eq!(answer["meta"]["chan_out"]["status"], json!("draft"));
    assert_eq!(answer["meta"]["chan_out"]["decision"], json!(r.decision_id));
    h.ch.react_settled(&i.message_id).await;
    assert!(h.fake.reactions_on("v1").is_empty(), "{:?}", log(&h));
    assert_eq!(
        one(
            &h,
            "SELECT count(*) FROM chan_reactions WHERE message_id = ?",
            &[&i.message_id]
        ),
        json!(0),
        "no reaction as you on somebody else's message"
    );
}

/// Production drafted, as you, the router's “前面还有 3 个问题在排队…” and its clarifying question with
/// the options spelled out (“（可以回复：A / B）”). Neither is the composer's reply to the turn, so
/// neither is said as you: not the queue sentence marked as a system text, not an older one
/// without the mark, not a question or a suggestion card, not an item's receipt; an error is not
/// said either, only the trigger word's fixed failure note (#254). The composer's own answer to the
/// same turn goes out exactly as it wrote it, as your trigger words' settings say (`[AI]`-marked).
#[tokio::test(flavor = "multi_thread")]
async fn the_routers_queue_and_question_never_become_your_draft() {
    let h = setup(true).await;
    let t = h.thread.clone().unwrap();
    bot_command_everywhere(&h);
    let i = push(
        &h,
        &h.me.clone(),
        msg("C_OPS", "v2", "upx", "/bot 跟他说周四可以", false, false),
    )
    .await;
    assert_eq!(i.response.unwrap().label, "你的指令，交给对话回答");
    let (turn, _) = t.turns.lock().unwrap().last().cloned().expect("a turn");
    let space = i.space_id.clone();
    for (text, item, meta) in [
        (
            steve_core::thread::research::QUEUE_FULL,
            None,
            json!({ steve_core::thread::research::SYSTEM: true }),
        ),
        (steve_core::thread::research::QUEUE_FULL, None, json!({})),
        (
            "你说的“很稳定”具体是指什么？",
            None,
            json!({"ask": {"options": ["指某个产品", "指某项服务"]}}),
        ),
        (
            "要哪种？",
            None,
            json!({"suggest": {"choices": [{"id": "here", "label": "在这里出方案"}]}}),
        ),
        (
            "好，停下了。要我换个方式就直接说。",
            None,
            json!({"stopped": true}),
        ),
        ("建好了", Some("ST-9"), json!({})),
    ] {
        let id = steve_says(&h, &space, &turn, text, item, meta);
        let d = h.ch.conv_deliver(WS, &id).await.unwrap();
        assert_eq!(d, None, "{text}");
    }
    assert!(out_texts(&h).is_empty(), "{:?}", out_texts(&h));
    assert!(sent(&h).is_empty());
    // an error: only the fixed note of a failed trigger word, never the error's text
    let id = steve_says(
        &h,
        &space,
        &turn,
        "（出错了：超时）",
        None,
        json!({"error": true}),
    );
    let d = h.ch.conv_deliver(WS, &id).await.unwrap();
    assert!(matches!(d, Some(Delivered::Sent { .. })), "{d:?}");
    assert_eq!(
        sent(&h),
        [format!("[AI] {}", conversations::TRIGGER_FAILED_NOTE)]
    );
    assert_eq!(
        one(
            &h,
            "SELECT work_item_id FROM chan_messages WHERE id = ?",
            &[&i.message_id]
        ),
        json!("ST-9"),
        "the item is still linked to your message"
    );

    // the composer's answer: kept on the turn, said as it wrote it (no options added)
    let said = h.ch.conv_answer(WS, &turn).await.expect("answer");
    assert_eq!(
        one(
            &h,
            "SELECT json_extract(meta, '$.chan_gen.outcome') FROM messages WHERE id = ?",
            &[&turn]
        ),
        json!("reply")
    );
    let id = steve_says(
        &h,
        &space,
        &turn,
        &said,
        None,
        json!({"ask": {"options": ["A", "B"]}}),
    );
    let d = h.ch.conv_deliver(WS, &id).await.unwrap();
    assert!(matches!(d, Some(Delivered::Sent { .. })), "{d:?}");
    let last = sent(&h).last().cloned().unwrap();
    assert_eq!(last, format!("[AI] {}", said.trim()));
    assert!(!last.contains(conversations::OPTIONS_HEAD));
    // the page shows what the composer decided, not the knowledge it used
    let page = conversations::messages_of(
        &h.ch,
        WS,
        &Viewer::bare(WS, ME),
        &conversations::conv_space(&space),
    )
    .expect("page");
    let kept = page["messages"]
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["id"] == json!(turn))
        .map(|m| m["meta"][conversations::CHAN_GEN].clone())
        .expect("the turn");
    assert_eq!(kept["outcome"], json!("reply"));
    assert!(
        kept.get("hits").is_none() && kept.get("text").is_none(),
        "{kept}"
    );
}

/// On your account Steve's fixed receipts are never said as you: an item made from a message and a
/// handoff to you say nothing in the space (they reach you in Steve).
#[tokio::test(flavor = "multi_thread")]
async fn on_your_account_no_receipt_is_said_as_you() {
    let h = setup(false).await;
    let me = h.me.clone();
    set_rules(
        &h,
        &me,
        json!([{"when": {"kind": "mention_or_dm"}, "act": "create_item", "mode": "start"}]),
    );
    let i = push(
        &h,
        &me,
        msg(
            "C_OPS",
            "v3",
            "ulilei",
            "@你 告警脚本坏了，修一下",
            true,
            false,
        ),
    )
    .await;
    assert!(i.response.unwrap().item_id.is_some(), "the item is made");
    set_rules(
        &h,
        &me,
        json!([{"when": {"kind": "mention_or_dm"}, "act": "handoff"}]),
    );
    let j = push(
        &h,
        &me,
        msg("C_OPS", "v4", "ulilei", "@你 这个你来定", true, false),
    )
    .await;
    assert!(
        j.response.unwrap().decision_id.is_some(),
        "the handoff reaches you"
    );
    assert!(out_texts(&h).is_empty(), "{:?}", out_texts(&h));
    assert!(sent(&h).is_empty());
}

/// A full queue is the thread's own state: never said, even by a bot; the message reaches you as a
/// handoff instead of going unanswered.
#[tokio::test(flavor = "multi_thread")]
async fn a_full_queue_is_never_said_and_reaches_you_instead() {
    let h = setup(true).await;
    let t = h.thread.clone().unwrap();
    let i = push(
        &h,
        &h.bot.clone(),
        msg("C_OPS", "q1", "ulilei", "部署挂了怎么办", true, false),
    )
    .await;
    let (turn, _) = t.turns.lock().unwrap().last().cloned().expect("a turn");
    let id = steve_says(
        &h,
        &i.space_id,
        &turn,
        steve_core::thread::research::QUEUE_FULL,
        None,
        json!({ steve_core::thread::research::SYSTEM: true }),
    );
    assert_eq!(h.ch.conv_deliver(WS, &id).await.unwrap(), None);
    assert!(sent(&h).is_empty());
    assert!(out_texts(&h).is_empty());
    assert_eq!(
        one(
            &h,
            "SELECT kind FROM decisions WHERE subject_id = ? AND status = 'open'",
            &[&i.message_id]
        ),
        json!("ask")
    );
}

// ---------------------------------------------------------------- the identity block and the chat read tools

use steve_channels::im_context::{CLOSE, OPEN, RULE_DATA, RULE_NOT_OWNER};
use steve_channels::im_conv_mcp;

fn set_trigger(h: &H, v: Value) {
    exec(
        h,
        "UPDATE channel_settings SET trigger = ? WHERE connection_id = ?",
        &[&v.to_string(), &h.me],
    );
}

/// The research context of turn `turn` (in its own conversation).
fn research_of(h: &H, turn: &str) -> String {
    let conv = h.env.store().messages().get(turn).unwrap().unwrap().space;
    h.ch.conv_research_context(WS, &conv, turn)
}

/// The identity block in `text`.
fn block_of(text: &str) -> String {
    let (a, b) = (
        text.find(OPEN).expect("the block"),
        text.find(CLOSE).expect("its end"),
    );
    text[a..b + CLOSE.len()].to_string()
}

/// The composer's prompt for turn `turn`: its identity block comes right after the safety rules.
async fn composed_prompt(h: &H, turn: &str) -> String {
    h.env.gateway.prompts.lock().unwrap().clear();
    h.ch.conv_answer(WS, turn).await.expect("answer");
    let p = h
        .env
        .gateway
        .prompts
        .lock()
        .unwrap()
        .iter()
        .find(|p| p.starts_with(H_SAFETY))
        .cloned()
        .expect("the composer's prompt");
    let at = p.find(OPEN).expect("the block in the prompt");
    let first_layer = p
        .find(steve_channels::compose::H_BOT)
        .expect("the bot layer");
    assert!(at < first_layer, "the block is the first layer: {p}");
    p
}

/// Your own `/ai` in a private chat on your account: you, as you, quoting the message (a private
/// chat has no threads), `[AI]`-marked, the instruction and the source message named.
#[tokio::test(flavor = "multi_thread")]
async fn the_identity_block_of_your_trigger_word_in_a_private_chat() {
    let h = setup(true).await;
    push(
        &h,
        &h.me.clone(),
        msg("D_LI", "d0", "ulilei", "在吗", false, false),
    )
    .await;
    set_trigger(
        &h,
        json!({"on": true, "words": ["/ai"], "where": ["dm:*"], "who": ["me"], "reply": "thread", "mark": true}),
    );
    let turn = your_trigger(&h, "d1", "/ai 看下告警").await;
    let cm = one(
        &h,
        "SELECT id FROM chan_messages WHERE external_id = ?",
        &["d1"],
    );
    let cm = cm.as_str().unwrap();
    let ctx = research_of(&h, &turn);
    assert!(
        ctx.starts_with(OPEN),
        "first in the research context: {ctx}"
    );
    assert!(ctx.contains(conversations::CONV_TOOLS_NOTE), "{ctx}");
    assert!(ctx.contains("【对话卡】"), "the card stays: {ctx}");
    let b = block_of(&ctx);
    for want in [
        "kind=\"own\"",
        "本人账号发出</as_whom>",
        "kind=\"dm\"",
        "placement=\"quote\"",
        "is_owner=\"true\" typed_by_human=\"true\"",
        &format!("<trigger word=\"/ai\" instruction=\"看下告警\" source_message=\"{cm}\"/>"),
        "开头加 [AI]",
        RULE_DATA,
    ] {
        assert!(b.contains(want), "{want} in {b}");
    }
    assert!(!b.contains(RULE_NOT_OWNER), "you asked: {b}");
    let p = composed_prompt(&h, &turn).await;
    assert_eq!(block_of(&p), b, "the composer gets the same block");
}

/// Your own `/ai` in a group with outside members: they are listed by name and organization, and
/// only what may be said outside may be said.
#[tokio::test(flavor = "multi_thread")]
async fn the_identity_block_of_a_group_with_external_members() {
    let h = setup(true).await;
    push(
        &h,
        &h.me.clone(),
        msg("C_ACME", "a0", "ulilei", "大家好", false, false),
    )
    .await;
    let party = one(
        &h,
        "SELECT sender_party_id FROM chan_messages WHERE external_id = ?",
        &["a0"],
    );
    let party = party.as_str().unwrap().to_string();
    exec(
        &h,
        "INSERT INTO parties (id, workspace_id, type, name, org_kind, created_by, created, updated) VALUES ('org_acme', ?, 'org', 'ACME 客户', 'customer', 'u', 1, 1)",
        &[WS],
    );
    exec(
        &h,
        "INSERT INTO links (workspace_id, subject_type, subject_id, predicate, object_type, object_id, source, created) VALUES (?, 'party', ?, 'works_at', 'party', 'org_acme', 'manual', 1)",
        &[WS, &party],
    );
    set_trigger(
        &h,
        json!({"on": true, "words": ["/ai"], "where": ["*"], "who": ["me"], "reply": "thread", "mark": true}),
    );
    let t = h.thread.clone().unwrap();
    push(
        &h,
        &h.me.clone(),
        msg("C_ACME", "a1", "upx", "/ai 总结一下", false, false),
    )
    .await;
    let (turn, tt) = t.turns.lock().unwrap().last().unwrap().clone();
    assert_eq!(tt.meta["chan"]["command"], json!(true));
    assert!(!tt.research, "outside members: answered in place only");
    let b = block_of(&research_of(&h, &turn));
    for want in [
        "kind=\"own\"",
        "kind=\"group\"",
        "external=\"1: 李雷(外部·ACME 客户)\"",
        "placement=\"thread\"",
        "<disclosure level=\"public\"/>",
        "含外部或身份未知的成员",
        "instruction=\"总结一下\"",
    ] {
        assert!(b.contains(want), "{want} in {b}");
    }
    let p = composed_prompt(&h, &turn).await;
    assert!(block_of(&p).contains("external=\"1: 李雷(外部·ACME 客户)\""));
}

/// A bot account answering in a group: the bot speaks, unmarked; the asker is not the owner, so the
/// block says not to commit or take positions for the owner; no trigger.
#[tokio::test(flavor = "multi_thread")]
async fn the_identity_block_of_a_bot_in_a_group() {
    let h = setup(true).await;
    let t = h.thread.clone().unwrap();
    push(
        &h,
        &h.bot.clone(),
        msg("C_OPS", "o1", "ulilei", "部署失败了", true, false),
    )
    .await;
    let (turn, _) = t.turns.lock().unwrap().last().unwrap().clone();
    let b = block_of(&research_of(&h, &turn));
    for want in [
        "kind=\"bot\"",
        "发出</as_whom>",
        "kind=\"group\"",
        "is_owner=\"false\" typed_by_human=\"true\"",
        "不加 [AI]",
        RULE_DATA,
        RULE_NOT_OWNER,
    ] {
        assert!(b.contains(want), "{want} in {b}");
    }
    assert!(!b.contains("本人账号"), "{b}");
    assert!(!b.contains("<trigger"), "{b}");
}

/// Someone else uses your trigger word on your account: the block says the sender is not the owner
/// and carries the rule against committing for the owner, in research and in the composer.
#[tokio::test(flavor = "multi_thread")]
async fn the_identity_block_of_someone_elses_trigger_word() {
    let h = setup(true).await;
    push(
        &h,
        &h.me.clone(),
        msg("C_OPS", "g0", "ulilei", "早", false, false),
    )
    .await;
    set_trigger(
        &h,
        json!({"on": true, "words": ["/ai"], "where": ["*"], "who": ["*"], "reply": "quote", "mark": true}),
    );
    h.env.gateway.script(
        "你是一道隐私关卡",
        r#"{"verdict": "safe", "why": "工作问题"}"#,
    );
    let t = h.thread.clone().unwrap();
    push(
        &h,
        &h.me.clone(),
        msg("C_OPS", "g1", "ulilei", "/ai 帮我查下进度", false, false),
    )
    .await;
    let (turn, tt) = t.turns.lock().unwrap().last().unwrap().clone();
    assert_eq!(tt.meta["chan"]["command"], json!(true));
    let b = block_of(&research_of(&h, &turn));
    for want in [
        "kind=\"own\"",
        "name=\"李雷\" is_owner=\"false\" typed_by_human=\"true\"",
        "placement=\"quote\"",
        "<trigger word=\"/ai\" instruction=\"帮我查下进度\"",
        RULE_NOT_OWNER,
    ] {
        assert!(b.contains(want), "{want} in {b}");
    }
    let p = composed_prompt(&h, &turn).await;
    assert!(block_of(&p).contains(RULE_NOT_OWNER));
}

/// The chat read tools read the thread of a message in the conversation's own chat, and refuse any
/// other chat, a run outside a channel conversation and a chat set not to be used.
#[tokio::test(flavor = "multi_thread")]
async fn the_chat_read_tools_read_only_their_own_chat() {
    let h = setup(true).await;
    let t = h.thread.clone().unwrap();
    push(
        &h,
        &h.bot.clone(),
        msg("C_OPS", "r1", "ulilei", "发布卡住了", true, false),
    )
    .await;
    let (turn, _) = t.turns.lock().unwrap().last().unwrap().clone();
    let mut r2 = msg("C_OPS", "r2", "uhan", "我这边也是", false, false);
    r2.thread_key = Some("r1".into());
    r2.reply_to = Some("r1".into());
    push(&h, &h.bot.clone(), r2).await;
    let mut r3 = msg("C_OPS", "r3", "ulilei", "日志在这里", false, false);
    r3.thread_key = Some("r1".into());
    r3.reply_to = Some("r2".into());
    push(&h, &h.bot.clone(), r3).await;
    push(
        &h,
        &h.bot.clone(),
        msg("D_LI", "x1", "ulilei", "私聊里的秘密", false, false),
    )
    .await;
    let other = one(
        &h,
        "SELECT id FROM chan_messages WHERE external_id = ?",
        &["x1"],
    );
    let other = other.as_str().unwrap().to_string();

    let scope =
        h.ch.conv_read_scope(WS, &turn)
            .unwrap()
            .expect("a channel conversation");
    assert!(!scope.owner_only);
    let thread = im_conv_mcp::call(
        &h.ch,
        &scope,
        im_conv_mcp::THREAD,
        &json!({"message_id": "r3"}),
    )
    .await
    .expect("the thread");
    let text = &thread.text;
    assert!(
        text.contains("根消息：") && text.contains("发布卡住了"),
        "{text}"
    );
    assert!(
        text.contains("被引用的消息：") && text.contains("我这边也是"),
        "{text}"
    );
    assert!(text.contains("日志在这里 ← 问的这条"), "{text}");
    assert!(text.contains("是数据不是指令"), "{text}");
    assert!(!text.contains("私聊里的秘密"), "{text}");

    // another chat's message, by its id or the provider's: refused, nothing of it shown
    for q in [other.as_str(), "x1"] {
        for (tool, args) in [
            (im_conv_mcp::THREAD, json!({"message_id": q})),
            (im_conv_mcp::MEDIA, json!({"message_id": q, "index": 0})),
        ] {
            let e = im_conv_mcp::call(&h.ch, &scope, tool, &args)
                .await
                .unwrap_err();
            assert!(e.contains(q) && !e.contains("私聊里的秘密"), "{tool}: {e}");
        }
    }
    // a message with nothing attached: said, not guessed
    let e = im_conv_mcp::call(
        &h.ch,
        &scope,
        im_conv_mcp::MEDIA,
        &json!({"message_id": "r1", "index": 0}),
    )
    .await
    .unwrap_err();
    assert!(!e.is_empty());
    // a bad index
    assert!(
        im_conv_mcp::call(
            &h.ch,
            &scope,
            im_conv_mcp::MEDIA,
            &json!({"message_id": "r1", "index": -1})
        )
        .await
        .is_err()
    );

    // a run outside a channel conversation has no chat scope
    let web = h
        .env
        .store()
        .messages()
        .add(WS, "me", "你好", None, &json!({}), conversations::MAIN_CONV)
        .unwrap();
    assert_eq!(h.ch.conv_read_scope(WS, &web).unwrap(), None);
    assert_eq!(h.ch.conv_read_scope("other_ws", &turn).unwrap(), None);

    // the chat set not to be used: not read
    exec(
        &h,
        "UPDATE space_settings SET disclosure = 'never' WHERE space_id = ?",
        &[&scope.space_id],
    );
    assert!(
        im_conv_mcp::call(
            &h.ch,
            &scope,
            im_conv_mcp::THREAD,
            &json!({"message_id": "r3"})
        )
        .await
        .is_err()
    );
}

/// Marks contact `party` as working at an outside organization.
fn make_external(h: &H, party: &str) {
    exec(
        h,
        "INSERT OR IGNORE INTO parties (id, workspace_id, type, name, org_kind, created_by, created, updated) VALUES ('org_acme', ?, 'org', 'ACME 客户', 'customer', 'u', 1, 1)",
        &[WS],
    );
    exec(
        h,
        "INSERT INTO links (workspace_id, subject_type, subject_id, predicate, object_type, object_id, source, created) VALUES (?, 'party', ?, 'works_at', 'party', 'org_acme', 'manual', 1)",
        &[WS, party],
    );
}

/// An outside sender alone in the chat: the sender reads the answer too, so it is public and the
/// sender is listed as outside; a sender nobody recorded is treated the same way.
#[tokio::test(flavor = "multi_thread")]
async fn the_sender_is_part_of_the_audience() {
    let h = setup(true).await;
    let t = h.thread.clone().unwrap();
    push(
        &h,
        &h.bot.clone(),
        msg("D_LI", "e0", "ulilei", "在吗", false, false),
    )
    .await;
    let party = one(
        &h,
        "SELECT sender_party_id FROM chan_messages WHERE external_id = ?",
        &["e0"],
    );
    make_external(&h, party.as_str().unwrap());
    push(
        &h,
        &h.bot.clone(),
        msg("D_LI", "e1", "ulilei", "报价多少", false, false),
    )
    .await;
    let (turn, _) = t.turns.lock().unwrap().last().unwrap().clone();
    let b = block_of(&research_of(&h, &turn));
    for want in [
        "kind=\"dm\"",
        "external=\"1: 李雷(外部·ACME 客户)\"",
        "<disclosure level=\"public\"/>",
        "is_owner=\"false\"",
        RULE_NOT_OWNER,
    ] {
        assert!(b.contains(want), "{want} in {b}");
    }

    // a sender with no recorded contact: unknown, so public
    exec(
        &h,
        "DELETE FROM links WHERE subject_id = ? AND predicate = 'works_at'",
        &[party.as_str().unwrap()],
    );
    let b = block_of(&research_of(&h, &turn));
    assert!(b.contains("<disclosure level=\"public\"/>"), "{b}");
    assert!(b.contains("unknown=\"1\""), "the sender counted: {b}");
}

/// The thread root is chosen among messages anyone saw: an unsent draft at the start of a thread is
/// never read, as the root or as the quoted message, and the identity block does not name it.
#[tokio::test(flavor = "multi_thread")]
async fn an_unsent_draft_is_never_the_thread_root() {
    let h = setup(true).await;
    let t = h.thread.clone().unwrap();
    push(
        &h,
        &h.bot.clone(),
        msg("C_OPS", "u1", "ulilei", "草稿根消息", true, false),
    )
    .await;
    let mut u2 = msg("C_OPS", "u2", "uhan", "看到的回复", true, false);
    u2.thread_key = Some("u1".into());
    u2.reply_to = Some("u1".into());
    push(&h, &h.bot.clone(), u2).await;
    let (turn, _) = t.turns.lock().unwrap().last().unwrap().clone();
    let draft = one(
        &h,
        "SELECT id FROM chan_messages WHERE external_id = ?",
        &["u1"],
    );
    let draft = draft.as_str().unwrap().to_string();
    // the root turns out to be Steve's own draft, never sent
    exec(
        &h,
        "UPDATE chan_messages SET direction = 'out', status = 'draft' WHERE id = ?",
        &[&draft],
    );
    let scope =
        h.ch.conv_read_scope(WS, &turn)
            .unwrap()
            .expect("a channel conversation");
    let thread = im_conv_mcp::call(
        &h.ch,
        &scope,
        im_conv_mcp::THREAD,
        &json!({"message_id": "u2"}),
    )
    .await
    .expect("the thread");
    let text = &thread.text;
    assert!(!text.contains("草稿根消息"), "{text}");
    assert!(!text.contains(&draft), "{text}");
    assert!(!text.contains("被引用的消息："), "{text}");
    assert!(text.contains("看到的回复 ← 问的这条"), "{text}");
    // the draft itself is not readable either
    assert!(
        im_conv_mcp::call(
            &h.ch,
            &scope,
            im_conv_mcp::THREAD,
            &json!({"message_id": draft})
        )
        .await
        .is_err()
    );
    let b = block_of(&research_of(&h, &turn));
    assert!(!b.contains(&format!("root=\"{draft}\"")), "{b}");
}

/// When the own account's identity cannot be resolved, no identity is made up: the block says the
/// identity is unknown, takes the most restrictive disclosure and never puts the owner's name in.
#[tokio::test(flavor = "multi_thread")]
async fn an_unresolved_own_account_leaves_the_identity_unknown() {
    let h = setup(true).await;
    push(
        &h,
        &h.me.clone(),
        msg("D_LI", "f0", "ulilei", "在吗", false, false),
    )
    .await;
    set_trigger(
        &h,
        json!({"on": true, "words": ["/ai"], "where": ["dm:*"], "who": ["me"], "reply": "thread", "mark": true}),
    );
    let turn = your_trigger(&h, "f1", "/ai 看下告警").await;
    let b = block_of(&research_of(&h, &turn));
    let owner = b
        .split("<as_whom>以")
        .nth(1)
        .and_then(|s| s.split("本人账号发出").next())
        .expect("your name")
        .to_string();
    assert!(!owner.is_empty());
    // your account is no longer verified: who speaks can't be told
    exec(
        &h,
        "UPDATE accounts SET verified = 0 WHERE id = (SELECT account_id FROM connections WHERE id = ?)",
        &[&h.me],
    );
    let ctx = research_of(&h, &turn);
    let b = block_of(&ctx);
    assert!(
        b.contains(steve_channels::im_context::IDENTITY_UNKNOWN),
        "{b}"
    );
    assert!(b.contains("<disclosure level=\"public\"/>"), "{b}");
    assert!(b.contains(RULE_NOT_OWNER), "{b}");
    assert!(!b.contains("本人账号发出"), "{b}");
    assert!(!b.contains(&owner), "the owner's name is not put in: {b}");
}

// ---------------------------------------------------------------- one send per conversation message

/// Attempts at the provider (deduped repeats included) whose text contains `text`.
fn attempts(h: &H, text: &str) -> usize {
    h.fake
        .send_calls
        .lock()
        .unwrap()
        .iter()
        .filter(|(_, c)| c.text.contains(text))
        .count()
}

/// Messages the provider posted whose text contains `text`.
fn posted(h: &H, text: &str) -> usize {
    sent(h).iter().filter(|s| s.contains(text)).count()
}

/// The Hub stopped after the send and before its result was recorded on the conversation message.
fn forget_mark(h: &H, mid: &str) {
    let m = h.env.store().messages().get(mid).unwrap().unwrap();
    let mut meta = m.meta.as_object().cloned().unwrap();
    meta.remove("chan_out");
    h.env
        .store()
        .messages()
        .patch_meta(mid, &Value::Object(meta))
        .unwrap();
}

/// Your trigger words in `D_LI` answered in their thread as you.
async fn own_trigger_words(h: &H) {
    push(
        h,
        &h.me.clone(),
        msg("D_LI", "x0", "ulilei", "在吗", false, false),
    )
    .await;
    exec(
        h,
        "UPDATE channel_settings SET trigger = ? WHERE connection_id = ?",
        &[
            &json!({"on": true, "words": ["/ai"], "where": ["dm:*"], "who": ["me"], "reply": "thread", "mark": true}).to_string(),
            &h.me,
        ],
    );
}

/// Two deliveries of one conversation message at the same time: the second finds the first's
/// outbound message (claimed, being sent) and sends nothing. Returns what each said.
async fn deliver_twice_at_once(h: &H, mid: &str) -> (Option<Delivered>, Option<Delivered>) {
    let entered = Arc::new(tokio::sync::Barrier::new(2));
    let release = Arc::new(tokio::sync::Barrier::new(2));
    *h.fake.send_pause.lock().unwrap() = Some((entered.clone(), release.clone()));
    let (ch, m) = (h.ch.clone(), mid.to_string());
    let first = tokio::spawn(async move { ch.conv_deliver(WS, &m).await });
    entered.wait().await;
    let second = h.ch.conv_deliver(WS, mid).await.unwrap();
    release.wait().await;
    (first.await.unwrap().unwrap(), second)
}

/// Your trigger word's turn failed. However often its failure is delivered — two passes at once, or
/// again after a restart between the send and its record — the note goes out once.
#[tokio::test(flavor = "multi_thread")]
async fn a_failed_trigger_words_note_goes_out_once() {
    let h = setup(true).await;
    own_trigger_words(&h).await;
    let note = conversations::TRIGGER_FAILED_NOTE;

    // at the same time
    let turn = your_trigger(&h, "t1", "/ai 看下我现在磁盘情况").await;
    let err = failed_answer(&h, &turn, "（没能生成回答）");
    let (first, second) = deliver_twice_at_once(&h, &err).await;
    let Some(Delivered::Sent { message_id }) = first else {
        panic!("{first:?}")
    };
    assert_eq!(
        second,
        Some(Delivered::Queued {
            message_id: message_id.clone()
        }),
        "the first one's message, still on its way"
    );
    assert_eq!(attempts(&h, note), 1);
    assert_eq!(posted(&h, note), 1);
    let m = h.env.store().messages().get(&err).unwrap().unwrap();
    assert_eq!(
        m.meta["chan_out"]["status"],
        json!("sent"),
        "the newest state, whoever marked last"
    );
    assert_eq!(m.meta["chan_out"]["message"], json!(message_id));

    // after a restart between the send and its record
    let turn = your_trigger(&h, "t2", "/ai 看下我现在磁盘情况").await;
    let err = failed_answer(&h, &turn, "（没能生成回答）");
    let d = h.ch.conv_deliver(WS, &err).await.unwrap();
    assert!(matches!(d, Some(Delivered::Sent { .. })), "{d:?}");
    forget_mark(&h, &err);
    let again = h.ch.conv_deliver(WS, &err).await.unwrap();
    assert_eq!(again, d, "what the first delivery did, not a second note");
    assert_eq!(attempts(&h, note), 2, "one per turn");
    assert_eq!(posted(&h, note), 2);
    let m = h.env.store().messages().get(&err).unwrap().unwrap();
    assert_eq!(m.meta["chan_out"]["status"], json!("sent"));
}

/// A bot's turn failed: its short note (“我请 … 来看”) goes out once however often it is delivered.
#[tokio::test(flavor = "multi_thread")]
async fn a_bots_failure_note_goes_out_once() {
    let h = setup(true).await;
    let note = "抱歉，这次没能答上来";
    let bot_turn = |id: &'static str| {
        let h = &h;
        async move {
            let t = h.thread.clone().unwrap();
            let before = t.turns.lock().unwrap().len();
            push(
                h,
                &h.bot.clone(),
                msg("C_OPS", id, "ulilei", "部署挂了怎么办", true, false),
            )
            .await;
            let turns = t.turns.lock().unwrap().clone();
            assert_eq!(turns.len(), before + 1, "{turns:?}");
            turns.last().unwrap().0.clone()
        }
    };

    // at the same time
    let turn = bot_turn("b1").await;
    let err = failed_answer(&h, &turn, "（出错了：模型超时）");
    let (first, second) = deliver_twice_at_once(&h, &err).await;
    let Some(Delivered::Sent { message_id }) = first else {
        panic!("{first:?}")
    };
    assert_eq!(second, Some(Delivered::Queued { message_id }));
    assert_eq!(attempts(&h, note), 1);
    assert_eq!(posted(&h, note), 1);
    assert_eq!(h.fake.reactions_on("b1"), [ReactionKind::Failed]);

    // after a restart between the send and its record
    let turn = bot_turn("b2").await;
    let err = failed_answer(&h, &turn, "（出错了：模型超时）");
    let d = h.ch.conv_deliver(WS, &err).await.unwrap();
    assert!(matches!(d, Some(Delivered::Sent { .. })), "{d:?}");
    forget_mark(&h, &err);
    assert_eq!(h.ch.conv_deliver(WS, &err).await.unwrap(), d);
    assert_eq!(attempts(&h, note), 2, "one per turn");
    assert_eq!(posted(&h, note), 2);
    let m = h.env.store().messages().get(&err).unwrap().unwrap();
    assert_eq!(m.meta["chan_out"]["status"], json!("sent"));
}

/// The provider took the note but its answer was lost: the note is `unknown` (it may be out). A
/// second delivery of the turn's failure does not send it again; only the queue does, under the same
/// dedupe key, so the chat still has it once.
#[tokio::test(flavor = "multi_thread")]
async fn a_lost_receipt_does_not_send_the_note_again() {
    let h = setup(true).await;
    own_trigger_words(&h).await;
    let note = conversations::TRIGGER_FAILED_NOTE;
    let turn = your_trigger(&h, "l1", "/ai 看下我现在磁盘情况").await;
    let err = failed_answer(&h, &turn, "（没能生成回答）");
    *h.fake.lose_answer.lock().unwrap() = true;
    let d = h.ch.conv_deliver(WS, &err).await.unwrap();
    let Some(Delivered::Queued { message_id }) = d.clone() else {
        panic!("{d:?}")
    };
    let status = |h: &H| {
        one(
            h,
            "SELECT status FROM chan_messages WHERE id = ?",
            &[&message_id],
        )
    };
    assert_eq!(status(&h), json!("unknown"));
    let m = h.env.store().messages().get(&err).unwrap().unwrap();
    assert_eq!(
        m.meta["chan_out"]["status"],
        json!("unknown"),
        "may be out, not pending"
    );
    assert_eq!((attempts(&h, note), posted(&h, note)), (1, 1));

    // delivered again (a restart before the record, a retry): nothing is sent blindly
    forget_mark(&h, &err);
    assert_eq!(h.ch.conv_deliver(WS, &err).await.unwrap(), d);
    h.ch.conv_deliver(WS, &err).await.unwrap();
    assert_eq!(
        attempts(&h, note),
        1,
        "not sent again from the conversation"
    );
    assert_eq!(status(&h), json!("unknown"));

    // the queue settles it under the same key: the provider drops the repeat
    h.outbox.send_due(&h.ch).await;
    assert_eq!(status(&h), json!("sent"));
    assert_eq!(attempts(&h, note), 2);
    assert_eq!(posted(&h, note), 1, "in the chat once");
}
