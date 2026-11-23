//! When Steve replies for you, says nothing or asks you, on your own account, and the card it opens
//! (synthetic chats; names A/B and made-up text): the gates before any model (nothing to answer, a
//! nudge onto the open card, a bot's notice by its structure), the card's content and its choices
//! (send the draft, tell it how to reply, your words as written; YOLO clicks none of them), its life
//! (a newer card that shows it, the lifetime you set), starred contacts, and what a draft keeps for
//! YOLO.

#[path = "support/digest_env.rs"]
mod digest_env;
use digest_env::TestEnv;
use std::sync::Arc;

use serde_json::{Value, json};
use steve_channels::compose::{Composer, H_SAFETY, H_TASK};
use steve_channels::decide::{
    self, BY_EXPIRY, BY_NEWER, LABEL_BOT_ITEM, LABEL_BOT_SEEN, LABEL_NO_CONTENT, LABEL_NUDGE,
    NUDGED, Reason, STARRED,
};
use steve_channels::disclosure::{CHECK_HEAD, Disclosure};
use steve_channels::fake::channel::FakeChannel;
use steve_channels::outbound::{COMMITS, OutLink, Outbox};
use steve_channels::respond::{HANDOFF_NO_DRAFT_OPTIONS, HANDOFF_OPTIONS, SUBJECT_MESSAGE};
use steve_channels::{Channels, Inbound, InboundMessage, Mention, Replies, SpaceKind, spaces};
use steve_flow::testkit::FakeCore;
use steve_flow::{FlowSetup, LinearExecutor};
use steve_modules::ApiResult;
use steve_modules::accounts::connections::{self, Connections, ConnectionsConfig};
use steve_modules::outbound::YOLO_ACTOR;
use steve_modules::testkit::{ME, WS, caller};
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
    core: Arc<FakeCore>,
    me: String,
}

async fn setup() -> H {
    setup_with(PASS).await
}

