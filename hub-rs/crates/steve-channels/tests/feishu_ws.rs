//! The long-connection client (`adapters::feishu::ws`) against the fake's frame server.

use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

use serde_json::{Value, json};
use steve_channels::adapters::feishu::crypto;
use steve_channels::adapters::feishu::ws::{self, WsError, WsHandler, WsOptions, WsState};
use steve_channels::fake::feishu::{FakeFeishu, WsClientConfig, fixture, ids};
use tokio::sync::mpsc;

/// Forwards payloads; fails the first `fail` events.
struct Recorder {
    tx: mpsc::UnboundedSender<Value>,
    fail: AtomicU32,
}

impl WsHandler for Recorder {
    async fn on_event(&self, payload: Vec<u8>) -> Result<Option<Value>, String> {
        let v: Value = serde_json::from_slice(&payload).map_err(|e| e.to_string())?;
        let _ = self.tx.send(v);
        if self
            .fail
            .try_update(Ordering::SeqCst, Ordering::SeqCst, |n| n.checked_sub(1))
            .is_ok()
        {
            return Err("boom".into());
        }
        Ok(None)
    }
}

fn recorder(fail: u32) -> (Arc<Recorder>, mpsc::UnboundedReceiver<Value>) {
    let (tx, rx) = mpsc::unbounded_channel();
    (
        Arc::new(Recorder {
            tx,
            fail: AtomicU32::new(fail),
        }),
        rx,
    )
}

/// Fast server config: 1 s pings and reconnects, no jitter.
async fn fake() -> FakeFeishu {
    let f = FakeFeishu::start().await.unwrap();
    f.ws_set_client_config(WsClientConfig {
        reconnect_count: -1,
        reconnect_interval: 1,
        reconnect_nonce: 0,
        ping_interval: 1,
    });
    f
}

fn opts(f: &FakeFeishu) -> WsOptions {
    let mut o = WsOptions::new(f.base_url(), ids::APP_ID, ids::APP_SECRET);
    o.pong_grace = Duration::from_millis(300);
    o
}

const T: Duration = Duration::from_secs(5);

async fn next(rx: &mut mpsc::UnboundedReceiver<Value>) -> Value {
    tokio::time::timeout(T, rx.recv())
        .await
        .expect("event in time")
        .expect("channel open")
}

fn connected(s: &WsState) -> bool {
    matches!(s, WsState::Connected { .. })
}

#[tokio::test]
async fn event_is_delivered_and_acked_within_3s() {
    let f = fake().await;
    let (h, mut rx) = recorder(0);
    let c = ws::spawn(opts(&f), h).unwrap();
    let s = c.wait_for(T, connected).await.unwrap();
    let WsState::Connected {
        conn_id,
        service_id,
    } = s
    else {
        unreachable!()
    };
    let info = &f.ws_connections()[0];
    assert_eq!(info.conn_id, conn_id);
    assert_eq!(info.service_id, service_id);

    let env = fixture("message_p2p_text").unwrap();
    let id = f.ws_push_envelope(env.clone());
    assert_eq!(next(&mut rx).await, env);
    let d = f.ws_wait_acked(&id, T).await.expect("acked");
    let ack = d.ack.unwrap();
    assert_eq!(ack.code, 200);
    assert!(ack.latency < Duration::from_secs(3), "{:?}", ack.latency);
    assert!(ack.biz_rt.is_some());
    assert!(!d.late);
    // The ack echoes the frame: same message id, method, service.
    assert_eq!(
        ack.frame.header("message_id"),
        Some(d.frame_message_id.as_str())
    );
    assert_eq!(ack.frame.method, 1);
    assert_eq!(ack.frame.service, service_id);
    assert_eq!(f.ws_deliveries_of(&id).len(), 1);

    // Pinged right away and then every PingInterval; pongs keep the connection alive.
    tokio::time::sleep(Duration::from_millis(2500)).await;
    assert!(f.ws_connections()[0].pings >= 3);
    assert_eq!(f.ws_connects_total(), 1);
    c.stop().await.unwrap();
}

