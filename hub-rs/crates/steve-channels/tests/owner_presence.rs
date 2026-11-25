//! Owner presence and the per-space reply setting, on your own account (synthetic chats): a reply in
//! your name is not drafted, opened, approved by YOLO, retried by YOLO or sent once you answered the
//! chat yourself, or while you are chatting there; your own message closes what Steve still had
//! waiting about older messages; one sender's run of messages is answered once; and `reply_as_you`
//! (`draft` / `record` / `off`) is decided in code before any model reads a message.

#[path = "support/digest_env.rs"]
mod digest_env;
use digest_env::TestEnv;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{Arc, Mutex};
use steve_store::testkit::channels::inspect;
use steve_store::testkit::channels::seed;
use steve_store::testkit::channels::seed::people::Bot;

use async_trait::async_trait;
use axum::Router;
use serde_json::{Value, json};
use steve_channels::compose::{self, Composer};
use steve_channels::disclosure::{CHECK_HEAD, Disclosure};
use steve_channels::fake::channel::FakeChannel;
use steve_channels::inbound::{LABEL_REPLY_OFF, LABEL_REPLY_RECORD};
use steve_channels::outbound::{OutLink, Outbox};
use steve_channels::presence::{
    self, BY_LATER, BY_REACTED, BY_YOU, LABEL_LIVE, LABEL_REACTED, LABEL_REPLIED,
};
use steve_channels::respond::SUBJECT_MESSAGE;
use steve_channels::{
    Channels, Inbound, InboundMessage, Mention, ReactionEvent, ReactionOrigin, ReactionTimeSource,
    Replies, SpaceKind, api, spaces,
};
use steve_flow::nodes::action::Action;
use steve_flow::testkit::FakeCore;
use steve_flow::{FlowSetup, LinearExecutor, NodeOutcome, RunCx};
use steve_modules::accounts::connections::{self, Connections, ConnectionsConfig};
use steve_modules::testkit::{ME, WS, call, caller};
use steve_modules::vault::Vault;
use steve_modules::vault::keystore::FileKeyStore;
use steve_modules::{ApiResult, FakeClock};
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

