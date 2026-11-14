//! The frames and shapes of the external channel protocol ([`super::protocol`]), and the checks
//! every field gets before the Hub uses it (§2, §6, §10 "Validation"). Pure: no socket, no store.

use std::collections::BTreeSet;

use serde_json::{Map, Value, json};

/// The protocol version this Hub speaks (§3.4).
pub const PROTOCOL: u32 = 1;
/// The frame cap in each direction (§2).
pub const MAX_FRAME: usize = 1 << 20;
/// The features a channel service must offer in v1 (§3.5).
pub const REQUIRED_FEATURES: [&str; 6] = [
    "me",
    "chats.get",
    "messages.get",
    "messages.history",
    "changes",
    "media.download",
];
/// What the Hub can use, sent in hello (§3.2).
pub const CAPABILITIES: [&str; 8] = [
    "changes",
    "history",
    "media",
    "send",
    "reply",
    "recall",
    "reactions",
    "people",
];
/// The earliest time a receiver accepts (2000-01-01, §6).
pub const MIN_TIME_MS: i64 = 946_684_800_000;
/// How far ahead of the Hub's clock a time may be (§6).
pub const MAX_AHEAD_MS: i64 = 24 * 3_600_000;
/// The longest history range one `messages.history` call asks for (§4.3).
pub const MAX_HISTORY_RANGE_MS: i64 = 31 * 24 * 3_600_000;
/// A Hub-made id; never accepted from a remote (§4).
const HUB_PREFIX: &str = "fp:";

// ---------------------------------------------------------------------------------------------
// Limits.

/// The limits in force on one session: the lower of the server's hello and the Hub's ceilings.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Limits {
    pub max_frame_bytes: usize,
    pub max_text_bytes: usize,
    pub max_batch_items: usize,
    pub max_unacked_batches: usize,
    pub max_ids_per_call: usize,
    pub history_max_limit: usize,
    /// Writes per second as the user; `None`: unpaced.
    pub write_per_sec: Option<f64>,
    pub write_burst: Option<u32>,
    pub max_attachment_bytes: Option<u64>,
    pub max_inflight_requests: usize,
}

impl Limits {
    /// The Hub's ceilings (§2).
    pub const CEILING: Limits = Limits {
        max_frame_bytes: MAX_FRAME,
        max_text_bytes: 256 * 1024,
        max_batch_items: 200,
        max_unacked_batches: 16,
        max_ids_per_call: 100,
        history_max_limit: 200,
        write_per_sec: None,
        write_burst: None,
        max_attachment_bytes: None,
        max_inflight_requests: 32,
    };

