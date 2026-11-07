//! C1 end to end on the fake Feishu: the bot connected through the real connect route, its long
//! connection started from the vault, `im.message.receive_v1` frames pushed by the fake's ws
//! server land as spaces, identified senders, `chan_messages` and `ext_events`; repeats of an
//! `event_id` are processed once, also after a restart of the long connection.

use std::sync::Arc;
use std::time::Duration;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, header};
use serde_json::{Value, json};
use steve_channels::Channels;
use steve_channels::adapters::feishu::FeishuApp;
use steve_channels::adapters::feishu::inbound::LongConnection;
use steve_channels::adapters::feishu::ws::WsState;
use steve_channels::fake::feishu::{FakeFeishu, WsClientConfig, fixture, ids};
use steve_channels::inbound::{LABEL_BOT, LABEL_OWN};
use steve_flow::testkit::FakeCore;
use steve_flow::{FlowSetup, LinearExecutor};
use steve_modules::accounts::connections::{self, Connections, ConnectionsConfig};
use steve_modules::testkit::{ME, TestEnv, WS, caller};
use steve_modules::vault::Vault;
use steve_modules::vault::keystore::FileKeyStore;
use tempfile::TempDir;
use tower::ServiceExt;

const T: Duration = Duration::from_secs(10);

struct H {
    env: TestEnv,
    _keys: TempDir,
    fake: FakeFeishu,
    ch: Channels,
    con: String,
    bot_acc: String,
}

async fn setup() -> H {
    let fake = FakeFeishu::start().await.expect("fake feishu");
    fake.ws_set_client_config(WsClientConfig {
        reconnect_count: -1,
        reconnect_interval: 1,
        reconnect_nonce: 0,
        ping_interval: 1,
    });
    let core = Arc::new(FakeCore::default());
    let flow_setup = FlowSetup::new(core);
    let env = TestEnv::new(|r| {
        flow_setup.register(r);
        connections::register(r);
    });
    // the Hub's clock just before the fixtures were written: connected then, they are news
    env.clock.set(1_761_899_000_000);
    let keys = tempfile::tempdir().expect("tmp");
    let vault = Vault::new(
        env.store().clone(),
        Arc::new(FileKeyStore::open(keys.path()).expect("keys")),
    );
    vault.init(WS, ME).expect("vault init");
    let app = Arc::new(FeishuApp::new());
    let conns = Connections::builder(
        env.modules.clone(),
        vault,
        ConnectionsConfig::new(vec!["http://hub.test:8440".into()], true, |_| None),
    )
    .provider(app.clone())
    .build();
    let flow = flow_setup
        .connections(conns.clone())
        .build(env.modules.clone(), Arc::new(LinearExecutor));
    let ch = Channels::builder(env.modules.clone(), conns.clone(), Arc::new(flow))
        .adapter(app)
        .build();

    // connect the bot like the dialog does
    let api: Router = connections::routes(conns).with_state(env.modules.clone());
    let body = json!({
        "adapter": "feishu_app",
        "fields": {"App ID": ids::APP_ID, "App Secret": ids::APP_SECRET},
        "config": {"api_base": fake.base_url()},
    });
    let mut req = Request::builder()
        .method("POST")
        .uri("/connections")
        .header(header::HOST, "hub.test:8440")
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(body.to_string()))
        .expect("request");
    req.extensions_mut().insert(caller(WS, ME));
    let res = api.oneshot(req).await.expect("infallible");
    let status = res.status().as_u16();
    let v: Value = serde_json::from_slice(
        &axum::body::to_bytes(res.into_body(), 1 << 20)
            .await
            .expect("body"),
    )
    .expect("json");
    assert_eq!(status, 200, "{v}");
    H {
        env,
        _keys: keys,
        fake,
        ch,
        con: v["id"].as_str().expect("id").into(),
        bot_acc: v["account_id"].as_str().expect("account").into(),
    }
}

