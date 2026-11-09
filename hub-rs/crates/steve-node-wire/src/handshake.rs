//! The first three frames of every link (control, JSON, 10 s budget):
//!
//! 1. Hub → Node `challenge {nonce, hub_id, conn_id, proto, hub_time}`
//! 2. Node → Hub `hello {node_id, incarnation, proto, codecs, heads, caps, sig}`, where `sig` is
//!    Ed25519 over `"steve-node-hello-v1" ‖ hub_id ‖ conn_id ‖ nonce ‖ sha256(canonical hello
//!    without sig)`: bound to this Hub, this connection and this nonce, so it replays nowhere.
//! 3. Hub → Node `welcome {gen, proto, paused, heartbeat_ms, hub_key, sig}`, or `error {code}` and
//!    a close. `sig` is the Hub's Ed25519 over `"steve-hub-welcome-v1" ‖ conn_id ‖ nonce ‖
//!    sha256(canonical hello) ‖ sha256(canonical welcome without sig)` (M1): the Hub's identity end
//!    to end, wherever TLS ends (a proxy, a tunnel, plain loopback). A Node that knows its Hub's
//!    key (from pairing, or remembered from the first signed welcome) refuses any other; a Hub of
//!    an earlier release sends neither field, and a Node of one ignores them.
//!
//! Unknown, pending and revoked Nodes and bad signatures all get one `denied` text: a refusal tells
//! the dialler nothing about which it was. `denied` means only that: a Hub that could not decide
//! (its store failed, the handshake ran out of time) answers `internal`, which a Node retries.

use std::time::Duration;

use async_trait::async_trait;
use futures_util::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use steve_node_api::{Capabilities, NodeId, PROTOCOL, StreamHead, canonical_json};

use crate::envelope::{Envelope, Frame, FrameSink, FrameStream, Kind};
use crate::identity::{self, Identity};

