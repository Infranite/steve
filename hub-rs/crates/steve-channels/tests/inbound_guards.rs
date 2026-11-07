//! The core's guards on inbound messages, whatever adapter delivers them: the watermark (old and
//! late messages are history: stored and recorded, nothing else), your own messages are never
//! answered as you (however the adapter named you), a new contact that looks like you gets a merge
//! proposal into “我”, and `steve_im_read` reads a conversation as context only.

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use serde_json::{Value, json};
use steve_channels::channel::{HandleRef, InboundMessage, Sender};
use steve_channels::fake::channel::FakeChannel;
use steve_channels::history::HistoryRead;
use steve_channels::inbound::{LABEL_HISTORY_BEFORE, LABEL_HISTORY_LATE, LABEL_OWN};
use steve_channels::{Channels, Inbound, Mention, Next, SpaceKind, im_mcp};
use steve_flow::testkit::FakeCore;
use steve_flow::{FlowSetup, LinearExecutor};
use steve_modules::accounts::connections::{self, ConnCx, Connections, ConnectionsConfig};
use steve_modules::contacts::mcp::McpCx;
use steve_modules::testkit::{ME, OTHER, TestEnv, WS, caller};
use steve_modules::vault::Vault;
use steve_modules::vault::keystore::FileKeyStore;
use tempfile::TempDir;

const HOUR: i64 = 3_600_000;

/// A history view that answers what the test scripted and remembers what it was asked.
#[derive(Default)]
struct Scripted {
    msgs: Mutex<Vec<InboundMessage>>,
    asked: Mutex<Vec<(String, i64, i64, usize)>>,
}

#[async_trait]
impl HistoryRead for Scripted {
    fn adapter_id(&self) -> &str {
        FakeChannel::ID
    }

    async fn read(
        &self,
        _: &ConnCx,
        space: &str,
        from: i64,
        to: i64,
        limit: usize,
    ) -> Result<Vec<InboundMessage>, steve_flow::AdapterError> {
        self.asked
            .lock()
            .unwrap()
            .push((space.into(), from, to, limit));
        Ok(self.msgs.lock().unwrap().clone())
    }
}

struct H {
    env: TestEnv,
    _keys: TempDir,
    ch: Channels,
    history: Arc<Scripted>,
    /// The bot's connection.
    bot: String,
    /// Your own account's connection (handle `upx`).
    mine: String,
    /// “我” of ME.
    me: String,
}

fn setup(with_history: bool) -> H {
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
    let history = Arc::new(Scripted::default());
    let mut b = Channels::builder(env.modules.clone(), conns, Arc::new(flow)).adapter(fake.clone());
    if with_history {
        b = b.history(history.clone());
    }
    let ch = b.build();
    let (bot, _) = FakeChannel::seed_connection(env.store(), &caller(WS, ME), None).expect("bot");
    let (mine, _) =
        FakeChannel::seed_connection(env.store(), &caller(WS, ME), Some("upx")).expect("mine");
    fake.space(
        "G1",
        "运维群",
        SpaceKind::Group,
        false,
        &[("ualice", "王小丽"), ("ubob", "张伟")],
    );
    fake.space("D1", "", SpaceKind::Dm, false, &[("ualice", "王小丽")]);
    fake.space("P1", "", SpaceKind::Dm, false, &[("ucarol", "李娜")]);
    fake.space("P2", "", SpaceKind::Dm, false, &[]);
    let me: String = env
        .store()
        .reader(|c| {
            Ok::<_, steve_store::StoreError>(c.query_row(
                "SELECT id FROM parties WHERE workspace_id = ? AND user_id = ?",
                [WS, ME],
                |r| r.get(0),
            )?)
        })
        .expect("我");
    H {
        env,
        _keys: keys,
        ch,
        history,
        bot,
        mine,
        me,
    }
}

