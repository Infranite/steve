//! SG45: reader dispatch, generation fences and malformed transport closure.
use async_trait::async_trait;
use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use std::sync::{Arc, Mutex};
use steve_node_api::{ApiResult, PROTOCOL};
use steve_node_wire::{
    Conn, ConnOptions, Handler, close,
    envelope::{Envelope, Frame, Kind},
    testkit::pair,
};
#[derive(Default)]
struct Echo(Mutex<Vec<String>>);
#[async_trait]
impl Handler for Echo {
    async fn handle(&self, method: &str, p: Value) -> ApiResult<Value> {
        self.0.lock().unwrap().push(method.into());
        Ok(p)
    }
}
fn request(generation: u64) -> String {
    Envelope {
        v: PROTOCOL,
        t: Kind::Req,
        id: format!("r{generation}"),
        re: None,
        m: "probe".into(),
        generation,
        trace: None,
        deadline_at: 0,
        p: json!({"value":7}),
    }
    .encode()
    .unwrap()
}
#[tokio::test]
async fn sg45_reader_refuses_stale_requests_without_dispatch_then_answers_current_generation() {
    let (a, mut b) = pair();
    let echo = Arc::new(Echo::default());
    let c = Conn::spawn(a.sink, a.stream, 7, echo.clone(), ConnOptions::default());
    for generation in [6, 7] {
        b.sink.send(Frame::Text(request(generation))).await.unwrap();
        let reply = loop {
            if let Frame::Text(t) = b.stream.next().await.unwrap().unwrap() {
                let e = Envelope::decode(&t).unwrap();
                if e.t == Kind::Res {
                    break e;
                }
            }
        };
        assert_eq!(reply.re.as_deref(), Some(format!("r{generation}").as_str()));
        if generation == 6 {
            assert_eq!(reply.p["err"]["code"], "stale");
        } else {
            assert_eq!(reply.p["ok"], json!({"value":7}));
        }
    }
    assert_eq!(*echo.0.lock().unwrap(), ["probe"]);
    c.close(None, close::GOODBYE, "done");
}
#[tokio::test]
async fn sg45_reader_closes_malformed_and_sanitizes_peer_close_text() {
    for (frame, expected, reason) in [
        (
            Frame::Text("{broken".into()),
            close::MALFORMED,
            "malformed envelope",
        ),
        (
            Frame::Close(4000, "hello\u{1b}[31m\nworld".into()),
            4000,
            "hello [31m world",
        ),
    ] {
        let (a, mut b) = pair();
        let c = Conn::spawn(
            a.sink,
            a.stream,
            7,
            Arc::new(Echo::default()),
            ConnOptions::default(),
        );
        b.sink.send(frame).await.unwrap();
        let got = c.closed().await;
        assert_eq!(got.0, expected);
        assert_eq!(got.1, reason);
    }
}