    /// The server's advertised limits under the Hub's ceilings; a missing or nonsensical value is
    /// the ceiling.
    pub fn from_hello(v: &Value) -> Limits {
        let c = Self::CEILING;
        let n = |k: &str, ceil: usize| -> usize {
            v.get(k)
                .and_then(Value::as_u64)
                .filter(|n| *n >= 1)
                .map_or(ceil, |n| (n as usize).min(ceil))
        };
        Limits {
            max_frame_bytes: n("max_frame_bytes", c.max_frame_bytes).max(1024),
            max_text_bytes: n("max_text_bytes", c.max_text_bytes),
            max_batch_items: n("max_batch_items", c.max_batch_items),
            max_unacked_batches: n("max_unacked_batches", c.max_unacked_batches),
            max_ids_per_call: n("max_ids_per_call", c.max_ids_per_call),
            history_max_limit: n("history_max_limit", c.history_max_limit),
            write_per_sec: v
                .get("write_per_sec")
                .and_then(Value::as_f64)
                .filter(|r| *r > 0.0),
            write_burst: v
                .get("write_burst")
                .and_then(Value::as_u64)
                .filter(|b| *b >= 1)
                .map(|b| b.min(1000) as u32),
            max_attachment_bytes: v.get("max_attachment_bytes").and_then(Value::as_u64),
            max_inflight_requests: n("max_inflight_requests", c.max_inflight_requests),
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Errors (§7).

/// `error.data.reason`, the normative value of an error.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Reason {
    ParseError,
    Invalid,
    Unsupported,
    Internal,
    NotReady,
    UnsupportedVersion,
    Unauthorized,
    SessionExpired,
    SessionLocked,
    AccountChanged,
    WriteForbidden,
    RateLimited,
    NotFound,
    Forbidden,
    UnknownCursor,
    CursorTooOld,
    TooLarge,
    IdempotencyConflict,
    UpstreamUnavailable,
    Timeout,
    TargetForbidden,
}

const REASONS: [(Reason, &str, i64); 21] = [
    (Reason::ParseError, "parse_error", -32700),
    (Reason::Invalid, "invalid", -32602),
    (Reason::Unsupported, "unsupported", -32601),
    (Reason::Internal, "internal", -32603),
    (Reason::NotReady, "not_ready", -32000),
    (Reason::UnsupportedVersion, "unsupported_version", -32001),
    (Reason::Unauthorized, "unauthorized", -32002),
    (Reason::SessionExpired, "session_expired", -32003),
    (Reason::SessionLocked, "session_locked", -32004),
    (Reason::AccountChanged, "account_changed", -32005),
    (Reason::WriteForbidden, "write_forbidden", -32006),
    (Reason::RateLimited, "rate_limited", -32007),
    (Reason::NotFound, "not_found", -32008),
    (Reason::Forbidden, "forbidden", -32009),
    (Reason::UnknownCursor, "unknown_cursor", -32010),
    (Reason::CursorTooOld, "cursor_too_old", -32011),
    (Reason::TooLarge, "too_large", -32012),
    (Reason::IdempotencyConflict, "idempotency_conflict", -32013),
    (Reason::UpstreamUnavailable, "upstream_unavailable", -32014),
    (Reason::Timeout, "timeout", -32015),
    (Reason::TargetForbidden, "target_forbidden", -32016),
];

impl Reason {
    pub fn name(self) -> &'static str {
        REASONS
            .iter()
            .find(|r| r.0 == self)
            .map_or("internal", |r| r.1)
    }

    pub fn code(self) -> i64 {
        REASONS.iter().find(|r| r.0 == self).map_or(-32603, |r| r.2)
    }

    /// By `data.reason`; an unknown one is `internal` (§12).
    pub fn from_name(s: &str) -> Reason {
        REASONS
            .iter()
            .find(|r| r.1 == s)
            .map_or(Reason::Internal, |r| r.0)
    }

    fn from_code(code: i64) -> Reason {
        REASONS
            .iter()
            .find(|r| r.2 == code)
            .map_or(Reason::Internal, |r| r.0)
    }
}

/// An error a server answered.
#[derive(Debug, Clone, PartialEq)]
pub struct RpcError {
    pub reason: Reason,
    /// At most 512 characters, control characters dropped: it may be shown.
    pub message: String,
    pub data: Value,
}

impl RpcError {
    pub fn new(reason: Reason, message: impl Into<String>) -> Self {
        Self {
            reason,
            message: message.into(),
            data: json!({"reason": reason.name()}),
        }
    }

    /// The `error` member of a response.
    pub fn parse(v: &Value) -> RpcError {
        let data = v.get("data").cloned().unwrap_or(Value::Null);
        let reason = match data.get("reason").and_then(Value::as_str) {
            Some(r) => Reason::from_name(r),
            None => Reason::from_code(v.get("code").and_then(Value::as_i64).unwrap_or(-32603)),
        };
        let message = v
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or("")
            .chars()
            .filter(|c| !c.is_control())
            .take(512)
            .collect();
        RpcError {
            reason,
            message,
            data,
        }
    }

    /// `data.retry_after_ms`, at most 10 minutes.
    pub fn retry_after_ms(&self) -> Option<u64> {
        self.data
            .get("retry_after_ms")
            .and_then(Value::as_u64)
            .map(|ms| ms.min(600_000))
    }

    /// The `error` member to send.
    pub fn to_value(&self) -> Value {
        let mut data = match &self.data {
            Value::Object(m) => m.clone(),
            _ => Map::new(),
        };
        data.insert("reason".into(), self.reason.name().into());
        json!({"code": self.reason.code(), "message": self.message, "data": data})
    }
}

impl std::fmt::Display for RpcError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.message.is_empty() {
            f.write_str(self.reason.name())
        } else {
            write!(f, "{}: {}", self.reason.name(), self.message)
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Frames.

/// One received frame, classified (§2, §10).
#[derive(Debug, Clone, PartialEq)]
pub enum Incoming {
    /// The answer to request `id`.
    Response {
        id: u64,
        result: Result<Value, RpcError>,
    },
    /// A notification of the server (§4.6).
    Note { method: String, params: Value },
    /// A request of the server: answered `-32601` and ignored (§10).
    Request { id: Value },
}

/// Reads one text frame. `Err`: a protocol violation (not JSON, a batch array, not JSON-RPC 2.0):
/// the socket is closed.
pub fn classify(text: &str) -> Result<Incoming, String> {
    let v: Value =
        serde_json::from_str(text).map_err(|_| "a frame that is not JSON".to_string())?;
    let Value::Object(o) = v else {
        return Err("a frame that is not one JSON-RPC object".into());
    };
    if o.get("jsonrpc").and_then(Value::as_str) != Some("2.0") {
        return Err("a frame without jsonrpc 2.0".into());
    }
    if let Some(method) = o.get("method").and_then(Value::as_str) {
        let params = o.get("params").cloned().unwrap_or(Value::Null);
        return Ok(match o.get("id") {
            Some(id) if !id.is_null() => Incoming::Request { id: id.clone() },
            _ => Incoming::Note {
                method: method.to_string(),
                params,
            },
        });
    }
    let id = o
        .get("id")
        .and_then(Value::as_u64)
        .ok_or("a response without an integer id")?;
    let result = match (o.get("result"), o.get("error")) {
        (_, Some(e)) if !e.is_null() => Err(RpcError::parse(e)),
        (Some(r), _) => Ok(r.clone()),
        _ => return Err("a response without result or error".into()),
    };
    Ok(Incoming::Response { id, result })
}

pub fn request(id: u64, method: &str, params: Value) -> String {
    json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}).to_string()
}