pub const BUDGET: Duration = Duration::from_secs(10);
pub const DENIED: &str = "denied";
/// The Hub could not decide (store error, timeout): not a refusal, the Node dials again.
pub const INTERNAL: &str = "internal";

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProtoRange {
    pub min: (u16, u16),
    pub max: (u16, u16),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Challenge {
    pub nonce: String,
    pub hub_id: String,
    pub conn_id: String,
    pub proto: ProtoRange,
    pub hub_time: i64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Hello {
    pub node_id: NodeId,
    pub incarnation: u64,
    pub proto: (u16, u16),
    pub codecs: Vec<String>,
    #[serde(default)]
    pub heads: Vec<StreamHead>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub caps: Option<Capabilities>,
    #[serde(default)]
    pub sig: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Welcome {
    #[serde(rename = "gen")]
    pub generation: u64,
    pub proto: (u16, u16),
    pub paused: bool,
    pub heartbeat_ms: u64,
    /// The Hub's public identity key (hex); empty from a Hub without one.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub hub_key: String,
    /// The pins of the Hub's certificate now (`sha256:…`), when it terminates TLS itself: a Node
    /// that pinned it takes these (L4), signed as the rest of the welcome.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub pins: Vec<String>,
    /// [`welcome_message`] signed by that key (hex).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub sig: String,
}

/// The bytes a welcome signs: this connection, this nonce, the hello it answers, and itself.
pub fn welcome_message(c: &Challenge, h: &Hello, w: &Welcome) -> Vec<u8> {
    let mut unsigned = w.clone();
    unsigned.sig = String::new();
    let hello = serde_json::to_value(h).unwrap_or(Value::Null);
    let welcome = serde_json::to_value(&unsigned).unwrap_or(Value::Null);
    let mut m = b"steve-hub-welcome-v1".to_vec();
    for part in [&c.conn_id, &c.nonce] {
        m.push(0);
        m.extend_from_slice(part.as_bytes());
    }
    m.push(0);
    m.extend_from_slice(&identity::sha256(canonical_json(&hello).as_bytes()));
    m.push(0);
    m.extend_from_slice(&identity::sha256(canonical_json(&welcome).as_bytes()));
    m
}

/// The key a signed welcome proves, `None` for an unsigned one; an error when it is signed and
/// the signature does not hold.
pub fn welcome_key(c: &Challenge, h: &Hello, w: &Welcome) -> Result<Option<[u8; 32]>, Refused> {
    if w.sig.is_empty() && w.hub_key.is_empty() {
        return Ok(None);
    }
    let bad = || refused("invalid", "a welcome whose signature does not hold");
    let key: [u8; 32] = hex::decode(&w.hub_key)
        .ok()
        .and_then(|k| k.try_into().ok())
        .ok_or_else(bad)?;
    let sig = hex::decode(&w.sig).map_err(|_| bad())?;
    if !identity::verify(&key, &welcome_message(c, h, w), &sig) {
        return Err(bad());
    }
    Ok(Some(key))
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Refused {
    pub code: String,
    pub detail: String,
}

impl std::fmt::Display for Refused {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.code, self.detail)
    }
}

fn refused(code: &str, detail: impl Into<String>) -> Refused {
    Refused {
        code: code.into(),
        detail: detail.into(),
    }
}

/// What the Hub's store answers during a handshake.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Admitted {
    pub generation: u64,
    pub paused: bool,
}

#[async_trait]
pub trait Admission: Send + Sync {
    /// The public key of a Node that may connect now (active or paused); `Ok(None)` for anything
    /// else. `Err`: the store could not say (answered `internal`, not `denied`).
    async fn key(&self, node: &NodeId) -> Result<Option<[u8; 32]>, String>;
    /// The signature checked out: take the link (generation + 1, the incarnation recorded), in one
    /// transaction that again requires active or paused. `Ok(None)`: refused after all; `Err`: the
    /// store failed.
    async fn admit(&self, hello: &Hello) -> Result<Option<Admitted>, String>;
}

/// The bytes a hello signs.
pub fn hello_message(c: &Challenge, h: &Hello) -> Vec<u8> {
    let mut unsigned = h.clone();
    unsigned.sig = String::new();
    let v = serde_json::to_value(&unsigned).unwrap_or(Value::Null);
    let mut m = b"steve-node-hello-v1".to_vec();
    for part in [&c.hub_id, &c.conn_id, &c.nonce] {
        m.push(0);
        m.extend_from_slice(part.as_bytes());
    }
    m.push(0);
    m.extend_from_slice(&identity::sha256(canonical_json(&v).as_bytes()));
    m
}

async fn send_ctl(sink: &mut FrameSink, m: &str, p: Value) -> Result<(), Refused> {
    let text = Envelope::ctl(format!("hs_{m}"), m, 0, p)
        .encode()
        .map_err(|e| refused("internal", e.safe_detail))?;
    sink.send(Frame::Text(text))
        .await
        .map_err(|e| refused("offline", e.to_string()))
}

async fn recv_ctl(stream: &mut FrameStream, want: &str) -> Result<Value, Refused> {
    match stream.next().await {
        Some(Ok(Frame::Text(t))) => match Envelope::decode(&t) {
            Some(e) if e.t == Kind::Ctl && e.m == want => Ok(e.p),
            Some(e) if e.t == Kind::Ctl && e.m == "error" => Err(refused(
                e.p["code"].as_str().unwrap_or(DENIED),
                e.p["detail"].as_str().unwrap_or(""),
            )),
            _ => Err(refused("invalid", format!("expected {want}"))),
        },
        // a bare close is a refusal only when it says so (a proxy or a restart closes too)
        Some(Ok(Frame::Close(code, reason))) => Err(refused(
            match code {
                crate::close::UNSUPPORTED => "unsupported",
                crate::close::DENIED => DENIED,
                _ => "offline",
            },
            reason,
        )),
        _ => Err(refused("offline", "link closed during the handshake")),
    }
}

async fn refuse(sink: &mut FrameSink, r: &Refused) {
    let code = match r.code.as_str() {
        "unsupported" => crate::close::UNSUPPORTED,
        "invalid" => crate::close::MALFORMED,
        DENIED => crate::close::DENIED,
        _ => crate::close::INTERNAL,
    };
    let _ = send_ctl(sink, "error", json!({"code": r.code, "detail": r.detail})).await;
    let _ = sink.send(Frame::Close(code, r.code.clone())).await;
    let _ = sink.close().await;
}

/// The Hub's side, its welcome unsigned (a Hub without an identity key). On refusal the error and
/// the close are already sent.
pub async fn accept(
    sink: &mut FrameSink,
    stream: &mut FrameStream,
    hub_id: &str,
    admission: &dyn Admission,
) -> Result<(Hello, Admitted), Refused> {
    accept_as(sink, stream, hub_id, admission, None, &[]).await
}

/// [`accept`], the welcome signed with `hub`, the Hub's identity (M1), announcing `pins`.
pub async fn accept_as(
    sink: &mut FrameSink,
    stream: &mut FrameStream,
    hub_id: &str,
    admission: &dyn Admission,
    hub: Option<&Identity>,
    pins: &[String],
) -> Result<(Hello, Admitted), Refused> {
    let run = async {
        let challenge = Challenge {
            nonce: hex::encode(
                identity::random::<32>().map_err(|e| refused("internal", e.to_string()))?,
            ),
            hub_id: hub_id.into(),
            conn_id: hex::encode(
                identity::random::<12>().map_err(|e| refused("internal", e.to_string()))?,
            ),
            proto: ProtoRange {
                min: (PROTOCOL.0, 0),
                max: PROTOCOL,
            },
            hub_time: crate::now_ms(),
        };
        send_ctl(
            sink,
            "challenge",
            serde_json::to_value(&challenge).unwrap_or_default(),
        )
        .await?;
        let p = recv_ctl(stream, "hello").await?;
        let hello: Hello =
            serde_json::from_value(p).map_err(|_| refused("invalid", "malformed hello"))?;
        if hello.proto.0 != PROTOCOL.0 || !hello.codecs.iter().any(|c| c == "json") {
            return Err(refused("unsupported", "no common protocol"));
        }
        let denied = || refused(DENIED, "this node may not connect");
        let internal = |e: String| {
            // the store's text stays in the Hub's log
            tracing::warn!("node handshake: {e}");
            refused(INTERNAL, "the hub could not decide; dial again")
        };
        let key = admission
            .key(&hello.node_id)
            .await
            .map_err(internal)?
            .ok_or_else(denied)?;
        let sig = hex::decode(&hello.sig).map_err(|_| denied())?;
        if !identity::verify(&key, &hello_message(&challenge, &hello), &sig) {
            return Err(denied());
        }
        let admitted = admission
            .admit(&hello)
            .await
            .map_err(internal)?
            .ok_or_else(denied)?;
        let mut welcome = Welcome {
            generation: admitted.generation,
            proto: PROTOCOL,
            paused: admitted.paused,
            heartbeat_ms: 10_000,
            hub_key: hub.map(|k| hex::encode(k.public())).unwrap_or_default(),
            // announced only when signed: an unsigned list is no one's word
            pins: if hub.is_some() { pins.to_vec() } else { vec![] },
            sig: String::new(),
        };
        if let Some(k) = hub {
            welcome.sig = hex::encode(k.sign(&welcome_message(&challenge, &hello, &welcome)));
        }
        send_ctl(
            sink,
            "welcome",
            serde_json::to_value(&welcome).unwrap_or_default(),
        )
        .await?;
        Ok((hello, admitted))
    };
    let r = match tokio::time::timeout(BUDGET, run).await {
        Ok(r) => r,
        Err(_) => Err(refused(INTERNAL, "handshake too slow")),
    };
    if let Err(e) = &r
        && e.code != "offline"
    {
        refuse(sink, e).await;
    }
    r
}

/// What the Node says about itself in its hello.
pub struct NodeIntro<'a> {
    pub node: &'a NodeId,
    pub identity: &'a Identity,
    pub incarnation: u64,
    pub heads: Vec<StreamHead>,
    pub caps: Option<Capabilities>,
    /// The Hub this Node paired with: a challenge from any other is refused.
    pub hub_id: Option<&'a str>,
    /// That Hub's identity key, when known: a welcome it did not sign is refused (M1).
    pub hub_key: Option<[u8; 32]>,
}

