//! Owner presence and the per-space reply setting, on your own account (synthetic chats): a reply in
//! your name is not drafted, opened, approved by YOLO, retried by YOLO or sent once you answered the
//! chat yourself, or while you are chatting there; your own message closes what Steve still had
//! waiting about older messages; one sender's run of messages is answered once; and `reply_as_you`
//! (`draft` / `record` / `off`) is decided in code before any model reads a message.

use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use axum::Router;
use serde_json::{Value, json};
use steve_channels::compose::{self, Composer};
use steve_channels::disclosure::{CHECK_HEAD, Disclosure};
use steve_channels::fake::channel::FakeChannel;
use steve_channels::inbound::{LABEL_REPLY_OFF, LABEL_REPLY_RECORD};
use steve_channels::outbound::{OutLink, Outbox};
use steve_channels::presence::{self, BY_LATER, BY_YOU, LABEL_LIVE, LABEL_REPLIED};
use steve_channels::respond::SUBJECT_MESSAGE;
use steve_channels::{Channels, Inbound, InboundMessage, Mention, Replies, SpaceKind, api, spaces};
use steve_flow::nodes::action::Action;
use steve_flow::testkit::FakeCore;
use steve_flow::{FlowSetup, LinearExecutor, NodeOutcome, RunCx};
use steve_modules::accounts::connections::{self, Connections, ConnectionsConfig};
use steve_modules::outbound::YOLO_ACTOR;
use steve_modules::testkit::{ME, TestEnv, WS, call, caller};
use steve_modules::vault::Vault;
use steve_modules::vault::keystore::FileKeyStore;
use steve_modules::{ApiResult, FakeClock, GatewayOut};
use steve_store::base::Viewer;
use tempfile::TempDir;

const PASS: &str = r#"{"verdict": "pass", "spans": []}"#;
const REPLY: &str = r#"{"text": "好 我晚点看", "needs_approval": false, "why": "对方在等"}"#;

/// What happens elsewhere while the composer “thinks”.
type During = Box<dyn FnOnce() + Send>;

/// The composer's model: answers [`REPLY`], keeps the prompts, lets `advance` ms pass while it
/// “thinks” (drafting takes minutes in real life) and runs `during` (once) meanwhile.
struct Slow {
    clock: Arc<FakeClock>,
    advance: AtomicI64,
    prompts: Mutex<Vec<String>>,
    during: Mutex<Option<During>>,
}

#[async_trait]
impl compose::PinnedModel for Slow {
    async fn run(
        &self,
        model: &str,
        prompt: &str,
        t: std::time::Duration,
    ) -> Result<GatewayOut, String> {
        self.run_with_images(model, prompt, &[], t).await
    }
    async fn run_with_images(
        &self,
        _: &str,
        prompt: &str,
        _: &[Value],
        _: std::time::Duration,
    ) -> Result<GatewayOut, String> {
        self.prompts.lock().unwrap().push(prompt.into());
        self.clock.advance(self.advance.load(Ordering::SeqCst));
        let during = self.during.lock().unwrap().take();
        if let Some(f) = during {
            f();
        }
        Ok(GatewayOut {
            text: REPLY.into(),
            ..Default::default()
        })
    }
}

/// C4's `reply_as`, so a space's rules turned into a flow validate (it never sends here).
struct StubReplyAs;

#[async_trait]
impl Action for StubReplyAs {
    fn name(&self) -> &'static str {
        "reply_as"
    }
    fn effectful(&self, _: &Value) -> bool {
        true
    }
    fn validate(&self, _: &Value) -> Result<(), String> {
        Ok(())
    }
    async fn run(&self, _: &mut RunCx<'_>, _: &Value, _: &Value) -> NodeOutcome {
        NodeOutcome::Fail("stub".into())
    }
}