#[tokio::test]
async fn split_payload_is_reassembled_in_any_order() {
    let f = fake().await;
    let (h, mut rx) = recorder(0);
    let c = ws::spawn(opts(&f), h).unwrap();
    c.wait_for(T, connected).await.unwrap();
    for shuffle in [false, true] {
        f.ws_split(4, shuffle);
        let env = fixture("message_post_rich").unwrap();
        let id = f.ws_push_envelope(env.clone());
        assert_eq!(next(&mut rx).await, env);
        let d = f.ws_wait_acked(&id, T).await.unwrap();
        assert_eq!(d.parts, 4);
    }
    c.stop().await.unwrap();
}

#[tokio::test]
async fn reconnects_after_drop_and_after_ping_timeout() {
    let f = fake().await;
    let (h, mut rx) = recorder(0);
    let c = ws::spawn(opts(&f), h).unwrap();
    c.wait_for(T, connected).await.unwrap();

    f.ws_disconnect_all();
    c.wait_for(T, |s| matches!(s, WsState::Reconnecting { .. }))
        .await
        .unwrap();
    c.wait_for(T, connected).await.unwrap();
    assert_eq!(f.ws_connects_total(), 2);
    let id = f.ws_push("im.chat.updated_v1", json!({"chat_id": ids::GROUP_CHAT}));
    assert_eq!(next(&mut rx).await["header"]["event_id"], id);

    // Half-dead connection: no pongs, no frames. Read deadline 2×1 s + 0.3 s.
    f.ws_set_silent(true);
    let deadline = std::time::Instant::now() + Duration::from_secs(6);
    while f.ws_connects_total() < 3 && std::time::Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert_eq!(
        f.ws_connects_total(),
        3,
        "client noticed the silence and redialled"
    );
    f.ws_set_silent(false);
    c.wait_for(T, connected).await.unwrap();
    c.stop().await.unwrap();
}

#[tokio::test]
async fn unacked_or_failed_events_are_redelivered_with_the_same_event_id() {
    let f = fake().await;
    f.ws_set_ack_timeout(Duration::from_millis(300));
    f.ws_set_retry_schedule(vec![Duration::from_millis(100); 3]);
    let (h, mut rx) = recorder(1);
    let c = ws::spawn(opts(&f), h).unwrap();
    c.wait_for(T, connected).await.unwrap();

    // Handler error → ack 500 → redelivered.
    let id = f.ws_push("im.chat.updated_v1", json!({}));
    assert_eq!(next(&mut rx).await["header"]["event_id"], id);
    assert_eq!(next(&mut rx).await["header"]["event_id"], id);
    f.ws_wait_acked(&id, T).await.unwrap();
    let codes: Vec<i64> = f
        .ws_deliveries_of(&id)
        .iter()
        .map(|d| d.ack.as_ref().unwrap().code)
        .collect();
    assert_eq!(codes, [500, 200]);

    // Lost ack → redelivered after the ack timeout.
    f.ws_ignore_acks(1);
    let id = f.ws_push("im.chat.updated_v1", json!({}));
    assert_eq!(next(&mut rx).await["header"]["event_id"], id);
    assert_eq!(next(&mut rx).await["header"]["event_id"], id);
    f.ws_wait_acked(&id, T).await.unwrap();
    assert_eq!(f.ws_deliveries_of(&id).len(), 2);

    // Explicit duplicate push.
    assert!(f.ws_redeliver(&id));
    assert_eq!(next(&mut rx).await["header"]["event_id"], id);
    c.stop().await.unwrap();
}

