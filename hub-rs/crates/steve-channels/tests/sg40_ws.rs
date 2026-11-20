//! SG40: transport admission, ordered data/ACK and cancellation through public WS APIs.
use serde_json::{Value, json};
use std::{sync::Arc, time::Duration};
use steve_channels::adapters::feishu::ws::{self, WsError, WsHandler, WsOptions, WsState};
use steve_channels::fake::feishu::{FakeFeishu, WsClientConfig, ids};
use tokio::sync::mpsc;
const T: Duration = Duration::from_secs(5);
struct Handler(mpsc::UnboundedSender<Value>);
impl WsHandler for Handler {
    async fn on_event(&self, payload: Vec<u8>) -> Result<Option<Value>, String> {
        let v: Value = serde_json::from_slice(&payload).unwrap();
        self.0.send(v.clone()).unwrap();
        if v["event"]["fail"] == true {
            Err("scripted refusal".into())
        } else {
            Ok(Some(json!({"accepted": true})))
        }
    }
}
fn opts(f: &FakeFeishu) -> WsOptions {
    let mut o = WsOptions::new(f.base_url(), ids::APP_ID, ids::APP_SECRET);
    o.initial_config.reconnect_nonce = Duration::ZERO;
    o.initial_config.reconnect_interval = Duration::ZERO;
    o
}
#[tokio::test]
async fn sg40_ws_split_events_keep_arrival_order_ack_codes_and_handler_data() {
    let f = FakeFeishu::start().await.unwrap();
    f.ws_set_retry_schedule(vec![]);
    f.ws_set_client_config(WsClientConfig {
        reconnect_count: 2,
        reconnect_interval: 0,
        reconnect_nonce: 0,
        ping_interval: 10,
    });
    let (tx, mut rx) = mpsc::unbounded_channel();
    let c = ws::spawn(opts(&f), Arc::new(Handler(tx))).unwrap();
    c.wait_for(T, |s| matches!(s, WsState::Connected { .. }))
        .await
        .unwrap();
    for (n, fail) in [(1, false), (2, true), (3, false)] {
        f.ws_split(3, true);
        let id = f.ws_push("im.chat.updated_v1", json!({"n": n, "fail": fail}));
        let event = tokio::time::timeout(T, rx.recv()).await.unwrap().unwrap();
        assert_eq!(event["event"]["n"], n);
        steve_runtime::testkit::eventually_sync("the handler result is acknowledged", || {
            f.ws_deliveries_of(&id).iter().any(|d| d.ack.is_some())
        })
        .await;
        let d = f.ws_deliveries_of(&id).pop().unwrap();
        let ack = d.ack.unwrap();
        assert_eq!(ack.code, if fail { 500 } else { 200 });
        assert_eq!(d.parts, 3);
        assert!(ack.biz_rt.is_some());
        assert_eq!(ack.data, (!fail).then(|| json!({"accepted": true})));
    }
    c.stop().await.unwrap();
}
#[tokio::test]
async fn sg40_ws_retryable_handshake_reconnects_but_client_refusal_stops() {
    let f = FakeFeishu::start().await.unwrap();
    f.ws_set_client_config(WsClientConfig {
        reconnect_count: 2,
        reconnect_interval: 0,
        reconnect_nonce: 0,
        ping_interval: 10,
    });
    f.ws_fail_next_endpoint(1, "busy");
    f.ws_refuse_next_handshake(514, "retryable", None);
    let (tx, _rx) = mpsc::unbounded_channel();
    let c = ws::spawn(opts(&f), Arc::new(Handler(tx))).unwrap();
    c.wait_for(T, |s| matches!(s, WsState::Connected { .. }))
        .await
        .unwrap();
    assert_eq!(f.ws_connects_total(), 1);
    f.ws_refuse_next_handshake(403, "forbidden", None);
    f.ws_disconnect_all();
    let e = tokio::time::timeout(T, c.join())
        .await
        .unwrap()
        .unwrap_err();
    assert!(matches!(e, WsError::Client { code: 403, .. }));
    assert_eq!(f.ws_connects_total(), 1);
}
#[tokio::test]
async fn sg40_ws_endpoint_rejects_empty_credentials_and_preserves_server_error_kind() {
    let f = FakeFeishu::start().await.unwrap();
    let http = reqwest::Client::new();
    assert!(matches!(
        ws::fetch_endpoint(&http, f.base_url(), ids::APP_ID, "").await,
        Err(WsError::Client { code: 0, .. })
    ));
    f.ws_fail_next_endpoint(1, "busy");
    let e = ws::fetch_endpoint(&http, f.base_url(), ids::APP_ID, ids::APP_SECRET)
        .await
        .unwrap_err();
    assert!(e.retryable());
    assert!(matches!(e, WsError::Server { code: 1, .. }));
    let ep = ws::fetch_endpoint(&http, f.base_url(), ids::APP_ID, ids::APP_SECRET)
        .await
        .unwrap();
    assert!(ep.url.contains("service_id="));
    assert!(ep.config.is_some());
}
