//! W3.4 synthetic replay fixtures — Channels / Outbound driver (14 scenarios).
//!
//! Exercises real `steve-channels` conversation delivery (`conv_deliver`, `conv_operation`, `Outbox`,
//! `FakeChannel`) for outbound idempotency, sent-unknown (`lose_answer`), guest eligibility,
//! disconnected spaces, and `Mode::Own` / trigger-word boundaries.
//!
//! Run in baseline mode:
//!   `cargo test --manifest-path hub-rs/Cargo.toml -p steve-channels --test thread_replay -- --nocapture`
//! Run in strict W3.4 acceptance mode:
//!   `STEVE_THREAD_REPLAY_STRICT=1 cargo test --manifest-path hub-rs/Cargo.toml -p steve-channels --test thread_replay -- --nocapture`

#[path = "support/digest_env.rs"]
mod digest_env;
#[path = "../../../tests/thread_replay/report.rs"]
mod report;

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use digest_env::TestEnv;
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
    Channels, Inbound, InboundMessage, Mention, ReactionKind, Replies, SpaceKind, spaces,
};
use steve_flow::ports::{Core, ItemRequest};
use steve_flow::{FlowSetup, LinearExecutor};
use steve_modules::accounts::connections::{self, Connections, ConnectionsConfig};
use steve_modules::testkit::{ME, WS, caller};
use steve_modules::vault::Vault;
use steve_modules::vault::keystore::FileKeyStore;
use steve_store::base::Viewer;
use steve_store::testkit::channels::seed::people::Bot;
use steve_store::testkit::channels::{inspect, seed};
use tempfile::TempDir;

use report::Report;

const REPLY_OK: &str =
    r#"{"text": "先看部署日志，再按回滚手册处理。", "needs_approval": false, "why": "按手册"}"#;
const REPLY_SKIP: &str = r#"{"outcome": "skip", "text": "群里同事已经回复了", "needs_approval": false, "why": "已有人处理"}"#;
const REPLY_ASK: &str = r#"{"outcome": "ask_owner", "text": "请问客户折扣是否可以按九折报价？", "needs_approval": false, "why": "需主人确认"}"#;
const PASS: &str = r#"{"verdict": "pass", "spans": []}"#;

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
    _outbox: Arc<Outbox>,
    thread: Arc<FakeThread>,
    me: String,
    bot: String,
}

struct NoopCore;

#[async_trait]
impl Core for NoopCore {
    async fn create_item(&self, _req: ItemRequest) -> Result<String, String> {
        Ok("ST-1".into())
    }
    async fn pause_item(&self, _ws: &str, _item_id: &str) -> Result<(), String> {
        Ok(())
    }
    async fn resume_item(&self, _ws: &str, _item_id: &str) -> Result<(), String> {
        Ok(())
    }
    async fn post_steve_message(&self, _ws: &str, _text: &str, _meta: Value) -> Result<(), String> {
        Ok(())
    }
    async fn digest_text(&self, _ws: &str) -> Result<String, String> {
        Ok(String::new())
    }
}

async fn setup_with(composed: &str) -> H {
    let core = Arc::new(NoopCore);
    let flow_setup = FlowSetup::new(core.clone());
    let disclosure = Disclosure::new();
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
    seed::people::bots(env.store(), &[Bot::default()]).unwrap();
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
    let thread = Arc::new(FakeThread {
        store: env.store().clone(),
        turns: Mutex::new(vec![]),
    });
    let ch = Channels::builder(env.modules.clone(), conns, Arc::new(flow.clone()))
        .adapter(fake.clone())
        .responder(replies)
        .flow(flow)
        .reactor(outbox.clone())
        .thread(thread.clone())
        .build();
    late.set(&ch);
    let (me, _) =
        FakeChannel::seed_connection(env.store(), &caller(WS, ME), Some("upx")).expect("me");
    let (bot, _) =
        FakeChannel::seed_bot_connection(env.store(), &caller(WS, ME), "uoncall", "值班小运")
            .expect("bot");
    seed::connections::link_personal_connection(env.store(), &bot, &me).expect("link");
    let people = [("ulilei", "李雷"), ("uhan", "韩梅梅")];
    fake.space("C_OPS", "运维群", SpaceKind::Group, false, &people);
    fake.space("D_LI", "", SpaceKind::Dm, false, &[("ulilei", "李雷")]);
    let h = H {
        env,
        _keys: keys,
        ch,
        fake,
        _outbox: outbox,
        thread,
        me,
        bot,
    };
    let now = h.ch.now();
    h.env
        .store()
        .write(|tx| {
            spaces::set_account_rules(
                tx,
                &Viewer::bare(WS, ME),
                &h.me,
                &json!([{"when": {"kind": "mention_or_dm"}, "act": "draft"}]),
                now,
            )?;
            spaces::set_account_rules(
                tx,
                &Viewer::bare(WS, ME),
                &h.bot,
                &json!([{"when": {"kind": "mention_or_dm"}, "act": "reply"}]),
                now,
            )
        })
        .unwrap();
    h.env.gateway.script(CHECK_HEAD, PASS);
    h.env.gateway.script(H_SAFETY, composed);
    h
}