/// The Node's side.
pub async fn dial(
    sink: &mut FrameSink,
    stream: &mut FrameStream,
    intro: NodeIntro<'_>,
) -> Result<(Challenge, Welcome), Refused> {
    dial_as(sink, stream, intro, PROTOCOL).await
}

/// [`dial`] saying `proto` in the hello: a test's stand-in for a Node of an earlier release.
#[doc(hidden)]
pub async fn dial_as(
    sink: &mut FrameSink,
    stream: &mut FrameStream,
    intro: NodeIntro<'_>,
    proto: (u16, u16),
) -> Result<(Challenge, Welcome), Refused> {
    let run = async {
        let p = recv_ctl(stream, "challenge").await?;
        let challenge: Challenge =
            serde_json::from_value(p).map_err(|_| refused("invalid", "malformed challenge"))?;
        if let Some(want) = intro.hub_id
            && want != challenge.hub_id
        {
            return Err(refused(DENIED, "not the hub this node paired with"));
        }
        if challenge.proto.max.0 < proto.0 || challenge.proto.min.0 > proto.0 {
            return Err(refused("unsupported", "no common protocol"));
        }
        let mut hello = Hello {
            node_id: intro.node.clone(),
            incarnation: intro.incarnation,
            proto,
            codecs: vec!["json".into()],
            heads: intro.heads,
            caps: intro.caps,
            sig: String::new(),
        };
        hello.sig = hex::encode(intro.identity.sign(&hello_message(&challenge, &hello)));
        send_ctl(
            sink,
            "hello",
            serde_json::to_value(&hello).unwrap_or_default(),
        )
        .await?;
        let p = recv_ctl(stream, "welcome").await?;
        let welcome: Welcome =
            serde_json::from_value(p).map_err(|_| refused("invalid", "malformed welcome"))?;
        if welcome.proto.0 != proto.0 {
            return Err(refused("unsupported", "no common protocol"));
        }
        let signed = welcome_key(&challenge, &hello, &welcome)?;
        if let Some(want) = intro.hub_key
            && signed != Some(want)
        {
            return Err(refused(DENIED, "not the hub this node paired with"));
        }
        Ok((challenge, welcome))
    };
    match tokio::time::timeout(BUDGET, run).await {
        Ok(r) => r,
        Err(_) => Err(refused("offline", "handshake too slow")),
    }
}