pub fn response(id: &Value, result: Result<Value, RpcError>) -> String {
    match result {
        Ok(r) => json!({"jsonrpc": "2.0", "id": id, "result": r}),
        Err(e) => json!({"jsonrpc": "2.0", "id": id, "error": e.to_value()}),
    }
    .to_string()
}

pub fn notification(method: &str, params: Value) -> String {
    json!({"jsonrpc": "2.0", "method": method, "params": params}).to_string()
}

// ---------------------------------------------------------------------------------------------
// Hello (§3).

/// The params of the Hub's `hello`.
pub fn hello_params(version: &str, now_ms: i64) -> Value {
    json!({
        "protocol": {"min": PROTOCOL, "max": PROTOCOL},
        "client": {"name": "steve-hub", "version": version},
        "time_ms": now_ms,
        "capabilities": CAPABILITIES,
    })
}

/// The version both sides speak: the lower maximum, if it is not below the higher minimum (§3.4).
/// `Err`: none, with what the server supports.
pub fn negotiate(client_min: u32, client_max: u32, server: &[u32]) -> Result<u32, Vec<u32>> {
    let (smin, smax) = match (server.iter().min(), server.iter().max()) {
        (Some(a), Some(b)) => (*a, *b),
        _ => return Err(Vec::new()),
    };
    let v = client_max.min(smax);
    if v >= client_min.max(smin) && server.contains(&v) {
        Ok(v)
    } else {
        let mut s = server.to_vec();
        s.sort_unstable();
        Err(s)
    }
}

/// `ok` | `expired` | `locked` (§3.3, §9.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionStatus {
    Ok,
    Expired,
    Locked,
}

/// `allowed` | `restricted` | `forbidden` (§3.6).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WritePolicy {
    Allowed,
    Restricted,
    Forbidden,
}

impl WritePolicy {
    fn parse(s: Option<&str>) -> WritePolicy {
        match s {
            Some("restricted") => Self::Restricted,
            Some("forbidden") => Self::Forbidden,
            // absent or unknown: allowed, the server refuses what it must
            _ => Self::Allowed,
        }
    }
}

impl SessionStatus {
    fn parse(s: Option<&str>) -> Result<SessionStatus, String> {
        match s {
            Some("ok") | None => Ok(Self::Ok),
            Some("expired") => Ok(Self::Expired),
            Some("locked") => Ok(Self::Locked),
            Some(x) => Err(format!("an unknown session status {x:?}")),
        }
    }
}

/// `session.status`'s params, or hello's `session` with its `write_policy`.
#[derive(Debug, Clone, PartialEq)]
pub struct Status {
    pub status: SessionStatus,
    pub reason: Option<String>,
    pub write_policy: WritePolicy,
}

impl Status {
    pub fn parse(v: &Value, write_policy: Option<&str>) -> Result<Status, String> {
        Ok(Status {
            status: SessionStatus::parse(v.get("status").and_then(Value::as_str))?,
            reason: v
                .get("reason")
                .and_then(Value::as_str)
                .map(|r| r.chars().filter(|c| !c.is_control()).take(512).collect()),
            write_policy: WritePolicy::parse(
                write_policy.or_else(|| v.get("write_policy").and_then(Value::as_str)),
            ),
        })
    }
}

/// The identity served by one credential. Missing declarations retain the original self contract.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Identity {
    #[default]
    #[serde(rename = "self")]
    Self_,
    Bot,
}

impl Identity {
    pub fn parse(v: Option<&Value>) -> Result<Self, String> {
        match v {
            None => Ok(Self::Self_),
            Some(Value::String(s)) if s == "self" => Ok(Self::Self_),
            Some(Value::String(s)) if s == "bot" => Ok(Self::Bot),
            _ => Err("identity must be self or bot".into()),
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::Self_ => "self",
            Self::Bot => "bot",
        }
    }
}

/// A checked hello result.
#[derive(Debug, Clone, PartialEq)]
pub struct Hello {
    pub protocol: u32,
    pub identity: Identity,
    pub realm: String,
    pub server: String,
    pub time_precision_ms: i64,
    pub time_ms: Option<i64>,
    pub features: BTreeSet<String>,
    pub limits: Limits,
    pub session: Status,
    /// `ok` | `degraded` | `down`.
    pub upstream: String,
    /// Generic connector extension, independent of messaging v1.
    pub connector: Option<crate::connector::Manifest>,
}

