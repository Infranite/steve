//! Wire format of Feishu's long connection (`pbbp2.proto` in larksuite/oapi-sdk-go `ws/`).
//!
//! Every WebSocket binary message is one [`Frame`]. Fields 6, 7 and 9 are proto2 `required`
//! here although the `.proto` marks them optional, because the Go SDK (gogo-generated) always
//! writes them, even empty; this keeps our encoding byte-identical to the official client.

use prost::Message as _;

/// One `key=value` frame header.
#[derive(Clone, PartialEq, Eq, prost::Message)]
pub struct Header {
    #[prost(string, required, tag = "1")]
    pub key: String,
    #[prost(string, required, tag = "2")]
    pub value: String,
}

/// A long-connection frame.
#[derive(Clone, PartialEq, Eq, prost::Message)]
pub struct Frame {
    #[prost(uint64, required, tag = "1")]
    pub seq_id: u64,
    #[prost(uint64, required, tag = "2")]
    pub log_id: u64,
    /// The `service_id` query parameter of the connection URL.
    #[prost(int32, required, tag = "3")]
    pub service: i32,
    /// [`METHOD_CONTROL`] or [`METHOD_DATA`].
    #[prost(int32, required, tag = "4")]
    pub method: i32,
    #[prost(message, repeated, tag = "5")]
    pub headers: Vec<Header>,
    #[prost(string, required, tag = "6")]
    pub payload_encoding: String,
    #[prost(string, required, tag = "7")]
    pub payload_type: String,
    #[prost(bytes = "vec", optional, tag = "8")]
    pub payload: Option<Vec<u8>>,
    #[prost(string, required, tag = "9")]
    pub log_id_new: String,
}

/// `Frame.method` of control frames (ping, pong).
pub const METHOD_CONTROL: i32 = 0;
/// `Frame.method` of data frames (events, cards, and our acks).
pub const METHOD_DATA: i32 = 1;

/// Header names (`ws/const.go`).
pub mod header {
    /// Message timestamp, ms.
    pub const TIMESTAMP: &str = "timestamp";
    /// `event` | `card` | `ping` | `pong`.
    pub const TYPE: &str = "type";
    /// Id of the logical message; split parts share it.
    pub const MESSAGE_ID: &str = "message_id";
    /// Number of parts; 1 (or absent) when not split.
    pub const SUM: &str = "sum";
    /// 0-based part index.
    pub const SEQ: &str = "seq";
    pub const TRACE_ID: &str = "trace_id";
    pub const INSTANCE_ID: &str = "instance_id";
    /// Handler time in ms, added to the ack.
    pub const BIZ_RT: &str = "biz_rt";
    /// Handshake failure details, as HTTP response headers of a refused upgrade.
    pub const HANDSHAKE_STATUS: &str = "Handshake-Status";
    pub const HANDSHAKE_MSG: &str = "Handshake-Msg";
    pub const HANDSHAKE_AUTH_ERR_CODE: &str = "Handshake-Autherrcode";
}

/// `type` header values.
pub mod kind {
    pub const EVENT: &str = "event";
    pub const CARD: &str = "card";
    pub const PING: &str = "ping";
    pub const PONG: &str = "pong";
}

/// Codes of the endpoint response and of `Handshake-Status` (`ws/const.go`).
pub mod code {
    pub const OK: i64 = 0;
    pub const SYSTEM_BUSY: i64 = 1;
    pub const FORBIDDEN: i64 = 403;
    pub const AUTH_FAILED: i64 = 514;
    /// `Handshake-Autherrcode` when the app already has the maximum number of connections (50).
    pub const EXCEED_CONN_LIMIT: i64 = 1_000_040_350;
    pub const INTERNAL_ERROR: i64 = 1_000_040_343;
}

/// Path of the endpoint discovery call, relative to the open-platform base.
pub const ENDPOINT_PATH: &str = "/callback/ws/endpoint";