fn q(h: &H, sql: &str, p: &str) -> Vec<Vec<Value>> {
    let (sql, p) = (sql.to_string(), p.to_string());
    h.env
        .store()
        .reader(move |c| {
            let mut st = c.prepare(&sql)?;
            let n = st.column_count();
            let rows = st
                .query_map([&p], |r| {
                    (0..n)
                        .map(|i| {
                            Ok(match r.get_ref(i)? {
                                rusqlite::types::ValueRef::Null => Value::Null,
                                rusqlite::types::ValueRef::Integer(x) => json!(x),
                                rusqlite::types::ValueRef::Text(t) => {
                                    json!(String::from_utf8_lossy(t))
                                }
                                _ => Value::Null,
                            })
                        })
                        .collect::<rusqlite::Result<Vec<_>>>()
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok::<_, steve_store::StoreError>(rows)
        })
        .expect("query")
}

fn n(h: &H, sql: &str) -> i64 {
    q(h, sql, "")[0][0].as_i64().expect("count")
}

/// Waits until `sql` (a count) reaches `want`.
async fn until(h: &H, sql: &str, want: i64) {
    let t0 = std::time::Instant::now();
    loop {
        let got = n(h, sql);
        if got >= want {
            assert_eq!(got, want, "{sql}");
            return;
        }
        assert!(t0.elapsed() < T, "{sql}: {got} < {want}");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

async fn start(h: &H) -> LongConnection {
    let lc = LongConnection::start(&h.ch, WS, &h.con)
        .await
        .expect("long connection");
    lc.ws
        .wait_for(T, |s| matches!(s, WsState::Connected { .. }))
        .await
        .expect("connected");
    lc
}

/// Inbound messages (the rules may add pending drafts, `direction=out`).
const MSGS: &str = "SELECT count(*) FROM chan_messages WHERE ?1 = ?1 AND direction = 'in'";
const EVENTS: &str = "SELECT count(*) FROM ext_events WHERE ?1 = ?1";

#[tokio::test(flavor = "multi_thread")]
async fn group_mention_over_the_long_connection() {
    let h = setup().await;
    let lc = start(&h).await;

    let id = h
        .fake
        .ws_push_envelope(fixture("message_group_at_bot").unwrap());
    let d = h.fake.ws_wait_acked(&id, T).await.expect("acked");
    assert_eq!(d.ack.expect("ack").code, 200);
    until(&h, MSGS, 1).await;
    // the message is stored before it is linked to its event: wait for the link too
    until(
        &h,
        "SELECT count(*) FROM chan_messages WHERE ?1 = ?1 AND direction = 'in' AND event_id IS NOT NULL",
        1,
    )
    .await;

    // the space, from GET chats/{id}
    let s = &q(
        &h,
        "SELECT s.id, s.name, s.kind, s.system_id, s.member_count, ss.configured FROM spaces s JOIN space_settings ss ON ss.space_id = s.id WHERE s.external_id = ?",
        ids::GROUP_CHAT,
    )[0];
    assert_eq!(
        &s[1..],
        &[
            json!("平台值班群"),
            json!("group"),
            json!("feishu_app"),
            json!(3),
            json!(0)
        ]
    );
    let space = s[0].as_str().unwrap().to_string();

    // the message: text with the mention replaced, the sender identified by union_id with the
    // name from the members list, the bot in `mentions`
    let m = &q(
        &h,
        "SELECT m.text, a.handle, a.display_name, a.system_id, m.mentions, m.thread_key, m.event_id FROM chan_messages m
           JOIN accounts a ON a.id = m.sender_account_id WHERE m.external_id = ?",
        "om_groupat00000000000000000000001",
    )[0];
    assert_eq!(m[0], json!("@值班小运 帮我查一下昨晚告警的原因"));
    assert_eq!(
        &m[1..4],
        &[json!(ids::ALICE.union_id), json!("王小丽"), json!("feishu")]
    );
    let mentions: Value = serde_json::from_str(m[4].as_str().unwrap()).unwrap();
    assert_eq!(
        mentions,
        json!([{"account_id": h.bot_acc, "name": "值班小运"}])
    );
    assert_eq!(m[5], json!("om_groupat00000000000000000000001"));

    // its event and the “新群” decision
    let e = &q(
        &h,
        "SELECT type, source, space_id, summary FROM ext_events WHERE id = ?",
        m[6].as_str().unwrap(),
    )[0];
    assert_eq!(
        e,
        &vec![
            json!("im.mention"),
            json!("im"),
            json!(space),
            json!("王小丽：@值班小运 帮我查一下昨晚告警的原因")
        ]
    );
    let dq = &q(
        &h,
        "SELECT question FROM decisions WHERE subject_type = 'space' AND subject_id = ?",
        &space,
    )[0];
    assert_eq!(
        dq[0],
        json!("值班小运被拉进了「平台值班群」，要设置它在这里做什么吗？")
    );
    // the group's members (owner, Alice, Bob) and the bot
    assert_eq!(
        n(
            &h,
            &format!("SELECT count(*) FROM space_members WHERE space_id = '{space}' AND ?1 = ?1")
        ),
        4
    );

    // a repeat of the same event_id: acked, processed once
    assert!(h.fake.ws_redeliver(&id));
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(h.fake.ws_deliveries_of(&id).len(), 2);
    assert_eq!(n(&h, MSGS), 1);
    assert_eq!(n(&h, EVENTS), 1);

    // after a restart (empty memory) the unique index still stops it
    lc.stop().await.unwrap();
    let lc = start(&h).await;
    assert!(h.fake.ws_redeliver(&id));
    let t0 = std::time::Instant::now();
    while h
        .fake
        .ws_deliveries_of(&id)
        .iter()
        .filter(|d| d.ack.is_some())
        .count()
        < 3
    {
        assert!(t0.elapsed() < T, "third delivery acked");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(n(&h, MSGS), 1);
    assert_eq!(n(&h, EVENTS), 1);
    lc.stop().await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn dm_external_bots_and_own_messages() {
    let h = setup().await;
    let lc = start(&h).await;

    // a DM: no decision, the space named after Alice
    h.fake
        .ws_push_envelope(fixture("message_p2p_text").unwrap());
    until(&h, MSGS, 1).await;
    // The channel message commits before the asynchronous event sink finishes.
    until(&h, EVENTS, 1).await;
    let s = &q(
        &h,
        "SELECT name, kind FROM spaces WHERE external_id = ?",
        ids::P2P_CHAT,
    )[0];
    assert_eq!(s, &vec![json!("王小丽"), json!("dm")]);
    assert_eq!(
        n(
            &h,
            "SELECT count(*) FROM decisions WHERE subject_type = 'space' AND ?1 = ?1"
        ),
        0
    );
    assert_eq!(
        q(&h, "SELECT type FROM ext_events WHERE ?1 = ?1", "")[0][0],
        json!("im.dm")
    );

    // a customer in the external group
    h.fake
        .ws_push_envelope(fixture("message_external_user").unwrap());
    until(&h, MSGS, 2).await;
    let s = &q(
        &h,
        "SELECT s.has_external, ss.disclosure FROM spaces s JOIN space_settings ss ON ss.space_id = s.id WHERE s.external_id = ?",
        ids::EXTERNAL_CHAT,
    )[0];
    assert_eq!(s, &vec![json!(1), json!("reasoning")]);

    // another app's bot: only recorded
    h.fake
        .ws_push_envelope(fixture("message_from_bot").unwrap());
    until(&h, MSGS, 3).await;
    let e = &q(
        &h,
        "SELECT e.outcome_label FROM ext_events e JOIN chan_messages m ON m.event_id = e.id WHERE m.external_id = ?",
        "om_otherbot0000000000000000000001",
    )[0];
    assert_eq!(e[0], json!(LABEL_BOT));

    // the bot itself (its open_id): identified as our own account, not processed
    let mut env = fixture("message_from_bot").unwrap();
    env["header"]["event_id"] = json!("evt_own_000000000000000000000001");
    env["event"]["sender"]["sender_id"]["open_id"] = json!(ids::BOT_OPEN_ID);
    env["event"]["message"]["message_id"] = json!("om_own00000000000000000000000001");
    h.fake.ws_push_envelope(env);
    until(&h, MSGS, 4).await;
    let r = &q(
        &h,
        "SELECT m.sender_account_id, e.outcome_label FROM chan_messages m JOIN ext_events e ON e.id = m.event_id WHERE m.external_id = ?",
        "om_own00000000000000000000000001",
    )[0];
    assert_eq!(r, &vec![json!(h.bot_acc), json!(LABEL_OWN)]);
    lc.stop().await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn the_bot_run_reports_the_connection_healthy_once_connected() {
    use steve_channels::adapters::feishu::BotHook;
    use steve_channels::ext::ChannelDecl;
    use steve_channels::lifecycle::ConnectionRuntime;
    use steve_modules::accounts::connections::Health;

    let h = setup().await;
    let status = |h: &H| q(h, "SELECT status FROM connections WHERE id = ?", &h.con)[0][0].clone();
    // failed starts before (the vault not readable at boot): reported, `error`
    let cx = h.ch.connections().cx(WS, &h.con).expect("cx");
    for _ in 0..3 {
        cx.report(Health::Failed("vault locked".into())).await;
    }
    assert_eq!(status(&h), json!("error"));
    // a run that gets its long connection up clears it
    let rt = ConnectionRuntime::builder(h.ch.clone())
        .hook(
            "feishu_app",
            Some(ChannelDecl::new("feishu_app")),
            Arc::new(BotHook::new(h.ch.clone())),
        )
        .build();
    let (_tx, rx) = tokio::sync::mpsc::unbounded_channel();
    drop(rt.start(rx));
    let t0 = std::time::Instant::now();
    while status(&h) != json!("ok") {
        assert!(t0.elapsed() < T, "still {}", status(&h));
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let fails = q(
        &h,
        "SELECT json_extract(health, '$.fails') FROM connections WHERE id = ?",
        &h.con,
    );
    assert_eq!(fails[0][0], json!(0));
    rt.shutdown().await;
}
