//! Permanent upgrade protocol v1, independent of the ordinary Node protocol.
//!
//! This is the complete compatibility boundary: bootstrap, frame headers, signatures, control
//! messages, five methods and their errors. Do not extend or change v1. Introduce v2 alongside
//! it and retain v1 when a change is necessary after the first production installation. Ordinary payloads are NOT covered by this promise.
//! In particular, never import ordinary Node errors, commands, events, ids or build types here.
//!
//! Every WebSocket text message is a JSON object with a top-level integer `v`: this is the
//! ONLY permanent constraint on ordinary envelopes. Read `v` BEFORE selecting a decoder.
//! Bootstrap (challenge/hello/welcome/report/noted/error), heartbeat and fenced ALWAYS use
//! v=1 and this same immutable schema, even after upgrade-v2 is introduced. Five-method
//! requests/responses use the selected upgrade version. Ordinary envelopes use PROTOCOL>=10
//! and evolve independently; 1..=9 is reserved for upgrade schemas. Binary frames are refused.
//!
//! Challenge announces the Hub's highest implemented upgrade version N (the Hub retains all
//! 1..=N). Hello selects the Node's highest implemented version <=N; Nodes accept ANY N>=1.
//! With no common implementation the Node sends signed Report "incompatible: upgrade Hub"
//! and closes after noted. Hub refuses Hello outside 1..=N. Welcome confirms only the Hub's
//! ordinary proto; equality with the Node proto selects normal mode, otherwise upgrade-only.
//! Selection controls only the five methods and errors, NEVER bootstrap or fixed controls.
//!
//! upgrade_fetch is Node -> Hub; probe_build/upgrade_stage/upgrade_switch/upgrade_status are
//! Hub -> Node. Fetch names only an operation, offset and requested maximum; the Hub may
//! return fewer bytes. Continue at offset+bytes.len until eof. Queue/chunk/control limits are
//! local policy, not wire constants. A protocol mismatch executes no ordinary business request.
//! Heartbeat is empty (only receipt/liveness matters); fenced ends the link by close 4409.
//! REPORT_MAX bounds the sender's report; HANDSHAKE_BUDGET_MS is the receiver's timeout.
//!
//! Signatures are Ed25519 over a domain, NUL-separated connection bindings, and SHA256 of
//! canonical JSON (sorted object keys, compact JSON scalars/arrays). Bootstrap carries no
//! ordinary capabilities or stream heads: those are queried only after an equal-version link.

//! Signed structures may contain only strings, integers, booleans and arrays of strings.
//! Floating point values and nested objects MUST NOT enter signed structures.
//! Authentication encoding is part of v1: Ed25519 (RFC 8032, pure Ed25519, no prehash),
//! a raw 32-byte compressed public key and raw 64-byte R || S signature. Writers emit lowercase
//! hexadecimal (64/128 ASCII digits, no prefix); readers accept either hex case, but no whitespace
//! or alternate lengths. Verification is strict: reject noncanonical S and small-order keys/R,
//! invalid point encodings, and any changed signed byte. The crypto implementation may live in
//! the transport; its RFC 8032 and malleability vectors must pass before shipping.
//!
//! Welcome pins identify SHA256 of the complete leaf certificate DER, encoded as `sha256:` plus
//! 64 lowercase hex digits. CLI input may also accept the bare digest and uppercase hex.
//! An empty hub_key AND empty sig means unsigned welcome; a Node with a pinned Hub identity must
//! reject it. Every other welcome must have both a well-formed key and a valid signature.
//!
//! Bootstrap refusal sends error then close: invalid=4400, denied=4403, unsupported=4426,
//! internal (including admission timeouts)=1011. A bare 4403/4426 close means denied/unsupported;
//! every other bare close is offline/retry. A noted report closes with 1000. Established links
//! use 4408 for heartbeat loss, 4409 for generation fencing and 1000 for normal disconnection.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

/// Close codes shared by bootstrap and upgrade-v1 links.
pub mod close {
    pub const MALFORMED: u16 = 4400;
    pub const DENIED: u16 = 4403;
    pub const DEAD: u16 = 4408;
    pub const FENCED: u16 = 4409;
    pub const UNSUPPORTED: u16 = 4426;
    pub const INTERNAL: u16 = 1011;
    pub const GOODBYE: u16 = 1000;
}