/// Why a hello result is refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HelloError {
    /// Not the version offered.
    Version(u32),
    /// Required features missing.
    Missing(Vec<String>),
    /// A malformed result.
    Malformed(String),
}

impl Hello {
    pub fn parse(v: &Value) -> Result<Hello, HelloError> {
        let bad = |m: &str| HelloError::Malformed(m.to_string());
        let protocol = v
            .get("protocol")
            .and_then(Value::as_u64)
            .ok_or_else(|| bad("hello without a protocol version"))? as u32;
        if protocol != PROTOCOL {
            return Err(HelloError::Version(protocol));
        }
        let realm = v
            .get("realm")
            .and_then(Value::as_str)
            .filter(|r| realm_ok(r))
            .ok_or_else(|| bad("hello without a valid realm"))?
            .to_string();
        let features: BTreeSet<String> = v
            .get("features")
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .filter_map(Value::as_str)
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default();
        let connector = v
            .get("connector")
            .filter(|v| !v.is_null())
            .map(crate::connector::Manifest::parse)
            .transpose()
            .map_err(HelloError::Malformed)?;
        let required: &[&str] = if connector.is_some() {
            &["me", "changes"]
        } else {
            &REQUIRED_FEATURES
        };
        let missing: Vec<String> = required
            .iter()
            .filter(|f| !features.contains(**f))
            .map(|f| f.to_string())
            .collect();
        if !missing.is_empty() {
            return Err(HelloError::Missing(missing));
        }
        let time_precision_ms = v
            .get("time_precision_ms")
            .and_then(Value::as_i64)
            .filter(|p| (1..=60_000).contains(p))
            .ok_or_else(|| bad("hello without a valid time_precision_ms"))?;
        let server = v
            .get("server")
            .map(|s| {
                let n = s.get("name").and_then(Value::as_str).unwrap_or("");
                let ver = s.get("version").and_then(Value::as_str).unwrap_or("");
                format!("{n} {ver}")
                    .trim()
                    .chars()
                    .filter(|c| !c.is_control())
                    .take(128)
                    .collect()
            })
            .unwrap_or_default();
        let session = Status::parse(
            v.get("session").unwrap_or(&Value::Null),
            v.get("write_policy").and_then(Value::as_str),
        )
        .map_err(HelloError::Malformed)?;
        let upstream = match v
            .get("upstream")
            .and_then(|u| u.get("status"))
            .and_then(Value::as_str)
        {
            Some(s @ ("ok" | "degraded" | "down")) => s.to_string(),
            _ => "ok".to_string(),
        };
        Ok(Hello {
            identity: Identity::parse(v.get("identity")).map_err(HelloError::Malformed)?,
            protocol,
            realm,
            server,
            time_precision_ms,
            time_ms: v.get("time_ms").and_then(Value::as_i64),
            features,
            limits: Limits::from_hello(v.get("limits").unwrap_or(&Value::Null)),
            session,
            upstream,
            connector,
        })
    }

    pub fn has(&self, feature: &str) -> bool {
        self.features.contains(feature)
            || (feature == "messages.send" && self.features.contains("messages.reply"))
    }
}

/// `[a-z0-9._-]`, 1–64 bytes (§3.3).
pub fn realm_ok(r: &str) -> bool {
    (1..=64).contains(&r.len())
        && r.bytes().all(|b| {
            b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'.' || b == b'_' || b == b'-'
        })
}

/// A system name of a `platform` handle: `[a-z0-9._-]`, 1–64 chars (§6).
pub fn system_ok(s: &str) -> bool {
    realm_ok(s)
}

// ---------------------------------------------------------------------------------------------
// Field checks (§4, §6).

/// An opaque id (`chat_id`, `id`, `source_id`, handle values): 1–`max` bytes, no control
/// characters, not the Hub's own prefix.
pub fn opaque_ok(s: &str, max: usize) -> bool {
    !s.is_empty()
        && s.len() <= max
        && !s.chars().any(char::is_control)
        && !s.starts_with(HUB_PREFIX)
}

fn field<'a>(v: &'a Value, k: &str) -> Option<&'a Value> {
    v.get(k).filter(|x| !x.is_null())
}

/// A required opaque string.
pub fn id(v: &Value, k: &str) -> Result<String, String> {
    match field(v, k).and_then(Value::as_str) {
        Some(s) if opaque_ok(s, 256) => Ok(s.to_string()),
        Some(_) => Err(format!("{k} is not a valid id")),
        None => Err(format!("{k} is missing")),
    }
}

