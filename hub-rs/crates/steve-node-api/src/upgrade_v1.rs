//! Permanent upgrade protocol v1, independent of the ordinary Node protocol.
//!
//! This is the complete compatibility boundary: bootstrap, frame headers, signatures, control
//! messages, five methods and their errors. Do not extend or change v1. Introduce v2 alongside
//! it and retain v1 when a change is necessary. Ordinary payloads are NOT covered by this promise.
//! In particular, never import ordinary Node errors, commands, events, ids or build types here.
//!
//! Each upgrade WebSocket text message is one JSON Frame. `v` is UPGRADE_PROTOCOL, including
//! bootstrap; ordinary frames use the application's protocol number instead. The independent
//! `proto` in bootstrap is the ordinary application protocol: exact
//! equality selects normal mode; any difference selects upgrade mode after authenticating.
//! Binary WebSocket messages are refused. Heartbeats keep an upgrade link alive; fencing and
//! goodbye end it. No other control message changes an upgrade-mode Node's execution state.
//!
//! Signatures are Ed25519 over a domain, NUL-separated connection bindings, and SHA256 of
//! canonical JSON (sorted object keys, compact JSON scalars/arrays). Bootstrap carries no
//! ordinary capabilities or stream heads: those are queried only after an equal-version link.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

pub const UPGRADE_PROTOCOL: u32 = 1;
pub const REPORT_MAX: usize = 1024;
pub const HANDSHAKE_BUDGET_MS: u64 = 10_000;
pub const MAX_FRAME: usize = 288 * 1024;
pub const CONTROL_CAP: usize = 64 * 1024;
pub const BULK_CAP: usize = 256 * 1024;
pub const BULK_FRAME: usize = 48 * 1024;
pub const CHUNK_BYTES: u32 = (BULK_FRAME / 2) as u32 - 1024;
pub const ARTIFACT_PREFIX: &str = "node-upgrade.";
pub const CHALLENGE: &str = "challenge";
pub const HELLO: &str = "hello";
pub const WELCOME: &str = "welcome";
pub const REPORT: &str = "report";
pub const NOTED: &str = "noted";
pub const ERROR: &str = "error";
pub const HEARTBEAT: &str = "heartbeat";
pub const FENCED: &str = "fenced";
pub const GOODBYE: &str = "goodbye";
pub const PROBE_BUILD: &str = "probe_build";
pub const UPGRADE_STAGE: &str = "upgrade_stage";
pub const BLOB_FETCH: &str = "blob.fetch";
pub const UPGRADE_SWITCH: &str = "upgrade_switch";
pub const UPGRADE_STATUS: &str = "upgrade_status";
pub const METHODS: [&str; 5] = [
    PROBE_BUILD,
    UPGRADE_STAGE,
    BLOB_FETCH,
    UPGRADE_SWITCH,
    UPGRADE_STATUS,
];

pub fn is_method(method: &str) -> bool {
    METHODS.contains(&method)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
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
    pub proto: u32,
    pub upgrade_protocol: u32,
    pub hub_time: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Hello {
    pub node_id: String,
    pub incarnation: u64,
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
    pub upgrade_protocol: u32,
    pub mode: Mode,
    pub paused: bool,
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
pub struct Heartbeat {
    pub at: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Fenced {
    #[serde(rename = "gen")]
    pub generation: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Goodbye {
    pub reason: String,
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trace: Option<String>,
    #[serde(default)]
    pub deadline_at: i64,
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
            trace: None,
            deadline_at: 0,
            p,
        }
    }

    pub fn encode(&self) -> Result<String> {
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Code {
    Incompatible,
    Downgrade,
    Offline,
    Busy,
    Unsupported,
    Denied,
    Expired,
    Stale,
    Conflict,
    Exhausted,
    Invalid,
    OutcomeUnknown,
    Gap,
    Internal,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Retry {
    Never,
    Later,
    Reconcile,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Error {
    pub code: Code,
    pub retry: Retry,
    pub operation: Option<String>,
    pub detail: String,
}

impl Error {
    pub fn new(code: Code, detail: impl Into<String>) -> Self {
        let retry = match code {
            Code::Offline | Code::Busy | Code::Exhausted => Retry::Later,
            Code::OutcomeUnknown | Code::Gap => Retry::Reconcile,
            _ => Retry::Never,
        };
        Self {
            code,
            retry,
            operation: None,
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
    pub artifact: String,
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
    pub access: FileAccess,
    pub id: String,
    pub offset: u64,
    pub max: u32,
}

/// No execution scope, general filesystem path or upload exists in the permanent protocol.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "for", rename_all = "snake_case", deny_unknown_fields)]
pub enum FileAccess {
    Operation { operation: String },
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
    pub abandon: bool,
    pub force: bool,
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
