//! C1 acceptance (channels-personas.md §11) on the [`FakeChannel`]: a pushed message creates its
//! space, identifies the sender and reaches `ext_events`; a new group opens a decision; the bot's
//! own messages are not processed; purged text is NULL; the message is on the contact timeline.
//! Plus the `/spaces` routes, duplicates, `im.*` types, other bots, and the loop guard.

use std::sync::Arc;

use axum::Router;
use rusqlite::params;
use serde_json::{Value, json};
use steve_channels::fake::channel::FakeChannel;
use steve_channels::inbound::{LABEL_BOT, LABEL_OWN};
use steve_channels::messages::{self, MessageTimeline};
use steve_channels::{Channels, Inbound, Next, SpaceKind, api};
use steve_flow::testkit::FakeCore;
use steve_flow::{FlowSetup, Ingested, LinearExecutor};
use steve_modules::accounts::connections::{self, Connections, ConnectionsConfig};
use steve_modules::testkit::{ME, OTHER, TestEnv, WS, WS2, call, caller};
use steve_modules::vault::Vault;
use steve_modules::vault::keystore::FileKeyStore;
use tempfile::TempDir;

struct H {
    env: TestEnv,
    _keys: TempDir,
    fake: Arc<FakeChannel>,
    ch: Channels,
    api: Router,
    con: String,
    bot_acc: String,
}

fn setup() -> H {
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
    let ch = Channels::builder(env.modules.clone(), conns, Arc::new(flow))
        .adapter(fake.clone())
        .build();
    let mut ports = steve_modules::accounts::ports();
    ports.timelines.push(MessageTimeline::arc());
    let api = api::routes(ch.clone())
        .merge(steve_modules::contacts::routes(ports))
        .with_state(env.modules.clone());
    let (con, bot_acc) =
        FakeChannel::seed_connection(env.store(), &caller(WS, ME), None).expect("connection");
    fake.space(
        "G1",
        "运维群",
        SpaceKind::Group,
        false,
        &[("ualice", "王小丽"), ("ubob", "张伟")],
    );
    fake.space("D1", "", SpaceKind::Dm, false, &[("ualice", "王小丽")]);
    fake.space(
        "X1",
        "Acme 客户群",
        SpaceKind::Group,
        true,
        &[("ualice", "王小丽"), ("uext", "Sarah")],
    );
    H {
        env,
        _keys: keys,
        fake,
        ch,
        api,
        con,
        bot_acc,
    }
}

