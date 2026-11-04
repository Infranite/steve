//! IM conversations (channels-personas.md §2.11–§2.12, domain-model.md §1.4–§1.6):
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

/// CONTRACT.md §4: an inbound turn carries its chat message (`channel_message`), read from the
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
