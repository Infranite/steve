//! One JSON envelope per WebSocket text message, and the size limits checked around it.

use std::pin::Pin;

use futures_util::{Sink, Stream};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use steve_node_api::ApiError;

// The permanent frame header is shared; ordinary payloads still require exact PROTOCOL equality.
pub use steve_node_api::upgrade_v1::{
    BULK_CAP, BULK_FRAME, CHUNK_BYTES, CONTROL_CAP, Frame as Envelope, Kind, MAX_FRAME,
};
/// Event batches use the existing, smaller transport scheduling limit.
pub const EVENTS_FRAME: usize = 64 * 1024;

/// A result as it travels: `{"ok":T}` or `{"err":ApiError}`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    Ok(Value),
    Err(ApiError),
}

/// What a link carries, independent of the WebSocket library (the Node uses tungstenite, the Hub
/// axum's upgrade).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Frame {
    Text(String),
    Close(u16, String),
}

pub type FrameSink = Pin<Box<dyn Sink<Frame, Error = std::io::Error> + Send>>;
pub type FrameStream = Pin<Box<dyn Stream<Item = std::io::Result<Frame>> + Send>>;

#[cfg(test)]
mod tests {
    use super::*;
    use steve_node_api::upgrade_v1::Code as ErrorCode;

    #[test]
    fn round_trip_and_caps() {
        let e = Envelope {
            v: crate::WIRE,
            t: Kind::Req,
            id: "r_1".into(),
            re: None,
            m: "turn.start".into(),
            generation: 7,
            trace: None,
            deadline_at: 0,
            p: serde_json::json!({"x": 1}),
        };
        let s = e.encode().unwrap();
        assert_eq!(Envelope::decode(&s).unwrap(), e);
        // an unknown kind decodes and is answered, it does not break the link
        let u = Envelope::decode(r#"{"v":1,"t":"stream","id":"x"}"#).unwrap();
        assert_eq!(u.t, Kind::Unknown);
        assert!(Envelope::decode("{not json").is_none());
        assert!(Envelope::decode(&"x".repeat(MAX_FRAME + 1)).is_none());
        let big = Envelope::ctl("c", "hello", 0, Value::String("y".repeat(CONTROL_CAP)));
        assert_eq!(big.encode().unwrap_err().code, ErrorCode::Exhausted);
        let mut raw = big.clone();
        raw.t = Kind::Req;
        let text = raw.encode().unwrap().replace("\"req\"", "\"ctl\"");
        assert!(
            Envelope::decode(&text).is_none(),
            "an oversize control frame is refused"
        );
        let mut huge = e.clone();
        huge.p = Value::String("z".repeat(MAX_FRAME));
        let err = huge.encode().unwrap_err();
        assert_eq!(err.code, ErrorCode::Exhausted);
        assert!(
            err.detail.starts_with("request turn.start is "),
            "{}",
            err.detail
        );
        // an answer carries no method: the error still names what was too big
        huge.t = Kind::Res;
        huge.m = String::new();
        huge.re = Some("n_4".into());
        let err = huge.encode().unwrap_err();
        assert!(
            err.detail.starts_with("the answer to request n_4 is "),
            "{}",
            err.detail
        );
        assert_eq!(
            big.encode().unwrap_err().detail.split(" is ").next(),
            Some("control message hello")
        );
        let ok = serde_json::to_string(&Outcome::Ok(serde_json::json!(1))).unwrap();
        assert_eq!(ok, r#"{"ok":1}"#);
    }
}
