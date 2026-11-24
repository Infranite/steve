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

    report.finish(14);
}