fn rows(h: &H, sql: &str, p: &[&str]) -> Vec<Vec<Value>> {
    let (sql, p): (String, Vec<String>) = (sql.into(), p.iter().map(|s| s.to_string()).collect());
    h.env
        .store()
        .reader(move |c| {
            let mut st = c.prepare(&sql)?;
            let n = st.column_count();
            let out = st
                .query_map(rusqlite::params_from_iter(p.iter()), |r| {
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
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok::<_, steve_store::StoreError>(out)
        })
        .expect("rows")
}

fn one(h: &H, sql: &str, p: &[&str]) -> Vec<Value> {
    let mut r = rows(h, sql, p);
    assert_eq!(r.len(), 1, "{sql} {p:?} → {r:?}");
    r.remove(0)
}

fn count(h: &H, sql: &str, p: &[&str]) -> i64 {
    one(h, sql, p)[0].as_i64().expect("count")
}

async fn push(h: &H, m: steve_channels::InboundMessage) -> Inbound {
    h.ch.on_inbound(WS, &h.con, m).await.expect("inbound")
}

#[tokio::test(flavor = "multi_thread")]
async fn pushed_message_creates_space_identifies_sender_and_records_event() {
    let h = setup();
    let r = push(
        &h,
        FakeChannel::message("G1", "m1", "ualice", "昨晚的告警是怎么回事"),
    )
    .await;
    assert!(r.space_created && !r.duplicate);
    assert_eq!(r.type_key, "im.message");
    assert_eq!(r.next, Next::Handle);

    // the space: name and kind from the adapter, settings not yet configured, default disclosure
    let s = one(
        &h,
        "SELECT s.name, s.kind, s.member_count, s.has_external, ss.configured, ss.disclosure, s.system_id FROM spaces s
           JOIN space_settings ss ON ss.space_id = s.id WHERE s.id = ?",
        &[&r.space_id],
    );
    assert_eq!(
        s,
        vec![
            json!("运维群"),
            json!("group"),
            json!(2),
            json!(0),
            json!(0),
            json!("conclusion"),
            json!("slack")
        ]
    );

    // the sender: identified with the name from the members list, a member of the space
    let acc = r.sender_account_id.clone().expect("sender account");
    let party = r.sender_party_id.clone().expect("sender party");
    let a = one(
        &h,
        "SELECT handle, display_name, party_id FROM accounts WHERE id = ?",
        &[&acc],
    );
    assert_eq!(a, vec![json!("ualice"), json!("王小丽"), json!(party)]);
    assert_eq!(
        one(
            &h,
            "SELECT name, contact_kind FROM parties WHERE id = ?",
            &[&party]
        ),
        vec![json!("王小丽"), json!("unconfirmed")]
    );
    assert_eq!(
        count(
            &h,
            "SELECT count(*) FROM space_members WHERE space_id = ?",
            &[&r.space_id]
        ),
        3,
        "bot + two members"
    );
    assert!(!r.sender_is_ours);

    // the message and its event
    let m = one(
        &h,
        "SELECT direction, text, status, sender_party_id, event_id, external_id FROM chan_messages WHERE id = ?",
        &[&r.message_id],
    );
    let Some(Ingested::Stored { id: evt, .. }) = r.event.clone() else {
        panic!("event stored: {:?}", r.event);
    };
    assert_eq!(
        m,
        vec![
            json!("in"),
            json!("昨晚的告警是怎么回事"),
            json!("received"),
            json!(party),
            json!(evt),
            json!("m1")
        ]
    );
    let e = one(
        &h,
        "SELECT source, type, space_id, message_id, actor_party_id, connection_id, summary, outcome_label FROM ext_events WHERE id = ?",
        &[&evt],
    );
    assert_eq!(
        e,
        vec![
            json!("im"),
            json!("im.message"),
            json!(r.space_id),
            json!(r.message_id),
            json!(party),
            json!(h.con),
            json!("王小丽：昨晚的告警是怎么回事"),
            json!("没有流程，只记录"),
        ]
    );

    // a second message in the same space: no new space, no new decision, the members not refetched
    let calls = *h.fake.member_calls.lock().unwrap();
    let r2 = push(&h, FakeChannel::message("G1", "m2", "ualice", "还有一件事")).await;
    assert!(!r2.space_created && r2.decision_id.is_none());
    assert_eq!(r2.space_id, r.space_id);
    assert_eq!(r2.sender_party_id, r.sender_party_id);
    assert_eq!(*h.fake.member_calls.lock().unwrap(), calls);
    assert_eq!(count(&h, "SELECT count(*) FROM spaces", &[]), 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn new_group_opens_one_decision_dm_and_existing_spaces_none() {
    let h = setup();
    let r = push(&h, FakeChannel::message("G1", "m1", "ualice", "hi")).await;
    let d = r.decision_id.clone().expect("decision");
    let row = one(
        &h,
        "SELECT kind, subject_type, subject_id, question, options, status, item_id FROM decisions WHERE id = ?",
        &[&d],
    );
    assert_eq!(
        row,
        vec![
            json!("ask"),
            json!("space"),
            json!(r.space_id),
            json!("测试机器人被拉进了「运维群」，要设置它在这里做什么吗？"),
            json!("[\"open\",\"mute\",\"quiet\",\"block\"]"),
            json!("open"),
            Value::Null,
        ]
    );
    push(&h, FakeChannel::message("G1", "m2", "ubob", "hi")).await;

    // a DM: no decision; named after the other person; DMs default to `reasoning`
    let dm = push(&h, FakeChannel::message("D1", "d1", "ualice", "私下问一句")).await;
    assert!(dm.space_created && dm.decision_id.is_none());
    assert_eq!(dm.type_key, "im.dm");
    assert_eq!(
        one(
            &h,
            "SELECT s.name, s.kind, ss.disclosure FROM spaces s JOIN space_settings ss ON ss.space_id = s.id WHERE s.id = ?",
            &[&dm.space_id]
        ),
        vec![json!("王小丽"), json!("dm"), json!("reasoning")]
    );
    // a customer group: `reasoning`, has_external
    let x = push(&h, FakeChannel::message("X1", "x1", "uext", "方案呢")).await;
    assert_eq!(
        one(
            &h,
            "SELECT s.has_external, ss.disclosure FROM spaces s JOIN space_settings ss ON ss.space_id = s.id WHERE s.id = ?",
            &[&x.space_id]
        ),
        vec![json!(1), json!("reasoning")]
    );
    assert_eq!(
        count(
            &h,
            "SELECT count(*) FROM decisions WHERE subject_type = 'space'",
            &[]
        ),
        2
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn space_info_failure_falls_back_to_the_payload() {
    let h = setup();
    *h.fake.fail_info.lock().unwrap() = true;
    let mut m = FakeChannel::message("G9", "m1", "ucarol", "hello");
    m.space_name = Some("临时群".into());
    let r = push(&h, m).await;
    assert!(r.space_created);
    assert_eq!(
        one(
            &h,
            "SELECT name, kind FROM spaces WHERE id = ?",
            &[&r.space_id]
        ),
        vec![json!("临时群"), json!("group")]
    );
    assert!(r.sender_account_id.is_some(), "still identified by handle");
}

#[tokio::test(flavor = "multi_thread")]
async fn own_bot_messages_are_not_processed_other_bots_only_recorded() {
    let h = setup();
    push(&h, FakeChannel::message("G1", "m0", "ualice", "hi")).await;

    // the connection's own account
    let own = push(
        &h,
        FakeChannel::message("G1", "m1", FakeChannel::BOT, "我是机器人自己发的"),
    )
    .await;
    assert_eq!(own.next, Next::Own);
    assert_eq!(own.sender_account_id.as_deref(), Some(h.bot_acc.as_str()));
    assert!(own.sender_is_ours);
    let evt = own
        .event
        .as_ref()
        .and_then(|e| e.id())
        .expect("event")
        .to_string();
    assert_eq!(
        one(
            &h,
            "SELECT outcome, outcome_label FROM ext_events WHERE id = ?",
            &[&evt]
        ),
        vec![json!("recorded"), json!(LABEL_OWN)]
    );

    // another bot: only recorded …
    let mut m = FakeChannel::message("G1", "m2", "umonitor", "【监控】CPU 90%");
    m.sender.is_bot = true;
    let other = push(&h, m).await;
    assert_eq!(other.next, Next::Record(LABEL_BOT));
    let evt = other
        .event
        .as_ref()
        .and_then(|e| e.id())
        .expect("event")
        .to_string();
    assert_eq!(
        one(
            &h,
            "SELECT outcome_label FROM ext_events WHERE id = ?",
            &[&evt]
        ),
        vec![json!(LABEL_BOT)]
    );
    assert_eq!(
        one(
            &h,
            "SELECT role FROM space_members WHERE space_id = ? AND account_id = ?",
            &[&other.space_id, other.sender_account_id.as_deref().unwrap()]
        ),
        vec![json!("bot")]
    );

    // … unless the space collaborates
    let (s, v) = call(
        &h.api,
        Some(&caller(WS, ME)),
        "PATCH",
        &format!("/spaces/{}", other.space_id),
        Some(&json!({"collab": true})),
    )
    .await;
    assert_eq!(s, 200, "{v}");
    let mut m = FakeChannel::message("G1", "m3", "umonitor", "【监控】恢复");
    m.sender.is_bot = true;
    assert_eq!(push(&h, m).await.next, Next::Handle);
}

#[tokio::test(flavor = "multi_thread")]
async fn redelivery_is_a_duplicate_and_writes_nothing() {
    let h = setup();
    let m = FakeChannel::message("G1", "m1", "ualice", "一次就好");
    let a = push(&h, m.clone()).await;
    let mut again = m.clone();
    again.thread_key = Some("t1".into());
    let b = push(&h, again).await;
    assert!(b.duplicate && b.event.is_none());
    assert_eq!(b.message_id, a.message_id);
    assert_eq!(count(&h, "SELECT count(*) FROM chan_messages", &[]), 1);
    assert_eq!(count(&h, "SELECT count(*) FROM ext_events", &[]), 1);
    // the second path filled what the first lacked
    assert_eq!(
        one(
            &h,
            "SELECT thread_key FROM chan_messages WHERE id = ?",
            &[&a.message_id]
        ),
        vec![json!("t1")]
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn types_mention_reply_and_mentions_column() {
    let h = setup();
    let first = push(&h, FakeChannel::message("G1", "m0", "ualice", "hi")).await;
    let mut m = FakeChannel::message("G1", "m1", "ubob", "@测试机器人 帮我看看 @王小丽");
    m.mentions = vec![
        steve_channels::Mention {
            handle: None,
            name: "测试机器人".into(),
            is_connection: true,
        },
        steve_channels::Mention {
            handle: Some(FakeChannel::handle("ualice")),
            name: "王小丽".into(),
            is_connection: false,
        },
    ];
    let r = push(&h, m).await;
    assert_eq!(r.type_key, "im.mention");
    let mentions: Value = serde_json::from_str(
        one(
            &h,
            "SELECT mentions FROM chan_messages WHERE id = ?",
            &[&r.message_id],
        )[0]
        .as_str()
        .unwrap(),
    )
    .unwrap();
    assert_eq!(
        mentions,
        json!([{"account_id": h.bot_acc, "name": "测试机器人"}, {"account_id": first.sender_account_id, "name": "王小丽"}])
    );

    // a reply to a message Steve sent
    h.env
        .store()
        .with_conn(|c| {
            c.execute(
                "INSERT INTO chan_messages (id, workspace_id, space_id, external_id, direction, sender_account_id, on_behalf, text, status, created, sent)
                 VALUES ('cm_out1', 'default', ?, 'fm_1', 'out', ?, 'persona', '已处理', 'sent', 1, 1)",
                params![r.space_id, h.bot_acc],
            )?;
            Ok(())
        })
        .unwrap();
    let mut m = FakeChannel::message("G1", "m2", "ualice", "谢谢");
    m.reply_to = Some("fm_1".into());
    let r = push(&h, m).await;
    assert_eq!(r.type_key, "im.reply");
    let evt = r.event.as_ref().and_then(|e| e.id()).unwrap().to_string();
    assert_eq!(
        one(&h, "SELECT type FROM ext_events WHERE id = ?", &[&evt]),
        vec![json!("im.reply")]
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn too_many_replies_mute_the_space_with_a_fail_decision() {
    let h = setup();
    let r = push(&h, FakeChannel::message("G1", "m0", "ualice", "hi")).await;
    let now = h.ch.now();
    h.env
        .store()
        .with_conn(|c| {
            for i in 0..21 {
                c.execute(
                    "INSERT INTO chan_messages (id, workspace_id, space_id, external_id, direction, text, status, created, sent)
                     VALUES (?, 'default', ?, ?, 'out', 'x', 'sent', ?, ?)",
                    params![format!("cm_o{i}"), r.space_id, format!("fm_{i}"), now, now],
                )?;
            }
            Ok(())
        })
        .unwrap();
    let r2 = push(&h, FakeChannel::message("G1", "m1", "ualice", "再来")).await;
    assert!(r2.muted_now);
    assert_eq!(
        one(
            &h,
            "SELECT muted FROM space_settings WHERE space_id = ?",
            &[&r.space_id]
        ),
        vec![json!(1)]
    );
    assert_eq!(
        one(
            &h,
            "SELECT kind, question FROM decisions WHERE subject_id = ? AND kind = 'fail'",
            &[&r.space_id]
        ),
        vec![json!("fail"), json!("运维群 里回复太多，已静默")]
    );
    // once muted, no second decision
    assert!(
        !push(&h, FakeChannel::message("G1", "m2", "ualice", "还在吗"))
            .await
            .muted_now
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn purged_text_is_null_everywhere() {
    let h = setup();
    let r = push(
        &h,
        FakeChannel::message("G1", "m1", "ualice", "密码是什么来着"),
    )
    .await;
    // 7 days (the default keepRaw) and a bit later
    let later = h.env.clock.advance(8 * 24 * 3_600_000);
    assert_eq!(messages::sweep(&h.env.modules, later).unwrap(), 1);
    assert_eq!(
        one(
            &h,
            "SELECT text, purged FROM chan_messages WHERE id = ?",
            &[&r.message_id]
        ),
        vec![Value::Null, json!(later)]
    );
    assert_eq!(messages::sweep(&h.env.modules, later).unwrap(), 0, "once");
    let (s, v) = call(
        &h.api,
        Some(&caller(WS, ME)),
        "GET",
        &format!("/spaces/{}/messages", r.space_id),
        None,
    )
    .await;
    assert_eq!(s, 200, "{v}");
    assert_eq!(v[0]["text"], Value::Null);
    assert_eq!(v[0]["purged"], json!(later));

    // keepRaw = 30: a 8-day-old message stays
    let r2 = push(&h, FakeChannel::message("G1", "m2", "ualice", "新的")).await;
    h.env
        .store()
        .with_conn(|c| {
            c.execute("INSERT INTO settings (workspace_id, key, value, updated) VALUES ('default', 'keepRaw', '\"30\"', 1)", [])?;
            Ok(())
        })
        .unwrap();
    assert_eq!(
        messages::sweep(&h.env.modules, later + 8 * 24 * 3_600_000).unwrap(),
        0
    );
    assert_eq!(
        one(
            &h,
            "SELECT text FROM chan_messages WHERE id = ?",
            &[&r2.message_id]
        ),
        vec![json!("新的")]
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn message_shows_on_the_contact_timeline() {
    let h = setup();
    let r = push(
        &h,
        FakeChannel::message("G1", "m1", "ualice", "下周三上线可以吗"),
    )
    .await;
    let party = r.sender_party_id.clone().unwrap();
    let (s, v) = call(
        &h.api,
        Some(&caller(WS, ME)),
        "GET",
        &format!("/parties/{party}/timeline"),
        None,
    )
    .await;
    assert_eq!(s, 200, "{v}");
    let line = v
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["kind"] == "message")
        .unwrap_or_else(|| panic!("message line in {v}"));
    assert_eq!(line["text"], "在「运维群」说：下周三上线可以吗");
    assert_eq!(line["href"], format!("#/channels/space/{}", r.space_id));

    // after the retention period the line stays, without the text
    let later = h.env.clock.advance(8 * 24 * 3_600_000);
    messages::sweep(&h.env.modules, later).unwrap();
    let (_, v) = call(
        &h.api,
        Some(&caller(WS, ME)),
        "GET",
        &format!("/parties/{party}/timeline"),
        None,
    )
    .await;
    let line = v
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["kind"] == "message")
        .unwrap();
    assert_eq!(line["text"], "在「运维群」发了一条消息（原文已过保留期限）");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_disconnected_space_whose_account_is_gone_is_the_owners_alone() {
    let h = setup();
    let r = push(
        &h,
        FakeChannel::message("G1", "m1", "ualice", "下周三上线可以吗"),
    )
    .await;
    let party = r.sender_party_id.clone().unwrap();
    let lines = |v: &Value| {
        v.as_array()
            .map(|a| a.iter().filter(|e| e["kind"] == "message").count())
            .unwrap_or(0)
    };
    let timeline = |who: &'static str| {
        let (api, path) = (h.api.clone(), format!("/parties/{party}/timeline"));
        async move { call(&api, Some(&caller(WS, who)), "GET", &path, None).await }
    };
    // a bot account's group: everyone sees it, connected or disconnected
    assert_eq!(lines(&timeline(OTHER).await.1), 1);
    h.env
        .store()
        .with_conn(|c| {
            c.execute(
                "UPDATE spaces SET status = 'left', account_id = (SELECT account_id FROM connections WHERE id = connection_id), connection_id = NULL WHERE id = ?",
                [&r.space_id],
            )?;
            // what ChannelUsage does to the identities
            c.execute(
                "UPDATE space_identities SET left_at = 1 WHERE space_id = ?",
                [&r.space_id],
            )?;
            Ok(())
        })
        .unwrap();
    assert_eq!(lines(&timeline(OTHER).await.1), 1);
    let path = format!("/spaces/{}", r.space_id);
    let (st, _) = call(&h.api, Some(&caller(WS, OTHER)), "GET", &path, None).await;
    assert_eq!(st, 200);

    // the account no longer resolves: fail closed, only the workspace owner (ME) still sees it
    h.env
        .store()
        .with_conn(|c| {
            c.execute(
                "UPDATE spaces SET account_id = 'acc_gone' WHERE id = ?",
                [&r.space_id],
            )?;
            Ok(())
        })
        .unwrap();
    let (st, _) = call(&h.api, Some(&caller(WS, OTHER)), "GET", &path, None).await;
    assert_eq!(st, 404);
    let (_, v) = call(&h.api, Some(&caller(WS, OTHER)), "GET", "/spaces", None).await;
    assert!(
        v.as_array()
            .unwrap()
            .iter()
            .all(|x| x["id"] != r.space_id.as_str()),
        "{v}"
    );
    assert_eq!(lines(&timeline(OTHER).await.1), 0);
    let (st, _) = call(&h.api, Some(&caller(WS, ME)), "GET", &path, None).await;
    assert_eq!(st, 200);
    assert_eq!(lines(&timeline(ME).await.1), 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn space_routes_list_detail_patch_and_404s() {
    let h = setup();
    let me = caller(WS, ME);
    let r = push(&h, FakeChannel::message("G1", "m1", "ualice", "hi")).await;
    push(&h, FakeChannel::message("D1", "d1", "ualice", "私聊")).await;

    let (s, v) = call(&h.api, Some(&me), "GET", "/spaces", None).await;
    assert_eq!(s, 200, "{v}");
    assert_eq!(v.as_array().unwrap().len(), 2);
    let (_, v) = call(&h.api, Some(&me), "GET", "/spaces?q=运维", None).await;
    assert_eq!(v.as_array().unwrap().len(), 1);
    assert_eq!(v[0]["name"], "运维群");
    assert_eq!(
        v[0]["effective_rules"],
        json!([{"when": {"kind": "mention_or_dm"}, "act": "reply"}])
    );
    assert_eq!(v[0]["rules"], Value::Null);
    assert_eq!(v[0]["configured"], false);
    assert_eq!(v[0]["has_external"], false);
    let (_, v) = call(
        &h.api,
        Some(&me),
        "GET",
        &format!("/spaces?connection={}", "con_nope"),
        None,
    )
    .await;
    assert_eq!(v, json!([]));

    let (s, v) = call(
        &h.api,
        Some(&me),
        "GET",
        &format!("/spaces/{}", r.space_id),
        None,
    )
    .await;
    assert_eq!(s, 200, "{v}");
    assert_eq!(v["recent"][0]["text"], "hi");
    assert_eq!(v["recent"][0]["sender"]["name"], "王小丽");
    assert_eq!(v["recent"][0]["basis"], Value::Null);

    let body = json!({"duty": "负责告警排查", "muted": true, "disclosure": "reasoning", "memory_scope": "persona",
                      "item_defaults": {"tags": [], "project_id": null, "bot_id": null}});
    let (s, v) = call(
        &h.api,
        Some(&me),
        "PATCH",
        &format!("/spaces/{}", r.space_id),
        Some(&body),
    )
    .await;
    assert_eq!(s, 200, "{v}");
    assert_eq!(
        (
            v["duty"].clone(),
            v["muted"].clone(),
            v["disclosure"].clone(),
            v["configured"].clone(),
            v["revision"].clone()
        ),
        (
            json!("负责告警排查"),
            json!(true),
            json!("reasoning"),
            json!(true),
            json!(2)
        )
    );
    assert_eq!(
        count(
            &h,
            "SELECT count(*) FROM revisions WHERE object_type = 'space' AND object_id = ?",
            &[&r.space_id]
        ),
        1
    );
    assert_eq!(
        count(
            &h,
            "SELECT count(*) FROM audit WHERE action = 'space.patch'",
            &[]
        ),
        1
    );

    // validation and references
    let (s, v) = call(
        &h.api,
        Some(&me),
        "PATCH",
        &format!("/spaces/{}", r.space_id),
        Some(&json!({"disclosure": "secret"})),
    )
    .await;
    assert_eq!(
        (s, v["error"].clone()),
        (400, json!("参数不对：disclosure"))
    );
    let (s, v) = call(
        &h.api,
        Some(&me),
        "PATCH",
        &format!("/spaces/{}", r.space_id),
        Some(&json!({"item_defaults": {"bot_id": "nobot"}})),
    )
    .await;
    assert_eq!((s, v["error"].clone()), (404, json!("没有这个 Bot")));
    let (s, _) = call(
        &h.api,
        Some(&me),
        "GET",
        &format!("/spaces/{}/messages?limit=101", r.space_id),
        None,
    )
    .await;
    assert_eq!(s, 400);

    // another workspace sees nothing
    let w2 = caller(WS2, ME);
    let (s, v) = call(
        &h.api,
        Some(&w2),
        "GET",
        &format!("/spaces/{}", r.space_id),
        None,
    )
    .await;
    assert_eq!((s, v["error"].clone()), (404, json!("没有这个空间")));
    let (_, v) = call(&h.api, Some(&w2), "GET", "/spaces", None).await;
    assert_eq!(v, json!([]));
}

#[tokio::test(flavor = "multi_thread")]
async fn spaces_of_a_personal_connection_are_private_to_its_owner() {
    let h = setup();
    let (mine, _) = FakeChannel::seed_connection(h.env.store(), &caller(WS, ME), Some("ume"))
        .expect("personal");
    let r =
        h.ch.on_inbound(
            WS,
            &mine,
            FakeChannel::message("G1", "p1", "ualice", "只给我看"),
        )
        .await
        .unwrap();
    // a self connection opens no “新群” decision and gets the self default rules
    assert!(r.decision_id.is_none());
    assert_eq!(
        one(
            &h,
            "SELECT mode FROM channel_settings WHERE connection_id = ?",
            &[&mine]
        ),
        vec![json!("self")]
    );
    let (s, _) = call(
        &h.api,
        Some(&caller(WS, ME)),
        "GET",
        &format!("/spaces/{}", r.space_id),
        None,
    )
    .await;
    assert_eq!(s, 200);
    let (s, _) = call(
        &h.api,
        Some(&caller(WS, OTHER)),
        "GET",
        &format!("/spaces/{}", r.space_id),
        None,
    )
    .await;
    assert_eq!(s, 404);
    // the event is private too (S1: a connection of someone's “我”)
    let evt = r.event.as_ref().and_then(|e| e.id()).unwrap().to_string();
    assert_eq!(
        one(
            &h,
            "SELECT visibility FROM ext_events WHERE id = ?",
            &[&evt]
        ),
        vec![json!("private")]
    );
    // you writing in the group from your own account: not processed
    let own =
        h.ch.on_inbound(
            WS,
            &mine,
            FakeChannel::message("G1", "p2", "ume", "/bot 帮我回一下"),
        )
        .await
        .unwrap();
    assert_eq!(own.next, Next::Own);
}
