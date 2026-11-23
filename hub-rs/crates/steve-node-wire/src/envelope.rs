//! One JSON envelope per WebSocket text message, and the size limits checked around it.

use std::pin::Pin;

use futures_util::{Sink, Stream};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use steve_node_api::{Code, Error};

// Local scheduling policy; these limits may evolve with the ordinary protocol.
pub const MAX_FRAME: usize = 288 * 1024;
pub const CONTROL_CAP: usize = 64 * 1024;
pub const BULK_CAP: usize = 256 * 1024;
pub const BULK_FRAME: usize = 48 * 1024;
pub const CHUNK_BYTES: u32 = (BULK_FRAME / 2) as u32 - 1024;

/// The only permanent ordinary-frame constraint: a top-level integer v selects its decoder.
pub fn version(text: &str) -> Option<u32> {
    #[derive(Deserialize)]
    struct Version {
        v: u32,
    }
    if text.len() > MAX_FRAME {
        return None;
    }
    serde_json::from_str::<Version>(text).ok().map(|v| v.v)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    Req,
    Res,
    Ctl,
    #[serde(other)]
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Envelope {
    pub v: u32,
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
    #[serde(default)]
    pub deadline_at: i64,
    #[serde(default)]
    pub p: Value,
}

impl Envelope {
    pub fn ctl(id: impl Into<String>, method: &str, generation: u64, p: Value) -> Self {
        Self {
            v: steve_node_api::PROTOCOL,
            t: Kind::Ctl,
            id: id.into(),
            re: None,
            m: method.into(),
            generation,
            trace: None,
            deadline_at: 0,
            p,
        }
    }

    pub fn encode(&self) -> Result<String, Error> {
        let text =
            serde_json::to_string(self).map_err(|e| Error::new(Code::Internal, e.to_string()))?;
        let cap = if self.t == Kind::Ctl {
            CONTROL_CAP
        } else {
            MAX_FRAME
        };
        if text.len() > cap {
            return Err(Error::new(
                Code::Exhausted,
                format!(
                    "{} is {} bytes, over the {cap} limit",
                    self.subject(),
                    text.len()
                ),
            ));
        }
        Ok(text)
    }

    fn subject(&self) -> String {
        match (self.t, self.m.is_empty()) {
            (Kind::Res, _) => self.re.as_ref().map_or_else(
                || "an answer".into(),
                |re| format!("the answer to request {re}"),
            ),
            (Kind::Ctl, false) => format!("control message {}", self.m),
            (Kind::Req, false) => format!("request {}", self.m),
            (Kind::Ctl, true) => "a control message".into(),
            _ => "a message".into(),
        }
    }

    pub fn decode(text: &str) -> Option<Self> {
        if text.len() > MAX_FRAME {
            return None;
        }
        let frame: Self = serde_json::from_str(text).ok()?;
        if frame.t == Kind::Ctl && text.len() > CONTROL_CAP {
            return None;
        }
        Some(frame)
    }
}

/// Event batches use the existing, smaller transport scheduling limit.
pub const EVENTS_FRAME: usize = 64 * 1024;

/// A result as it travels: `{"ok":T}` or `{"err":Error}`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    Ok(Value),
    Err(Error),
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
            v: steve_node_api::PROTOCOL,
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
        assert_eq!(big.encode().unwrap_err().code, Code::Exhausted);
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
        assert_eq!(err.code, Code::Exhausted);
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