/// Raw fixed-width hex; no prefixes, whitespace or short forms.
pub fn decode_hex<const N: usize>(text: &str) -> Option<[u8; N]> {
    hex::decode(text).ok()?.try_into().ok()
}

pub fn certificate_pin(der: &[u8]) -> String {
    format!("sha256:{}", hex::encode(Sha256::digest(der)))
}

pub fn parse_pin(pin: &str) -> Option<[u8; 32]> {
    decode_hex(pin.strip_prefix("sha256:").unwrap_or(pin))
}

pub const UPGRADE_PROTOCOL: u32 = 1;
pub const REPORT_MAX: usize = 1024;
pub const HANDSHAKE_BUDGET_MS: u64 = 10_000;
/// Receivers MUST accept and senders MUST NOT exceed this many UTF-8 bytes per v1 frame.
pub const MAX_FRAME: usize = 65536;
pub const CHALLENGE: &str = "challenge";
pub const HELLO: &str = "hello";
pub const WELCOME: &str = "welcome";
pub const REPORT: &str = "report";
pub const NOTED: &str = "noted";
pub const ERROR: &str = "error";
pub const HEARTBEAT: &str = "heartbeat";
pub const FENCED: &str = "fenced";
pub const PROBE_BUILD: &str = "probe_build";
pub const UPGRADE_STAGE: &str = "upgrade_stage";
pub const UPGRADE_FETCH: &str = "upgrade_fetch";
pub const UPGRADE_SWITCH: &str = "upgrade_switch";
pub const UPGRADE_STATUS: &str = "upgrade_status";
pub const METHODS: [&str; 5] = [
    PROBE_BUILD,
    UPGRADE_STAGE,
    UPGRADE_FETCH,
    UPGRADE_SWITCH,
    UPGRADE_STATUS,
];

pub fn is_method(method: &str) -> bool {
    METHODS.contains(&method)
}

/// Derived connection state, not an on-wire enum: compare the two ordinary protocol numbers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Normal,
    Upgrade,
}

