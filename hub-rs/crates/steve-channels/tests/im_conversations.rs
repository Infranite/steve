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
//!   failure sends a short note.

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
}

impl Thread for FakeThread {
    fn say(&self, ws: &str, turn: Turn) -> Result<String, String> {
        let id = self
            .store
            .messages()
            .add(ws, "me", &turn.text, None, &turn.meta, &turn.space)
            .map_err(|e| e.to_string())?;
        self.turns.lock().unwrap().push((id.clone(), turn));
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
    h.env.gateway.script(H_SAFETY, REPLY);
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
    // your own account in a DM nobody else is in: you speak, and your rules draft
    let i = push(
        &h,
        &h.me.clone(),
        msg("D_LI", "d1", "ulilei", "明天几点开会？", false, false),
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
async fn your_bot_command_is_answered_by_the_voice_once() {
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
    // the bot's connection sees your command first (it has the group's messages)
    let b = push(
        &h,
        &h.bot.clone(),
        msg("C_OPS", "o8", "upx", "/bot 告诉李雷先回滚", false, false),
    )
    .await;
    let r = b.response.unwrap();
    assert!(matches!(r.by, steve_channels::By::Trigger { .. }), "{r:?}");
    assert_eq!(sent(&h).len(), 1, "the bot answers your command");
    assert!(
        !sent(&h)[0].contains(steve_channels::outbound::PROXY_MARK),
        "as the bot, not as you"
    );
    // your poller's copy: a duplicate, not a second answer
    let s = push(
        &h,
        &h.me.clone(),
        msg("C_OPS", "o8", "upx", "/bot 告诉李雷先回滚", false, false),
    )
    .await;
    assert!(s.duplicate);
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
async fn your_command_without_a_voice_goes_to_your_web_conversation() {
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
    let r = h.ch.respond(WS, &h.me.clone(), &c).await.expect("respond");
    assert_eq!(r.label, "你的指令，没有发言身份，转到网页对话");
    let turns = h.thread.as_ref().unwrap().turns.lock().unwrap().clone();
    let last = turns.last().unwrap();
    assert_eq!(last.1.space, "steve");
    assert_eq!(last.1.text, "记一下明天给李雷回电话");
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
    let draft = |id: &'static str| {
        let h = &h;
        async move {
            let i = push(
                h,
                &h.me.clone(),
                msg("D_LI", id, "ulilei", "明天几点开会？", false, false),
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
    let i = push(
        &h,
        &h.me.clone(),
        msg("D_LI", "d1", "ulilei", "明天几点开会？", false, false),
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
async fn a_members_bot_command_does_not_start_work() {
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
    assert!(turns.len() > before, "the command was taken: {turns:?}");
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
                msg("C_OPS", id, "ulilei", "@李鹏翔 明天", true, false),
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
                    "这条看起来是发给李鹏翔的，要我处理吗？",
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