#[tokio::test]
async fn cluster_mode_delivers_each_event_to_exactly_one_connection() {
    let f = fake().await;
    let (h1, mut rx1) = recorder(0);
    let (h2, mut rx2) = recorder(0);
    let c1 = ws::spawn(opts(&f), h1).unwrap();
    let c2 = ws::spawn(opts(&f), h2).unwrap();
    assert!(f.ws_wait_connections(2, T).await);

    // What a health check sees (lark-cli's remote_preflight).
    let http = reqwest::Client::new();
    let tok: Value = http
        .post(format!(
            "{}/open-apis/auth/v3/tenant_access_token/internal",
            f.base_url()
        ))
        .json(&json!({"app_id": ids::APP_ID, "app_secret": ids::APP_SECRET}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let cnt: Value = http
        .get(format!("{}/open-apis/event/v1/connection", f.base_url()))
        .bearer_auth(tok["tenant_access_token"].as_str().unwrap())
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(cnt["data"]["online_instance_cnt"], 2);

    let ids: Vec<String> = (0..20)
        .map(|_| f.ws_push("im.chat.updated_v1", json!({})))
        .collect();
    for id in &ids {
        f.ws_wait_acked(id, T).await.unwrap();
        assert_eq!(f.ws_deliveries_of(id).len(), 1);
    }
    let (mut n1, mut n2) = (0, 0);
    while rx1.try_recv().is_ok() {
        n1 += 1;
    }
    while rx2.try_recv().is_ok() {
        n2 += 1;
    }
    assert_eq!(n1 + n2, 20);
    assert!(n1 > 0 && n2 > 0, "random spread, got {n1}/{n2}");
    c1.stop().await.unwrap();
    c2.stop().await.unwrap();
}

#[tokio::test]
async fn connection_limit_and_bad_credentials_are_not_retried() {
    let f = fake().await;
    f.ws_set_max_connections(1);
    let (h, _rx) = recorder(0);
    let c1 = ws::spawn(opts(&f), h.clone()).unwrap();
    c1.wait_for(T, connected).await.unwrap();
    let c2 = ws::spawn(opts(&f), h.clone()).unwrap();
    let err = tokio::time::timeout(T, c2.join())
        .await
        .unwrap()
        .unwrap_err();
    assert!(matches!(err, WsError::Client { code: 514, .. }), "{err:?}");

    let mut bad = opts(&f);
    bad.app_secret = "wrong".into();
    let c3 = ws::spawn(bad, h).unwrap();
    let err = tokio::time::timeout(T, c3.join())
        .await
        .unwrap()
        .unwrap_err();
    assert!(matches!(err, WsError::Client { .. }), "{err:?}");
    assert_eq!(f.ws_connects_total(), 1);
    c1.stop().await.unwrap();
}

#[tokio::test]
async fn busy_endpoint_and_refused_handshake_are_retried() {
    let f = fake().await;
    f.ws_fail_next_endpoint(1, "system busy");
    f.ws_refuse_next_handshake(514, "auth failed", None);
    let (h, _rx) = recorder(0);
    // The first failure happens before any server config arrived: the local defaults apply.
    let mut o = opts(&f);
    o.initial_config.reconnect_nonce = Duration::ZERO;
    o.initial_config.reconnect_interval = Duration::from_millis(200);
    let c = ws::spawn(o, h).unwrap();
    c.wait_for(T, connected).await.unwrap();
    assert_eq!(f.ws_connects_total(), 1);
    c.stop().await.unwrap();
}

#[tokio::test]
async fn reconnect_count_is_honoured() {
    let f = fake().await;
    f.ws_set_client_config(WsClientConfig {
        reconnect_count: 2,
        reconnect_interval: 0,
        reconnect_nonce: 0,
        ping_interval: 1,
    });
    let (h, _rx) = recorder(0);
    let c = ws::spawn(opts(&f), h).unwrap();
    c.wait_for(T, connected).await.unwrap();
    for _ in 0..3 {
        f.ws_refuse_next_handshake(500, "down", None);
    }
    f.ws_disconnect_all();
    let err = tokio::time::timeout(T, c.join())
        .await
        .unwrap()
        .unwrap_err();
    assert!(matches!(err, WsError::ReconnectExhausted(2)), "{err:?}");
}

#[tokio::test]
async fn non_self_built_app_cannot_connect() {
    let f = fake().await;
    f.world(|w| w.app.self_built = false);
    let (h, _rx) = recorder(0);
    let c = ws::spawn(opts(&f), h).unwrap();
    let err = tokio::time::timeout(T, c.join())
        .await
        .unwrap()
        .unwrap_err();
    assert!(matches!(err, WsError::Client { code: 403, .. }), "{err:?}");
}

#[tokio::test]
async fn encrypted_event_bodies_decrypt_with_the_library() {
    let f = fake().await;
    f.ws_encrypt_events(true);
    let (h, mut rx) = recorder(0);
    let c = ws::spawn(opts(&f), h).unwrap();
    c.wait_for(T, connected).await.unwrap();
    let env = fixture("message_group_at_bot").unwrap();
    f.ws_push_envelope(env.clone());
    let got = next(&mut rx).await;
    let plain = crypto::decrypt(ids::ENCRYPT_KEY, got["encrypt"].as_str().unwrap()).unwrap();
    assert_eq!(serde_json::from_slice::<Value>(&plain).unwrap(), env);
    c.stop().await.unwrap();
}