struct H {
    env: TestEnv,
    _keys: TempDir,
    ch: Channels,
    fake: Arc<FakeChannel>,
    outbox: Arc<Outbox>,
    model: Arc<Slow>,
    api: Router,
    me: String,
    my_account: String,
    oncall: String,
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
    env.store()
        .with_conn(|c| {
            c.execute_batch(
                "INSERT INTO bots (id, workspace_id, name, main, harness, persona, role, rules, model, updated) VALUES
                    ('steve', 'default', 'Steve', 1, 'claude', '', '', '', 'fixture-model', 1);
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
    let model = Arc::new(Slow {
        clock: env.clock.clone(),
        advance: AtomicI64::new(0),
        prompts: Mutex::new(Vec::new()),
        during: Mutex::new(None),
    });
    let replies = Arc::new(
        Replies::new()
            .outbound(outbox.clone())
            .composer(Composer::new().pinned(model.clone())),
    );
    let flow = flow_setup
        .connections(conns.clone())
        .action(Arc::new(StubReplyAs))
        .build(env.modules.clone(), Arc::new(LinearExecutor));
    let ch = Channels::builder(env.modules.clone(), conns, Arc::new(flow.clone()))
        .adapter(fake.clone())
        .responder(replies)
        .flow(flow)
        .build();
    let api = api::routes(ch.clone()).with_state(env.modules.clone());
    let (me, my_account) =
        FakeChannel::seed_connection(env.store(), &caller(WS, ME), Some("upx")).expect("you");
    let (oncall, _) =
        FakeChannel::seed_connection(env.store(), &caller(WS, ME), None).expect("bot");
    let people = [("ulilei", "李雷"), ("uhan", "韩梅梅")];
    fake.space("C_DEV", "开发群", SpaceKind::Group, false, &people);
    fake.space("D_LI", "", SpaceKind::Dm, false, &[("ulilei", "李雷")]);
    fake.space("C_BOT", "值班群", SpaceKind::Group, false, &people);
    let h = H {
        env,
        _keys: keys,
        ch,
        fake,
        outbox,
        model,
        api,
        me,
        my_account,
        oncall,
    };
    h.env.gateway.script(CHECK_HEAD, PASS);
    let now = h.ch.now();
    let v = Viewer::bare(WS, ME);
    h.env
        .store()
        .transaction(|tx| {
            spaces::set_account_rules(
                tx,
                &v,
                &h.me,
                &json!([{"when": {"kind": "mention_or_dm"}, "act": "draft"}]),
                now,
            )?;
            spaces::set_account_rules(
                tx,
                &v,
                &h.oncall,
                &json!([{"when": {"kind": "mention_or_dm"}, "act": "reply"}]),
                now,
            )
        })
        .expect("rules");
    h
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

async fn push(h: &H, conn: &str, m: InboundMessage) -> Inbound {
    h.ch.on_message(WS, conn, m).await.expect("message")
}

fn row(h: &H, sql: &str, p: &[&str]) -> Vec<Value> {
    let (sql, p): (String, Vec<String>) = (sql.into(), p.iter().map(|s| s.to_string()).collect());
    h.env
        .store()
        .reader(move |c| {
            let mut st = c.prepare(&sql)?;
            let n = st.column_count();
            let v = st.query_row(rusqlite::params_from_iter(p.iter()), |r| {
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
            })?;
            Ok::<_, steve_store::StoreError>(v)
        })
        .expect("row")
}

fn count(h: &H, sql: &str, p: &[&str]) -> i64 {
    row(h, sql, p)[0].as_i64().expect("count")
}

fn drafts(h: &H) -> i64 {
    count(
        h,
        "SELECT count(*) FROM chan_messages WHERE direction = 'out' AND status = 'draft'",
        &[],
    )
}

fn decision(h: &H, id: &str) -> (String, Value) {
    let r = row(
        h,
        "SELECT status, result FROM decisions WHERE id = ?",
        &[id],
    );
    (
        r[0].as_str().unwrap().to_string(),
        serde_json::from_str(r[1].as_str().unwrap_or("null")).unwrap(),
    )
}

fn status(h: &H, id: &str) -> String {
    row(h, "SELECT status FROM chan_messages WHERE id = ?", &[id])[0]
        .as_str()
        .unwrap()
        .to_string()
}

/// Your own message stored by another way in (another device's connection), so nothing reacts to it
/// when it lands: only the later gates can see it.
fn you_said_elsewhere(h: &H, space_ext: &str, secs: i64) {
    let now = h.ch.now() + secs * 1000;
    let space: String = row(
        h,
        "SELECT id FROM spaces WHERE external_id = ?",
        &[space_ext],
    )[0]
    .as_str()
    .unwrap()
    .into();
    let party: String = row(
        h,
        "SELECT party_id FROM accounts WHERE id = ?",
        &[&h.my_account],
    )[0]
    .as_str()
    .unwrap()
    .into();
    h.env
        .store()
        .with_conn(|c| {
            c.execute(
                "INSERT INTO chan_messages (id, workspace_id, space_id, external_id, direction, sender_account_id,
                    sender_party_id, text, status, created)
                 VALUES (?, ?, ?, ?, 'in', ?, ?, '我来吧', 'received', ?)",
                rusqlite::params![
                    steve_store::uid("cm"),
                    WS,
                    space,
                    format!("elsewhere_{now}"),
                    h.my_account,
                    party,
                    now
                ],
            )?;
            Ok(())
        })
        .expect("your message");
}

fn yolo(h: &H, d: &str, choice: &str) -> ApiResult<steve_store::Decision> {
    steve_modules::decisions::dispatch(&h.env.modules, &caller(WS, YOLO_ACTOR), d, choice, "")
        .expect("ours")
}

fn you_decide(h: &H, d: &str, choice: &str) -> ApiResult<steve_store::Decision> {
    steve_modules::decisions::dispatch(&h.env.modules, &caller(WS, ME), d, choice, "")
        .expect("ours")
}

/// A draft in your name for a private chat message, and its card.
async fn drafted(h: &H, id: &str) -> (String, String) {
    let r = push(
        h,
        &h.me.clone(),
        msg(h, "D_LI", id, "ulilei", "今晚能看下吗", 0),
    )
    .await
    .response
    .expect("response");
    (
        r.out_message_id.expect("draft"),
        r.decision_id.expect("card"),
    )
}

// ---------------------------------------------------------------- gate 1: before drafting

#[tokio::test(flavor = "multi_thread")]
async fn you_chatting_in_a_private_chat_stops_the_draft_before_any_model() {
    let h = setup().await;
    let me = h.me.clone();
    push(&h, &me, msg(&h, "D_LI", "y1", "upx", "在吗", -60)).await;
    let r = push(&h, &me, msg(&h, "D_LI", "l1", "ulilei", "在 你说", 0))
        .await
        .response
        .expect("response");
    assert_eq!(r.label, LABEL_LIVE);
    assert_eq!(r.out_message_id, None);
    assert_eq!(drafts(&h), 0);
    assert!(
        h.model.prompts.lock().unwrap().is_empty(),
        "no composer ran"
    );

    // once you have been quiet for longer than the window, it drafts again
    h.env.clock.advance(presence::OWNER_LIVE_MS + 1_000);
    let r = push(&h, &me, msg(&h, "D_LI", "l2", "ulilei", "还在吗", 0))
        .await
        .response
        .unwrap();
    assert!(r.out_message_id.is_some(), "{r:?}");
}

#[tokio::test(flavor = "multi_thread")]
async fn in_a_group_an_at_while_you_are_chatting_there_is_yours() {
    let h = setup().await;
    let me = h.me.clone();
    push(
        &h,
        &me,
        msg(&h, "C_DEV", "y1", "upx", "这个版本先这样", -60),
    )
    .await;
    push(&h, &me, msg(&h, "C_DEV", "h1", "uhan", "午饭吃啥", -30)).await;
    // someone else spoke between, but it @-s you while you are there
    let r = push(
        &h,
        &me,
        at_you(msg(&h, "C_DEV", "l1", "ulilei", "你看呢", 0)),
    )
    .await
    .response
    .unwrap();
    assert_eq!(r.label, LABEL_LIVE);
    assert_eq!(drafts(&h), 0);
    // after you have been away for longer than the window, the next @ is drafted
    h.env.clock.advance(presence::OWNER_LIVE_MS + 1_000);
    let r = push(
        &h,
        &me,
        at_you(msg(&h, "C_DEV", "l2", "ulilei", "还在吗", 0)),
    )
    .await
    .response
    .unwrap();
    assert!(r.out_message_id.is_some(), "{r:?}");
}

#[tokio::test(flavor = "multi_thread")]
async fn an_answer_you_already_gave_stops_the_draft() {
    let h = setup().await;
    let me = h.me.clone();
    // your answer reached Steve first (its queue was behind)
    push(&h, &me, msg(&h, "D_LI", "y1", "upx", "看了 没问题", -5)).await;
    let r = push(
        &h,
        &me,
        msg(&h, "D_LI", "l1", "ulilei", "帮忙看下这个", -30),
    )
    .await
    .response
    .unwrap();
    assert_eq!(r.label, LABEL_REPLIED);
    assert_eq!(drafts(&h), 0);
    assert!(h.model.prompts.lock().unwrap().is_empty());
}

// ---------------------------------------------------------------- gate 2: after drafting

#[tokio::test(flavor = "multi_thread")]
async fn you_answering_while_it_drafts_drops_the_draft() {
    let h = setup().await;
    let me = h.me.clone();
    // drafting takes a minute and a half; you answer thirty seconds in
    push(&h, &me, msg(&h, "D_LI", "l0", "ulilei", "早", -600)).await;
    assert_eq!(drafts(&h), 1);
    h.model.prompts.lock().unwrap().clear();
    h.model.advance.store(90_000, Ordering::SeqCst);
    you_said_elsewhere(&h, "D_LI", 30);
    let r = push(&h, &me, msg(&h, "D_LI", "l1", "ulilei", "帮忙看下这个", 0))
        .await
        .response
        .unwrap();
    assert_eq!(h.model.prompts.lock().unwrap().len(), 1, "the composer ran");
    assert_eq!(r.label, LABEL_REPLIED);
    assert_eq!(r.out_message_id, None);
    assert_eq!(r.decision_id, None);
    assert_eq!(drafts(&h), 1, "only the earlier message's");
}

// ---------------------------------------------------------------- your own message

#[tokio::test(flavor = "multi_thread")]
async fn your_own_message_closes_older_drafts_and_cards() {
    let h = setup().await;
    let me = h.me.clone();
    let (draft, card) = drafted(&h, "l1").await;
    // a hand-off card in a group
    let space = push(&h, &me, msg(&h, "C_DEV", "h0", "uhan", "早", -600))
        .await
        .space_id;
    let now = h.ch.now();
    h.env
        .store()
        .transaction(|tx| {
            spaces::patch(
                tx,
                &Viewer::bare(WS, ME),
                &space,
                &spaces::SpacePatch {
                    rules: Some(Some(
                        json!([{"when": {"kind": "mention_or_dm"}, "act": "handoff"}]),
                    )),
                    ..Default::default()
                },
                now,
            )
        })
        .expect("rules");
    let ask = push(
        &h,
        &me,
        at_you(msg(&h, "C_DEV", "h1", "uhan", "这个谁负责", 0)),
    )
    .await
    .response
    .unwrap()
    .decision_id
    .expect("hand-off");
    assert_eq!(decision(&h, &ask).0, "open");

    h.env.clock.advance(10_000);
    push(&h, &me, msg(&h, "D_LI", "y1", "upx", "晚上看", 0)).await;
    assert_eq!(status(&h, &draft), "discarded");
    let (st, res) = decision(&h, &card);
    assert_eq!(
        (st.as_str(), res["superseded"].as_str()),
        ("expired", Some(BY_YOU))
    );
    // the group's card waits: you spoke elsewhere
    assert_eq!(decision(&h, &ask).0, "open");
    push(&h, &me, msg(&h, "C_DEV", "y2", "upx", "我负责", 0)).await;
    let (st, res) = decision(&h, &ask);
    assert_eq!(
        (st.as_str(), res["superseded"].as_str()),
        ("expired", Some(BY_YOU))
    );
    assert!(h.fake.sent.lock().unwrap().is_empty());
}

// ---------------------------------------------------------------- gate 3: YOLO

#[tokio::test(flavor = "multi_thread")]
async fn yolo_never_approves_what_you_answered_yourself() {
    let h = setup().await;
    let (draft, card) = drafted(&h, "l1").await;
    you_said_elsewhere(&h, "D_LI", 5);
    h.env.clock.advance(10_000);
    let d = yolo(&h, &card, "yes").expect("answered");
    assert_eq!(d.status, "expired");
    assert_eq!(d.result["superseded"], BY_YOU);
    assert_eq!(status(&h, &draft), "discarded");
    assert_eq!(h.outbox.send_due(&h.ch).await, 0);
    assert!(h.fake.sent.lock().unwrap().is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn what_you_approve_yourself_still_goes() {
    let h = setup().await;
    let (draft, card) = drafted(&h, "l1").await;
    you_said_elsewhere(&h, "D_LI", 5);
    h.env.clock.advance(10_000);
    you_decide(&h, &card, "yes").expect("approved");
    h.env.clock.advance(11_000);
    assert_eq!(h.outbox.send_due(&h.ch).await, 1);
    assert_eq!(status(&h, &draft), "sent");
}

#[tokio::test(flavor = "multi_thread")]
async fn yolo_never_retries_what_you_answered_yourself() {
    let h = setup().await;
    let (draft, card) = drafted(&h, "l1").await;
    you_decide(&h, &card, "yes").expect("approved");
    *h.fake.fail_send.lock().unwrap() = Some("假渠道发不出去".into());
    h.env.clock.advance(11_000);
    h.outbox.send_due(&h.ch).await;
    assert_eq!(status(&h, &draft), "failed");
    let fail: String = row(
        &h,
        "SELECT decision_id FROM chan_messages WHERE id = ?",
        &[&draft],
    )[0]
    .as_str()
    .unwrap()
    .into();
    *h.fake.fail_send.lock().unwrap() = None;
    you_said_elsewhere(&h, "D_LI", 1);
    h.env.clock.advance(5_000);
    let d = yolo(&h, &fail, "retry").expect("answered");
    assert_eq!(d.status, "expired");
    assert_eq!(d.result["superseded"], BY_YOU);
    assert_eq!(status(&h, &draft), "discarded");
    assert_eq!(h.outbox.send_due(&h.ch).await, 0);
    assert!(h.fake.sent.lock().unwrap().is_empty());
}

// ---------------------------------------------------------------- gate 4: sending

#[tokio::test(flavor = "multi_thread")]
async fn a_yolo_approved_reply_is_not_sent_once_you_answered() {
    let h = setup().await;
    let (draft, card) = drafted(&h, "l1").await;
    let d = yolo(&h, &card, "yes").expect("approved");
    assert_eq!(d.status, "resolved");
    assert_eq!(status(&h, &draft), "pending");
    // you answer before the queue picks it up
    you_said_elsewhere(&h, "D_LI", 1);
    h.env.clock.advance(2_000);
    assert_eq!(h.outbox.send_due(&h.ch).await, 0);
    assert_eq!(status(&h, &draft), "discarded");
    assert!(h.fake.sent.lock().unwrap().is_empty());
}

// ---------------------------------------------------------------- one sender's turn

#[tokio::test(flavor = "multi_thread")]
async fn one_senders_run_of_messages_is_answered_once() {
    let h = setup().await;
    let me = h.me.clone();
    let first = push(&h, &me, msg(&h, "D_LI", "l1", "ulilei", "在吗", 0))
        .await
        .response
        .unwrap();
    let card1 = first.decision_id.expect("first card");
    h.env.clock.advance(20_000);
    let second = push(
        &h,
        &me,
        msg(&h, "D_LI", "l2", "ulilei", "想问下周五的会", 0),
    )
    .await
    .response
    .unwrap();
    assert!(second.out_message_id.is_some());
    let prompts = h.model.prompts.lock().unwrap().clone();
    let last = prompts.last().unwrap();
    assert!(last.contains("在吗\n想问下周五的会"), "{last}");
    let (st, res) = decision(&h, &card1);
    assert_eq!(
        (st.as_str(), res["superseded"].as_str()),
        ("expired", Some(BY_LATER))
    );
    assert_eq!(drafts(&h), 1);
}

// ---------------------------------------------------------------- reply_as_you

async fn set_reply(h: &H, space: &str, v: &str) -> (u16, Value) {
    call(
        &h.api,
        Some(&caller(WS, ME)),
        "PATCH",
        &format!("/spaces/{space}"),
        Some(&json!({"reply_as_you": v})),
    )
    .await
}

#[tokio::test(flavor = "multi_thread")]
async fn record_only_drafts_nothing_and_reads_nothing() {
    let h = setup().await;
    let me = h.me.clone();
    let space = push(&h, &me, msg(&h, "D_LI", "y1", "upx", "嗯", -3600))
        .await
        .space_id;
    assert_eq!(set_reply(&h, &space, "never").await.0, 400);
    assert_eq!(set_reply(&h, &space, "record").await.0, 200);
    let s = call(
        &h.api,
        Some(&caller(WS, ME)),
        "GET",
        &format!("/spaces/{space}"),
        None,
    )
    .await
    .1;
    assert_eq!(s["reply_as_you"], "record");
    assert_eq!(s["role"], "record");
    let r = push(&h, &me, msg(&h, "D_LI", "l1", "ulilei", "帮我看下", 0))
        .await
        .response
        .unwrap();
    assert_eq!(r.label, LABEL_REPLY_RECORD);
    assert_eq!(drafts(&h), 0);
    assert!(h.model.prompts.lock().unwrap().is_empty());
    assert!(
        h.env.gateway.prompts.lock().unwrap().is_empty(),
        "no model read it"
    );

    // back to draft: drafted as before
    assert_eq!(set_reply(&h, &space, "draft").await.0, 200);
    h.env.clock.advance(1_000);
    let r = push(&h, &me, msg(&h, "D_LI", "l2", "ulilei", "帮我看下这个", 0))
        .await
        .response
        .unwrap();
    assert!(r.out_message_id.is_some());
}

#[tokio::test(flavor = "multi_thread")]
async fn record_only_starts_no_flow_on_someone_elses_message() {
    let h = setup().await;
    let me = h.me.clone();
    let space = push(&h, &me, msg(&h, "D_LI", "y1", "upx", "嗯", -3600))
        .await
        .space_id;
    let (s, v) = call(
        &h.api,
        Some(&caller(WS, ME)),
        "POST",
        &format!("/spaces/{space}/rules/to-flow"),
        None,
    )
    .await;
    assert_eq!(s, 200, "{v}");
    let runs = |id: &str| {
        count(
            &h,
            "SELECT count(*) FROM flow_runs r JOIN chan_messages m ON m.event_id = r.event_id WHERE m.id = ?",
            &[id],
        )
    };
    let i = push(&h, &me, msg(&h, "D_LI", "l0", "ulilei", "早", -60)).await;
    assert_eq!(runs(&i.message_id), 1, "the space's flow takes a message");
    assert_eq!(set_reply(&h, &space, "record").await.0, 200);
    let i = push(&h, &me, msg(&h, "D_LI", "l1", "ulilei", "帮我看下", 0)).await;
    assert_eq!(runs(&i.message_id), 0, "under 只记录 it does not");
    let ev = i.event.as_ref().and_then(|e| e.id()).expect("event");
    let r = row(
        &h,
        "SELECT outcome, outcome_label FROM ext_events WHERE id = ?",
        &[ev],
    );
    assert_eq!(r, [json!("recorded"), json!(LABEL_REPLY_RECORD)]);
}

#[tokio::test(flavor = "multi_thread")]
async fn off_leaves_the_space_alone_on_your_account() {
    let h = setup().await;
    let me = h.me.clone();
    let space = push(&h, &me, msg(&h, "D_LI", "y1", "upx", "嗯", -3600))
        .await
        .space_id;
    assert_eq!(set_reply(&h, &space, "off").await.0, 200);
    let i = push(&h, &me, msg(&h, "D_LI", "l1", "ulilei", "帮我看下", 0)).await;
    assert_eq!(i.response.as_ref().unwrap().label, LABEL_REPLY_OFF);
    assert_eq!(drafts(&h), 0);
    assert!(h.model.prompts.lock().unwrap().is_empty());
    // the message is kept, and its event wakes nothing
    let ev = i.event.as_ref().and_then(|e| e.id()).expect("event");
    let r = row(&h, "SELECT outcome FROM ext_events WHERE id = ?", &[ev]);
    assert_eq!(r[0], "recorded");
    assert_eq!(
        count(
            &h,
            "SELECT count(*) FROM chan_messages WHERE id = ?",
            &[&i.message_id]
        ),
        1
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn the_setting_does_not_apply_where_a_bot_speaks() {
    let h = setup().await;
    let oncall = h.oncall.clone();
    h.env.gateway.script(compose::H_SAFETY, REPLY);
    let space = push(&h, &oncall, msg(&h, "C_BOT", "b0", "uhan", "早", -3600))
        .await
        .space_id;
    assert_eq!(set_reply(&h, &space, "off").await.0, 200);
    let s = call(
        &h.api,
        Some(&caller(WS, ME)),
        "GET",
        &format!("/spaces/{space}"),
        None,
    )
    .await
    .1;
    assert!(
        s["conflicts"]
            .as_array()
            .unwrap()
            .iter()
            .any(|c| c["key"] == "reply_as_you" && c["level"] == "conflict"),
        "{s}"
    );
    let r = push(
        &h,
        &oncall,
        at_you(msg(&h, "C_BOT", "b1", "uhan", "在吗", 0)),
    )
    .await
    .response
    .unwrap();
    assert_ne!(r.label, LABEL_REPLY_OFF);
    assert!(r.out_message_id.is_some(), "{r:?}");
}

// ---------------------------------------------------------------- only your own words, only your own account

/// A message stored as if another way in delivered it (nothing reacts to it): written `secs` after
/// the message `like` (external id) by its sender. Returns its id.
fn stored_in(store: &steve_store::Store, like: &str, ext: &str, secs: i64) -> String {
    let id = steve_store::uid("cm");
    let (like, ext, idc) = (like.to_string(), ext.to_string(), id.clone());
    store
        .with_conn(move |c| {
            c.execute(
                "INSERT INTO chan_messages (id, workspace_id, space_id, external_id, direction, sender_account_id,
                    sender_party_id, text, status, created)
                 SELECT ?, workspace_id, space_id, ?, 'in', sender_account_id, sender_party_id, '还有个事', 'received',
                        created + ?
                   FROM chan_messages WHERE external_id = ? AND direction = 'in'",
                rusqlite::params![idc, ext, secs * 1000, like],
            )?;
            Ok(())
        })
        .expect("stored");
    id
}

fn handoff_here(h: &H, space: &str) {
    let now = h.ch.now();
    h.env
        .store()
        .transaction(|tx| {
            spaces::patch(
                tx,
                &Viewer::bare(WS, ME),
                space,
                &spaces::SpacePatch {
                    rules: Some(Some(
                        json!([{"when": {"kind": "mention_or_dm"}, "act": "handoff"}]),
                    )),
                    ..Default::default()
                },
                now,
            )
        })
        .expect("rules");
}

#[tokio::test(flavor = "multi_thread")]
async fn an_automatic_reply_on_your_account_is_not_you() {
    let h = setup().await;
    let me = h.me.clone();
    // another assistant answers on your account while you are away
    let auto = "[AI] 自动回复：我在开会，稍后回复";
    push(&h, &me, msg(&h, "D_LI", "y1", "upx", auto, -10)).await;
    let r = push(&h, &me, msg(&h, "D_LI", "l1", "ulilei", "在吗 有个事", 0))
        .await
        .response
        .unwrap();
    assert!(r.out_message_id.is_some(), "{r:?}");
    let card = r.decision_id.expect("card");
    // nor does it close the draft when it comes after it
    h.env.clock.advance(5_000);
    push(&h, &me, msg(&h, "D_LI", "y2", "upx", auto, 0)).await;
    assert_eq!(decision(&h, &card).0, "open");
    h.env.clock.advance(5_000);
    assert_eq!(yolo(&h, &card, "yes").expect("answered").status, "resolved");
}

#[tokio::test(flavor = "multi_thread")]
async fn where_a_bot_speaks_your_message_leaves_its_hand_offs_open() {
    let h = setup().await;
    let oncall = h.oncall.clone();
    let space = push(&h, &oncall, msg(&h, "C_BOT", "b0", "uhan", "早", -3600))
        .await
        .space_id;
    handoff_here(&h, &space);
    let ask = push(
        &h,
        &oncall,
        at_you(msg(&h, "C_BOT", "b1", "uhan", "这个谁负责", 0)),
    )
    .await
    .response
    .unwrap()
    .decision_id
    .expect("hand-off");
    h.env.clock.advance(10_000);
    // you write in the group (the bot receives it), and the bot's own account speaks
    let i = push(&h, &oncall, msg(&h, "C_BOT", "y1", "upx", "午饭吃啥", 0)).await;
    assert!(i.from_owner, "you, as the bot sees you");
    push(
        &h,
        &oncall,
        msg(&h, "C_BOT", "k1", FakeChannel::BOT, "收到", 0),
    )
    .await;
    assert_eq!(decision(&h, &ask).0, "open");
}

#[tokio::test(flavor = "multi_thread")]
async fn in_a_group_what_you_say_to_someone_else_leaves_another_persons_card_open() {
    let h = setup().await;
    let me = h.me.clone();
    let space = push(&h, &me, msg(&h, "C_DEV", "h0", "uhan", "早", -3600))
        .await
        .space_id;
    handoff_here(&h, &space);
    let ask = push(
        &h,
        &me,
        at_you(msg(&h, "C_DEV", "h1", "uhan", "这个谁负责", 0)),
    )
    .await
    .response
    .unwrap()
    .decision_id
    .expect("hand-off");
    // well after it, you answer someone else there
    h.env.clock.advance(presence::OWNER_LIVE_MS + 60_000);
    push(&h, &me, msg(&h, "C_DEV", "l1", "ulilei", "午饭吃啥", 0)).await;
    let mut to_li = msg(&h, "C_DEV", "y1", "upx", "都行", 0);
    to_li.reply_to = Some("l1".into());
    push(&h, &me, to_li).await;
    assert_eq!(decision(&h, &ask).0, "open");
    // your answer to it closes it
    h.env.clock.advance(10_000);
    let mut answer = msg(&h, "C_DEV", "y2", "upx", "我负责", 0);
    answer.reply_to = Some("h1".into());
    push(&h, &me, answer).await;
    let (st, res) = decision(&h, &ask);
    assert_eq!(
        (st.as_str(), res["superseded"].as_str()),
        ("expired", Some(BY_YOU))
    );
}

// ---------------------------------------------------------------- a turn and what was answered

#[tokio::test(flavor = "multi_thread")]
async fn a_message_already_answered_in_your_name_stays_out_of_the_next_turn() {
    let h = setup().await;
    let me = h.me.clone();
    let first = push(&h, &me, msg(&h, "D_LI", "l1", "ulilei", "在吗", 0))
        .await
        .response
        .unwrap();
    let (draft1, card1) = (first.out_message_id.unwrap(), first.decision_id.unwrap());
    assert_eq!(
        yolo(&h, &card1, "yes").expect("approved").status,
        "resolved"
    );
    h.env.clock.advance(11_000);
    assert_eq!(h.outbox.send_due(&h.ch).await, 1);
    assert_eq!(status(&h, &draft1), "sent");
    h.env.clock.advance(9_000);
    let second = push(
        &h,
        &me,
        msg(&h, "D_LI", "l2", "ulilei", "想问下周五的会", 0),
    )
    .await
    .response
    .unwrap();
    assert!(second.out_message_id.is_some());
    let last = h.model.prompts.lock().unwrap().last().unwrap().clone();
    assert!(!last.contains("在吗\n想问下周五的会"), "{last}");
    assert_eq!(status(&h, &draft1), "sent");
    assert_eq!(drafts(&h), 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn yolo_never_approves_a_draft_a_later_turn_took_while_it_drafts() {
    let h = setup().await;
    let me = h.me.clone();
    // the earlier message's draft is open when the later one's composer starts…
    let first = push(&h, &me, msg(&h, "D_LI", "l1", "ulilei", "在吗", 0))
        .await
        .response
        .unwrap();
    let (draft1, card1) = (first.out_message_id.unwrap(), first.decision_id.unwrap());
    h.env.clock.advance(20_000);
    // … and YOLO comes to it while that composer runs
    let got = Arc::new(Mutex::new(None));
    let (modules, card, g) = (h.env.modules.clone(), card1.clone(), got.clone());
    *h.model.during.lock().unwrap() = Some(Box::new(move || {
        let d =
            steve_modules::decisions::dispatch(&modules, &caller(WS, YOLO_ACTOR), &card, "yes", "")
                .expect("ours");
        *g.lock().unwrap() = Some(d);
    }));
    let second = push(
        &h,
        &me,
        msg(&h, "D_LI", "l2", "ulilei", "想问下周五的会", 0),
    )
    .await
    .response
    .unwrap();
    assert!(second.out_message_id.is_some());
    let d = got.lock().unwrap().take().expect("ran").expect("answered");
    assert_eq!(d.status, "expired");
    assert_eq!(d.result["superseded"], BY_LATER);
    assert_eq!(status(&h, &draft1), "discarded");
    assert_eq!(drafts(&h), 1);
    h.env.clock.advance(11_000);
    assert_eq!(h.outbox.send_due(&h.ch).await, 0);
    assert!(h.fake.sent.lock().unwrap().is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn a_draft_whose_message_a_later_turn_took_while_it_drafted_never_opens() {
    let h = setup().await;
    let me = h.me.clone();
    // the earlier message's composer is slow; meanwhile the later message comes and its turn,
    // answered first, takes the earlier one
    let store = h.env.store().clone();
    *h.model.during.lock().unwrap() = Some(Box::new(move || {
        let later = stored_in(&store, "l1", "l2", 20);
        store
            .transaction(|tx| presence::take_turn(tx, WS, &later))
            .expect("turn");
    }));
    let r = push(&h, &me, msg(&h, "D_LI", "l1", "ulilei", "在吗", 0))
        .await
        .response
        .unwrap();
    assert_eq!(r.out_message_id, None, "{r:?}");
    assert_eq!(r.decision_id, None);
    assert_eq!(r.label, BY_LATER);
    assert_eq!(drafts(&h), 0);
}

// ---------------------------------------------------------------- reply_as_you after the message came in

#[tokio::test(flavor = "multi_thread")]
async fn the_setting_holds_drafts_made_before_it_changed() {
    let h = setup().await;
    let (draft1, card1) = drafted(&h, "l1").await;
    let space: String = row(
        &h,
        "SELECT space_id FROM chan_messages WHERE id = ?",
        &[&draft1],
    )[0]
    .as_str()
    .unwrap()
    .into();
    h.env.clock.advance(120_000);
    let (draft2, card2) = drafted(&h, "l2").await;
    assert_eq!(
        yolo(&h, &card2, "yes").expect("approved").status,
        "resolved"
    );
    // set to 只记录 behind the page's back (nothing closed on the way): YOLO approves nothing more
    // and what it approved is not sent
    let now = h.ch.now();
    h.env
        .store()
        .transaction(|tx| {
            spaces::write_reply_as_you(tx, WS, &space, spaces::ReplyAsYou::Record, now)
        })
        .expect("set");
    let d = yolo(&h, &card1, "yes").expect("answered");
    assert_eq!(d.status, "expired");
    assert_eq!(d.result["superseded"], presence::BY_SETTING);
    assert_eq!(status(&h, &draft1), "discarded");
    h.env.clock.advance(11_000);
    assert_eq!(h.outbox.send_due(&h.ch).await, 0);
    assert_eq!(status(&h, &draft2), "discarded");
    assert!(h.fake.sent.lock().unwrap().is_empty());

    // set from the page: what is still open closes at once
    h.env
        .store()
        .transaction(|tx| {
            spaces::write_reply_as_you(tx, WS, &space, spaces::ReplyAsYou::Draft, now)
        })
        .expect("set");
    h.env.clock.advance(120_000);
    let (draft3, card3) = drafted(&h, "l3").await;
    assert_eq!(set_reply(&h, &space, "off").await.0, 200);
    let (st, res) = decision(&h, &card3);
    assert_eq!(
        (st.as_str(), res["superseded"].as_str()),
        ("expired", Some(presence::BY_SETTING))
    );
    assert_eq!(status(&h, &draft3), "discarded");
}
