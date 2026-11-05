//! The first three frames of every link (control, JSON, 10 s budget):
//!
//! 1. Hub → Node `challenge {nonce, hub_id, conn_id, proto, hub_time}`
//! 2. Node → Hub `hello {node_id, incarnation, proto, codecs, heads, caps, sig}`, where `sig` is
//!    Ed25519 over `"steve-node-hello-v1" ‖ hub_id ‖ conn_id ‖ nonce ‖ sha256(canonical hello
//!    without sig)`: bound to this Hub, this connection and this nonce, so it replays nowhere.
//! 3. Hub → Node `welcome {gen, proto, paused, heartbeat_ms}`, or `error {code}` and a close.
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

/// The Hub's side. On refusal the error and the close are already sent.
pub async fn accept(
    sink: &mut FrameSink,
    stream: &mut FrameStream,
    hub_id: &str,
    admission: &dyn Admission,
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
        let welcome = Welcome {
            generation: admitted.generation,
            proto: PROTOCOL,
            paused: admitted.paused,
            heartbeat_ms: 10_000,
        };
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
}

/// The Node's side.
pub async fn dial(
    sink: &mut FrameSink,
    stream: &mut FrameStream,
    intro: NodeIntro<'_>,
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
        if challenge.proto.max.0 < PROTOCOL.0 || challenge.proto.min.0 > PROTOCOL.0 {
            return Err(refused("unsupported", "no common protocol"));
        }
        let mut hello = Hello {
            node_id: intro.node.clone(),
            incarnation: intro.incarnation,
            proto: PROTOCOL,
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
        if welcome.proto.0 != PROTOCOL.0 {
            return Err(refused("unsupported", "no common protocol"));
        }
        Ok((challenge, welcome))
    };
    match tokio::time::timeout(BUDGET, run).await {
        Ok(r) => r,
        Err(_) => Err(refused("offline", "handshake too slow")),
    }
}