impl Slow {
    fn on_compose(&self, prompt: &str) {
        self.prompts.lock().unwrap().push(prompt.into());
        self.clock.advance(self.advance.load(Ordering::SeqCst));
        let during = self.during.lock().unwrap().take();
        if let Some(f) = during {
            f();
        }
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
    *core.store.lock().unwrap() = Some(env.store().clone());
    steve_store::testkit::channels::seed::people::bots(
        env.store(),
        &[Bot {
            model: Some("fixture-model"),
            ..Bot::default()
        }],
    )
    .unwrap();
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
    env.gateway.script(compose::H_SAFETY, REPLY);
    let slow_hook = model.clone();
    env.gateway.on_run(move |_, prompt| {
        if steve_modules::gateway::current_tag().purpose == "compose" {
            slow_hook.on_compose(prompt);
        }
    });
    let replies = Arc::new(
        Replies::new()
            .outbound(outbox.clone())
            .composer(Composer::new()),
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
        .write(|tx| {
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

fn drafts(h: &H) -> i64 {
    inspect::messages::count_chan_messages_by_outgoing_and_draft(h.env.store()).unwrap()
}

fn decision(h: &H, id: &str) -> (String, Value) {
    let r = inspect::decisions::decision_status_result(h.env.store(), id).unwrap();
    (
        r[0].as_str().unwrap().to_string(),
        serde_json::from_str(r[1].as_str().unwrap_or("null")).unwrap(),
    )
}

fn status(h: &H, id: &str) -> String {
    inspect::messages::message_status(h.env.store(), id).unwrap()[0]
        .as_str()
        .unwrap()
        .to_string()
}

/// Your own message stored by another way in (another device's connection), so nothing reacts to it
/// when it lands: only the later gates can see it.
fn you_said_elsewhere(h: &H, space_ext: &str, secs: i64) {
    your_line_in(h, space_ext)(h.ch.now() + secs * 1000);
}

/// [`you_said_elsewhere`] as a closure that stores your line with the time it is given (to run while
/// the composer “thinks”).
fn your_line_in(h: &H, space_ext: &str) -> impl Fn(i64) + Send + 'static {
    let space: String = inspect::spaces::space_id_by_external_row(h.env.store(), space_ext)
        .unwrap()[0]
        .as_str()
        .unwrap()
        .into();
    let party: String = inspect::people::account_party_id(h.env.store(), &h.my_account).unwrap()[0]
        .as_str()
        .unwrap()
        .into();
    let (store, account) = (h.env.store().clone(), h.my_account.clone());
    move |now| {
        seed::messages::seed_owner_message(&store, WS, &space, &account, &party, now)
            .expect("your message");
    }
}

fn you_decide(h: &H, d: &str, choice: &str) -> ApiResult<steve_store::Decision> {
    steve_modules::decisions::dispatch(&h.env.modules, &caller(WS, ME), d, choice, "")
        .expect("ours")
}

/// A draft in your name for a private chat message, and its card, after its disclosure check (which
/// runs once the draft is written).
async fn drafted(h: &H, id: &str) -> (String, String) {
    let r = push(
        h,
        &h.me.clone(),
        msg(h, "D_LI", id, "ulilei", "今晚能看下吗", 0),
    )
    .await
    .response
    .expect("response");
    checked(h).await;
    (
        r.out_message_id.expect("draft"),
        r.decision_id.expect("card"),
    )
}

/// Runs the queue until no draft's disclosure check is still running.
async fn checked(h: &H) {
    steve_runtime::testkit::eventually("all draft disclosure checks finish", || async {
        let waiting: i64 = inspect::delivery::checking_basis_count(h.env.store()).unwrap();
        if waiting == 0 {
            true
        } else {
            h.outbox.send_due(&h.ch).await;
            false
        }
    })
    .await;
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
    let (say, at) = (your_line_in(&h, "D_LI"), h.ch.now() + 30_000);
    *h.model.during.lock().unwrap() = Some(Box::new(move || say(at)));
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
        .write(|tx| {
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
    // and a line of yours in the group that answers nobody leaves it open too
    push(&h, &me, msg(&h, "C_DEV", "y2", "upx", "我先去开会", 0)).await;
    assert_eq!(decision(&h, &ask).0, "open");
    let mut answer = msg(&h, "C_DEV", "y3", "upx", "我负责", 0);
    answer.reply_to = Some("h1".into());
    push(&h, &me, answer).await;
    let (st, res) = decision(&h, &ask);
    assert_eq!(
        (st.as_str(), res["superseded"].as_str()),
        ("expired", Some(BY_YOU))
    );
    assert!(h.fake.sent.lock().unwrap().is_empty());
}

// ---------------------------------------------------------------- gate 3: YOLO

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

// ---------------------------------------------------------------- gate 4: sending

// ---------------------------------------------------------------- times: late and early clocks
// ---------------------------------------------------------------- a busy group

/// A busy group: two people @ you a few seconds apart and others talk; you answer one of them by
/// quoting their message. Only that one is yours now; the other's draft stays for you (or YOLO) to
/// decide, where the old rule (anything you said within the window) closed both.
#[tokio::test(flavor = "multi_thread")]
async fn in_a_busy_group_answering_one_person_leaves_the_others_draft() {
    let h = setup().await;
    let me = h.me.clone();
    push(&h, &me, msg(&h, "C_DEV", "x0", "uhan", "早", -3600)).await;
    let li = push(
        &h,
        &me,
        at_you(msg(&h, "C_DEV", "l1", "ulilei", "接口文档在哪", 0)),
    )
    .await
    .response
    .unwrap();
    h.env.clock.advance(8_000);
    let han = push(
        &h,
        &me,
        at_you(msg(&h, "C_DEV", "h1", "uhan", "周五发布能赶上吗", 0)),
    )
    .await
    .response
    .unwrap();
    let (li_draft, han_draft) = (
        li.out_message_id.expect("li's draft"),
        han.out_message_id.expect("han's draft"),
    );
    h.env.clock.advance(20_000);
    let mut to_li = msg(&h, "C_DEV", "y1", "upx", "群公告里有", 0);
    to_li.reply_to = Some("l1".into());
    push(&h, &me, to_li).await;
    assert_eq!(status(&h, &li_draft), "discarded", "the one you answered");
    assert_eq!(status(&h, &han_draft), "draft", "not the other one");
    // YOLO, judging again once its disclosure check is done, still may approve it
    checked(&h).await;
    let card: String = inspect::messages::message_decision_id(h.env.store(), &han_draft).unwrap()
        [0]
    .as_str()
    .unwrap()
    .into();
    h.env.clock.advance(10_000);
    assert_eq!(
        you_decide(&h, &card, "yes").expect("approved").status,
        steve_types::DecisionStatus::Resolved
    );
    assert_eq!(status(&h, &han_draft), "pending");
}

/// Case-shaped (a group of six the owner dismissed: only gaps and who spoke): someone @-s several
/// people including you; you speak 27 s and 69 s later, quoting nobody and @-ing nobody, and the
/// next voices are other people's. That is not an answer to them: the draft stays for you to
/// decide. Quoting it is.
#[tokio::test(flavor = "multi_thread")]
async fn in_a_group_talking_after_a_cc_is_not_answering_it() {
    let h = setup().await;
    let me = h.me.clone();
    push(&h, &me, msg(&h, "C_DEV", "x0", "uhan", "早", -3600)).await;
    let r = push(
        &h,
        &me,
        at_you(msg(&h, "C_DEV", "l1", "ulilei", "这个需求大家看一下", 0)),
    )
    .await
    .response
    .unwrap();
    let draft = r.out_message_id.expect("draft");
    h.env.clock.advance(27_000);
    push(&h, &me, msg(&h, "C_DEV", "y1", "upx", "我在路上", 0)).await;
    h.env.clock.advance(42_000);
    push(&h, &me, msg(&h, "C_DEV", "y2", "upx", "十分钟到", 0)).await;
    h.env.clock.advance(172_000);
    push(&h, &me, msg(&h, "C_DEV", "h1", "uhan", "收到", 0)).await;
    assert_eq!(status(&h, &draft), "draft");
    let mut quote = msg(&h, "C_DEV", "y3", "upx", "我晚点看", 0);
    quote.reply_to = Some("l1".into());
    push(&h, &me, quote).await;
    assert_eq!(status(&h, &draft), "discarded");
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
    let runs = |id: &str| inspect::flows::count_flow_runs_by_id(h.env.store(), id).unwrap();
    let i = push(&h, &me, msg(&h, "D_LI", "l0", "ulilei", "早", -60)).await;
    assert_eq!(runs(&i.message_id), 1, "the space's flow takes a message");
    assert_eq!(set_reply(&h, &space, "record").await.0, 200);
    let i = push(&h, &me, msg(&h, "D_LI", "l1", "ulilei", "帮我看下", 0)).await;
    assert_eq!(runs(&i.message_id), 0, "under 只记录 it does not");
    let ev = i.event.as_ref().and_then(|e| e.id()).expect("event");
    let r = inspect::signals::signal_outcome_outcome_label(h.env.store(), ev).unwrap();
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
    let r = inspect::signals::signal_outcome(h.env.store(), ev).unwrap();
    assert_eq!(r[0], "recorded");
    assert_eq!(
        inspect::messages::count_chan_messages_by_id(h.env.store(), &i.message_id).unwrap(),
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
    assert_eq!(
        call(
            &h.api,
            Some(&caller(WS, ME)),
            "PATCH",
            &format!("/spaces/{space}"),
            Some(&json!({"muted": false})),
        )
        .await
        .0,
        200
    );
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
    seed::messages::seed_followup_copy(store, &idc, &like, &ext, secs).expect("stored");
    id
}

fn handoff_here(h: &H, space: &str) {
    let now = h.ch.now();
    h.env
        .store()
        .write(|tx| {
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
    checked(&h).await;
    assert_eq!(
        you_decide(&h, &card, "yes").expect("answered").status,
        steve_types::DecisionStatus::Resolved
    );
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
    checked(&h).await;
    assert_eq!(
        you_decide(&h, &card1, "yes").expect("approved").status,
        steve_types::DecisionStatus::Resolved
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
async fn a_draft_whose_message_a_later_turn_took_while_it_drafted_never_opens() {
    let h = setup().await;
    let me = h.me.clone();
    // the earlier message's composer is slow; meanwhile the later message comes and its turn,
    // answered first, takes the earlier one
    let store = h.env.store().clone();
    *h.model.during.lock().unwrap() = Some(Box::new(move || {
        let later = stored_in(&store, "l1", "l2", 20);
        store
            .write(|tx| presence::take_turn(tx, WS, &later))
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

// ---------------------------------------------------------------- incoming owner reactions (#277)

fn reaction(
    space: &str,
    msg_ext: &str,
    actor: &str,
    emoji: &str,
    origin: ReactionOrigin,
    at: i64,
    removed: bool,
) -> ReactionEvent {
    ReactionEvent {
        space_ext_id: Some(space.into()),
        message_ext_id: Some(msg_ext.into()),
        message_source_id: None,
        actor: FakeChannel::handle(actor),
        actor_name: None,
        actor_is_bot: false,
        origin,
        emoji: emoji.into(),
        at,
        time_source: ReactionTimeSource::Provider,
        revision: None,
        removed,
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn owner_reaction_manual_and_unknown_supersede_draft_without_silencing_later_messages() {
    for origin in [ReactionOrigin::Manual, ReactionOrigin::Unknown] {
        let h = setup().await;
        let me = h.me.clone();
        let first = push(&h, &me, msg(&h, "D_LI", "r1", "ulilei", "帮我看下报告", 0))
            .await
            .response
            .unwrap();
        let (draft1, card1) = (first.out_message_id.unwrap(), first.decision_id.unwrap());
        assert_eq!(status(&h, &draft1), "draft");

        h.ch.on_reaction(
            WS,
            &me,
            reaction(
                "D_LI",
                "r1",
                "upx",
                "THUMBSUP",
                origin,
                h.ch.now() + 5_000,
                false,
            ),
        )
        .unwrap();
        assert_eq!(status(&h, &draft1), "discarded");
        let (st, res) = decision(&h, &card1);
        assert_eq!(st, "expired");
        assert_eq!(res["superseded"], BY_REACTED);

        // A reaction is NOT a live-presence signal: a later message still gets drafted.
        let second = push(
            &h,
            &me,
            msg(&h, "D_LI", "r2", "ulilei", "另外明天的会几点？", 10),
        )
        .await
        .response
        .unwrap();
        assert!(
            second.out_message_id.is_some(),
            "origin {origin:?}: {second:?}"
        );
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn automated_and_our_own_outgoing_reactions_never_handle_a_message() {
    let h = setup().await;
    let me = h.me.clone();
    let first = push(&h, &me, msg(&h, "D_LI", "a1", "ulilei", "在吗", 0))
        .await
        .response
        .unwrap();
    let draft1 = first.out_message_id.unwrap();

    // Automated reaction on owner's account must not supersede.
    h.ch.on_reaction(
        WS,
        &me,
        reaction(
            "D_LI",
            "a1",
            "upx",
            "OK",
            ReactionOrigin::Automated,
            h.ch.now() + 2_000,
            false,
        ),
    )
    .unwrap();
    assert_eq!(status(&h, &draft1), "draft");

    // Steve's own outgoing user-account lifecycle reaction echoed back must not supersede.
    let inbound_id = h
        .env
        .store()
        .read(|r| {
            steve_store::channel_message_repo::incoming_reactions::resolve_target(
                r, WS, &me, "D_LI", "a1", "",
            )
        })
        .unwrap()
        .unwrap();
    h.env
        .store()
        .write(|w| {
            steve_store::channel_message_repo::reactions::acknowledge(
                w,
                WS,
                &inbound_id,
                h.ch.now(),
            )?;
            steve_store::channel_message_repo::reactions::save_result(
                w,
                &inbound_id,
                steve_store::channel_message_repo::reactions::Change {
                    id: Some("rx_1"),
                    emoji: Some("👀"),
                    as_user: true,
                    error: None,
                },
                None,
                h.ch.now(),
            )
        })
        .unwrap();
    h.ch.on_reaction(
        WS,
        &me,
        reaction(
            "D_LI",
            "a1",
            "upx",
            "👀",
            ReactionOrigin::Manual,
            h.ch.now() + 3_000,
            false,
        ),
    )
    .unwrap();
    assert_eq!(status(&h, &draft1), "draft");
}

#[tokio::test(flavor = "multi_thread")]
async fn reaction_scope_group_exact_dm_burst_tombstone_and_out_of_order() {
    let h = setup().await;
    let me = h.me.clone();

    // 1. Out-of-order reaction arrives before message: attaches and suppresses drafting.
    h.ch.on_reaction(
        WS,
        &me,
        reaction(
            "D_LI",
            "ooo1",
            "upx",
            "DONE",
            ReactionOrigin::Unknown,
            h.ch.now() + 1_000,
            false,
        ),
    )
    .unwrap();
    let ooo = push(&h, &me, msg(&h, "D_LI", "ooo1", "ulilei", "麻烦确认下", 0))
        .await
        .response
        .unwrap();
    assert_eq!(ooo.out_message_id, None);
    assert_eq!(ooo.label, LABEL_REACTED);

    // 2. Removal tombstone prevents an older create from resurrecting.
    h.ch.on_reaction(
        WS,
        &me,
        reaction(
            "D_LI",
            "tomb1",
            "upx",
            "DONE",
            ReactionOrigin::Manual,
            h.ch.now() + 20_000,
            true,
        ),
    )
    .unwrap();
    h.ch.on_reaction(
        WS,
        &me,
        reaction(
            "D_LI",
            "tomb1",
            "upx",
            "DONE",
            ReactionOrigin::Manual,
            h.ch.now() + 10_000,
            false,
        ),
    )
    .unwrap();
    let tomb = push(&h, &me, msg(&h, "D_LI", "tomb1", "ulilei", "还活着吗", 15))
        .await
        .response
        .unwrap();
    assert!(tomb.out_message_id.is_some());

    // 3. Group reaction handles ONLY the reacted message, not an earlier message in the group.
    let g1 = push(
        &h,
        &me,
        at_you(msg(&h, "C_DEV", "g1", "ulilei", "第一个问题", 30)),
    )
    .await
    .response
    .unwrap();
    let g1_draft = g1.out_message_id.unwrap();
    let g2 = push(
        &h,
        &me,
        at_you(msg(&h, "C_DEV", "g2", "uhan", "第二个问题", 35)),
    )
    .await
    .response
    .unwrap();
    let g2_draft = g2.out_message_id.unwrap();
    h.ch.on_reaction(
        WS,
        &me,
        reaction(
            "C_DEV",
            "g2",
            "upx",
            "OK",
            ReactionOrigin::Unknown,
            h.ch.now() + 40_000,
            false,
        ),
    )
    .unwrap();
    assert_eq!(status(&h, &g2_draft), "discarded");
    assert_eq!(status(&h, &g1_draft), "draft");
}