/// An optional opaque string (absent or null: `None`; present and invalid: an error).
pub fn opt_id(v: &Value, k: &str) -> Result<Option<String>, String> {
    match field(v, k) {
        None => Ok(None),
        Some(Value::String(s)) if opaque_ok(s, 256) => Ok(Some(s.clone())),
        Some(_) => Err(format!("{k} is not a valid id")),
    }
}

/// A time in ms: after 2000-01-01, at most a day ahead of `now`.
pub fn time(v: &Value, k: &str, now: i64) -> Result<i64, String> {
    match field(v, k).and_then(Value::as_i64) {
        Some(t) if t >= MIN_TIME_MS && t <= now + MAX_AHEAD_MS => Ok(t),
        Some(_) => Err(format!("{k} is out of bounds")),
        None => Err(format!("{k} is missing")),
    }
}

pub fn opt_time(v: &Value, k: &str, now: i64) -> Result<Option<i64>, String> {
    match field(v, k) {
        None => Ok(None),
        Some(_) => time(v, k, now).map(Some),
    }
}

/// A short display string: control characters dropped, at most `max` chars.
pub fn text_field(v: &Value, k: &str, max: usize) -> Option<String> {
    field(v, k)
        .and_then(Value::as_str)
        .map(|s| {
            s.chars()
                .filter(|c| !c.is_control())
                .take(max)
                .collect::<String>()
        })
        .filter(|s| !s.trim().is_empty())
}

/// The kind of a [`Handle`] (§6).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HandleKind {
    Id,
    Stable,
    Email,
    Platform,
}

impl HandleKind {
    pub fn name(self) -> &'static str {
        match self {
            Self::Id => "id",
            Self::Stable => "stable",
            Self::Email => "email",
            Self::Platform => "platform",
        }
    }
}

/// A `Handle` on the wire.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Handle {
    pub kind: HandleKind,
    pub value: String,
    /// For [`HandleKind::Platform`]: the named system.
    pub system: Option<String>,
}

impl Handle {
    pub fn id(value: impl Into<String>) -> Self {
        Self {
            kind: HandleKind::Id,
            value: value.into(),
            system: None,
        }
    }

    pub fn parse(v: &Value) -> Result<Handle, String> {
        let kind = match v.get("kind").and_then(Value::as_str) {
            Some("id") => HandleKind::Id,
            Some("stable") => HandleKind::Stable,
            Some("email") => HandleKind::Email,
            Some("platform") => HandleKind::Platform,
            _ => return Err("a handle of an unknown kind".into()),
        };
        let value = v
            .get("value")
            .and_then(Value::as_str)
            .filter(|s| opaque_ok(s, 256))
            .ok_or("a handle without a valid value")?
            .to_string();
        if kind == HandleKind::Email && value.matches('@').count() != 1 {
            return Err("an email handle without exactly one @".into());
        }
        let system = match kind {
            HandleKind::Platform => Some(
                v.get("system")
                    .and_then(Value::as_str)
                    .filter(|s| system_ok(s))
                    .ok_or("a platform handle without a valid system")?
                    .to_string(),
            ),
            _ => None,
        };
        Ok(Handle {
            kind,
            value,
            system,
        })
    }

    pub fn to_value(&self) -> Value {
        let mut v = json!({"kind": self.kind.name(), "value": self.value});
        if let Some(s) = &self.system {
            v["system"] = s.clone().into();
        }
        v
    }

    /// The handle in the Hub's systems (§4.1): `id` and `stable` → the adapter's own system as
    /// `<realm>:<kind>:<value>`; `email` → `mail`, lowercased; `platform` → its system as is.
    pub fn hub(&self, realm: &str) -> (String, String) {
        match self.kind {
            HandleKind::Id | HandleKind::Stable => (
                super::SYSTEM.to_string(),
                format!("{realm}:{}:{}", self.kind.name(), self.value),
            ),
            HandleKind::Email => ("mail".to_string(), self.value.to_lowercase()),
            HandleKind::Platform => (self.system.clone().unwrap_or_default(), self.value.clone()),
        }
    }

    /// The other way: a handle the Hub stored under the adapter's system, back on the wire.
    pub fn from_hub(system: &str, handle: &str, realm: &str) -> Option<Handle> {
        if system == "mail" {
            return Some(Handle {
                kind: HandleKind::Email,
                value: handle.to_string(),
                system: None,
            });
        }
        if system != super::SYSTEM {
            return None;
        }
        let rest = handle.strip_prefix(realm)?.strip_prefix(':')?;
        let (kind, value) = rest.split_once(':')?;
        let kind = match kind {
            "id" => HandleKind::Id,
            "stable" => HandleKind::Stable,
            _ => return None,
        };
        opaque_ok(value, 256).then(|| Handle {
            kind,
            value: value.to_string(),
            system: None,
        })
    }
}