fn one(h: &H, sql: &str, p: &[&str]) -> Value {
    let (sql, p): (String, Vec<String>) = (sql.into(), p.iter().map(|s| s.to_string()).collect());
    h.env
        .store()
        .reader(move |c| {
            Ok::<_, steve_store::StoreError>(c.query_row(
                &sql,
                rusqlite::params_from_iter(p.iter()),
                |r| {
                    Ok(match r.get_ref(0)? {
                        rusqlite::types::ValueRef::Null => Value::Null,
                        rusqlite::types::ValueRef::Integer(x) => json!(x),
                        rusqlite::types::ValueRef::Real(x) => json!(x),
                        rusqlite::types::ValueRef::Text(t) => json!(String::from_utf8_lossy(t)),
                        rusqlite::types::ValueRef::Blob(_) => Value::Null,
                    })
                },
            )?)
        })
        .expect("row")
}

fn count(h: &H, sql: &str, p: &[&str]) -> i64 {
    one(h, sql, p).as_i64().expect("count")
}

fn exec(h: &H, sql: &str, p: &[&str]) {
    let (sql, p): (String, Vec<String>) = (sql.into(), p.iter().map(|s| s.to_string()).collect());
    h.env
        .store()
        .transaction(move |tx| {
            Ok::<_, steve_store::StoreError>(
                tx.execute(&sql, rusqlite::params_from_iter(p.iter()))?,
            )
        })
        .expect("exec");
}

fn at(mut m: InboundMessage, created: i64) -> InboundMessage {
    m.created = created;
    m
}

fn mention(mut m: InboundMessage) -> InboundMessage {
    m.mentions.push(Mention {
        handle: None,
        name: "值班小运".into(),
        is_connection: true,
    });
    m
}

async fn push(h: &H, conn: &str, m: InboundMessage) -> Inbound {
    h.ch.on_message(WS, conn, m).await.expect("message")
}

fn label(i: &Inbound) -> String {
    i.response.as_ref().expect("response").label.clone()
}

fn decisions(h: &H) -> i64 {
    count(
        h,
        "SELECT count(*) FROM decisions WHERE workspace_id = ?",
        &[WS],
    )
}

#[tokio::test(flavor = "multi_thread")]
async fn a_batch_of_day_old_messages_arriving_now_runs_nothing() {
    let h = setup(false);
    let now = h.ch.now();
    let before = decisions(&h);
    // ten messages of yesterday morning, delivered now: a private chat and @-mentions in a new group
    for i in 0..10 {
        let m = if i % 2 == 0 {
            FakeChannel::message("D1", &format!("old-dm-{i}"), "ualice", "在吗？帮我看下这个")
        } else {
            mention(FakeChannel::message(
                "G1",
                &format!("old-g-{i}"),
                "ubob",
                "@值班小运 昨晚的告警怎么回事",
            ))
        };
        let r = push(&h, &h.bot, at(m, now - 24 * HOUR + i * 60_000)).await;
        assert!(!r.duplicate);
        assert_eq!(r.next, Next::Record(LABEL_HISTORY_LATE));
        assert_eq!(label(&r), LABEL_HISTORY_LATE);
        assert!(r.response.as_ref().unwrap().decision_id.is_none());
        assert!(r.response.as_ref().unwrap().out_message_id.is_none());
        assert!(r.decision_id.is_none(), "no “新群” card for history");
    }
    // stored as history, nothing drafted or sent, no card, the events only recorded
    assert_eq!(
        count(
            &h,
            "SELECT count(*) FROM chan_messages WHERE direction = 'in' AND history = 'late'",
            &[]
        ),
        10
    );
    assert_eq!(
        count(
            &h,
            "SELECT count(*) FROM chan_messages WHERE direction = 'out'",
            &[]
        ),
        0
    );
    assert_eq!(decisions(&h), before);
    assert_eq!(
        count(
            &h,
            "SELECT count(*) FROM ext_events WHERE source = 'im' AND outcome = 'recorded' AND outcome_label = ?",
            &[LABEL_HISTORY_LATE]
        ),
        10
    );
    assert_eq!(
        count(
            &h,
            "SELECT count(*) FROM ext_events WHERE source = 'im' AND outcome <> 'recorded'",
            &[]
        ),
        0
    );
    // counted on the connection
    assert_eq!(
        count(
            &h,
            "SELECT history_late FROM channel_settings WHERE connection_id = ?",
            &[&h.bot]
        ),
        10
    );
    // the space keeps its own time, and its messages say they are history
    let space = one(&h, "SELECT id FROM spaces WHERE external_id = 'G1'", &[]);
    let page = h
        .env
        .store()
        .reader(|c| steve_channels::messages::page(c, space.as_str().unwrap(), None, 50))
        .expect("page");
    assert!(page.iter().all(|m| m.history.as_deref() == Some("late")));
    assert!(
        count(
            &h,
            "SELECT last_message FROM spaces WHERE external_id = 'G1'",
            &[]
        ) < now - 20 * HOUR
    );

    // someone writes now: acted on, and the group asks “新群” now
    let live = push(
        &h,
        &h.bot,
        mention(FakeChannel::message(
            "G1",
            "live-1",
            "ubob",
            "@值班小运 现在呢？",
        )),
    )
    .await;
    assert_eq!(live.next, Next::Handle);
    assert!(live.history.is_none());
    assert!(
        live.decision_id.is_some(),
        "“新群” on the first live message"
    );
    assert_ne!(label(&live), LABEL_HISTORY_LATE);
}