fn msg(space: &str, id: &str, from: &str, text: &str, at: bool) -> InboundMessage {
    let mut m = FakeChannel::message(space, id, from, text);
    if at {
        m.mentions.push(Mention {
            handle: None,
            name: "值班小运".into(),
            is_connection: true,
        });
    }
    m
}

async fn push(h: &H, conn: &str, m: InboundMessage) -> Inbound {
    h.ch.on_message(WS, conn, m).await.expect("inbound")
}

async fn bot_turn(h: &H, ext_id: &str, text: &str) -> (Inbound, String, String) {
    let before = h.thread.turns.lock().unwrap().len();
    let inbound = push(
        h,
        &h.bot.clone(),
        msg("C_OPS", ext_id, "ulilei", text, true),
    )
    .await;
    let turns = h.thread.turns.lock().unwrap().clone();
    assert_eq!(turns.len(), before + 1);
    let (turn, t) = turns.last().unwrap().clone();
    (inbound, turn, t.space)
}

async fn enable_trigger(h: &H) {
    push(
        h,
        &h.me.clone(),
        msg("D_LI", "d_seed", "ulilei", "在吗", false),
    )
    .await;
    seed::connections::set_channel_trigger(
        h.env.store(),
        &json!({"on": true, "words": ["/ai"], "where": ["dm:*"], "who": ["me"], "reply": "thread", "mark": true}).to_string(),
        &h.me,
    )
    .unwrap();
}

async fn trigger_turn(h: &H, ext_id: &str, text: &str) -> (Inbound, String, String) {
    let before = h.thread.turns.lock().unwrap().len();
    let inbound = push(h, &h.me.clone(), msg("D_LI", ext_id, "upx", text, false)).await;
    let turns = h.thread.turns.lock().unwrap().clone();
    assert_eq!(turns.len(), before + 1);
    let (turn, t) = turns.last().unwrap().clone();
    (inbound, turn, t.space)
}