/// Up to 8 aliases; a malformed one is skipped.
pub fn aliases(v: &Value) -> Vec<Handle> {
    v.get("aliases")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|h| Handle::parse(h).ok())
                .take(8)
                .collect()
        })
        .unwrap_or_default()
}

/// A checked `Sender` (§6).
#[derive(Debug, Clone, PartialEq)]
pub struct WireSender {
    pub id: Handle,
    pub display: Option<String>,
    pub is_bot: bool,
    pub external: bool,
    pub aliases: Vec<Handle>,
}

impl WireSender {
    pub fn parse(v: &Value) -> Result<WireSender, String> {
        let id = Handle::parse(v.get("id").ok_or("a sender without an id")?)?;
        if !matches!(id.kind, HandleKind::Id | HandleKind::Email) {
            return Err("a sender's primary handle is neither id nor email".into());
        }
        Ok(WireSender {
            id,
            display: text_field(v, "display", 256),
            is_bot: v.get("is_bot").and_then(Value::as_bool).unwrap_or(false),
            external: v.get("external").and_then(Value::as_bool).unwrap_or(false),
            aliases: aliases(v),
        })
    }
}

/// `dm` | `group` | `channel` | `thread`.
pub fn chat_kind(s: Option<&str>) -> Option<crate::channel::SpaceKind> {
    crate::channel::SpaceKind::parse(s?)
}

/// A checked `Chat` (§6).
#[derive(Debug, Clone, PartialEq)]
pub struct Chat {
    pub chat_id: String,
    pub kind: Option<crate::channel::SpaceKind>,
    pub name: Option<String>,
    pub member_count: Option<i64>,
    pub has_external: bool,
}

impl Chat {
    pub fn parse(v: &Value) -> Result<Chat, String> {
        Ok(Chat {
            chat_id: id(v, "chat_id")?,
            kind: chat_kind(v.get("kind").and_then(Value::as_str)),
            name: text_field(v, "name", 256),
            member_count: v
                .get("member_count")
                .and_then(Value::as_i64)
                .filter(|n| *n >= 0),
            has_external: v
                .get("has_external")
                .and_then(Value::as_bool)
                .unwrap_or(false),
        })
    }
}

/// One mention of a message.
#[derive(Debug, Clone, PartialEq)]
pub struct WireMention {
    pub user: Option<Handle>,
    pub name: String,
    pub is_me: bool,
}

/// A checked `Message` (§6), or a feed item's metadata with its body still to fetch.
#[derive(Debug, Clone, PartialEq)]
pub struct WireMessage {
    pub chat_id: String,
    pub source_id: String,
    pub id: Option<String>,
    pub created_ms: i64,
    pub edited_ms: Option<i64>,
    pub sender: WireSender,
    pub is_mine: bool,
    pub chat_kind: Option<crate::channel::SpaceKind>,
    pub chat_name: Option<String>,
    pub thread_id: Option<String>,
    pub reply_to: Option<String>,
    pub root_id: Option<String>,
    pub text: String,
    pub mentions: Vec<WireMention>,
    pub attachments: Vec<Value>,
    pub recalled: bool,
    pub raw: Value,
}

/// The largest `raw` kept (§6).
const MAX_RAW: usize = 64 * 1024;

impl WireMessage {
    /// A full message (`messages.get`, `messages.history`, an inlined body). `max_text`: the
    /// session's `max_text_bytes`.
    pub fn parse(v: &Value, now: i64, max_text: usize) -> Result<WireMessage, String> {
        let mut m = Self::meta(v, now, false)?;
        let text = field(v, "text").and_then(Value::as_str).unwrap_or("");
        if text.len() > max_text {
            return Err("a message text over max_text_bytes".into());
        }
        m.text = text.to_string();
        m.chat_name = text_field(v, "chat_name", 256);
        m.root_id = opt_id(v, "root_id")?;
        m.reply_to = match field(v, "reply_to") {
            Some(r) => opt_id(r, "id")?,
            None => None,
        };
        m.mentions = field(v, "mentions")
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .take(256)
                    .map(|x| WireMention {
                        user: x.get("user").and_then(|u| Handle::parse(u).ok()),
                        name: text_field(x, "name", 256).unwrap_or_default(),
                        is_me: x.get("is_me").and_then(Value::as_bool).unwrap_or(false),
                    })
                    .collect()
            })
            .unwrap_or_default();
        m.attachments = field(v, "attachments")
            .and_then(Value::as_array)
            .map(|a| a.iter().take(64).filter_map(attachment).collect())
            .unwrap_or_default();
        m.recalled = v.get("recalled").and_then(Value::as_bool).unwrap_or(false);
        m.raw = match field(v, "raw") {
            Some(r) if r.to_string().len() <= MAX_RAW => r.clone(),
            _ => Value::Null,
        };
        Ok(m)
    }

    /// A feed item's metadata (§5.2): `is_mine` required there.
    pub fn meta(v: &Value, now: i64, item: bool) -> Result<WireMessage, String> {
        let is_mine = match v.get("is_mine").and_then(Value::as_bool) {
            Some(b) => b,
            None if item => return Err("a message item without is_mine".into()),
            None => false,
        };
        Ok(WireMessage {
            chat_id: id(v, "chat_id")?,
            source_id: id(v, "source_id")?,
            id: opt_id(v, "id")?,
            created_ms: time(v, "created_ms", now)?,
            edited_ms: opt_time(v, "edited_ms", now)?,
            sender: WireSender::parse(v.get("sender").ok_or("a message without a sender")?)?,
            is_mine,
            chat_kind: chat_kind(v.get("chat_kind").and_then(Value::as_str)),
            chat_name: None,
            thread_id: opt_id(v, "thread_id")?,
            reply_to: None,
            root_id: None,
            text: String::new(),
            mentions: Vec::new(),
            attachments: Vec::new(),
            recalled: false,
            raw: Value::Null,
        })
    }
}