/// [`setup`] with the disclosure check's answer.
async fn setup_with(check: &str) -> H {
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
            // no model of its own: the composer asks the gateway, which the tests script
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
    let replies = Arc::new(
        Replies::new()
            .outbound(outbox.clone())
            .composer(Composer::new()),
    );
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
    let people = [("ua", "阿甲"), ("ub", "阿乙")];
    fake.space("C_DEV", "开发群", SpaceKind::Group, false, &people);
    fake.space("D_A", "", SpaceKind::Dm, false, &[("ua", "阿甲")]);
    fake.space(
        "D_ALERT",
        "",
        SpaceKind::Dm,
        false,
        &[("ualert", "告警机器人")],
    );
    env.gateway.script(CHECK_HEAD, check);
    let now = env.modules.clock.now();
    let v = Viewer::bare(WS, ME);
    env.store()
        .write(|tx| {
            spaces::set_account_rules(
                tx,
                &v,
                &me,
                &json!([{"when": {"kind": "mention_or_dm"}, "act": "draft"}]),
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
        core,
        me,
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

/// The composer answers `answer` next.
fn answers(h: &H, answer: Value) {
    h.env.gateway.reply(answer.to_string());
}

fn compose_prompts(h: &H) -> Vec<String> {
    h.env
        .gateway
        .prompts
        .lock()
        .unwrap()
        .iter()
        .filter(|p| p.starts_with(H_SAFETY))
        .cloned()
        .collect()
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

fn exec(h: &H, sql: &str, p: &[&str]) {
    let (sql, p): (String, Vec<String>) = (sql.into(), p.iter().map(|s| s.to_string()).collect());
    digest_env::write(h.env.store(), move |c| {
        c.execute(&sql, rusqlite::params_from_iter(p.iter()))?;
        Ok(())
    })
    .expect("exec");
}

fn card(h: &H, d: &str) -> (String, String, String, String, Value) {
    let r = row(
        h,
        "SELECT status, question, draft, options, result FROM decisions WHERE id = ?",
        &[d],
    );
    let s = |i: usize| r[i].as_str().unwrap_or_default().to_string();
    (
        s(0),
        s(1),
        s(2),
        s(3),
        serde_json::from_str(r[4].as_str().unwrap_or("null")).unwrap(),
    )
}

fn open_cards(h: &H) -> i64 {
    row(
        h,
        "SELECT count(*) FROM decisions WHERE subject_type = 'chan_message' AND kind = 'ask' AND status = 'open'",
        &[],
    )[0]
    .as_i64()
    .unwrap()
}

fn status(h: &H, id: &str) -> String {
    row(h, "SELECT status FROM chan_messages WHERE id = ?", &[id])[0]
        .as_str()
        .unwrap()
        .to_string()
}

fn decide_as(
    h: &H,
    who: &str,
    d: &str,
    choice: &str,
    text: &str,
) -> ApiResult<steve_store::Decision> {
    steve_modules::decisions::dispatch(&h.env.modules, &caller(WS, who), d, choice, text)
        .expect("ours")
}

fn ask(reason: &str, summary: &str, draft: &str) -> Value {
    json!({"outcome": "ask_owner", "reason": reason, "addressee": "owner", "summary": summary,
           "text": "要你定", "draft": draft, "needs_commitment": false, "why": "要你定"})
}

/// A question to you about A's private message, with a candidate draft: `(message, card)`.
async fn asked(h: &H, id: &str, secs: i64) -> (String, String) {
    answers(
        h,
        ask(
            "commitment",
            "阿甲问周四下午能不能一起过一下方案",
            "周四可以",
        ),
    );
    let i = push(
        h,
        msg(h, "D_A", id, "ua", "周四下午能一起过一下方案吗？", secs),
    )
    .await;
    let d = i.response.and_then(|r| r.decision_id).expect("a card");
    (i.message_id, d)
}

fn party_of(h: &H, handle: &str) -> String {
    row(
        h,
        "SELECT party_id FROM accounts WHERE handle = ?",
        &[handle],
    )[0]
    .as_str()
    .expect("a contact")
    .to_string()
}

// ---------------------------------------------------------------- G4, G5: before any model

/// A message of Feishu type `ty` with body `content`, as the Feishu reader stores it (its text and
/// attachments), from A in the private chat.
fn feishu_msg(h: &H, id: &str, ty: &str, content: &str) -> InboundMessage {
    let mut e = steve_channels::fake::feishu::fixture("message_p2p_text").expect("fixture");
    e["event"]["message"]["message_type"] = json!(ty);
    e["event"]["message"]["content"] = json!(content);
    let read = steve_channels::adapters::feishu::inbound::parse_receive(&e, "ou_bot")
        .expect("read")
        .expect("a message");
    let mut m = msg(h, "D_A", id, "ua", &read.text, 0);
    m.attachments = read.attachments;
    m
}

#[tokio::test(flavor = "multi_thread")]
async fn nothing_to_answer_is_recorded_before_any_model() {
    let h = setup().await;
    let sticker = feishu_msg(&h, "s1", "sticker", r#"{"file_key":"k1"}"#);
    assert_eq!(
        sticker.attachments[0]["kind"], "sticker",
        "the reader's shape"
    );
    let system = feishu_msg(&h, "s3", "system", r#"{"template":"x"}"#);
    for m in [sticker, msg(&h, "D_A", "s2", "ua", "👍", 0), system] {
        h.env.clock.advance(2 * 60_000);
        let id = m.external_id.clone();
        let mut m = m;
        m.created = h.ch.now();
        let r = push(&h, m).await.response.expect("response");
        assert_eq!(r.label, LABEL_NO_CONTENT, "{id}");
        assert!(r.decision_id.is_none() && r.out_message_id.is_none());
    }
    assert!(compose_prompts(&h).is_empty(), "no composer ran");
    // a forwarded chat and a task have something to read: the composer reads them
    for (id, ty) in [("f1", "merge_forward"), ("t1", "todo")] {
        answers(
            &h,
            json!({"outcome": "skip", "text": "看过了", "why": "转发"}),
        );
        h.env.clock.advance(40 * 60_000);
        let n = compose_prompts(&h).len();
        let mut m = feishu_msg(&h, id, ty, r#"{"x":"y"}"#);
        m.created = h.ch.now();
        let r = push(&h, m).await.response.expect("response");
        assert_ne!(r.label, LABEL_NO_CONTENT, "{ty}");
        assert_eq!(compose_prompts(&h).len(), n + 1, "{ty} was read");
    }
    // a question of theirs still waiting: the sticker after it is read with it
    answers(
        &h,
        json!({"outcome": "skip", "text": "等会儿再说", "why": "闲聊"}),
    );
    h.env.clock.advance(40 * 60_000);
    let n = compose_prompts(&h).len();
    push(&h, msg(&h, "D_A", "q1", "ua", "晚上吃啥？", 0)).await;
    assert_eq!(compose_prompts(&h).len(), n + 1);
    h.env.clock.advance(2 * 60_000);
    answers(
        &h,
        json!({"outcome": "skip", "text": "表情", "why": "闲聊"}),
    );
    let r = push(&h, msg(&h, "D_A", "s4", "ua", "[OK]", 0))
        .await
        .response
        .unwrap();
    assert_ne!(r.label, LABEL_NO_CONTENT);
    assert_eq!(
        compose_prompts(&h).len(),
        n + 2,
        "read, since the question waits"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_nudge_goes_onto_the_open_card() {
    let h = setup().await;
    let (_, d) = asked(&h, "a1", 0).await;
    assert_eq!(compose_prompts(&h).len(), 1);
    h.env.clock.advance(5 * 60_000);
    let r = push(&h, msg(&h, "D_A", "a2", "ua", "？？", 0))
        .await
        .response
        .unwrap();
    assert_eq!(r.label, LABEL_NUDGE);
    assert_eq!(r.decision_id.as_deref(), Some(d.as_str()));
    assert_eq!(compose_prompts(&h).len(), 1, "no composer for a nudge");
    assert_eq!(open_cards(&h), 1, "no second card");
    let (st, q, ..) = card(&h, &d);
    assert_eq!(st, "open");
    assert!(q.ends_with(&format!("{NUDGED}？？")), "{q}");
}

// ---------------------------------------------------------------- the card

#[tokio::test(flavor = "multi_thread")]
async fn the_card_says_who_why_the_lines_and_the_draft() {
    let h = setup().await;
    // you spoke there earlier (outside the stretch Steve counts as you being there)
    push(&h, msg(&h, "C_DEV", "y1", "upx", "方案我晚点发", -20 * 60)).await;
    answers(
        &h,
        ask(
            "commitment",
            "阿甲问周四下午能不能一起过一下方案",
            "周四可以",
        ),
    );
    let i = push(
        &h,
        at_you(msg(&h, "C_DEV", "g1", "ua", "周四下午能过一下吗", 0)),
    )
    .await;
    let r = i.response.unwrap();
    assert!(r.out_message_id.is_none());
    let (_, q, draft, options, _) = card(&h, &r.decision_id.unwrap());
    let lines: Vec<&str> = q.lines().collect();
    assert_eq!(
        lines[0],
        "「开发群」里 阿甲 找你：阿甲问周四下午能不能一起过一下方案"
    );
    assert_eq!(
        lines[1],
        format!("为什么要你：{}（@了你）", Reason::Commitment.label())
    );
    assert_eq!(lines[2], "最近的对话：");
    assert!(lines.contains(&"- 我：方案我晚点发"), "{q}");
    assert_eq!(*lines.last().unwrap(), "→ 阿甲：周四下午能过一下吗");
    assert!(!q.contains("要你定"), "never the model's own question: {q}");
    assert_eq!(draft, "周四可以");
    assert_eq!(options, serde_json::to_string(&HANDOFF_OPTIONS).unwrap());

    // without a draft it offers no “发这条草稿”
    h.env.clock.advance(5 * 60_000);
    answers(&h, ask("owner_fact", "阿乙问告警是不是你那边改的", ""));
    let r = push(
        &h,
        at_you(msg(&h, "C_DEV", "g2", "ub", "告警是你那边改的吗", 0)),
    )
    .await
    .response
    .unwrap();
    let (_, q, draft, options, _) = card(&h, &r.decision_id.unwrap());
    assert!(q.contains(Reason::OwnerFact.label()), "{q}");
    assert_eq!(draft, "");
    assert_eq!(
        options,
        serde_json::to_string(&HANDOFF_NO_DRAFT_OPTIONS).unwrap()
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn your_click_sends_the_draft_or_your_words_and_yolo_clicks_none() {
    let h = setup().await;
    let (_, d) = asked(&h, "a1", 0).await;
    for choice in ["send_draft", "verbatim", "instruct"] {
        assert!(
            decide_as(&h, YOLO_ACTOR, &d, choice, "好").is_err(),
            "{choice}"
        );
    }
    // the card's draft is checked before it goes out; it passes: approved by your click
    decide_as(&h, ME, &d, "send_draft", "").expect("answered");
    let out = checked(&h, &d).await;
    assert_eq!(card(&h, &d).4["check"], "pass");
    assert_eq!(status(&h, &out), "pending");
    assert!(
        h.env
            .gateway
            .prompts
            .lock()
            .unwrap()
            .iter()
            .any(|p| p.starts_with(CHECK_HEAD) && p.contains("周四可以")),
        "the check read the draft"
    );
    h.env.clock.advance(11_000);
    assert_eq!(h.outbox.send_due(&h.ch).await, 1);
    assert_eq!(status(&h, &out), "sent");
    assert_eq!(
        row(&h, "SELECT text FROM chan_messages WHERE id = ?", &[&out])[0],
        "周四可以"
    );

    // 按原文发: your words, as you wrote them; words are needed
    h.env.clock.advance(60 * 60_000);
    let (_, d) = asked(&h, "a2", 0).await;
    assert!(decide_as(&h, ME, &d, "verbatim", "  ").is_err());
    let r = decide_as(&h, ME, &d, "verbatim", "周四三点吧").expect("sent");
    let out = r.result["draft_message"].as_str().unwrap().to_string();
    h.env.clock.advance(11_000);
    assert_eq!(h.outbox.send_due(&h.ch).await, 1);
    assert_eq!(
        row(
            &h,
            "SELECT text, status FROM chan_messages WHERE id = ?",
            &[&out]
        ),
        vec![json!("周四三点吧"), json!("sent")]
    );
    assert_eq!(h.fake.sent.lock().unwrap().len(), 2);
}

/// The queue checks a card's draft answered “发这条草稿”: the message it made (`result.composed`).
async fn checked(h: &H, d: &str) -> String {
    let composed = std::sync::Mutex::new(None);
    steve_runtime::testkit::eventually("the answered card's draft is checked", || async {
        h.outbox.send_due(&h.ch).await;
        let r = row(
            h,
            "SELECT coalesce(json_extract(result, '$.composed'), '') FROM decisions WHERE id = ?",
            &[d],
        );
        if let Some(m) = r[0].as_str().filter(|m| !m.is_empty()) {
            *composed.lock().unwrap() = Some(m.to_string());
            true
        } else {
            false
        }
    })
    .await;
    composed.into_inner().unwrap().unwrap()
}

/// A card's draft the check flags is not sent on your click: it becomes the usual approval with
/// the suspicious parts marked, and waits for you again.
#[tokio::test(flavor = "multi_thread")]
async fn a_cards_draft_that_may_leak_waits_for_you_again() {
    let h = setup_with(
        r#"{"verdict": "leak", "spans": [{"text": "周四可以", "reason": "内部排期", "basis": "internal_detail"}]}"#,
    )
    .await;
    let (_, d) = asked(&h, "a1", 0).await;
    decide_as(&h, ME, &d, "send_draft", "").expect("answered");
    let out = checked(&h, &d).await;
    assert_eq!(card(&h, &d).4["check"], "leak");
    assert_eq!(status(&h, &out), "draft");
    let approval = row(
        &h,
        "SELECT decision_id FROM chan_messages WHERE id = ?",
        &[&out],
    )[0]
    .as_str()
    .unwrap()
    .to_string();
    let (st, q, ..) = card(&h, &approval);
    assert_eq!(st, "open");
    assert!(q.contains("可能泄露") && q.contains("「周四可以」"), "{q}");
    h.env.clock.advance(60_000);
    assert_eq!(h.outbox.send_due(&h.ch).await, 0);
    assert!(h.fake.sent.lock().unwrap().is_empty());
    // checked once
    let n = h
        .env
        .gateway
        .prompts
        .lock()
        .unwrap()
        .iter()
        .filter(|p| p.starts_with(CHECK_HEAD))
        .count();
    h.outbox.send_due(&h.ch).await;
    // Negative window: another queue pass must not schedule a second model check or composition.
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    let again = h
        .env
        .gateway
        .prompts
        .lock()
        .unwrap()
        .iter()
        .filter(|p| p.starts_with(CHECK_HEAD))
        .count();
    assert_eq!(again, n);
}

#[tokio::test(flavor = "multi_thread")]
async fn telling_it_how_to_reply_drafts_with_your_words_once() {
    let h = setup().await;
    let (_, d) = asked(&h, "a1", 0).await;
    decide_as(&h, ME, &d, "instruct", "跟他说周四下午三点").expect("answered");
    answers(
        &h,
        json!({"outcome": "reply", "text": "周四下午三点吧", "why": "按你说的"}),
    );
    h.outbox.send_due(&h.ch).await;
    let mut draft = None;
    steve_runtime::testkit::eventually_sync("the instructed reply draft is composed", || {
        let r = row(
            &h,
            "SELECT coalesce(json_extract(result, '$.composed'), '') FROM decisions WHERE id = ?",
            &[&d],
        );
        if r[0].as_str().is_some_and(|s| !s.is_empty()) {
            draft = r[0].as_str().map(String::from);
            true
        } else {
            false
        }
    })
    .await;
    let draft = draft.expect("drafted");
    let p = compose_prompts(&h);
    let last = p.last().unwrap();
    assert!(
        last.contains(H_TASK) && last.contains("跟他说周四下午三点"),
        "{last}"
    );
    // your words are the instruction: the decision rules are not in the prompt
    assert!(!last.contains(steve_channels::compose::H_DECIDE), "{last}");
    // the reply is a draft for your approval, as any
    assert_eq!(
        row(
            &h,
            "SELECT text, status FROM chan_messages WHERE id = ?",
            &[&draft]
        ),
        vec![json!("周四下午三点吧"), json!("draft")]
    );
    assert!(h.fake.sent.lock().unwrap().is_empty());
    // once
    let n = p.len();
    h.outbox.send_due(&h.ch).await;
    // Negative window: another queue pass must not schedule a second model check or composition.
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    assert_eq!(compose_prompts(&h).len(), n);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_card_opened_before_keeps_its_reply_field() {
    let h = setup().await;
    let (_, d) = asked(&h, "a1", 0).await;
    exec(
        &h,
        "UPDATE decisions SET options = '[\"draft_reply\",\"to_item\",\"done\"]', draft = '' WHERE id = ?",
        &[&d],
    );
    let r = decide_as(&h, ME, &d, "draft_reply", "我晚点回你").expect("drafted");
    let out = r.result["draft_message"].as_str().unwrap().to_string();
    assert_eq!(status(&h, &out), "draft", "still waits for your approval");
}

// ---------------------------------------------------------------- the card's life

/// A's card at your @ in the dev group, with the composer's question `summary` and `reason`.
async fn card_at(h: &H, id: &str, from: &str, text: &str, reason: &str, summary: &str) -> String {
    answers(h, ask(reason, summary, ""));
    push(h, at_you(msg(h, "C_DEV", id, from, text, 0)))
        .await
        .response
        .unwrap()
        .decision_id
        .expect("a card")
}

#[tokio::test(flavor = "multi_thread")]
async fn a_newer_card_takes_over_only_what_it_shows_of_the_same_person() {
    let h = setup().await;
    // the same sender, the older message on the newer card: taken over
    let a1 = card_at(
        &h,
        "g1",
        "ua",
        "方案定了吗",
        "owner_fact",
        "阿甲问方案定了没",
    )
    .await;
    h.env.clock.advance(5 * 60_000);
    let a2 = card_at(
        &h,
        "g2",
        "ua",
        "今晚谁值班",
        "owner_fact",
        "阿甲问今晚谁值班",
    )
    .await;
    let (st, _, _, _, result) = card(&h, &a1);
    assert_eq!(
        (st.as_str(), result["superseded"].as_str()),
        ("expired", Some(BY_NEWER))
    );
    let (st, q, ..) = card(&h, &a2);
    assert_eq!(st, "open");
    assert!(
        q.contains("- 阿甲：方案定了吗"),
        "the newer card shows it: {q}"
    );

    // someone else's card does not close it, though it shows A's line
    h.env.clock.advance(5 * 60_000);
    let b1 = card_at(&h, "g3", "ub", "日志在哪", "owner_fact", "阿乙问日志在哪").await;
    assert_eq!(card(&h, &a2).0, "open");
    assert!(card(&h, &b1).1.contains("- 阿甲：今晚谁值班"));

    // a commitment stays until another commitment takes it over
    h.env.clock.advance(5 * 60_000);
    let c1 = card_at(
        &h,
        "g4",
        "ub",
        "周四能上线吗",
        "commitment",
        "阿乙问周四能不能上线",
    )
    .await;
    h.env.clock.advance(5 * 60_000);
    let b2 = card_at(
        &h,
        "g5",
        "ub",
        "日志找到了吗",
        "owner_fact",
        "阿乙问日志找到没",
    )
    .await;
    assert_eq!(
        card(&h, &c1).0,
        "open",
        "a commitment is not lost to a question"
    );
    assert_eq!(card(&h, &b1).0, "expired", "B's own question is");
    h.env.clock.advance(5 * 60_000);
    card_at(
        &h,
        "g6",
        "ub",
        "那周五呢",
        "commitment",
        "阿乙问周五能不能上线",
    )
    .await;
    assert_eq!(card(&h, &c1).0, "expired");

    assert_eq!(card(&h, &b2).0, "expired", "and B's question with it");

    // more lines than the card shows between them: the older one stays
    h.env.clock.advance(5 * 60_000);
    let x = card_at(
        &h,
        "g7",
        "ub",
        "还有别的吗",
        "owner_fact",
        "阿乙问还有没有别的",
    )
    .await;
    for i in 0..decide::EXCERPT_MAX {
        h.env.clock.advance(30_000);
        push(
            &h,
            msg(
                &h,
                "C_DEV",
                &format!("x{i}"),
                "ua",
                &format!("第{i}条闲聊"),
                0,
            ),
        )
        .await;
    }
    h.env.clock.advance(60_000);
    let y = card_at(&h, "g8", "ub", "在吗", "owner_fact", "阿乙问你在不在").await;
    assert!(!card(&h, &y).1.contains("还有别的吗"));
    assert_eq!(
        card(&h, &x).0,
        "open",
        "B's earlier card is not on the newer one"
    );
}

/// Your own account's card lifetime, in hours (`None`: never on time).
fn lifetime(h: &H, hours: Option<i64>) {
    let v = hours.map(|x| x.to_string()).unwrap_or("NULL".into());
    exec(
        h,
        &format!("UPDATE channel_settings SET card_ttl_h = {v} WHERE connection_id = ?"),
        &[&h.me],
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn cards_close_after_the_lifetime_you_set_but_never_a_commitment_or_a_star() {
    let h = setup().await;
    // a question for what you know, in A's private chat
    answers(&h, ask("owner_fact", "阿甲问告警是不是你改的", ""));
    let fact = push(&h, msg(&h, "D_A", "f1", "ua", "告警是你改的吗", 0))
        .await
        .response
        .unwrap()
        .decision_id
        .expect("a card");
    // a commitment, in the dev group
    answers(
        &h,
        ask("commitment", "阿乙问周四下午能不能过方案", "周四可以"),
    );
    let commitment = push(
        &h,
        at_you(msg(&h, "C_DEV", "c1", "ub", "周四下午能过一下方案吗", 0)),
    )
    .await
    .response
    .unwrap()
    .decision_id
    .expect("a card");
    // a draft in your name, and one that promises something
    answers(
        &h,
        json!({"outcome": "reply", "text": "好 我看下", "why": "回一句"}),
    );
    let r = push(&h, at_you(msg(&h, "C_DEV", "g1", "ua", "帮忙看下日志", 0)))
        .await
        .response
        .unwrap();
    let (draft, approval) = (r.out_message_id.unwrap(), r.decision_id.unwrap());
    // a starred contact's card, a while later (not one turn with B's question)
    h.env.clock.advance(40 * 60_000);
    exec(
        &h,
        "UPDATE parties SET important = 1 WHERE id = ?",
        &[&party_of(&h, "ub")],
    );
    answers(
        &h,
        json!({"outcome": "skip", "summary": "阿乙说晚上一起吃饭", "text": "闲聊", "why": "闲聊"}),
    );
    let starred = push(&h, at_you(msg(&h, "C_DEV", "s1", "ub", "晚上一起吃饭", 0)))
        .await
        .response
        .unwrap()
        .decision_id
        .expect("a card");
    assert!(card(&h, &starred).1.starts_with(STARRED));

    // no lifetime set: a day later, everything still waits for you
    h.env.clock.advance(24 * 3_600_000);
    h.outbox.send_due(&h.ch).await;
    for (n, d) in [&fact, &commitment, &approval, &starred]
        .into_iter()
        .enumerate()
    {
        assert_eq!(card(&h, d).0, "open", "{n}: {:?}", card(&h, d));
    }
    // a lifetime of 30 hours: not yet; past it, the question and the draft close
    lifetime(&h, Some(30));
    h.outbox.send_due(&h.ch).await;
    assert_eq!(card(&h, &fact).0, "open");
    h.env.clock.advance(7 * 3_600_000);
    h.outbox.send_due(&h.ch).await;
    for d in [&fact, &approval] {
        let (st, _, _, _, result) = card(&h, d);
        assert_eq!(
            (st.as_str(), result["superseded"].as_str()),
            ("expired", Some(BY_EXPIRY))
        );
    }
    assert_eq!(status(&h, &draft), "discarded");
    // a commitment and a starred contact's card wait for you, however long
    for d in [&commitment, &starred] {
        assert_eq!(card(&h, d).0, "open");
    }
}

/// Cards the sweep never closes (a bot's account) do not hold up yours, however many there are.
#[tokio::test(flavor = "multi_thread")]
async fn cards_on_a_bots_account_never_hold_up_the_sweep() {
    let h = setup().await;
    let (bot, _) =
        FakeChannel::seed_connection(h.env.store(), &caller(WS, ME), None).expect("a bot");
    let m =
        h.ch.on_message(WS, &bot, msg(&h, "C_DEV", "b0", "ua", "早", -48 * 3600))
            .await
            .expect("message")
            .message_id;
    // even with a lifetime written on it
    exec(
        &h,
        "UPDATE channel_settings SET card_ttl_h = 1 WHERE connection_id = ?",
        &[&bot],
    );
    let old = h.ch.now() - 48 * 3_600_000;
    for i in 0..(steve_channels::outbound::EXPIRY_PAGE + 20) {
        let id = format!("bm{i}");
        exec(
            &h,
            "INSERT INTO chan_messages (id, workspace_id, space_id, thread_key, direction, sender_account_id,
                 sender_party_id, text, status, created, external_id)
             SELECT ?1, workspace_id, space_id, ?1, direction, sender_account_id, sender_party_id, text, status, created,
                 ?1 FROM chan_messages WHERE id = ?2",
            &[&id, &m],
        );
        exec(
            &h,
            &format!(
                "INSERT INTO decisions (id, workspace_id, kind, subject_type, subject_id, question, status, requested_by, created)
                 VALUES ('dbm{i}', 'default', 'ask', 'chan_message', ?, '机器人转给你', 'open', 'steve', {old})"
            ),
            &[&id],
        );
    }
    // yours, newer than all of them
    lifetime(&h, Some(1));
    answers(&h, ask("owner_fact", "阿甲问告警是不是你改的", ""));
    let fact = push(&h, msg(&h, "D_A", "f1", "ua", "告警是你改的吗", 0))
        .await
        .response
        .unwrap()
        .decision_id
        .expect("a card");
    h.env.clock.advance(2 * 3_600_000);
    h.outbox.send_due(&h.ch).await;
    assert_eq!(card(&h, &fact).0, "expired");
    assert_eq!(
        row(
            &h,
            "SELECT count(*) FROM decisions WHERE id LIKE 'dbm%' AND status = 'open'",
            &[]
        )[0],
        json!(steve_channels::outbound::EXPIRY_PAGE + 20)
    );
}

// ---------------------------------------------------------------- starred contacts

#[tokio::test(flavor = "multi_thread")]
async fn a_starred_contact_always_reaches_you_unless_you_are_there() {
    let h = setup().await;
    // A becomes known, then starred (the contact page's star)
    push(&h, msg(&h, "C_DEV", "k0", "ua", "早", -3600)).await;
    exec(
        &h,
        "UPDATE parties SET important = 1 WHERE id = ?",
        &[&party_of(&h, "ua")],
    );

    // the composer would say nothing: a card anyway, marked
    answers(
        &h,
        json!({"outcome": "skip", "summary": "阿甲说晚上一起吃饭", "text": "闲聊", "why": "闲聊"}),
    );
    let r = push(&h, msg(&h, "D_A", "s1", "ua", "晚上一起吃饭", 0))
        .await
        .response
        .unwrap();
    let (_, q, draft, ..) = card(&h, &r.decision_id.expect("a card"));
    assert!(q.starts_with(STARRED), "{q}");
    assert!(q.contains(Reason::Starred.label()), "{q}");
    assert_eq!(draft, "");

    // its reply is the card's candidate, not a draft sent your way
    h.env.clock.advance(10 * 60_000);
    answers(
        &h,
        json!({"outcome": "reply", "text": "好呀 几点", "why": "答应"}),
    );
    let r = push(&h, msg(&h, "D_A", "s2", "ua", "七点可以吗", 0))
        .await
        .response
        .unwrap();
    assert!(r.out_message_id.is_none());
    let (_, q, draft, ..) = card(&h, &r.decision_id.expect("a card"));
    assert!(
        q.starts_with(STARRED) && draft == "好呀 几点",
        "{q} / {draft}"
    );

    // even where nothing is drafted for you; no composer runs there
    let space = row(&h, "SELECT id FROM spaces WHERE external_id = 'D_A'", &[])[0]
        .as_str()
        .unwrap()
        .to_string();
    exec(
        &h,
        "INSERT INTO space_prefs (space_id, workspace_id, reply_as_you, updated) VALUES (?1, 'default', 'record', 1)
           ON CONFLICT (space_id) DO UPDATE SET reply_as_you = 'record'",
        &[&space],
    );
    h.env.clock.advance(10 * 60_000);
    let n = compose_prompts(&h).len();
    let r = push(&h, msg(&h, "D_A", "s3", "ua", "到了吗", 0))
        .await
        .response
        .unwrap();
    assert!(
        card(&h, &r.decision_id.expect("a card"))
            .1
            .starts_with(STARRED)
    );
    assert_eq!(compose_prompts(&h).len(), n);

    // you are there yourself: nothing
    exec(
        &h,
        "UPDATE space_prefs SET reply_as_you = 'draft' WHERE space_id = ?",
        &[&space],
    );
    h.env.clock.advance(10 * 60_000);
    push(&h, msg(&h, "D_A", "y1", "upx", "我到了", 0)).await;
    let r = push(&h, msg(&h, "D_A", "s4", "ua", "看到你了", 30))
        .await
        .response
        .unwrap();
    assert!(r.decision_id.is_none(), "{r:?}");

    // a newer card about someone else does not close a starred one
    h.env.clock.advance(10 * 60_000);
    answers(
        &h,
        json!({"outcome": "skip", "summary": "阿甲问明天几点", "text": "x", "why": "x"}),
    );
    let starred = push(&h, at_you(msg(&h, "C_DEV", "g1", "ua", "明天几点", 0)))
        .await
        .response
        .unwrap()
        .decision_id
        .unwrap();
    h.env.clock.advance(5 * 60_000);
    answers(&h, ask("owner_fact", "阿乙问值班表", ""));
    push(&h, at_you(msg(&h, "C_DEV", "g2", "ub", "值班表在哪", 0))).await;
    assert_eq!(card(&h, &starred).0, "open");
}

// ---------------------------------------------------------------- G0: a bot's notice

fn bot_msg(h: &H, id: &str, text: &str, link: Option<&str>) -> InboundMessage {
    let mut m = msg(h, "D_ALERT", id, "ualert", text, 0);
    m.sender.is_bot = true;
    let mut blocks = vec![json!({"t": "md", "text": text})];
    if let Some(l) = link {
        blocks.push(
            json!({"t": "buttons", "items": [{"text": "去处理", "callback": false, "url": l}]}),
        );
    }
    m.attachments = vec![json!({"type": "card", "format": "schema", "blocks": blocks})];
    m
}

#[tokio::test(flavor = "multi_thread")]
async fn a_bots_notice_becomes_an_item_by_its_structure_not_its_words() {
    let h = setup().await;
    // words alone (whatever they say) are recorded
    let r = push(&h, bot_msg(&h, "b1", "整改 截止 审批 失败", None))
        .await
        .response
        .unwrap();
    assert!(r.item_id.is_none(), "{r:?}");
    assert!(h.core.created().is_empty());
    // a card with an action link in the private chat: a proposed item, nothing said as you
    h.env.clock.advance(60_000);
    let r = push(
        &h,
        bot_msg(&h, "b2", "有一项待处理", Some("https://example.com/task/7")),
    )
    .await
    .response
    .unwrap();
    assert_eq!(r.label, LABEL_BOT_ITEM);
    let made = h.core.created();
    assert_eq!(made.len(), 1);
    assert!(made[0].propose, "proposed, not started");
    assert!(h.fake.sent.lock().unwrap().is_empty());
    assert!(compose_prompts(&h).is_empty());
    // the same link again: once
    h.env.clock.advance(60_000);
    let r = push(
        &h,
        bot_msg(
            &h,
            "b3",
            "提醒：有一项待处理",
            Some("https://example.com/task/7"),
        ),
    )
    .await
    .response
    .unwrap();
    assert_eq!(r.label, LABEL_BOT_SEEN);
    assert_eq!(h.core.created().len(), 1);

    // your setting: record only — or every message of this bot
    let space = row(
        &h,
        "SELECT id FROM spaces WHERE external_id = 'D_ALERT'",
        &[],
    )[0]
    .as_str()
    .unwrap()
    .to_string();
    exec(
        &h,
        "INSERT INTO space_prefs (space_id, workspace_id, bot_notices, updated) VALUES (?1, 'default', 'record', 1)
           ON CONFLICT (space_id) DO UPDATE SET bot_notices = 'record'",
        &[&space],
    );
    h.env.clock.advance(60_000);
    push(
        &h,
        bot_msg(&h, "b4", "新的一项", Some("https://example.com/task/8")),
    )
    .await;
    assert_eq!(h.core.created().len(), 1);
    exec(
        &h,
        "UPDATE space_prefs SET bot_notices = 'item' WHERE space_id = ?",
        &[&space],
    );
    h.env.clock.advance(60_000);
    let r = push(&h, bot_msg(&h, "b5", "日报已生成", None))
        .await
        .response
        .unwrap();
    assert_eq!(r.label, LABEL_BOT_ITEM);
    assert_eq!(h.core.created().len(), 2);
}

// ---------------------------------------------------------------- what a draft keeps for YOLO

#[tokio::test(flavor = "multi_thread")]
async fn a_draft_keeps_what_the_composer_said_about_it() {
    let h = setup().await;
    answers(
        &h,
        json!({"outcome": "reply", "text": "我今晚查完给你", "needs_commitment": true, "why": "答应查"}),
    );
    let r = push(&h, msg(&h, "D_A", "c1", "ua", "能帮忙查下吗", 0))
        .await
        .response
        .unwrap();
    let out = r.out_message_id.expect("a draft");
    let composed: Value = serde_json::from_str(
        row(
            &h,
            "SELECT composed FROM chan_messages WHERE id = ?",
            &[&out],
        )[0]
        .as_str()
        .unwrap(),
    )
    .unwrap();
    assert_eq!(composed["outcome"], "reply");
    assert_eq!(composed["needs_commitment"], true);
    let (_, q, ..) = card(&h, &r.decision_id.unwrap());
    assert!(q.ends_with(COMMITS), "{q}");
}