impl Mode {
    pub fn between(local: u32, peer: u32) -> Self {
        if local == peer {
            Self::Normal
        } else {
            Self::Upgrade
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Challenge {
    pub nonce: String,
    pub hub_id: String,
    pub conn_id: String,
    pub upgrade_protocol: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Hello {
    pub node_id: String,
    pub proto: u32,
    pub upgrade_protocol: u32,
    pub sig: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Welcome {
    #[serde(rename = "gen")]
    pub generation: u64,
    pub proto: u32,
    pub heartbeat_ms: u64,
    pub hub_key: String,
    pub pins: Vec<String>,
    pub sig: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Report {
    pub node_id: String,
    pub error: String,
    pub sig: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Noted {}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Heartbeat {}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Fenced {
    #[serde(rename = "gen")]
    pub generation: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    Req,
    Res,
    Ctl,
    #[serde(other, skip_serializing)]
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Frame {
    pub v: u32,
    pub t: Kind,
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub re: Option<String>,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub m: String,
    #[serde(default, rename = "gen")]
    pub generation: u64,
    #[serde(default)]
    pub p: Value,
}

impl Frame {
    pub fn ctl(id: impl Into<String>, method: &str, generation: u64, p: Value) -> Self {
        Self {
            v: UPGRADE_PROTOCOL,
            t: Kind::Ctl,
            id: id.into(),
            re: None,
            m: method.into(),
            generation,
            p,
        }
    }

    pub fn encode(&self) -> Result<String> {
        let text =
            serde_json::to_string(self).map_err(|e| Error::new(Code::Internal, e.to_string()))?;
        let cap = MAX_FRAME;
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
        Some(frame)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Code {
    Incompatible,
    Downgrade,
    Busy,
    Unsupported,
    Denied,
    Conflict,
    Exhausted,
    Invalid,
    Internal,
}

impl Code {
    /// Only transient capacity and implementation failures may be resent as the same operation.
    /// A lost switch response is a LOCAL transport outcome: reconnect and query upgrade_status.
    pub fn retryable(self) -> bool {
        matches!(self, Self::Busy | Self::Exhausted | Self::Internal)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Error {
    pub code: Code,
    pub detail: String,
}

impl Error {
    pub fn new(code: Code, detail: impl Into<String>) -> Self {
        Self {
            code,
            detail: detail.into(),
        }
    }
    pub fn incompatible() -> Self {
        Self::new(Code::Incompatible, "protocol mismatch: upgrade required")
    }
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:?}: {}", self.code, self.detail)
    }
}
impl std::error::Error for Error {}
pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    Ok(Value),
    Err(Error),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BuildIdentity {
    pub version: String,
    pub commit: String,
    pub target: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProbeBuildRequest {}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProbeBuildResponse {
    pub build: Option<BuildIdentity>,
    pub upgradable: bool,
    pub running: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StageRequest {
    pub operation: String,
    pub build: BuildIdentity,
    pub size: u64,
    pub sha256: String,
    pub health_secs: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Phase {
    Staging,
    Staged,
    Switching,
    Linked,
    Succeeded,
    Failed,
    RolledBack,
    Abandoned,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UpgradeState {
    pub operation: String,
    pub phase: Phase,
    pub version: String,
    pub previous: Option<String>,
    pub error: Option<String>,
    pub updated: i64,
}

pub type StageResponse = UpgradeState;
pub type SwitchResponse = UpgradeState;
pub type StatusResponse = UpgradeState;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FetchRequest {
    pub operation: String,
    pub offset: u64,
    pub max: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FetchResponse {
    pub offset: u64,
    #[serde(with = "hexbytes")]
    pub bytes: Vec<u8>,
    pub eof: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SwitchRequest {
    pub operation: String,
    pub action: SwitchAction,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SwitchAction {
    Switch,
    Force,
    Abandon,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StatusRequest {
    pub operation: String,
}

mod hexbytes {
    use serde::{Deserialize, Deserializer, Serializer};
    pub fn serialize<S: Serializer>(
        bytes: &[u8],
        serializer: S,
    ) -> std::result::Result<S::Ok, S::Error> {
        serializer.serialize_str(&hex::encode(bytes))
    }
    pub fn deserialize<'de, D: Deserializer<'de>>(
        deserializer: D,
    ) -> std::result::Result<Vec<u8>, D::Error> {
        hex::decode(String::deserialize(deserializer)?).map_err(serde::de::Error::custom)
    }
}

fn canonical(value: &Value) -> String {
    match value {
        Value::Object(map) => {
            let mut keys: Vec<_> = map.keys().collect();
            keys.sort();
            format!(
                "{{{}}}",
                keys.into_iter()
                    .map(|key| format!(
                        "{}:{}",
                        serde_json::to_string(key).unwrap(),
                        canonical(&map[key])
                    ))
                    .collect::<Vec<_>>()
                    .join(",")
            )
        }
        Value::Array(values) => format!(
            "[{}]",
            values.iter().map(canonical).collect::<Vec<_>>().join(",")
        ),
        scalar => serde_json::to_string(scalar).unwrap(),
    }
}

fn signing(domain: &[u8], bindings: &[&str], values: &[Value]) -> Vec<u8> {
    let mut bytes = domain.to_vec();
    for binding in bindings {
        bytes.push(0);
        bytes.extend_from_slice(binding.as_bytes());
    }
    for value in values {
        bytes.push(0);
        bytes.extend_from_slice(&Sha256::digest(canonical(value).as_bytes()));
    }
    bytes
}

pub fn hello_message(challenge: &Challenge, hello: &Hello) -> Vec<u8> {
    let mut unsigned = hello.clone();
    unsigned.sig.clear();
    signing(
        b"steve-node-hello-v1",
        &[&challenge.hub_id, &challenge.conn_id, &challenge.nonce],
        &[serde_json::to_value(unsigned).unwrap()],
    )
}

pub fn report_message(challenge: &Challenge, report: &Report) -> Vec<u8> {
    let mut unsigned = report.clone();
    unsigned.sig.clear();
    signing(
        b"steve-node-report-v1",
        &[&challenge.hub_id, &challenge.conn_id, &challenge.nonce],
        &[serde_json::to_value(unsigned).unwrap()],
    )
}

pub fn welcome_message(challenge: &Challenge, hello: &Hello, welcome: &Welcome) -> Vec<u8> {
    let mut unsigned = welcome.clone();
    unsigned.sig.clear();
    signing(
        b"steve-hub-welcome-v1",
        &[&challenge.conn_id, &challenge.nonce],
        &[
            serde_json::to_value(hello).unwrap(),
            serde_json::to_value(unsigned).unwrap(),
        ],
    )
}