/// A checked attachment reference. Preserve supported rich types and metadata; never store
/// inline media or document bodies here.
fn attachment(v: &Value) -> Option<Value> {
    let kind = match v.get("type").and_then(Value::as_str)? {
        "image" => "image",
        "file" => "file",
        k @ ("audio" | "video" | "document" | "webpage" | "commit" | "log") => k,
        _ => return None,
    };
    let key = v
        .get("key")
        .and_then(Value::as_str)
        .filter(|k| !k.is_empty() && k.len() <= 512 && !k.chars().any(char::is_control))?;
    let mut o = json!({"type": kind, "key": key});
    if let Some(n) = text_field(v, "name", 256) {
        o["name"] = n.into();
    }
    if let Some(title) = text_field(v, "title", 512) {
        o["title"] = title.into();
    }
    if let Some(url) = text_field(v, "url", 2048).filter(|u| {
        url::Url::parse(u).is_ok_and(|u| {
            matches!(u.scheme(), "http" | "https")
                && u.username().is_empty()
                && u.password().is_none()
        })
    }) {
        o["url"] = url.into();
    }
    if let Some(s) = v.get("size").and_then(Value::as_u64) {
        o["size"] = s.into();
    }
    if let Some(m) = text_field(v, "mime", 128) {
        o["mime"] = m.into();
    }
    Some(o)
}