impl Frame {
    /// First header named `key`.
    pub fn header(&self, key: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|h| h.key == key)
            .map(|h| h.value.as_str())
    }

    /// Header as an integer; 0 when absent or not a number (as the Go SDK does).
    pub fn header_int(&self, key: &str) -> i64 {
        self.header(key).and_then(|v| v.parse().ok()).unwrap_or(0)
    }

    /// Appends a header (duplicates are kept, as in the Go SDK).
    pub fn add_header(&mut self, key: impl Into<String>, value: impl Into<String>) {
        self.headers.push(Header {
            key: key.into(),
            value: value.into(),
        });
    }

    /// The `type` header.
    pub fn kind(&self) -> &str {
        self.header(header::TYPE).unwrap_or("")
    }

    /// The client's ping: a control frame with `type=ping` on the connection's service.
    pub fn ping(service: i32) -> Self {
        let mut f = Self {
            method: METHOD_CONTROL,
            service,
            ..Self::default()
        };
        f.add_header(header::TYPE, kind::PING);
        f
    }

    /// Protobuf bytes of the frame.
    pub fn to_bytes(&self) -> Vec<u8> {
        self.encode_to_vec()
    }

    /// Parses one frame.
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, prost::DecodeError> {
        Self::decode(bytes)
    }
}

/// JSON placed in the ack frame's payload (`ws/model.go` `Response`). `data` is base64 in JSON
/// because Go marshals `[]byte` that way.
pub fn response_payload(code: u16, data: Option<&[u8]>) -> Vec<u8> {
    use base64::Engine as _;
    let data = data.map(|d| base64::engine::general_purpose::STANDARD.encode(d));
    serde_json::to_vec(&serde_json::json!({"code": code, "headers": null, "data": data}))
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ping_matches_hand_encoded_bytes() {
        // SeqID=0, LogID=0, service=7, method=0, header{type,ping}, three empty strings.
        let want: Vec<u8> = [
            &[0x08, 0x00, 0x10, 0x00, 0x18, 0x07, 0x20, 0x00][..],
            &[0x2a, 0x0c, 0x0a, 0x04],
            b"type",
            &[0x12, 0x04],
            b"ping",
            &[0x32, 0x00, 0x3a, 0x00, 0x4a, 0x00],
        ]
        .concat();
        assert_eq!(Frame::ping(7).to_bytes(), want);
    }

    #[test]
    fn data_frame_matches_gogo_field_order() {
        // Bytes laid out as pbbp2.pb.go `MarshalToSizedBuffer` writes them: 1–5, then 6 and 7
        // always, 8 only when set, 9 always.
        let want = hex::decode(
            "0803100918f681801020012a0d0a047479706512056576656e742a100a0a6d6573736167655f69\
             6412026d312a080a0373756d12013132003a00420c7b22636f6465223a3230307d4a00",
        )
        .unwrap();
        let mut f = Frame {
            seq_id: 3,
            log_id: 9,
            service: 33_554_678,
            method: METHOD_DATA,
            payload: Some(br#"{"code":200}"#.to_vec()),
            ..Frame::default()
        };
        f.add_header(header::TYPE, kind::EVENT);
        f.add_header(header::MESSAGE_ID, "m1");
        f.add_header(header::SUM, "1");
        assert_eq!(f.to_bytes(), want);
        assert_eq!(Frame::from_bytes(&want).unwrap(), f);
    }

    #[test]
    fn round_trip_and_headers() {
        let mut f = Frame {
            seq_id: 3,
            log_id: u64::MAX,
            service: -1,
            method: METHOD_DATA,
            payload: Some(b"{}".to_vec()),
            log_id_new: "x".into(),
            ..Frame::default()
        };
        f.add_header(header::SUM, "2");
        f.add_header(header::SEQ, "1");
        let back = Frame::from_bytes(&f.to_bytes()).unwrap();
        assert_eq!(back, f);
        assert_eq!(back.header_int(header::SUM), 2);
        assert_eq!(back.header_int("missing"), 0);
    }

    #[test]
    fn response_payload_shape() {
        let v: serde_json::Value = serde_json::from_slice(&response_payload(200, None)).unwrap();
        assert_eq!(
            v,
            serde_json::json!({"code": 200, "headers": null, "data": null})
        );
        let v: serde_json::Value =
            serde_json::from_slice(&response_payload(200, Some(b"hi"))).unwrap();
        assert_eq!(v["data"], "aGk=");
    }
}
