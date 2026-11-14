//! The external channel (`ext_ws`) end to end against a fake channel service speaking the
//! protocol (`steve_channels::fake::ext_ws`): the Hub's conformance checklist and test vectors
//! (protocol §11.2, §11.3). A connection is made through the credential form, its feed runs under
//! the connection runtime, and every message goes through the real inbound pipeline.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use serde_json::{Value, json};
use steve_channels::adapters::ext_ws::{self, ExtWs, ExtWsHook, Timing};
use steve_channels::channel::{ActAsUser, ActTarget, MessageRef, OutContent, SendKey};
use steve_channels::fake::ext_ws::{self as fake, FakeExtWs};
use steve_channels::history::HistoryRead;
use steve_channels::inbound::{LABEL_HISTORY_BACKFILL, LABEL_OWN};
use steve_channels::lifecycle::ConnectionRuntime;
use steve_channels::media::{MediaError, MediaFetch};
use steve_channels::{Channels, HandleRef};
use steve_flow::testkit::FakeCore;
use steve_flow::{AdapterError, FlowSetup, LinearExecutor};
use steve_modules::Clock;
use steve_modules::accounts::connections::{
    self, ConnCx, Connections, ConnectionsConfig, ProviderError,
};
use steve_modules::accounts::repo as arepo;
use steve_modules::accounts::service::ensure_seeds;
use steve_modules::testkit::{ME, TestEnv, WS, caller};
use steve_modules::vault::Vault;
use steve_modules::vault::keystore::FileKeyStore;
use tempfile::TempDir;
use tokio::sync::mpsc;
use zeroize::Zeroizing;

const T: Duration = Duration::from_secs(10);

fn timing() -> Timing {
    Timing {
        ping: Duration::from_millis(300),
        silence: Duration::from_secs(3),
        hello: Duration::from_secs(3),
        call: Duration::from_secs(3),
        history: Duration::from_secs(3),
        media: Duration::from_secs(3),
        backoff_min: Duration::from_millis(50),
        backoff_max: Duration::from_millis(400),
        stable: Duration::from_secs(1),
    }
}

struct H {
    env: TestEnv,
    _keys: TempDir,
    fake: FakeExtWs,
    ext: Arc<ExtWs>,
    conns: Connections,
    ch: Channels,
    /// The Hub's clock when the test started (the fake's clock too).
    t0: i64,
}

async fn setup() -> H {
    let fake = FakeExtWs::start().await.expect("fake service");
    let core = Arc::new(FakeCore::default());
    let flow_setup = FlowSetup::new(core);
    let env = TestEnv::new(|r| {
        flow_setup.register(r);
        connections::register(r);
    });
    let t0 = env.clock.now();
    fake.with(|w| w.now = Some(t0));
    env.store()
        .transaction(|tx| -> steve_modules::ApiResult<()> {
            ensure_seeds(tx, WS)?;
            arepo::upsert_declared(tx, ext_ws::SYSTEM, ext_ws::SYSTEM_LABEL, "ext_ws")?;
            Ok(())
        })
        .expect("systems");
    let keys = tempfile::tempdir().expect("tmp");
    let vault = Vault::new(
        env.store().clone(),
        Arc::new(FileKeyStore::open(keys.path()).expect("keys")),
    );
    vault.init(WS, ME).expect("vault init");
    let ext = Arc::new(ExtWs::with_timing(timing()));
    let conns = Connections::builder(
        env.modules.clone(),
        vault,
        ConnectionsConfig::new(Vec::new(), true, |_| None),
    )
    .provider(ext.clone())
    .build();
    let flow = flow_setup
        .connections(conns.clone())
        .build(env.modules.clone(), Arc::new(LinearExecutor));
    let ch = Channels::builder(env.modules.clone(), conns.clone(), Arc::new(flow))
        .decl(ext_ws::decl())
        .media(ext.clone())
        .connector(ext.clone())
        .history(ext.clone())
        .profiles(ext.clone())
        .build();
    H {
        env,
        _keys: keys,
        fake,
        ext,
        conns,
        ch,
        t0,
    }
}

fn fields(url: &str, token: &str) -> BTreeMap<String, Zeroizing<String>> {
    let mut f = BTreeMap::new();
    f.insert(ext_ws::URL.to_string(), Zeroizing::new(url.to_string()));
    f.insert(ext_ws::TOKEN.to_string(), Zeroizing::new(token.to_string()));
    f
}

impl H {
    async fn connect(&self) -> String {
        self.conns
            .create(
                &caller(WS, ME),
                ext_ws::ADAPTER,
                &fields(&self.fake.url(), fake::TOKEN),
                json!({}),
            )
            .await
            .expect("connected")
            .id
    }

    fn cx(&self, id: &str) -> ConnCx {
        self.conns.cx(WS, id).expect("cx")
    }

    /// The connection runtime with the feed, every event so far published.
    fn runtime(&self) -> (ConnectionRuntime, mpsc::Sender<Value>) {
        let rt = ConnectionRuntime::builder(self.ch.clone())
            .hook(
                ext_ws::ADAPTER,
                Some(ext_ws::decl()),
                Arc::new(ExtWsHook::new(self.ch.clone(), self.ext.clone())),
            )
            .build();
        let (tx, rx) = mpsc::channel(256);
        drop(rt.start(rx));
        self.publish(&tx);
        (rt, tx)
    }

    fn publish(&self, tx: &mpsc::Sender<Value>) {
        for e in self.env.take_events() {
            let _ = tx.try_send(serde_json::to_value(e).expect("json"));
        }
    }

    fn rows(&self, sql: &str, p: &[&str]) -> Vec<Vec<Value>> {
        let (sql, p): (String, Vec<String>) =
            (sql.into(), p.iter().map(|s| s.to_string()).collect());
        self.env
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
                            .collect::<rusqlite::Result<Vec<Value>>>()
                    })?
                    .collect::<rusqlite::Result<Vec<_>>>()?;
                Ok::<_, steve_modules::ApiError>(out)
            })
            .expect("rows")
    }

    /// The messages of chat `chat`: `[text, history, outcome_label]`, oldest first.
    fn messages(&self, chat: &str) -> Vec<Vec<Value>> {
        self.rows(
            "SELECT m.text, m.history, coalesce(e.outcome_label, '') FROM chan_messages m
               JOIN spaces s ON s.id = m.space_id LEFT JOIN ext_events e ON e.id = m.event_id
              WHERE s.external_id = ? ORDER BY m.created, m.rowid",
            &[chat],
        )
    }

    fn status(&self, id: &str) -> String {
        self.rows("SELECT status FROM connections WHERE id = ?", &[id])[0][0]
            .as_str()
            .unwrap_or_default()
            .to_string()
    }

    fn cursor(&self, id: &str) -> Option<Value> {
        self.rows(
            "SELECT token FROM chan_cursors WHERE connection_id = ? AND container = ?",
            &[id, ext_ws::CURSOR],
        )
        .first()
        .and_then(|r| r[0].as_str().and_then(|t| serde_json::from_str(t).ok()))
    }

    /// The fake's subscriptions so far.
    fn subscribes(&self) -> Vec<Value> {
        self.fake.calls("changes.subscribe")
    }
}