/// An `idempotency_key` (§4.4): `[A-Za-z0-9._:-]`, 1–128 bytes. A Hub key that does not fit is
/// hashed, the same way every time.
pub fn idempotency_key(k: &str) -> String {
    let ok = (1..=128).contains(&k.len())
        && k.bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b':' | b'-'));
    if ok {
        k.to_string()
    } else {
        use sha2::Digest;
        format!("h:{}", hex::encode(sha2::Sha256::digest(k.as_bytes())))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// V1: version negotiation.
    #[test]
    fn v1_the_highest_common_version() {
        assert_eq!(negotiate(1, 1, &[1]), Ok(1));
        assert_eq!(negotiate(1, 3, &[1, 2]), Ok(2));
        assert_eq!(negotiate(1, 1, &[1, 2]), Ok(1));
        assert_eq!(negotiate(2, 3, &[1]), Err(vec![1]));
    }

    #[test]
    fn errors_by_reason_then_code() {
        let e = RpcError::parse(&json!({"code": -32007, "message": "slow\ndown",
            "data": {"reason": "rate_limited", "retry_after_ms": 800}}));
        assert_eq!(e.reason, Reason::RateLimited);
        assert_eq!(e.retry_after_ms(), Some(800));
        assert_eq!(e.message, "slowdown");
        // the code when there is no reason; an unknown reason is internal
        let e = RpcError::parse(&json!({"code": -32016, "message": ""}));
        assert_eq!(e.reason, Reason::TargetForbidden);
        let e = RpcError::parse(&json!({"code": -32016, "data": {"reason": "new_thing"}}));
        assert_eq!(e.reason, Reason::Internal);
        for (r, n, c) in REASONS {
            assert_eq!((r.name(), r.code()), (n, c));
        }
    }

    #[test]
    fn frames_are_single_json_rpc_objects() {
        assert!(classify("[]").is_err());
        assert!(classify("{}").is_err());
        assert!(classify("not json").is_err());
        assert_eq!(
            classify(r#"{"jsonrpc":"2.0","id":3,"result":{"a":1}}"#),
            Ok(Incoming::Response {
                id: 3,
                result: Ok(json!({"a": 1}))
            })
        );
        assert!(matches!(
            classify(r#"{"jsonrpc":"2.0","id":4,"method":"messages.send","params":{}}"#),
            Ok(Incoming::Request { .. })
        ));
        assert!(matches!(
            classify(r#"{"jsonrpc":"2.0","method":"push.status","params":{"status":"ok"}}"#),
            Ok(Incoming::Note { .. })
        ));
    }

    fn hello() -> Value {
        json!({"protocol": 1, "server": {"name": "x", "version": "1"}, "realm": "acme-7f3a",
            "time_precision_ms": 1, "time_ms": 1, "features": REQUIRED_FEATURES,
            "limits": {"max_frame_bytes": 4_000_000, "max_batch_items": 100, "write_per_sec": 5,
                       "write_burst": 5},
            "session": {"status": "ok"}, "upstream": {"status": "ok"}, "write_policy": "allowed"})
    }

    #[test]
    fn hello_is_checked_and_limited() {
        let h = Hello::parse(&hello()).expect("hello");
        assert_eq!(
            h.limits.max_frame_bytes, MAX_FRAME,
            "never above the ceiling"
        );
        assert_eq!(h.limits.max_batch_items, 100);
        assert_eq!(h.limits.max_unacked_batches, 16);
        assert_eq!(h.limits.write_per_sec, Some(5.0));
        assert_eq!(h.session.write_policy, WritePolicy::Allowed);
        let mut v = hello();
        v["features"] = json!(["me", "changes"]);
        assert!(matches!(Hello::parse(&v), Err(HelloError::Missing(m)) if m.len() == 4));
        let mut v = hello();
        v["protocol"] = json!(2);
        assert_eq!(Hello::parse(&v), Err(HelloError::Version(2)));
        let mut v = hello();
        v["realm"] = json!("Acme");
        assert!(Hello::parse(&v).is_err());
    }

    #[test]
    fn handles_map_into_the_hub_and_back() {
        let h = Handle::parse(&json!({"kind": "id", "value": "u_1"})).unwrap();
        assert_eq!(h.hub("r"), (super::super::SYSTEM.into(), "r:id:u_1".into()));
        assert_eq!(
            Handle::from_hub(super::super::SYSTEM, "r:id:u_1", "r"),
            Some(h)
        );
        assert_eq!(
            Handle::from_hub(super::super::SYSTEM, "q:id:u_1", "r"),
            None
        );
        let e = Handle::parse(&json!({"kind": "email", "value": "Ada@Example.com"})).unwrap();
        assert_eq!(e.hub("r"), ("mail".into(), "ada@example.com".into()));
        assert!(Handle::parse(&json!({"kind": "email", "value": "a@b@c"})).is_err());
        assert!(Handle::parse(&json!({"kind": "id", "value": "fp:x"})).is_err());
        assert!(Handle::parse(&json!({"kind": "platform", "value": "x"})).is_err());
        let p = Handle::parse(&json!({"kind": "platform", "value": "ou_1", "system": "chat_x"}))
            .unwrap();
        assert_eq!(p.hub("r"), ("chat_x".into(), "ou_1".into()));
    }

    #[test]
    fn messages_are_checked_field_by_field() {
        let now = 1_762_430_400_000;
        let item = json!({"type": "message", "chat_id": "c_42", "source_id": "ev_88",
            "id": "m_901", "created_ms": now, "is_mine": false, "chat_kind": "group",
            "sender": {"id": {"kind": "id", "value": "u_7"}, "display": "Grace"}});
        let m = WireMessage::meta(&item, now, true).expect("item");
        assert_eq!(
            (m.chat_id.as_str(), m.id.as_deref()),
            ("c_42", Some("m_901"))
        );
        let mut bad = item.clone();
        bad.as_object_mut().unwrap().remove("is_mine");
        assert!(WireMessage::meta(&bad, now, true).is_err());
        let mut bad = item.clone();
        bad["created_ms"] = json!(now + MAX_AHEAD_MS + 1);
        assert!(WireMessage::meta(&bad, now, true).is_err());
        let mut bad = item.clone();
        bad["source_id"] = json!("fp:v1:x");
        assert!(WireMessage::meta(&bad, now, true).is_err());
        let mut full = item.clone();
        full["text"] = json!("x".repeat(11));
        assert!(WireMessage::parse(&full, now, 10).is_err());
        full["attachments"] = json!([{"type": "video", "key": "k", "size": 3},
            {"type": "sticker", "key": "s"}]);
        let m = WireMessage::parse(&full, now, 100).expect("full");
        assert_eq!(
            m.attachments,
            vec![json!({"type": "video", "key": "k", "size": 3})]
        );
    }

    #[test]
    fn idempotency_keys_fit_the_alphabet() {
        assert_eq!(idempotency_key("cm_abc:1"), "cm_abc:1");
        let h = idempotency_key("a key with spaces");
        assert!(h.starts_with("h:") && h.len() == 66);
        assert_eq!(h, idempotency_key("a key with spaces"));
    }
}