#[tokio::test(flavor = "multi_thread")]
async fn written_before_the_connection_was_on_is_history_with_a_grace_for_clocks() {
    let h = setup(false);
    let now = h.ch.now();
    // connected (again) ten minutes ago
    let on = (now - 10 * 60_000).to_string();
    exec(
        &h,
        "UPDATE connections SET enabled_at = ? WHERE id = ?",
        &[&on, &h.bot],
    );
    let r = push(
        &h,
        &h.bot,
        at(
            FakeChannel::message("D1", "b1", "ualice", "上午说的那个"),
            now - 20 * 60_000,
        ),
    )
    .await;
    assert_eq!(r.next, Next::Record(LABEL_HISTORY_BEFORE));
    assert_eq!(label(&r), LABEL_HISTORY_BEFORE);
    assert_eq!(
        one(
            &h,
            "SELECT history FROM chan_messages WHERE id = ?",
            &[&r.message_id]
        ),
        json!("before")
    );
    assert_eq!(
        count(
            &h,
            "SELECT history_before FROM channel_settings WHERE connection_id = ?",
            &[&h.bot]
        ),
        1
    );
    // a minute before the connection was on: within the clocks' grace, live
    let r = push(
        &h,
        &h.bot,
        at(
            FakeChannel::message("D1", "b2", "ualice", "刚才发的"),
            now - 11 * 60_000,
        ),
    )
    .await;
    assert_eq!(r.next, Next::Handle);
    // a provider clock ahead of the Hub's: live
    let r = push(
        &h,
        &h.bot,
        at(
            FakeChannel::message("D1", "b3", "ualice", "时间在未来"),
            now + 5 * 60_000,
        ),
    )
    .await;
    assert_eq!(r.next, Next::Handle);
}

#[tokio::test(flavor = "multi_thread")]
async fn lateness_counts_from_the_last_edit_and_is_set_per_connection() {
    let h = setup(false);
    let now = h.ch.now();
    // written two hours ago, edited a minute ago: the edit is what is new
    let r = push(
        &h,
        &h.bot,
        at(
            FakeChannel::message("D1", "e1", "ualice", "改了一下：周五上线"),
            now - 2 * HOUR,
        )
        .edited(now - 60_000),
    )
    .await;
    assert_eq!(r.next, Next::Handle);
    // twenty minutes late is within the default 30
    let r = push(
        &h,
        &h.bot,
        at(
            FakeChannel::message("D1", "e2", "ualice", "晚到的"),
            now - 20 * 60_000,
        ),
    )
    .await;
    assert_eq!(r.next, Next::Handle);
    // five minutes on this connection
    exec(
        &h,
        "UPDATE channel_settings SET late_ms = 300000 WHERE connection_id = ?",
        &[&h.bot],
    );
    let r = push(
        &h,
        &h.bot,
        at(
            FakeChannel::message("D1", "e3", "ualice", "又晚到的"),
            now - 20 * 60_000,
        ),
    )
    .await;
    assert_eq!(r.next, Next::Record(LABEL_HISTORY_LATE));
    // an old `/bot` on your own account is no command
    let r = push(
        &h,
        &h.mine,
        at(
            FakeChannel::message("P1", "e4", "upx", "/bot 帮我回一下"),
            now - 3 * HOUR,
        ),
    )
    .await;
    assert_eq!(r.next, Next::Record(LABEL_HISTORY_LATE));
    assert_eq!(label(&r), LABEL_HISTORY_LATE);
}