async fn until(what: &str, mut f: impl FnMut() -> bool) {
    let t0 = std::time::Instant::now();
    while !f() {
        assert!(t0.elapsed() < T, "timed out: {what}");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

fn msg(h: &H, chat: &str, source: &str, from: &str, text: &str, dt: i64) -> Value {
    fake::message(chat, source, from, text, h.t0 + dt)
}

// ---------------------------------------------------------------------------------------------
// Connecting.

#[tokio::test(flavor = "multi_thread")]
async fn connecting_checks_the_url_says_hello_and_claims_who_is_signed_in() {
    let h = setup().await;
    let id = h.connect().await;
    // hello first, then me.get (§11.2)
    let log: Vec<String> = h
        .fake
        .with(|w| w.log.iter().map(|(m, _)| m.clone()).collect());
    assert_eq!(&log[..2], ["hello", "me.get"]);
    let hello = &h.fake.calls("hello")[0];
    assert_eq!(hello["protocol"], json!({"min": 1, "max": 1}));
    assert_eq!(hello["client"]["name"], "steve-hub");
    let r = h.rows(
        "SELECT a.handle, c.config FROM connections c JOIN accounts a ON a.id = c.account_id WHERE c.id = ?",
        &[&id],
    );
    assert_eq!(r[0][0], format!("{}:id:{}", fake::REALM, fake::ME));
    let config: Value = serde_json::from_str(r[0][1].as_str().unwrap()).unwrap();
    assert_eq!(config["realm"], fake::REALM);
    assert_eq!(config["me"], json!({"kind": "id", "value": fake::ME}));
    assert_eq!(config["url"], h.fake.url());
    assert!(
        !r[0][1].as_str().unwrap().contains(fake::TOKEN),
        "the token is never in config"
    );
    // the email alias is claimed into 我 with the primary handle
    let mail = h.rows(
        "SELECT count(*) FROM accounts a JOIN parties p ON p.id = a.party_id
          WHERE a.system_id = 'mail' AND a.handle = 'ada@example.com' AND p.user_id = ?",
        &[ME],
    );
    assert_eq!(mail[0][0], 1, "{mail:?}");

    // V2 at the form: a URL that is refused never dials
    let upgrades = h.fake.with(|w| w.upgrades);
    for bad in [
        "ws://localhost:7788/".to_string(),
        "ws://192.168.1.5/".into(),
        "wss://evil#@127.0.0.1/".into(),
        format!("{}?token=x", h.fake.url()),
        "https://a.example/".into(),
    ] {
        let e = h
            .conns
            .create(
                &caller(WS, ME),
                ext_ws::ADAPTER,
                &fields(&bad, fake::TOKEN),
                json!({}),
            )
            .await;
        assert!(e.is_err(), "{bad} accepted");
    }
    assert_eq!(h.fake.with(|w| w.upgrades), upgrades);
    // a token of the wrong shape is refused before a dial; a wrong one at the upgrade (401)
    assert!(
        h.conns
            .create(
                &caller(WS, ME),
                ext_ws::ADAPTER,
                &fields(&h.fake.url(), "short"),
                json!({})
            )
            .await
            .is_err()
    );
    let e = h
        .conns
        .create(
            &caller(WS, ME),
            ext_ws::ADAPTER,
            &fields(&h.fake.url(), "a-wrong-token-0123456"),
            json!({}),
        )
        .await;
    assert!(e.is_err());
    assert_eq!(h.fake.with(|w| w.refused_upgrades), 1);
}

/// V1: no common version, or a required feature missing, refuses the connection.
#[tokio::test(flavor = "multi_thread")]
async fn v1_a_service_without_a_common_version_or_a_required_feature_is_refused() {
    let h = setup().await;
    h.fake.with(|w| w.versions = vec![2]);
    let e = h
        .conns
        .create(
            &caller(WS, ME),
            ext_ws::ADAPTER,
            &fields(&h.fake.url(), fake::TOKEN),
            json!({}),
        )
        .await
        .expect_err("refused");
    assert!(format!("{e:?}").contains("协议版本"), "{e:?}");
    h.fake.with(|w| {
        w.versions = vec![1, 2];
        w.features.retain(|f| f != "messages.history");
    });
    let e = h
        .conns
        .create(
            &caller(WS, ME),
            ext_ws::ADAPTER,
            &fields(&h.fake.url(), fake::TOKEN),
            json!({}),
        )
        .await
        .expect_err("refused");
    assert!(format!("{e:?}").contains("messages.history"), "{e:?}");
}

// ---------------------------------------------------------------------------------------------
// The feed.

/// A live message is stored and acted on; the cursor is saved before the ack (§5.3); V4: a
/// message from before the watermark is only recorded.
#[tokio::test(flavor = "multi_thread")]
async fn the_feed_stores_then_saves_the_cursor_then_acks() {
    let h = setup().await;
    let id = h.connect().await;
    let (rt, _tx) = h.runtime();
    until("subscribed", || h.subscribes().len() == 1).await;
    assert_eq!(h.subscribes()[0]["from"], "now");
    until("connection healthy", || h.status(&id) == "ok").await;

    let seq = h
        .fake
        .push_message(msg(&h, "c_42", "ev_1", "u_7", "hello there", 1));
    until("acked", || h.fake.acked() == seq).await;
    let m = h.messages("c_42");
    assert_eq!(m.len(), 1, "{m:?}");
    assert_eq!(m[0][0], "hello there");
    assert_eq!(m[0][1], Value::Null, "a live message");
    // the body was fetched with messages.get (the item has metadata only)
    assert_eq!(
        h.fake.calls("messages.get")[0]["items"][0],
        json!({"chat_id": "c_42", "source_id": "ev_1", "id": "m_ev_1"})
    );
    assert_eq!(h.cursor(&id).expect("cursor")["cursor"], format!("c:{seq}"));
    // the space got its name from the message
    let name = h.rows(
        "SELECT name, kind FROM spaces WHERE external_id = 'c_42'",
        &[],
    );
    assert_eq!(name[0], vec![json!("Chat c_42"), json!("group")]);

    // V4: written before the subscription's watermark → only recorded
    let old = msg(&h, "c_42", "ev_0", "u_7", "from before", -10_000);
    let seq = h.fake.push_message(old);
    until("acked", || h.fake.acked() == seq).await;
    let m = h.messages("c_42");
    assert_eq!(m[0][0], "from before");
    assert_eq!(m[0][1], "backfill");
    assert_eq!(m[0][2], LABEL_HISTORY_BACKFILL);
    rt.shutdown().await;
}

/// V5: a redelivered batch (its ack was lost) is stored once and acked again.
#[tokio::test(flavor = "multi_thread")]
async fn v5_a_redelivery_is_stored_once_and_acked() {
    let h = setup().await;
    h.connect().await;
    let (rt, _tx) = h.runtime();
    until("subscribed", || h.subscribes().len() == 1).await;
    h.fake.with(|w| w.lose_acks = true);
    let seq = h
        .fake
        .push_message(msg(&h, "c_42", "ev_1", "u_7", "once", 1));
    until("acked once", || h.fake.calls("changes.ack").len() == 1).await;
    assert_eq!(h.fake.acked(), 0, "the ack was lost");
    h.fake.with(|w| w.lose_acks = false);
    h.fake.redeliver();
    until("acked again", || h.fake.acked() == seq).await;
    assert_eq!(h.fake.calls("changes.ack").len(), 2);
    assert_eq!(h.messages("c_42").len(), 1);
    rt.shutdown().await;
}

/// V6: after a disconnect the feed resumes after the saved cursor; nothing acked comes again.
/// A remote crash costs liveness only.
#[tokio::test(flavor = "multi_thread")]
async fn v6_a_reconnect_resumes_from_the_saved_cursor() {
    let h = setup().await;
    let id = h.connect().await;
    let (rt, _tx) = h.runtime();
    until("subscribed", || h.subscribes().len() == 1).await;
    let seq = h
        .fake
        .push_message(msg(&h, "c_42", "ev_1", "u_7", "one", 1));
    until("acked", || h.fake.acked() == seq).await;
    h.fake.drop_all();
    until("resubscribed", || h.subscribes().len() == 2).await;
    assert_eq!(
        h.subscribes()[1]["from"],
        json!({"cursor": format!("c:{seq}")})
    );
    // what came while it was away arrives after the resume
    let seq2 = h
        .fake
        .push_message(msg(&h, "c_42", "ev_2", "u_7", "two", 2));
    until("acked", || h.fake.acked() == seq2).await;
    let m = h.messages("c_42");
    assert_eq!(m.len(), 2, "{m:?}");
    assert!(m.iter().all(|r| r[1].is_null()));
    assert_eq!(h.status(&id), "ok");
    rt.shutdown().await;
}

/// V7: a cursor the service discarded → subscribe at now, and the time since the cursor was
/// saved is read back from history for every chat, only recorded.
#[tokio::test(flavor = "multi_thread")]
async fn v7_a_cursor_too_old_is_repaired_from_history_as_backfill() {
    let h = setup().await;
    let id = h.connect().await;
    let (rt, _tx) = h.runtime();
    until("subscribed", || h.subscribes().len() == 1).await;
    let seq = h
        .fake
        .push_message(msg(&h, "c_42", "ev_1", "u_7", "seen", 1));
    until("acked", || h.fake.acked() == seq).await;
    let at = h.cursor(&id).unwrap()["at"].as_i64().unwrap();
    // while away: one message the feed lost, and the cursor discarded
    h.fake.with(|w| {
        w.too_old.insert(format!("c:{seq}"));
        w.now = Some(h.t0 + 60_000);
    });
    h.fake
        .add_message(msg(&h, "c_42", "ev_lost", "u_7", "lost while away", 30_000));
    h.fake.drop_all();
    until("history read", || {
        !h.fake.calls("messages.history").is_empty()
    })
    .await;
    let subs = h.subscribes();
    assert_eq!(subs[1]["from"], json!({"cursor": format!("c:{seq}")}));
    assert_eq!(subs[2]["from"], "now");
    let read = &h.fake.calls("messages.history")[0];
    assert_eq!(read["chat_id"], "c_42");
    assert_eq!(read["from_ms"], at);
    assert_eq!(read["to_ms"], h.t0 + 60_000);
    until("backfilled", || h.messages("c_42").len() == 2).await;
    let m = h.messages("c_42");
    assert_eq!(m[1][0], "lost while away");
    assert_eq!(m[1][1], "backfill");
    assert_eq!(
        m[0][1],
        Value::Null,
        "the one seen before is not duplicated"
    );
    rt.shutdown().await;
}

/// V8: a gap item is repaired by reading history; what is found is only recorded, and a message
/// the Hub has already is not duplicated.
#[tokio::test(flavor = "multi_thread")]
async fn v8_a_gap_is_read_back_and_recorded_only() {
    let h = setup().await;
    h.connect().await;
    let (rt, _tx) = h.runtime();
    until("subscribed", || h.subscribes().len() == 1).await;
    let seq = h
        .fake
        .push_message(msg(&h, "c_42", "ev_1", "u_7", "known", 150));
    until("acked", || h.fake.acked() == seq).await;
    h.fake
        .add_message(msg(&h, "c_42", "ev_2", "u_8", "in the gap", 160));
    let seq = h.fake.push(vec![json!({"type": "gap", "chats": ["c_42"],
        "from_ms": h.t0 + 100, "to_ms": h.t0 + 200, "reason": "upstream reconnect"})]);
    until("acked", || h.fake.acked() == seq).await;
    let read = &h.fake.calls("messages.history")[0];
    assert_eq!(
        (
            read["chat_id"].clone(),
            read["from_ms"].clone(),
            read["to_ms"].clone(),
            read["limit"].clone()
        ),
        (
            json!("c_42"),
            json!(h.t0 + 100),
            json!(h.t0 + 200),
            json!(50)
        )
    );
    let m = h.messages("c_42");
    assert_eq!(m.len(), 2, "{m:?}");
    assert_eq!(m[0][1], Value::Null);
    assert_eq!(
        (m[1][0].clone(), m[1][1].clone()),
        (json!("in the gap"), json!("backfill"))
    );

    // a window the service could not see whole is noted for the next report
    h.fake.with(|w| w.incomplete = true);
    let seq = h.fake.push(vec![json!({"type": "gap", "chats": ["*"],
        "from_ms": h.t0 + 100, "to_ms": h.t0 + 200})]);
    until("acked", || h.fake.acked() == seq).await;
    let notes = h.rows(
        "SELECT text FROM digest_entries WHERE workspace_id = ? AND text LIKE '%可能不全%'",
        &[WS],
    );
    assert_eq!(notes.len(), 1, "{notes:?}");
    rt.shutdown().await;
}

/// V9: your own message (`is_mine`, or a handle claimed as you) is stored as yours and never
/// answered.
#[tokio::test(flavor = "multi_thread")]
async fn v9_own_messages_are_recorded_never_answered() {
    let h = setup().await;
    h.connect().await;
    let (rt, _tx) = h.runtime();
    until("subscribed", || h.subscribes().len() == 1).await;
    let mut mine = msg(&h, "c_42", "ev_1", "u_somebody", "@Ada note to self", 1);
    mine["is_mine"] = true.into();
    mine["mentions"] =
        json!([{"user": {"kind": "id", "value": fake::ME}, "name": "Ada", "is_me": true}]);
    let seq = h.fake.push_message(mine);
    until("acked", || h.fake.acked() == seq).await;
    // `is_mine: false`, but the sender's handle is the one claimed as you
    let seq = h
        .fake
        .push_message(msg(&h, "c_42", "ev_2", fake::ME, "also me", 2));
    until("acked", || h.fake.acked() == seq).await;
    let m = h.messages("c_42");
    assert_eq!(m.len(), 2, "{m:?}");
    assert!(m.iter().all(|r| r[2] == LABEL_OWN), "{m:?}");
    // nothing was sent
    assert!(h.fake.with(|w| w.writes.is_empty()));
    rt.shutdown().await;
}

/// V15: a batch over `max_batch_items` is not acked, and the socket is closed.
#[tokio::test(flavor = "multi_thread")]
async fn v15_a_batch_over_the_limit_closes_the_socket_unacked() {
    let h = setup().await;
    h.connect().await;
    let (rt, _tx) = h.runtime();
    until("subscribed", || h.subscribes().len() == 1).await;
    let items: Vec<Value> = (0..101)
        .map(|i| fake::item_of(&msg(&h, "c_1", &format!("e{i}"), "u_7", "x", 1)))
        .collect();
    h.fake.push(items);
    until("reconnected", || h.subscribes().len() == 2).await;
    // never acked; the next subscription starts at now
    assert!(h.fake.calls("changes.ack").is_empty());
    assert_eq!(h.subscribes()[1]["from"], "now");
    rt.shutdown().await;
}

/// V13: an expired session is shown and nothing is written; once it is back, the identity is
/// checked again and the feed resumes from the cursor.
#[tokio::test(flavor = "multi_thread")]
async fn v13_an_expired_session_waits_for_ok_then_resumes() {
    let h = setup().await;
    let id = h.connect().await;
    let (rt, _tx) = h.runtime();
    until("subscribed", || h.subscribes().len() == 1).await;
    until("connection healthy", || h.status(&id) == "ok").await;
    let seq = h
        .fake
        .push_message(msg(&h, "c_42", "ev_1", "u_7", "one", 1));
    until("acked", || h.fake.acked() == seq).await;
    h.fake.notify(
        "session.status",
        json!({"status": "expired", "write_policy": "allowed"}),
    );
    until("shown", || {
        h.rows("SELECT health FROM connections WHERE id = ?", &[&id])[0][0]
            .as_str()
            .is_some_and(|e| e.contains("重新登录"))
    })
    .await;
    // no writes while expired
    let cx = h.cx(&id);
    let e = h
        .ext
        .send(
            &cx,
            &ActTarget::Space {
                ext_id: "c_42".into(),
            },
            &OutContent::text("hi"),
            &SendKey("k_exp".into()),
        )
        .await;
    assert!(
        matches!(e, Err(AdapterError::Provider(ProviderError::Rejected(_)))),
        "{e:?}"
    );
    assert!(h.fake.calls("messages.send").is_empty());
    let me_calls = h.fake.calls("me.get").len();
    h.fake.notify(
        "session.status",
        json!({"status": "ok", "write_policy": "allowed"}),
    );
    until("resubscribed", || h.subscribes().len() == 2).await;
    assert_eq!(h.fake.calls("me.get").len(), me_calls + 1);
    assert_eq!(
        h.subscribes()[1]["from"],
        json!({"cursor": format!("c:{seq}")})
    );
    until("healthy again", || h.status(&id) == "ok").await;
    rt.shutdown().await;
}

/// An upgrade refused with 401 sets the connection to re-authorization; it is not redialled in
/// a loop.
#[tokio::test(flavor = "multi_thread")]
async fn a_revoked_token_needs_reauthorization_without_a_reconnect_loop() {
    let h = setup().await;
    let id = h.connect().await;
    h.fake.with(|w| w.token = "another-token-0123456789".into());
    let (rt, tx) = h.runtime();
    until("reauth", || h.status(&id) == "reauth").await;
    h.publish(&tx);
    tokio::time::sleep(Duration::from_millis(800)).await;
    let refused = h.fake.with(|w| w.refused_upgrades);
    tokio::time::sleep(Duration::from_millis(800)).await;
    assert_eq!(
        h.fake.with(|w| w.refused_upgrades),
        refused,
        "no redial loop"
    );
    assert!(rt.running().is_empty());
    rt.shutdown().await;
}

// ---------------------------------------------------------------------------------------------
// Writes as you.

fn space(chat: &str) -> ActTarget {
    ActTarget::Space {
        ext_id: chat.into(),
    }
}

/// V11: the same idempotency key gives the same result and one message; other params with it
/// are a conflict. A reply names its target; reactions and recall go through.
#[tokio::test(flavor = "multi_thread")]
async fn v11_writes_carry_the_sender_and_an_idempotency_key() {
    let h = setup().await;
    let id = h.connect().await;
    let cx = h.cx(&id);
    let a = h
        .ext
        .send(
            &cx,
            &space("c_42"),
            &OutContent::text("hi"),
            &SendKey("cm_1".into()),
        )
        .await
        .expect("sent");
    let b = h
        .ext
        .send(
            &cx,
            &space("c_42"),
            &OutContent::text("hi"),
            &SendKey("cm_1".into()),
        )
        .await
        .expect("sent again");
    assert_eq!(a, b);
    assert_eq!(a.source_id.as_deref(), Some("out_1"));
    assert_eq!(a.ext_id.as_deref(), Some("m_out_1"));
    assert_eq!(
        h.fake.with(|w| w.writes.len()),
        1,
        "one message at the remote"
    );
    let sent = &h.fake.calls("messages.send")[0];
    assert_eq!(
        sent["expected_sender"],
        json!({"kind": "id", "value": fake::ME})
    );
    assert_eq!(sent["idempotency_key"], "cm_1");
    let e = h
        .ext
        .send(
            &cx,
            &space("c_42"),
            &OutContent::text("other"),
            &SendKey("cm_1".into()),
        )
        .await;
    assert!(
        matches!(e, Err(AdapterError::Provider(ProviderError::Rejected(_)))),
        "{e:?}"
    );

    let to = {
        let mut r = MessageRef::platform("c_42", "m_9");
        r.source_id = Some("ev_9".into());
        r
    };
    let mut out = OutContent::text("re");
    out.at = vec![format!("{}:id:u_7", fake::REALM)];
    h.ext
        .send(
            &cx,
            &ActTarget::Reply {
                to: to.clone(),
                in_thread: true,
            },
            &out,
            &SendKey("cm_2".into()),
        )
        .await
        .expect("replied");
    let reply = &h.fake.calls("messages.reply")[0];
    assert_eq!(reply["to"], json!({"source_id": "ev_9", "id": "m_9"}));
    assert_eq!(reply["in_thread"], true);
    assert_eq!(reply["mentions"], json!([{"kind": "id", "value": "u_7"}]));
    let r = h
        .ext
        .add_reaction(&cx, &to, steve_channels::channel::ReactionKind::Ack)
        .await
        .expect("reacted");
    assert!(r.starts_with("r_"));
    assert_eq!(h.fake.calls("reactions.add")[0]["emoji"], "👀");
    h.ext.remove_reaction(&cx, &to, &r).await.expect("removed");
    h.ext.recall(&cx, &to).await.expect("recalled");
    assert_eq!(h.fake.calls("messages.recall").len(), 1);
}

/// V10: someone else signed in at the service: the write is refused before any side effect, and
/// it is a re-authorization.
#[tokio::test(flavor = "multi_thread")]
async fn v10_a_write_while_someone_else_is_signed_in_is_refused() {
    let h = setup().await;
    let id = h.connect().await;
    h.fake
        .with(|w| w.signed_in = json!({"kind": "id", "value": "u_999"}));
    let cx = h.cx(&id);
    let e = h
        .ext
        .send(
            &cx,
            &space("c_42"),
            &OutContent::text("hi"),
            &SendKey("cm_1".into()),
        )
        .await;
    // the session opened for the write sees the change at me.get already
    assert!(
        matches!(e, Err(AdapterError::Provider(ProviderError::Auth(_)))),
        "{e:?}"
    );
    assert!(h.fake.with(|w| w.writes.is_empty()));
    // over the feed's session the server refuses it with account_changed
    h.fake
        .with(|w| w.signed_in = json!({"kind": "id", "value": fake::ME}));
    let (rt, _tx) = h.runtime();
    until("subscribed", || h.subscribes().len() == 1).await;
    h.fake
        .with(|w| w.signed_in = json!({"kind": "id", "value": "u_999"}));
    let e = h
        .ext
        .send(
            &cx,
            &space("c_42"),
            &OutContent::text("hi"),
            &SendKey("cm_2".into()),
        )
        .await;
    assert!(
        matches!(e, Err(AdapterError::Provider(ProviderError::Auth(_)))),
        "{e:?}"
    );
    assert_eq!(h.fake.calls("messages.send").len(), 1);
    assert!(h.fake.with(|w| w.writes.is_empty()));
    rt.shutdown().await;
}

/// V12: writes are paced at `write_per_sec`; after `rate_limited` the retry waits the hint and
/// keeps the key.
#[tokio::test(flavor = "multi_thread")]
async fn v12_writes_are_paced_and_rate_limits_honoured() {
    let h = setup().await;
    let id = h.connect().await;
    let (rt, _tx) = h.runtime();
    until("subscribed", || h.subscribes().len() == 1).await;
    let cx = h.cx(&id);
    let t0 = std::time::Instant::now();
    for i in 0..10 {
        h.ext
            .send(
                &cx,
                &space("c_42"),
                &OutContent::text("x"),
                &SendKey(format!("cm_{i}")),
            )
            .await
            .expect("sent");
    }
    // a burst of 5, then 5 more at 5/s
    assert!(
        t0.elapsed() >= Duration::from_millis(900),
        "{:?}",
        t0.elapsed()
    );
    h.fake.with(|w| w.rate_limit_next = Some(800));
    let t1 = std::time::Instant::now();
    h.ext
        .send(
            &cx,
            &space("c_42"),
            &OutContent::text("y"),
            &SendKey("cm_rl".into()),
        )
        .await
        .expect("sent after the limit");
    assert!(
        t1.elapsed() >= Duration::from_millis(800),
        "{:?}",
        t1.elapsed()
    );
    let tries: Vec<Value> = h
        .fake
        .calls("messages.send")
        .into_iter()
        .filter(|p| p["idempotency_key"] == "cm_rl")
        .collect();
    assert_eq!(tries.len(), 2);
    rt.shutdown().await;
}

/// V14: with `write_policy: forbidden` a write is refused inside the Hub; the service sees none.
#[tokio::test(flavor = "multi_thread")]
async fn v14_read_only_refuses_writes_locally() {
    let h = setup().await;
    h.fake.with(|w| w.write_policy = "forbidden".into());
    let id = h.connect().await;
    let cx = h.cx(&id);
    let e = h
        .ext
        .send(
            &cx,
            &space("c_42"),
            &OutContent::text("hi"),
            &SendKey("cm_1".into()),
        )
        .await;
    assert!(
        matches!(e, Err(AdapterError::Provider(ProviderError::Rejected(_)))),
        "{e:?}"
    );
    assert!(h.fake.calls("messages.send").is_empty());
}

// ---------------------------------------------------------------------------------------------
// Reads: history for an agent, media, people.

#[tokio::test(flavor = "multi_thread")]
async fn history_media_and_people_are_read_through_the_service() {
    let h = setup().await;
    let id = h.connect().await;
    let cx = h.cx(&id);
    for i in 0..60 {
        h.fake.add_message(msg(
            &h,
            "c_42",
            &format!("e{i}"),
            "u_7",
            &format!("m{i}"),
            i * 10,
        ));
    }
    // the Hub's range includes its end; pages are followed up to the limit
    let got = h
        .ext
        .read(&cx, "c_42", h.t0, h.t0 + 590, 55)
        .await
        .expect("history");
    assert_eq!(got.len(), 55);
    assert_eq!(got[0].text, "m0");
    assert_eq!(got[0].via.as_deref(), Some("history"));
    assert_eq!(got[0].source_id.as_deref(), Some("e0"));
    assert_eq!(
        got[0].sender.handle,
        HandleRef::new(ext_ws::SYSTEM, format!("{}:id:u_7", fake::REALM))
    );
    let reads = h.fake.calls("messages.history");
    assert_eq!(reads.len(), 2);
    assert_eq!(reads[0]["to_ms"], h.t0 + 591);

    h.fake.with(|w| {
        w.media.insert("k_1".into(), b"PNGDATA".to_vec());
    });
    let mut r = MessageRef::platform("c_42", "m_e1");
    r.source_id = Some("e1".into());
    assert_eq!(
        h.ext.download(&cx, &r, "k_1", true, 1024).await,
        Ok(b"PNGDATA".to_vec())
    );
    assert_eq!(
        h.ext.download(&cx, &r, "k_1", true, 3).await,
        Err(MediaError::TooLarge)
    );
    assert_eq!(
        h.ext.download(&cx, &r, "nope", true, 1024).await,
        Err(MediaError::NotFound)
    );

    h.fake.with(|w| {
        w.people.insert(
            "u_7".into(),
            json!({"user": {"kind": "id", "value": "u_7"}, "display": "Grace",
                "email": "grace@example.com", "directory": true}),
        );
    });
    use steve_channels::channel::{ProfileAdapter, ProfileQuery};
    let a = h
        .ext
        .resolve_profiles(
            &cx,
            &ProfileQuery {
                space_ext_id: None,
                handles: vec![HandleRef::new(
                    ext_ws::SYSTEM,
                    format!("{}:id:u_7", fake::REALM),
                )],
                want_tenant: false,
            },
        )
        .await
        .expect("people");
    assert_eq!(a.profiles.len(), 1);
    assert_eq!(a.profiles[0].name, "Grace");
    assert_eq!(a.profiles[0].email.as_deref(), Some("grace@example.com"));
}

// Generic connector extension. Nothing in these tests contacts a real account or platform.
fn connector_manifest() -> Value {
    json!({"version":1,"id":"example","events":["issue.updated"],"operations":[
        {"name":"search","capability":"search","description":"Search documents on this account",
         "inputSchema":{"type":"object","properties":{"query":{"type":"string"}},"required":["query"],"additionalProperties":false},
         "annotations":{"readOnlyHint":true,"steve":{"resources":["document"],"asYou":false}}},
        {"name":"read","capability":"read","description":"Read an original by its Hub reference",
         "inputSchema":{"type":"object","properties":{"reference":{"type":"string"}},"required":["reference"],"additionalProperties":false},
         "annotations":{"readOnlyHint":true,"steve":{"resources":["document"],"asYou":false}},"bindings":{"reference":"reference"}},
        {"name":"update","capability":"write","description":"Update a referenced original",
         "inputSchema":{"type":"object","properties":{"reference":{"type":"string"},"text":{"type":"string"}},"required":["reference","text"],"additionalProperties":false},
         "annotations":{"readOnlyHint":false,"destructiveHint":true,"steve":{"resources":["document"],"asYou":true}},"bindings":{"reference":"reference"}}
    ]})
}

#[tokio::test(flavor = "multi_thread")]
async fn connector_discovery_explicit_read_references_and_unknown_write() {
    let h = setup().await;
    h.fake.with(|w| {
        w.connector = Some(connector_manifest());
        // Generic connectors need neither messaging history nor media support.
        w.features = vec!["me".into(), "changes".into()];
        w.connector_results.insert(
            "search".into(),
            json!({"content":[{"type":"text","text":"One document"}],"references":[{
            "kind":"reference","external_id":"provider-document-1","conversation_id":"project-1",
            "source_message_id":"provider-message-7","original_type":"document","title":"Design",
            "body":"must not persist","reference_id":"forged-by-plugin"}]}),
        );
        w.connector_results.insert(
            "read".into(),
            json!({"content":[{"type":"text","text":"Original fetched on demand"}]}),
        );
    });
    let conn = h.connect().await;
    let who = caller(WS, ME);
    let tools = h.ch.connector_tools(&who).await.unwrap();
    assert_eq!(tools.len(), 3);
    assert!(h.fake.calls("messages.history").is_empty());
    assert!(h.fake.calls("connector.call").is_empty());
    let search = &tools
        .iter()
        .find(|t| t.operation.name == "search")
        .unwrap()
        .name;
    let read = &tools
        .iter()
        .find(|t| t.operation.name == "read")
        .unwrap()
        .name;
    let write = &tools
        .iter()
        .find(|t| t.operation.name == "update")
        .unwrap()
        .name;
    let result =
        h.ch.connector_call(&who, search, json!({"query":"Design"}), "search-1")
            .await
            .unwrap();
    let reference = result["references"][0]["reference_id"].as_str().unwrap();
    assert!(reference.starts_with("ref_"));
    assert_ne!(reference, "forged-by-plugin");
    assert!(result["references"][0].get("external_id").is_none());
    assert!(result["references"][0].get("body").is_none());
    assert!(
        h.rows("SELECT id FROM chan_messages", &[]).is_empty(),
        "search must not import history"
    );
    assert!(
        h.ch.connector_call(
            &who,
            read,
            json!({"reference":"provider-document-1"}),
            "bad-id"
        )
        .await
        .is_err()
    );
    assert!(
        h.ch.connector_call(
            &who,
            read,
            json!({"reference":reference,"unexpected":true}),
            "bad-schema"
        )
        .await
        .is_err()
    );
    let original =
        h.ch.connector_call(&who, read, json!({"reference":reference}), "read-1")
            .await
            .unwrap();
    assert_eq!(original["content"][0]["text"], "Original fetched on demand");
    assert_eq!(
        h.fake.calls("connector.call").last().unwrap()["arguments"]["reference"],
        "provider-document-1"
    );
    h.fake.with(|w| w.lose_connector_response = true);
    let unknown =
        h.ch.connector_call(
            &who,
            write,
            json!({"reference":reference,"text":"updated"}),
            "write-1",
        )
        .await
        .unwrap();
    assert_eq!(unknown["status"], "unknown");
    assert_eq!(unknown["retry"], false);
    assert_eq!(
        h.fake.with(|w| w.writes.len()),
        1,
        "writes must not be replayed"
    );
    h.env
        .store()
        .transaction(|tx| {
            tx.execute(
                "UPDATE connector_refs SET expires=0 WHERE id=?",
                [reference],
            )?;
            Ok::<_, steve_modules::ApiError>(())
        })
        .unwrap();
    assert!(
        h.ch.connector_call(&who, read, json!({"reference":reference}), "expired")
            .await
            .unwrap_err()
            .contains("expired")
    );
    // A second personal account cannot use the first account's Hub-issued reference.
    h.fake
        .with(|w| w.signed_in = json!({"kind":"id","value":"u_other"}));
    let other = h.connect().await;
    assert_ne!(other, conn);
    let name = steve_channels::connector::tool_name(&other, "read");
    assert!(
        h.ch.connector_call(&who, &name, json!({"reference":reference}), "cross-account")
            .await
            .is_err()
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn connector_events_reconnect_dedupe_and_no_automatic_backfill() {
    let h = setup().await;
    h.fake.with(|w| w.connector = Some(connector_manifest()));
    let _id = h.connect().await;
    let (rt, _tx) = h.runtime();
    until("subscribed", || !h.subscribes().is_empty()).await;
    let event = json!({"type":"event","event_type":"issue.updated","event_id":"stable-1",
        "occurred_ms": h.t0+1,"summary":"Issue updated","conversation_id":"project-1","topic_id":"issue-7",
        "space_id":"forged-hub-space","message_id":"forged-hub-message"});
    let seq = h.fake.push(vec![event.clone()]);
    until("acked event", || h.fake.acked() == seq).await;
    h.fake.drop_all();
    until("connector reconnected", || h.subscribes().len() == 2).await;
    assert_eq!(
        h.subscribes()[1]["from"],
        json!({"cursor":format!("c:{seq}")})
    );
    let seq = h.fake.push(vec![
        event,
        json!({"type":"gap","chats":["project-1"],"from_ms":h.t0-1000,"to_ms":h.t0}),
    ]);
    until("acked duplicate and gap", || h.fake.acked() == seq).await;
    let rows = h.rows(
        "SELECT id,payload FROM ext_events WHERE source='connector'",
        &[],
    );
    assert_eq!(rows.len(), 1, "stable event ID deduplicates");
    assert!(!rows[0][1].as_str().unwrap().contains("forged-hub"));
    assert!(h.fake.calls("messages.history").is_empty());
    rt.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn connector_rich_message_keeps_only_reference_metadata() {
    let h = setup().await;
    h.fake.with(|w| w.connector = Some(connector_manifest()));
    let _conn = h.connect().await;
    let (rt, _tx) = h.runtime();
    until("subscribed", || !h.subscribes().is_empty()).await;
    let mut m = msg(&h, "pr-7", "rich-1", "u_7", "Pull request description", 1);
    m["chat_kind"] = json!("pr");
    m["thread_id"] = json!("topic-7");
    m["reply_to"] = json!({"id":"parent-message"});
    m["sender"]["aliases"] = json!([{"kind":"email","value":"dev@example.org"}]);
    m["attachments"] = json!([
        {"type":"document","key":"original-key","title":"Design","size":42,"body":"must not persist","reference_id":"forged"},
        {"type":"webpage","key":"page-key","url":"https://example.org/design","html":"must not persist"}
    ]);
    let seq = h.fake.push_message(m);
    until("rich reference ack", || h.fake.acked() == seq).await;
    let rows = h.rows("SELECT m.id,m.attachments,s.kind,m.thread_key,m.reply_to FROM chan_messages m JOIN spaces s ON s.id=m.space_id WHERE s.external_id='pr-7'",&[]);
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0][2], "pr");
    assert_eq!(rows[0][3], "topic-7");
    assert_eq!(rows[0][4], "parent-message");
    let a: Value = serde_json::from_str(rows[0][1].as_str().unwrap()).unwrap();
    assert!(a[0]["reference_id"].as_str().unwrap().starts_with("ref_"));
    assert!(a[0].get("body").is_none());
    assert!(a[1].get("html").is_none());
    let presented = h
        .env
        .store()
        .reader(|c| {
            Ok::<_, steve_modules::ApiError>(
                steve_channels::media::present(c, rows[0][0].as_str().unwrap()).unwrap(),
            )
        })
        .unwrap();
    assert!(presented.1[0].get("key").is_none());
    assert!(h.fake.calls("media.download").is_empty());
    rt.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn connector_catalog_and_references_do_not_bypass_current_permissions() {
    let h = setup().await;
    h.fake.with(|w| {
        w.connector = Some(connector_manifest());
    });
    let conn = h.connect().await;
    let who = caller(WS, ME);
    let name = steve_channels::connector::tool_name(&conn, "search");
    assert_eq!(h.ch.connector_tools(&who).await.unwrap().len(), 3);
    let unknown_user = caller(WS, "not-the-account-owner");
    assert!(
        h.ch.connector_tools(&unknown_user)
            .await
            .unwrap_or_default()
            .is_empty()
    );
    h.conns
        .patch(&who, &conn, None, Some(vec!["msg.receive".into()]))
        .unwrap();
    assert_eq!(
        h.ch.connector_tools(&who).await.unwrap().len(),
        2,
        "disabled write cap hides write operations"
    );
    h.conns.patch(&who, &conn, Some(true), None).unwrap();
    assert!(
        h.ch.connector_call(&who, &name, json!({"query":"anything"}), "paused")
            .await
            .is_err()
    );
    assert!(h.fake.calls("connector.call").is_empty());
    h.conns.patch(&who, &conn, Some(false), None).unwrap();
    h.fake.with(|w| {
        w.connector.as_mut().unwrap()["operations"] = json!([]);
    });
    assert!(
        h.ch.connector_call(&who, &name, json!({"query":"anything"}), "removed")
            .await
            .unwrap_err()
            .contains("no longer declared")
    );
    assert!(
        h.fake.calls("connector.call").is_empty(),
        "cached connect metadata is not a dispatch grant"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn connector_message_binding_uses_provider_id_or_connection_source_never_hub_fingerprint() {
    let h = setup().await;
    let mut manifest = connector_manifest();
    manifest["operations"].as_array_mut().unwrap().push(json!({
        "name":"read_message","capability":"read","description":"Read one known message",
        "inputSchema":{"type":"object","properties":{"message":{"type":"string"}},"required":["message"],"additionalProperties":false},
        "annotations":{"readOnlyHint":true},"bindings":{"message":"message"}
    }));
    h.fake.with(|w| w.connector = Some(manifest));
    let conn = h.connect().await;
    let (rt, _tx) = h.runtime();
    until("subscribed", || !h.subscribes().is_empty()).await;
    let mut no_id = msg(
        &h,
        "c_42",
        "source-without-platform-id",
        "u_7",
        "No platform ID",
        1,
    );
    no_id.as_object_mut().unwrap().remove("id");
    let seq = h.fake.push_message(no_id);
    until("stored fallback message", || h.fake.acked() == seq).await;
    let seq = h.fake.push_message(msg(
        &h,
        "c_42",
        "source-with-platform-id",
        "u_7",
        "Has platform ID",
        2,
    ));
    until("stored platform message", || h.fake.acked() == seq).await;
    let rows = h.rows("SELECT m.id,m.external_id,x.source_id FROM chan_messages m JOIN chan_message_seen x ON x.message_id=m.id WHERE x.connection_id=? ORDER BY m.created,m.id",&[&conn]);
    assert_eq!(rows.len(), 2);
    let name = steve_channels::connector::tool_name(&conn, "read_message");
    let fallback_hub = rows
        .iter()
        .find(|row| row[2] == "source-without-platform-id")
        .unwrap()[0]
        .as_str()
        .unwrap()
        .to_string();
    for row in rows {
        let hub = row[0].as_str().unwrap();
        let stored = row[1].as_str().unwrap();
        let source = row[2].as_str().unwrap();
        h.ch.connector_call(
            &caller(WS, ME),
            &name,
            json!({"message":hub}),
            "read-message",
        )
        .await
        .unwrap();
        let calls = h.fake.calls("connector.call");
        assert!(
            !calls.last().unwrap()["arguments"]
                .to_string()
                .contains("fp:"),
            "no Hub fingerprint in any connector.call argument"
        );
        let remote = calls.last().unwrap()["arguments"]["message"]
            .as_str()
            .unwrap();
        assert!(
            !remote.starts_with("fp:"),
            "Hub-only content IDs must never reach the provider"
        );
        assert_eq!(
            remote,
            if source == "source-without-platform-id" {
                assert!(stored.starts_with("fp:"));
                source
            } else {
                assert_eq!(stored, "m_source-with-platform-id");
                stored
            }
        );
    }
    // The connection's delivery mapping is required when the platform supplied no message ID.
    h.env
        .store()
        .transaction(|tx| {
            tx.execute(
                "DELETE FROM chan_message_seen WHERE connection_id=? AND message_id=?",
                rusqlite::params![conn, fallback_hub],
            )?;
            Ok::<_, steve_modules::ApiError>(())
        })
        .unwrap();
    let before = h.fake.calls("connector.call").len();
    assert!(
        h.ch.connector_call(
            &caller(WS, ME),
            &name,
            json!({"message":fallback_hub}),
            "missing-source"
        )
        .await
        .is_err()
    );
    assert_eq!(
        h.fake.calls("connector.call").len(),
        before,
        "missing source must be refused before dispatch"
    );
    rt.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn connector_participants_aliases_and_departed_conversation_cannot_reuse_references() {
    let h = setup().await;
    h.fake.with(|w| {
        let mut manifest = connector_manifest();
        manifest["operations"].as_array_mut().unwrap().push(json!({
            "name":"message","description":"Read one message","capability":"read",
            "inputSchema":{"type":"object","properties":{"message":{"type":"string"}},"required":["message"],"additionalProperties":false},
            "annotations":{"readOnlyHint":true},"bindings":{"message":"message"}
        }));
        w.connector = Some(manifest);
        w.connector_results.insert("search".into(), json!({"references":[{
            "kind":"reference","external_id":"doc","conversation_id":"issue-9","original_type":"document"
        }]}));
    });
    let conn = h.connect().await;
    let (rt, _tx) = h.runtime();
    until("subscribed", || !h.subscribes().is_empty()).await;
    let seq = h
        .fake
        .push_message(msg(&h, "issue-9", "message-9", "u_7", "Issue", 1));
    until("message ack", || h.fake.acked() == seq).await;
    let member = json!({"id":{"kind":"id","value":"u_8"},"display":"Contributor",
        "aliases":[{"kind":"email","value":"contributor@example.org"}],"role":"admin"});
    let seq = h.fake.push(vec![json!({"type":"participants","conversation_id":"issue-9","change":"added","members":[member.clone()]})]);
    until("participant ack", || h.fake.acked() == seq).await;
    let found = h.rows("SELECT sm.role FROM space_members sm JOIN spaces s ON s.id=sm.space_id JOIN accounts a ON a.id=sm.account_id JOIN accounts alias ON alias.party_id=a.party_id WHERE s.external_id='issue-9' AND alias.handle='contributor@example.org'", &[]);
    assert_eq!(
        found.len(),
        1,
        "participant alias resolves to the member's party"
    );
    assert_eq!(found[0][0], "admin");
    let seq = h.fake.push(vec![json!({"type":"participants","conversation_id":"issue-9","change":"removed","members":[member]})]);
    until("participant removal ack", || h.fake.acked() == seq).await;
    assert!(h.rows("SELECT sm.account_id FROM space_members sm JOIN accounts a ON a.id=sm.account_id JOIN accounts alias ON alias.party_id=a.party_id WHERE alias.handle='contributor@example.org'", &[]).is_empty());
    let who = caller(WS, ME);
    let search = steve_channels::connector::tool_name(&conn, "search");
    let read = steve_channels::connector::tool_name(&conn, "read");
    let message = steve_channels::connector::tool_name(&conn, "message");
    let result =
        h.ch.connector_call(&who, &search, json!({"query":"doc"}), "search")
            .await
            .unwrap();
    let reference = result["references"][0]["reference_id"].as_str().unwrap();
    h.ch.connector_call(&who, &read, json!({"reference":reference}), "before-leave")
        .await
        .unwrap();
    let rows = h.rows("SELECT m.id,m.space_id FROM chan_messages m JOIN spaces s ON s.id=m.space_id WHERE s.external_id='issue-9'", &[]);
    let (id, space) = (rows[0][0].as_str().unwrap(), rows[0][1].as_str().unwrap());
    h.env
        .store()
        .transaction(|tx| {
            tx.execute(
                "UPDATE space_identities SET left_at=1 WHERE space_id=? AND connection_id=?",
                rusqlite::params![space, conn],
            )?;
            Ok::<_, steve_modules::ApiError>(())
        })
        .unwrap();
    let before = h.fake.calls("connector.call").len();
    for (tool, args) in [
        (&read, json!({"reference":reference})),
        (&message, json!({"message":id})),
    ] {
        assert!(
            h.ch.connector_call(&who, tool, args, "after-leave")
                .await
                .is_err()
        );
    }
    assert_eq!(
        h.fake.calls("connector.call").len(),
        before,
        "first-seen-via is not current membership"
    );
    assert!(h.fake.calls("messages.history").is_empty());
    rt.shutdown().await;
}
