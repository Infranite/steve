//! One JSON envelope per WebSocket text message, and the size limits checked around it.

use std::pin::Pin;

use futures_util::{Sink, Stream};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use steve_node_api::{ApiError, ApiResult, ErrorCode};

/// Largest message the socket accepts at all (tungstenite refuses bigger ones before parsing).
pub const MAX_FRAME: usize = 288 * 1024;
/// Largest control envelope (handshake, heartbeat, fencing).
pub const CONTROL_CAP: usize = 64 * 1024;
/// Largest payload of an event batch or a replay page (a single event stays within it); batches are
/// cut much smaller ([`EVENTS_FRAME`]), every ack still covering a contiguous watermark.
pub const BULK_CAP: usize = 256 * 1024;
/// Largest frame of a blob or file chunk. A frame being written cannot be overtaken, and the writer
/// takes the next one while the socket still sends the last: a permission answer behind file
/// chunks waits for about two of them, some 70 ms at 10 Mbit/s (with 64 KiB frames, 120 ms).
pub const BULK_FRAME: usize = 48 * 1024;
/// The size [`crate::RemoteSink`] cuts event batches at (events go before chunks). A single bigger
/// event still goes, alone.
pub const EVENTS_FRAME: usize = 64 * 1024;
/// The most raw bytes of one blob or file chunk: hex doubles them, the envelope adds a little.
pub const CHUNK_BYTES: u32 = (BULK_FRAME / 2) as u32 - 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    Req,
    Res,
    Ctl,
    /// Anything newer: answered `unsupported`, the link stays up.
    #[serde(other)]
    Unknown,
}

/// `{"v":[1,0],"t":"req","id":"…","m":"turn.start","gen":7,"p":{…}}`. No node id: after the
/// handshake the link is the identity.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Envelope {
    pub v: (u16, u16),
    pub t: Kind,
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub re: Option<String>,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub m: String,
    #[serde(default, rename = "gen")]
    pub generation: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trace: Option<String>,
    /// Informational in N2: the receiver keeps its own clock (a command's `accept_before`).
    #[serde(default)]
    pub deadline_at: i64,
    #[serde(default)]
    pub p: Value,
}

impl Envelope {
    pub fn ctl(id: impl Into<String>, m: &str, generation: u64, p: Value) -> Envelope {
        Envelope {
            v: crate::WIRE,
            t: Kind::Ctl,
            id: id.into(),
            re: None,
            m: m.into(),
            generation,
            trace: None,
            deadline_at: 0,
            p,
        }
    }

    pub fn encode(&self) -> ApiResult<String> {
        let s = serde_json::to_string(self)
            .map_err(|e| ApiError::new(ErrorCode::Internal, format!("encode: {e}")))?;
        let cap = if self.t == Kind::Ctl {
            CONTROL_CAP
        } else {
            MAX_FRAME
        };
        if s.len() > cap {
            return Err(ApiError::new(
                ErrorCode::Exhausted,
                format!(
                    "{} is {} bytes, over the {cap} limit",
                    self.subject(),
                    s.len()
                ),
            ));
        }
        Ok(s)
    }

    /// What a size error names: the method, or for an answer (which carries none) what it is.
    fn subject(&self) -> String {
        match (self.t, self.m.is_empty()) {
            (Kind::Res, _) => match self.re.as_deref() {
                Some(re) => format!("the answer to request {re}"),
                None => "an answer".into(),
            },
            (Kind::Ctl, false) => format!("control message {}", self.m),
            (Kind::Req, false) => format!("request {}", self.m),
            (Kind::Ctl, true) => "a control message".into(),
            _ => "a message".into(),
        }
    }

    /// `None`: malformed or over its class cap; the link closes with [`crate::close::MALFORMED`].
    pub fn decode(s: &str) -> Option<Envelope> {
        if s.len() > MAX_FRAME {
            return None;
        }
        let e: Envelope = serde_json::from_str(s).ok()?;
        if e.t == Kind::Ctl && s.len() > CONTROL_CAP {
            return None;
        }
        Some(e)
    }
}

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
        let u = Envelope::decode(r#"{"v":[1,0],"t":"stream","id":"x"}"#).unwrap();
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
            err.safe_detail.starts_with("request turn.start is "),
            "{}",
            err.safe_detail
        );
        // an answer carries no method: the error still names what was too big
        huge.t = Kind::Res;
        huge.m = String::new();
        huge.re = Some("n_4".into());
        let err = huge.encode().unwrap_err();
        assert!(
            err.safe_detail.starts_with("the answer to request n_4 is "),
            "{}",
            err.safe_detail
        );
        assert_eq!(
            big.encode().unwrap_err().safe_detail.split(" is ").next(),
            Some("control message hello")
        );
        let ok = serde_json::to_string(&Outcome::Ok(serde_json::json!(1))).unwrap();
        assert_eq!(ok, r#"{"ok":1}"#);
    }
}