#[test]
fn the_watermark_rule() {
    use steve_channels::inbound::{History, history_of};
    let (on, late) = (1_000_000_000, 30 * 60_000);
    let now = on + 10 * HOUR;
    // before the connection was on, by more than the grace (2 min + precision)
    assert_eq!(
        history_of(on, late, on - 3 * 60_000, None, now, 1000),
        Some(History::Before)
    );
    assert_eq!(
        history_of(on, late, on - 60_000, None, on + 1000, 1000),
        None
    );
    // an edit after the connection was on does not make an old message new
    assert_eq!(
        history_of(on, late, on - HOUR, Some(now), now, 1000),
        Some(History::Before)
    );
    // late: more than the lateness plus the grace after writing (or the last edit)
    assert_eq!(
        history_of(on, late, now - 40 * 60_000, None, now, 1000),
        Some(History::Late)
    );
    assert_eq!(
        history_of(on, late, now - 31 * 60_000, None, now, 1000),
        None
    );
    assert_eq!(
        history_of(on, late, now - 2 * HOUR, Some(now - 60_000), now, 1000),
        None
    );
    // a time ahead of the Hub's clock is now
    assert_eq!(history_of(on, late, now + HOUR, None, now, 1000), None);
}

#[tokio::test(flavor = "multi_thread")]
async fn your_own_messages_are_never_answered_as_you_however_the_adapter_names_you() {
    let h = setup(false);
    // the baseline: someone else's private message on your account is drafted for you
    let r = push(
        &h,
        &h.mine,
        FakeChannel::message("P1", "o1", "ucarol", "明天的会还开吗？"),
    )
    .await;
    assert_eq!(r.next, Next::Handle);
    assert!(r.response.as_ref().unwrap().decision_id.is_some());
    let cards = decisions(&h);

    // your work email is an alias of “我”: a message under it is yours, though the adapter made a
    // new contact of it
    exec(
        &h,
        "UPDATE parties SET aliases = '[\"Lee@Corp.example\"]' WHERE id = ?",
        &[&h.me],
    );
    let you = InboundMessage::without_id(
        "P2",
        Sender::new(HandleRef::new("mail", "lee@corp.example")).display_name("Lee"),
        "我明天十点到",
        h.ch.now(),
    );
    let mut you = you;
    you.external_id = "o2".into();
    let r = push(&h, &h.mine, you).await;
    assert!(r.from_owner);
    assert_eq!(r.next, Next::Own);
    assert_eq!(label(&r), LABEL_OWN);
    assert!(r.response.as_ref().unwrap().decision_id.is_none());
    assert_ne!(r.sender_party_id.as_deref(), Some(h.me.as_str()));
    // no draft, no approval card; only the merge proposal into “我” (a suggestion, never a merge)
    assert_eq!(
        count(
            &h,
            "SELECT count(*) FROM decisions WHERE workspace_id = ? AND subject_type <> 'party_merge'",
            &[WS]
        ),
        cards
    );
    let p = r.sender_party_id.clone().expect("party");
    assert_eq!(
        count(
            &h,
            "SELECT count(*) FROM merge_proposals WHERE from_id = ? AND into_id = ? AND decision_id IS NOT NULL",
            &[&p, &h.me]
        ),
        1
    );
    assert_eq!(
        one(&h, "SELECT status FROM parties WHERE id = ?", &[&p]),
        json!("active")
    );

    // a contact merged into “我” is you too
    let r = push(
        &h,
        &h.mine,
        FakeChannel::message("P1", "o3", "ulee", "这是我另一个号"),
    )
    .await;
    assert_eq!(r.next, Next::Handle);
    let merged = r.sender_party_id.clone().expect("party");
    exec(
        &h,
        "UPDATE parties SET status = 'merged', merged_into = ? WHERE id = ?",
        &[&h.me, &merged],
    );
    let cards = decisions(&h);
    let r = push(
        &h,
        &h.mine,
        FakeChannel::message("P1", "o4", "ulee", "还是我"),
    )
    .await;
    assert!(r.from_owner);
    assert_eq!(label(&r), LABEL_OWN);
    assert_eq!(decisions(&h), cards);

    // to the bot, you are someone it answers: your messages there are not yours to keep quiet
    let r = push(
        &h,
        &h.bot,
        FakeChannel::message("D1", "o5", "upx", "帮我查下昨晚的告警"),
    )
    .await;
    assert!(r.from_owner);
    assert_eq!(r.next, Next::Handle);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_new_contact_with_your_name_and_your_domain_is_proposed_not_merged() {
    let h = setup(false);
    let name = one(&h, "SELECT name FROM parties WHERE id = ?", &[&h.me]);
    let name = name.as_str().unwrap().to_string();
    // “我” has a work email at corp.example
    h.env
        .store()
        .transaction(|tx| {
            steve_modules::accounts::repo::insert(
                tx,
                WS,
                &steve_modules::accounts::repo::NewAccount {
                    system_id: "mail".into(),
                    family: "email".into(),
                    party_id: h.me.clone(),
                    kind: "work_email".into(),
                    handle: "lee@corp.example".into(),
                    handle_display: "lee@corp.example".into(),
                    display_name: name.clone(),
                    verified: true,
                    confidence: 1.0,
                    source: "me".into(),
                    source_label: String::new(),
                },
            )
        })
        .expect("account");
    let m = InboundMessage::text("P1", "n1", FakeChannel::handle("ulee2"), "hi", h.ch.now());
    let mut m = m;
    m.sender = Sender::new(FakeChannel::handle("ulee2"))
        .display_name(&name)
        .alias(HandleRef::new("mail", "lee.p@corp.example"));
    let r = push(&h, &h.mine, m).await;
    // a namesake in your domain may be someone else: not treated as you, only proposed
    assert!(!r.from_owner);
    let p = r.sender_party_id.clone().expect("party");
    assert_ne!(p, h.me);
    let reason = one(
        &h,
        "SELECT reason FROM merge_proposals WHERE from_id = ? AND into_id = ?",
        &[&p, &h.me],
    );
    assert!(
        reason.as_str().unwrap().contains("corp.example"),
        "{reason}"
    );
    assert_eq!(
        count(
            &h,
            "SELECT count(*) FROM decisions WHERE subject_type = 'party_merge' AND status = 'open'",
            &[]
        ),
        1
    );
    // someone with another name in another domain: nothing proposed
    let mut m = InboundMessage::text("P1", "n2", FakeChannel::handle("uzed"), "hi", h.ch.now());
    m.sender = Sender::new(FakeChannel::handle("uzed"))
        .display_name("赵六")
        .alias(HandleRef::new("mail", "zed@other.example"));
    push(&h, &h.mine, m).await;
    assert_eq!(count(&h, "SELECT count(*) FROM merge_proposals", &[]), 1);
}

fn cx(user: &str) -> McpCx {
    McpCx {
        ws: WS.into(),
        user: user.into(),
        bot_id: "agent".into(),
        item_id: None,
        session_id: None,
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn steve_im_read_reads_through_the_adapter_as_context_only() {
    let h = setup(true);
    let now = h.ch.now();
    let r = push(
        &h,
        &h.bot,
        mention(FakeChannel::message("G1", "s1", "ualice", "@值班小运 在吗")),
    )
    .await;
    let space = r.space_id.clone();
    *h.history.msgs.lock().unwrap() = vec![
        at(
            FakeChannel::message("G1", "r1", "ualice", "上周五的发布回滚了"),
            now - 3 * HOUR,
        ),
        at(
            FakeChannel::message("G1", "r2", "ubob", "原因是配置没同步"),
            now - 2 * HOUR,
        ),
        // outside the range asked for: dropped
        at(
            FakeChannel::message("G1", "r0", "ubob", "很久以前"),
            now - 100 * HOUR,
        ),
    ];
    let (msgs, events) = (
        count(&h, "SELECT count(*) FROM chan_messages", &[]),
        count(&h, "SELECT count(*) FROM ext_events", &[]),
    );
    let out = im_mcp::call(
        &h.ch,
        &cx(ME),
        im_mcp::READ,
        &json!({"space": "运维群", "from": now - 24 * HOUR, "to": now, "limit": 10}),
    )
    .await
    .expect("read");
    assert!(out.contains("上周五的发布回滚了"), "{out}");
    assert!(out.contains("王小丽"), "{out}");
    assert!(out.contains("原因是配置没同步"), "{out}");
    assert!(!out.contains("很久以前"), "{out}");
    assert!(out.contains("平台实时读取"), "{out}");
    assert!(out.contains("level: "), "{out}");
    assert!(out.ends_with(steve_knowledge::tools::LEVELS));
    let asked = h.history.asked.lock().unwrap().clone();
    assert_eq!(asked.len(), 1);
    assert_eq!(asked[0].0, "G1");
    assert_eq!((asked[0].1, asked[0].2), (now - 24 * HOUR, now));
    // context only: nothing stored, no event, no rule ran
    assert_eq!(count(&h, "SELECT count(*) FROM chan_messages", &[]), msgs);
    assert_eq!(count(&h, "SELECT count(*) FROM ext_events", &[]), events);
    // by id too
    let by_id = im_mcp::call(&h.ch, &cx(ME), im_mcp::READ, &json!({"space": space}))
        .await
        .expect("by id");
    assert!(by_id.contains("运维群"));
    // a space set to never is not read
    exec(
        &h,
        "UPDATE space_settings SET disclosure = 'never' WHERE space_id = ?",
        &[&space],
    );
    let e = im_mcp::call(&h.ch, &cx(ME), im_mcp::READ, &json!({"space": space}))
        .await
        .expect_err("never");
    assert!(e.contains("不能读取"), "{e}");
    // bad arguments
    let e = im_mcp::call(
        &h.ch,
        &cx(ME),
        im_mcp::READ,
        &json!({"space": "运维群", "from": "昨天"}),
    )
    .await
    .expect_err("bad from");
    assert!(e.contains("from"), "{e}");
}

#[tokio::test(flavor = "multi_thread")]
async fn steve_im_read_without_a_history_view_reads_what_was_stored_and_only_what_you_see() {
    let h = setup(false);
    let now = h.ch.now();
    push(
        &h,
        &h.mine,
        at(
            FakeChannel::message("P1", "p1", "ucarol", "合同我发你邮箱了"),
            now - HOUR,
        ),
    )
    .await;
    let old = push(
        &h,
        &h.mine,
        at(
            FakeChannel::message("P1", "p0", "ucarol", "上个月的报价"),
            now - 30 * 24 * HOUR,
        ),
    )
    .await;
    assert!(old.history.is_some());
    let space = old.space_id.clone();
    let out = im_mcp::call(
        &h.ch,
        &cx(ME),
        im_mcp::READ,
        &json!({"space": space, "from": now - 40 * 24 * HOUR}),
    )
    .await
    .expect("read");
    assert!(out.contains("Steve 已存的消息"), "{out}");
    assert!(out.contains("合同我发你邮箱了"), "{out}");
    assert!(out.contains("上个月的报价"), "{out}");
    assert!(out.contains("接入时的旧消息"), "{out}");
    // your private chat is not someone else's to read
    let e = im_mcp::call(&h.ch, &cx(OTHER), im_mcp::READ, &json!({"space": space}))
        .await
        .expect_err("not theirs");
    assert!(e.contains("没有找到会话"), "{e}");
    // the limit holds and says there is more
    let out = im_mcp::call(
        &h.ch,
        &cx(ME),
        im_mcp::READ,
        &json!({"space": space, "from": now - 40 * 24 * HOUR, "limit": 1}),
    )
    .await
    .expect("read");
    assert!(out.contains("还有更多"), "{out}");
    assert!(!out.contains("合同我发你邮箱了"), "{out}");
}