fn clear_chan_out_meta(store: &steve_store::Store, mid: &str) {
    let m = store.messages().get(mid).unwrap().unwrap();
    let mut obj = m.meta.as_object().cloned().unwrap_or_default();
    obj.remove("chan_out");
    store
        .messages()
        .patch_meta(mid, &Value::Object(obj))
        .unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn w34_thread_replay_channels() {
    let mut report = Report::default();

    // 01: Single Bot reply sends exactly once
    {
        let h = setup_with(REPLY_OK).await;
        let (_, turn, space) = bot_turn(&h, "o1", "部署挂了怎么办").await;
        let text = h.ch.conv_answer(WS, &turn).await.unwrap();
        let reply = h
            .env
            .store()
            .messages()
            .add(WS, "steve", &text, None, &json!({"origin": turn}), &space)
            .unwrap();
        let d = h.ch.conv_deliver(WS, &reply).await.unwrap();
        report.case(
            "chan.out.01_single_bot_reply_sends_once",
            &[
                ("delivered_sent", matches!(d, Some(Delivered::Sent { .. }))),
                (
                    "le1_outbound_intent_per_origin",
                    h.fake.send_calls.lock().unwrap().len() == 1,
                ),
            ],
            &[],
        );
    }

    // 02: Replaying conv_deliver on same reply id (including crash before mark_out) sends once
    {
        let h = setup_with(REPLY_OK).await;
        let (_, turn, space) = bot_turn(&h, "o2", "部署挂了怎么办").await;
        let text = h.ch.conv_answer(WS, &turn).await.unwrap();
        let reply = h
            .env
            .store()
            .messages()
            .add(WS, "steve", &text, None, &json!({"origin": turn}), &space)
            .unwrap();
        let _ = h.ch.conv_deliver(WS, &reply).await.unwrap();
        // Repeat with chan_out present
        let d2 = h.ch.conv_deliver(WS, &reply).await.unwrap();
        // Simulate crash before mark_out persisted meta.chan_out, then replay conv_deliver
        clear_chan_out_meta(h.env.store(), &reply);
        let _ = h.ch.conv_deliver(WS, &reply).await.unwrap();
        report.case(
            "chan.out.02_replay_same_reply_id_idempotent",
            &[
                ("second_noop", d2.is_none()),
                (
                    "le1_outbound_intent_per_origin",
                    h.fake.send_calls.lock().unwrap().len() == 1,
                ),
            ],
            &[],
        );
    }

    // 03: Two Steve replies for SAME origin must NOT create 2 external outbound intents!
    // Baseline bug: conv_operation includes m.id (reply message id), so m1 and m2 get distinct operation_ids!
    {
        let h = setup_with(REPLY_OK).await;
        let (_, turn, space) = bot_turn(&h, "o3", "部署挂了怎么办").await;
        let text = h.ch.conv_answer(WS, &turn).await.unwrap();
        let m1 = h
            .env
            .store()
            .messages()
            .add(WS, "steve", &text, None, &json!({"origin": turn}), &space)
            .unwrap();
        let m2 = h
            .env
            .store()
            .messages()
            .add(WS, "steve", &text, None, &json!({"origin": turn}), &space)
            .unwrap();
        let _ = h.ch.conv_deliver(WS, &m1).await.unwrap();
        let _ = h.ch.conv_deliver(WS, &m2).await.unwrap();
        let sends = h.fake.send_calls.lock().unwrap().len();
        report.case(
            "chan.out.03_two_replies_same_origin_single_outbound_intent",
            &[("le1_outbound_intent_per_origin", sends <= 1)],
            &[],
        );
    }

    // 04: Sent-unknown (lose_answer) + replay on SAME reply id -> 0 second send
    {
        let h = setup_with(REPLY_OK).await;
        let (_, turn, space) = bot_turn(&h, "o4", "部署挂了怎么办").await;
        let text = h.ch.conv_answer(WS, &turn).await.unwrap();
        let m1 = h
            .env
            .store()
            .messages()
            .add(WS, "steve", &text, None, &json!({"origin": turn}), &space)
            .unwrap();
        *h.fake.lose_answer.lock().unwrap() = true;
        let _ = h.ch.conv_deliver(WS, &m1).await.unwrap();
        let st1 =
            h.env.store().messages().get(&m1).unwrap().unwrap().meta["chan_out"]["status"].clone();
        // Simulate crash before mark_out and replay same m1
        clear_chan_out_meta(h.env.store(), &m1);
        *h.fake.lose_answer.lock().unwrap() = false;
        let _ = h.ch.conv_deliver(WS, &m1).await.unwrap();
        let sends = h.fake.send_calls.lock().unwrap().len();
        report.case(
            "chan.out.04_sent_unknown_replay_same_reply_no_second_send",
            &[
                ("recorded_unknown", st1 == json!("unknown")),
                ("unknown_outbound_no_second_send", sends == 1),
            ],
            &[],
        );
    }

    // 05: Sent-unknown on m1, then second reply m2 for SAME origin -> must NOT send a 2nd message!
    {
        let h = setup_with(REPLY_OK).await;
        let (_, turn, space) = bot_turn(&h, "o5", "部署挂了怎么办").await;
        let text = h.ch.conv_answer(WS, &turn).await.unwrap();
        let m1 = h
            .env
            .store()
            .messages()
            .add(WS, "steve", &text, None, &json!({"origin": turn}), &space)
            .unwrap();
        *h.fake.lose_answer.lock().unwrap() = true;
        let _ = h.ch.conv_deliver(WS, &m1).await.unwrap();
        *h.fake.lose_answer.lock().unwrap() = false;
        let m2 = h
            .env
            .store()
            .messages()
            .add(WS, "steve", &text, None, &json!({"origin": turn}), &space)
            .unwrap();
        let _ = h.ch.conv_deliver(WS, &m2).await.unwrap();
        let sends = h.fake.send_calls.lock().unwrap().len();
        report.case(
            "chan.out.05_sent_unknown_then_second_reply_same_origin_no_second_send",
            &[
                ("unknown_outbound_no_second_send", sends <= 1),
                ("le1_outbound_intent_per_origin", sends <= 1),
            ],
            &[],
        );
    }

    // 06: Research reply with host_reply.status = "ineligible" -> 0 external sends
    {
        let h = setup_with(REPLY_OK).await;
        let (_, turn, space) = bot_turn(&h, "o6", "查一下日志").await;
        let reply = h
            .env
            .store()
            .messages()
            .add(
                WS,
                "steve",
                "未完成研究部分结果",
                None,
                &json!({"origin": turn, "host_reply": {"status": "ineligible", "reason": "研究已停止"}}),
                &space,
            )
            .unwrap();
        let d = h.ch.conv_deliver(WS, &reply).await.unwrap();
        let st = h.env.store().messages().get(&reply).unwrap().unwrap().meta["chan_out"]["status"]
            .clone();
        report.case(
            "chan.out.06_ineligible_research_reply_zero_outbound",
            &[
                ("none_delivered", d.is_none()),
                ("marked_skipped", st == json!("skipped")),
                (
                    "zero_provider_sends",
                    h.fake.send_calls.lock().unwrap().is_empty(),
                ),
            ],
            &[],
        );
    }

    // 07: Mode::Own turn where Steve reply text does NOT match Composed (chan_gen) -> 0 external sends
    {
        let h = setup_with(REPLY_OK).await;
        enable_trigger(&h).await;
        let (_, turn, space) = trigger_turn(&h, "d_unv", "/ai 查下磁盘").await;
        let _ = h.ch.conv_answer(WS, &turn).await.unwrap();
        // Reply text differs from composed text -> must not speak as owner!
        let reply = h
            .env
            .store()
            .messages()
            .add(
                WS,
                "steve",
                "未经渠道生成器核验的原始文本",
                None,
                &json!({"origin": turn}),
                &space,
            )
            .unwrap();
        let d = h.ch.conv_deliver(WS, &reply).await.unwrap();
        let code =
            h.env.store().messages().get(&reply).unwrap().unwrap().meta["chan_out"]["code"].clone();
        report.case(
            "chan.out.07_own_mode_unverified_text_zero_outbound",
            &[
                ("none_delivered", d.is_none()),
                ("unverified_code", code == json!("unverified_reply")),
                (
                    "zero_provider_sends",
                    h.fake.send_calls.lock().unwrap().is_empty(),
                ),
            ],
            &[],
        );
    }

    // 08: Composer Outcome::Skip -> 0 external sends, idempotent on replay
    {
        let h = setup_with(REPLY_SKIP).await;
        let (_, turn, space) = bot_turn(&h, "o8", "有人回复过了").await;
        let text = h.ch.conv_answer(WS, &turn).await.unwrap();
        let reply = h
            .env
            .store()
            .messages()
            .add(WS, "steve", &text, None, &json!({"origin": turn}), &space)
            .unwrap();
        let d1 = h.ch.conv_deliver(WS, &reply).await.unwrap();
        let d2 = h.ch.conv_deliver(WS, &reply).await.unwrap();
        report.case(
            "chan.out.08_composer_skip_zero_outbound",
            &[
                ("none_delivered", d1.is_none() && d2.is_none()),
                (
                    "zero_provider_sends",
                    h.fake.send_calls.lock().unwrap().is_empty(),
                ),
            ],
            &[],
        );
    }

    // 09: Composer Outcome::AskOwner -> opens 1 decision card (idempotent on replay), 0 external sends
    {
        let h = setup_with(REPLY_ASK).await;
        let (inbound, turn, space) = bot_turn(&h, "o9", "能不能给九折").await;
        let text = h.ch.conv_answer(WS, &turn).await.unwrap();
        let reply = h
            .env
            .store()
            .messages()
            .add(WS, "steve", &text, None, &json!({"origin": turn}), &space)
            .unwrap();
        let _ = h.ch.conv_deliver(WS, &reply).await.unwrap();
        clear_chan_out_meta(h.env.store(), &reply);
        // Replay with chan_out cleared -> open_card open_for keeps 1 open decision for this message!
        let _ = h.ch.conv_deliver(WS, &reply).await.unwrap();
        let msg_decisions = h
            .env
            .store()
            .decisions()
            .open(WS)
            .unwrap()
            .into_iter()
            .filter(|d| d.subject_type == SUBJECT_MESSAGE && d.subject_id == inbound.message_id)
            .count();
        report.case(
            "chan.out.09_composer_ask_owner_single_decision_zero_outbound",
            &[
                (
                    "zero_provider_sends",
                    h.fake.send_calls.lock().unwrap().is_empty(),
                ),
                ("single_decision_opened", msg_decisions == 1),
            ],
            &[],
        );
    }

    // 10: Stopped & system (queue full) thread messages -> 0 external sends
    {
        let h = setup_with(REPLY_OK).await;
        let (_, turn, space) = bot_turn(&h, "o10", "问一个问题").await;
        let stopped = h
            .env
            .store()
            .messages()
            .add(
                WS,
                "steve",
                "好，停下了。",
                None,
                &json!({"origin": turn, "stopped": true, "system": true}),
                &space,
            )
            .unwrap();
        let qfull = h
            .env
            .store()
            .messages()
            .add(
                WS,
                "steve",
                "前面还有 3 个问题在排队",
                None,
                &json!({"origin": turn, "system": true}),
                &space,
            )
            .unwrap();
        let d1 = h.ch.conv_deliver(WS, &stopped).await.unwrap();
        let d2 = h.ch.conv_deliver(WS, &qfull).await.unwrap();
        report.case(
            "chan.out.10_stopped_and_system_messages_zero_outbound",
            &[
                ("none_delivered", d1.is_none() && d2.is_none()),
                (
                    "zero_provider_sends",
                    h.fake.send_calls.lock().unwrap().is_empty(),
                ),
            ],
            &[],
        );
    }

    // 11: Disconnected space connection -> marked disconnected, 0 external sends
    {
        let h = setup_with(REPLY_OK).await;
        let (_, turn, space) = bot_turn(&h, "o11", "连接断开后回复").await;
        let text = h.ch.conv_answer(WS, &turn).await.unwrap();
        h.ch.connections()
            .disconnect(&caller(WS, ME), &h.bot)
            .await
            .unwrap();
        let reply = h
            .env
            .store()
            .messages()
            .add(WS, "steve", &text, None, &json!({"origin": turn}), &space)
            .unwrap();
        let d = h.ch.conv_deliver(WS, &reply).await.unwrap();
        let st = h.env.store().messages().get(&reply).unwrap().unwrap().meta["chan_out"]["status"]
            .clone();
        report.case(
            "chan.out.11_disconnected_space_zero_outbound",
            &[
                ("none_delivered", d.is_none()),
                (
                    "marked_disconnected",
                    st == json!(conversations::DISCONNECTED),
                ),
                (
                    "zero_provider_sends",
                    h.fake.send_calls.lock().unwrap().is_empty(),
                ),
            ],
            &[],
        );
    }

    // 12: Bot failure note — two error replies for SAME origin must send <=1 failure note!
    {
        let h = setup_with(REPLY_OK).await;
        let (_, turn, space) = bot_turn(&h, "o12", "模型挂了").await;
        let e1 = h
            .env
            .store()
            .messages()
            .add(
                WS,
                "steve",
                "（出错了：超时）",
                None,
                &json!({"origin": turn, "error": true}),
                &space,
            )
            .unwrap();
        let e2 = h
            .env
            .store()
            .messages()
            .add(
                WS,
                "steve",
                "（出错了：重试超时）",
                None,
                &json!({"origin": turn, "error": true}),
                &space,
            )
            .unwrap();
        let _ = h.ch.conv_deliver(WS, &e1).await.unwrap();
        let _ = h.ch.conv_deliver(WS, &e2).await.unwrap();
        let sends = h.fake.send_calls.lock().unwrap().len();
        report.case(
            "chan.out.12_bot_failure_note_two_error_replies_same_origin",
            &[("le1_outbound_intent_per_origin", sends <= 1)],
            &[],
        );
    }

    // 13: Trigger-word failure note — two error replies for SAME origin must send <=1 note!
    {
        let h = setup_with(REPLY_OK).await;
        enable_trigger(&h).await;
        let (_, turn, space) = trigger_turn(&h, "d_err2", "/ai 看下磁盘").await;
        let e1 = h
            .env
            .store()
            .messages()
            .add(
                WS,
                "steve",
                "（出错了：1）",
                None,
                &json!({"origin": turn, "error": true}),
                &space,
            )
            .unwrap();
        let e2 = h
            .env
            .store()
            .messages()
            .add(
                WS,
                "steve",
                "（出错了：2）",
                None,
                &json!({"origin": turn, "error": true}),
                &space,
            )
            .unwrap();
        let _ = h.ch.conv_deliver(WS, &e1).await.unwrap();
        let _ = h.ch.conv_deliver(WS, &e2).await.unwrap();
        let sends = h.fake.send_calls.lock().unwrap().len();
        report.case(
            "chan.out.13_trigger_failed_note_two_error_replies_same_origin",
            &[("le1_outbound_intent_per_origin", sends <= 1)],
            &[],
        );
    }

    // 14: converse_web fallback (owner trigger word on read-only personal conn) -> 0 external sends even if caps restored
    {
        let h = setup_with(REPLY_OK).await;
        enable_trigger(&h).await;
        seed::connections::set_connection_caps_msg_receive(h.env.store(), &h.me).unwrap();
        let a = push(
            &h,
            &h.me.clone(),
            msg("D_LI", "d_w1", "upx", "/ai 看下空间", false),
        )
        .await;
        let (t1, _) = h.thread.turns.lock().unwrap().last().unwrap().clone();
        let ans = h
            .env
            .store()
            .messages()
            .add(
                WS,
                "steve",
                "还剩 20G",
                None,
                &json!({"origin": t1}),
                conversations::MAIN_CONV,
            )
            .unwrap();
        // Restore send caps while turn was running -> must still NOT send externally!
        seed::connections::set_connection_caps(h.env.store(), &h.me).unwrap();
        let d = h.ch.conv_deliver(WS, &ans).await.unwrap();
        let react_done =
            inspect::messages::chan_reaction_state_by_message(h.env.store(), &a.message_id)
                .unwrap()
                == json!("done");
        let saw_done_reaction = h.fake.reactions_on("d_w1").contains(&ReactionKind::Done);
        report.case(
            "chan.out.14_converse_web_fallback_zero_outbound",
            &[
                ("none_delivered", d.is_none()),
                (
                    "zero_provider_sends",
                    h.fake.send_calls.lock().unwrap().is_empty(),
                ),
                ("reaction_finished_done", react_done && saw_done_reaction),
            ],
            &[],
        );
    }

    // -----------------------------------------------------------------
    // Step 4 channel-reply replay scenarios (#258, #268, #275, #277, #278, #213, P1-21)
    // -----------------------------------------------------------------

    // 15 (#258): Group chat deterministic addressee gate & quoted parent resolution
    {
        let h = setup_with(REPLY_OK).await;
        let now = h.ch.now();
        // Set personal account rule to `all -> draft` in group so only deterministic G6 can stop it.
        h.env
            .store()
            .write(|tx| {
                spaces::set_account_rules(
                    tx,
                    &Viewer::bare(WS, ME),
                    &h.me,
                    &json!([{"when": {"kind": "all"}, "act": "draft"}]),
                    now,
                )
            })
            .unwrap();
        // Seed `uhan` as a known account so structured mention resolves to her account_id.
        let _ = push(
            &h,
            &h.me.clone(),
            msg("C_OPS", "g_seed", "uhan", "大家好", false),
        )
        .await;
        h.env.clock.advance(5 * 60_000);

        // Message in group explicitly `@`-mentioning ONLY `uhan` (not owner): must be gated deterministically.
        let mut to_other =
            FakeChannel::message("C_OPS", "g_other", "ulilei", "@韩梅梅 帮我看下监控");
        to_other.created = h.ch.now();
        to_other.mentions.push(Mention {
            handle: Some(FakeChannel::handle("uhan")),
            name: "韩梅梅".into(),
            is_connection: false,
        });
        let prompts_before = h.env.gateway.prompts.lock().unwrap().len();
        let res_other = push(&h, &h.me.clone(), to_other).await;
        let stopped_at_inbound = res_other.response.as_ref().is_some_and(|r| {
            r.act == steve_channels::rules::Act::Record
                && (r.label == steve_channels::rules::LABEL_UNADDRESSED
                    || r.label == steve_channels::decide::LABEL_ADDRESSED_TO_OTHERS)
        });
        let g6_decision = h
            .env
            .store()
            .read(|r| steve_channels::decide::gate(r, WS, &res_other.message_id, false, h.ch.now()))
            .unwrap();
        let gated_other = stopped_at_inbound
            && g6_decision == Some(steve_channels::decide::Gate::AddressedToOthers);
        let zero_llm_calls = h.env.gateway.prompts.lock().unwrap().len() == prompts_before;

        // Owner speaks earlier, then 10 min later `ulilei` quotes owner's message via `reply_to`:
        // context::load must deterministically resolve `reply_to_sender = "我"` and `reply_to_is_owner = true`.
        h.env.clock.advance(5 * 60_000);
        let mut own_m = FakeChannel::message("C_OPS", "g_own1", "upx", "今晚八点发版");
        own_m.created = h.ch.now();
        let own_in = push(&h, &h.me.clone(), own_m).await;
        h.env.clock.advance(10 * 60_000);
        let mut quote_m = FakeChannel::message("C_OPS", "g_q1", "ulilei", "收到，需要我值守吗？");
        quote_m.created = h.ch.now();
        quote_m.reply_to = Some("g_own1".into());
        let q_in = push(&h, &h.me.clone(), quote_m).await;
        let loaded = h
            .env
            .store()
            .read(|r| {
                let mine = steve_channels::context::mine(r, WS, &own_in.space_id)?;
                steve_channels::context::load(
                    r,
                    &own_in.space_id,
                    &q_in.message_id,
                    &mine,
                    spaces::Mode::Own,
                )
            })
            .unwrap()
            .unwrap();
        let quote_resolved =
            loaded.0.reply_to_is_owner && loaded.0.reply_to_sender.as_deref() == Some("我");

        report.case(
            "chan.reply.15_issue258_group_addressee_gate_and_quote_resolution",
            &[
                ("other_mention_gated_deterministically", gated_other),
                ("zero_model_calls_for_other_mention", zero_llm_calls),
                ("quoted_owner_message_resolved", quote_resolved),
            ],
            &[],
        );
    }

    // 16 (#268): Burst single reply — 3 rapid DM messages on 1 topic -> 1 active draft
    {
        let h = setup_with(r#"{"outcome":"reply","text":"收到，今晚8点前我会同步测试同学。","needs_approval":false,"why":"合并回复整阵消息"}"#).await;
        let me = h.me.clone();
        let t0 = h.ch.now();
        let mut m1 = FakeChannel::message("D_LI", "b1", "ulilei", "在吗");
        m1.created = t0;
        let in1 = push(&h, &me, m1).await;
        let d1 = in1.response.as_ref().and_then(|r| r.out_message_id.clone());

        h.env.clock.advance(10_000);
        let mut m2 = FakeChannel::message("D_LI", "b2", "ulilei", "发版窗口改到今晚8点了");
        m2.created = h.ch.now();
        let in2 = push(&h, &me, m2).await;
        let d2 = in2.response.as_ref().and_then(|r| r.out_message_id.clone());

        h.env.clock.advance(10_000);
        let mut m3 = FakeChannel::message("D_LI", "b3", "ulilei", "需要提前通知测试吗？");
        m3.created = h.ch.now();
        let in3 = push(&h, &me, m3).await;
        let d3 = in3.response.as_ref().and_then(|r| r.out_message_id.clone());

        let open_drafts = h
            .env
            .store()
            .read(|r| steve_store::channel_message_repo::dialogue::pending(r, &in3.space_id, 10))
            .unwrap();
        // Combined burst text in the final composer prompt contains all 3 messages.
        let saw_all_three = h
            .env
            .gateway
            .prompts
            .lock()
            .unwrap()
            .last()
            .is_some_and(|p| {
                p.contains("在吗")
                    && p.contains("发版窗口改到今晚8点了")
                    && p.contains("需要提前通知测试吗")
            });
        report.case(
            "chan.reply.16_issue268_burst_coalesces_to_single_draft",
            &[
                ("single_open_draft_remains", open_drafts.len() == 1),
                (
                    "earlier_drafts_superseded",
                    d1.is_some() && d2.is_some() && d3.is_some(),
                ),
                ("composer_saw_full_burst", saw_all_three),
            ],
            &[],
        );
    }

    // 17 (#268): Multi-topic burst splits into separate drafts sharing burst_id without retiring each other
    {
        let split_json = r#"{"outcome":"reply","text":"","replies":[{"topic":"评审时间","text":"明早10点评审可以。","needs_approval":false,"why":"确认评审时间"},{"topic":"报销单","text":"报销单我下午看下进度。","needs_approval":false,"why":"回复报销查询"}]}"#;
        let h = setup_with(split_json).await;
        let me = h.me.clone();
        let mut m1 = FakeChannel::message(
            "D_LI",
            "s1",
            "ulilei",
            "明早10点评审能定吗？另外报销单审批过了吗？",
        );
        m1.created = h.ch.now();
        let in1 = push(&h, &me, m1).await;
        let open_drafts = h
            .env
            .store()
            .read(|r| steve_store::channel_message_repo::dialogue::pending(r, &in1.space_id, 10))
            .unwrap();
        let has_both = open_drafts.len() == 2
            && open_drafts
                .iter()
                .any(|(_, t)| t.contains("明早10点评审可以"))
            && open_drafts
                .iter()
                .any(|(_, t)| t.contains("报销单我下午看下进度"));
        report.case(
            "chan.reply.17_issue268_multi_topic_burst_splits_cleanly",
            &[("two_topic_drafts_open_together", has_both)],
            &[],
        );
    }

    // 18 (#275): Owner-voice cross-context reuses #573 me memories + other chat + main thread + open items
    {
        let h = setup_with(r#"{"outcome":"reply","text":"灰度准备中，今天会给结论。","needs_approval":false,"why":"结合上下文答复"}"#).await;
        let me = h.me.clone();
        let now = h.ch.now();
        // (a) Owner `me` personal memory (#573)
        h.env
            .store()
            .write(|w| {
                let me_party = steve_store::Contacts::new(w).user_party(WS, ME)?.unwrap();
                let _ = steve_store::knowledge::Memories(w).create(
                    WS,
                    &steve_store::base::mems::NewMem {
                        kind: "fact".into(),
                        text: "星河发布默认由主人最终签字确认".into(),
                        source_type: "manual".into(),
                        source_id: None,
                        source_label: "主人记忆".into(),
                        visibility: steve_store::base::Visibility::Workspace,
                        level: steve_store::base::Level::Reasoning,
                        status: "confirmed".into(),
                        valid_until: None,
                        writer: "manual".into(),
                        decision_id: None,
                        created_by: ME.into(),
                        pinned: true,
                        about: vec![steve_store::base::ObjRef::new("party", &me_party)],
                    },
                )?;
                Ok::<_, steve_store::StoreError>(())
            })
            .unwrap();
        // (c) Main Steve conversation turn
        h.env
            .store()
            .messages()
            .add(
                WS,
                "me",
                "星河发布灰度配置已经在预发验证通过",
                None,
                &json!({}),
                conversations::MAIN_CONV,
            )
            .unwrap();
        // (d) Open work item
        let feat_tag = h
            .env
            .store()
            .tags()
            .kinds(WS)
            .unwrap()
            .into_iter()
            .find(|tag| tag.name == "feat")
            .unwrap()
            .id;
        let _ = h
            .env
            .store()
            .items()
            .create(
                WS,
                &steve_store::NewItem {
                    title: "星河发布灰度准备".into(),
                    goal: "核对星河发布清单".into(),
                    kind: Some("task".into()),
                    tags: vec![feat_tag],
                    ..Default::default()
                },
            )
            .unwrap();
        // (b) Other visible IM space message
        let mut ops_msg =
            FakeChannel::message("C_OPS", "ops_xh", "uhan", "星河发布压测已完成无报错");
        ops_msg.created = now;
        let _ = push(&h, &me, ops_msg).await;

        // Now ulilei asks about 星河发布 in DM D_LI -> composer prompt must receive all 4 cross-context sources!
        let mut ask = FakeChannel::message("D_LI", "dm_xh", "ulilei", "星河发布现在进度怎么样？");
        ask.created = now + 5_000;
        let _ = push(&h, &me, ask).await;
        let prompt = h
            .env
            .gateway
            .prompts
            .lock()
            .unwrap()
            .last()
            .cloned()
            .unwrap_or_default();
        report.case(
            "chan.reply.18_issue275_owner_cross_context_and_573_reuse",
            &[
                (
                    "includes_573_me_memory",
                    prompt.contains("星河发布默认由主人最终签字确认"),
                ),
                (
                    "includes_other_chat_snippet",
                    prompt.contains("星河发布压测已完成无报错"),
                ),
                (
                    "includes_main_thread_snippet",
                    prompt.contains("星河发布灰度配置已经在预发验证通过"),
                ),
                ("includes_open_item", prompt.contains("星河发布灰度准备")),
                ("tagged_for_reasoning_only", prompt.contains("仅用于推理")),
            ],
            &[],
        );
    }

    // 19 (#277): Owner emoji reaction supersedes open draft and does NOT silence subsequent messages
    {
        let h = setup_with(REPLY_OK).await;
        let me = h.me.clone();
        let mut m1 = FakeChannel::message("D_LI", "rx1", "ulilei", "帮我看下这个文档");
        m1.created = h.ch.now();
        let in1 = push(&h, &me, m1).await;
        let draft1 = in1
            .response
            .as_ref()
            .and_then(|r| r.out_message_id.clone())
            .unwrap();

        h.ch.on_reaction(
            WS,
            &me,
            steve_channels::ReactionEvent {
                space_ext_id: Some("D_LI".into()),
                message_ext_id: Some("rx1".into()),
                message_source_id: None,
                actor: FakeChannel::handle("upx"),
                actor_name: Some("我".into()),
                actor_is_bot: false,
                origin: steve_channels::ReactionOrigin::Manual,
                emoji: "THUMBSUP".into(),
                at: h.ch.now() + 2_000,
                time_source: steve_channels::ReactionTimeSource::Provider,
                revision: None,
                removed: false,
            },
        )
        .unwrap();

        let draft1_status = h
            .env
            .store()
            .read(|r| steve_store::channel_message_repo::projection::get(r, &draft1))
            .unwrap()
            .unwrap()
            .status;

        // Subsequent unrelated message 10s later MUST still get drafted (reaction != live presence).
        h.env.clock.advance(10_000);
        let mut m2 = FakeChannel::message("D_LI", "rx2", "ulilei", "另外明天下午有空吗？");
        m2.created = h.ch.now();
        let in2 = push(&h, &me, m2).await;
        let draft2_opened = in2
            .response
            .as_ref()
            .and_then(|r| r.out_message_id.clone())
            .is_some();

        report.case(
            "chan.reply.19_issue277_owner_reaction_supersedes_without_live_silencing",
            &[
                ("draft_superseded_by_reaction", draft1_status == "discarded"),
                ("next_message_still_drafted", draft2_opened),
            ],
            &[],
        );
    }

    // 20 (#278): Unsupported media explicitly asks owner instead of silently skipping or guessing
    {
        let h = setup_with(REPLY_OK).await;
        let me = h.me.clone();
        // Image-only message with no MediaSource wired -> must open an explicit MediaUnsupported card!
        let mut img_m = FakeChannel::message("D_LI", "img1", "ulilei", "[image]");
        img_m.created = h.ch.now();
        img_m.attachments = vec![json!({"type": "image", "key": "img_k1", "name": "error.png"})];
        let prompts_before = h.env.gateway.prompts.lock().unwrap().len();
        let in_img = push(&h, &me, img_m).await;
        let decision_id = in_img.response.as_ref().and_then(|r| r.decision_id.clone());
        let question = decision_id
            .as_deref()
            .and_then(|id| {
                h.env
                    .store()
                    .read(|r| steve_store::decision_repo::get(r, id))
                    .ok()
                    .flatten()
            })
            .map(|d| d.question)
            .unwrap_or_default();
        let explicit_card = decision_id.is_some()
            && (question.contains("不支持")
                || question.contains("未接通")
                || question.contains("读取"));
        let did_not_guess = h.env.gateway.prompts.lock().unwrap().len() == prompts_before;

        report.case(
            "chan.reply.20_issue278_unreadable_media_explicitly_reports_to_owner",
            &[
                ("explicit_owner_card_opened", explicit_card),
                ("never_guessed_without_reading", did_not_guess),
            ],
            &[],
        );
    }

    // 21 (#213 + outbound confirm + [AI] mark preference): context endpoint + default-on & toggleable [AI]
    {
        let h = setup_with(REPLY_OK).await;
        let me = h.me.clone();
        let api = steve_channels::api::routes(h.ch.clone())
            .merge(steve_channels::channels_api::routes(h.ch.clone()))
            .with_state(h.env.modules.clone());
        let mut m1 = FakeChannel::message("D_LI", "ctx1", "ulilei", "上线前确认下配置");
        m1.created = h.ch.now();
        let in1 = push(&h, &me, m1).await;
        let out_id = in1
            .response
            .as_ref()
            .and_then(|r| r.out_message_id.clone())
            .unwrap();
        let (st, ctx) = steve_modules::testkit::call(
            &api,
            Some(&caller(WS, ME)),
            "GET",
            &format!("/chan-messages/{out_id}/context"),
            None,
        )
        .await;
        let has_who_what_why = st == 200
            && ctx["recipient"]["space_id"] == json!(in1.space_id)
            && ctx["recipient"]["reply_to_sender"].is_string()
            && ctx["text"].as_str().is_some_and(|s| !s.is_empty())
            && ctx["why"].as_str().is_some_and(|s| !s.is_empty())
            && ctx["ai_mark"] == json!(true);

        // Toggle `ai_mark = false` on the space and verify context reflects it.
        let (pst, _) = steve_modules::testkit::call(
            &api,
            Some(&caller(WS, ME)),
            "PATCH",
            &format!("/spaces/{}", in1.space_id),
            Some(&json!({"ai_mark": false})),
        )
        .await;
        let (_, ctx_off) = steve_modules::testkit::call(
            &api,
            Some(&caller(WS, ME)),
            "GET",
            &format!("/chan-messages/{out_id}/context"),
            None,
        )
        .await;
        report.case(
            "chan.reply.21_issue213_outbound_confirm_context_and_ai_mark_toggle",
            &[
                ("context_has_who_what_why_and_ai_mark", has_who_what_why),
                (
                    "space_ai_mark_toggle_persisted",
                    pst == 200 && ctx_off["ai_mark"] == json!(false),
                ),
            ],
            &[],
        );
    }

    // 22 (P1-21): Generic non-Feishu provider open link resolves without any Feishu hardcoding
    {
        struct AcmeLinks;
        impl steve_channels::open_link::PlatformLinks for AcmeLinks {
            fn id(&self) -> &'static str {
                "acme"
            }
            fn native(&self, adapter: &str) -> bool {
                adapter == "acme_bot"
            }
            fn chat(&self, _config: &Value, chat_id: &str) -> Option<String> {
                Some(format!("https://acme.example/chat/{chat_id}"))
            }
        }
        steve_channels::open_link::register(Arc::new(AcmeLinks));
        let p = steve_channels::open_link::platform_of("acme_bot", &json!({}));
        let url = p.as_ref().and_then(|l| l.chat(&json!({}), "room_42"));
        report.case(
            "chan.reply.22_p1_21_generic_provider_open_link",
            &[(
                "resolves_generic_provider_link",
                p.as_ref().map(|l| l.id()) == Some("acme")
                    && url.as_deref() == Some("https://acme.example/chat/room_42"),
            )],
            &[],
        );
    }

    report.finish(22);
}
