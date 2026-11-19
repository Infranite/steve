//! Authenticated bootstrap using the self-contained upgrade-v1 schema.
//!
//! Hub challenge → signed Node hello → signed Hub welcome. The signed welcome confirms both
//! application protocol numbers and the mode: exact equality is Normal; every mismatch is
//! Upgrade, without closing an authenticated connection. Capabilities and stream heads are
//! ordinary data, never part of bootstrap. Existing identity pins, replay binding, generation
//! fencing and the 10-second handshake budget remain in force.
//!
//! Unknown/revoked Nodes and bad signatures receive the same denied error. A store failure is
//! internal (retry), not an authorization verdict. Signed reports carry a bounded diagnostic
//! instead of linking, receive an empty noted acknowledgement, and close.

use std::time::Duration;

use async_trait::async_trait;
use futures_util::{SinkExt, StreamExt};
use serde_json::Value;
use steve_node_api::upgrade_v1::{self as upgrade, Mode, UPGRADE_PROTOCOL};
use steve_node_api::{NodeId, PROTOCOL};

use crate::envelope::{Envelope, Frame, FrameSink, FrameStream, Kind};
use crate::identity::{self, Identity};

pub const BUDGET: Duration = Duration::from_millis(upgrade::HANDSHAKE_BUDGET_MS);
pub const DENIED: &str = "denied";
/// The Hub could not decide (store error, timeout): not a refusal, the Node dials again.
pub const INTERNAL: &str = "internal";
/// The handshake ended with a [`Report`] noted, not a link.
pub const REPORTED: &str = "reported";
/// The longest reason a [`Report`] carries, in bytes.
pub use steve_node_api::upgrade_v1::REPORT_MAX;

pub use steve_node_api::upgrade_v1::{Challenge, Hello, Report, Welcome};
pub use steve_node_api::upgrade_v1::{hello_message, report_message, welcome_message};

/// `s` cut to at most [`REPORT_MAX`] bytes, on a character boundary.
pub fn clip_report(s: &str) -> &str {
    let mut end = s.len().min(REPORT_MAX);
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

/// The key a signed welcome proves, `None` for an unsigned one; an error when it is signed and
/// the signature does not hold.
pub fn welcome_key(c: &Challenge, h: &Hello, w: &Welcome) -> Result<Option<[u8; 32]>, Refused> {
    if w.sig.is_empty() && w.hub_key.is_empty() {
        return Ok(None);
    }
    let bad = || refused("invalid", "a welcome whose signature does not hold");
    let key = upgrade::decode_hex::<32>(&w.hub_key).ok_or_else(bad)?;
    let sig = upgrade::decode_hex::<64>(&w.sig).ok_or_else(bad)?;
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
    /// A Node whose key may connect said why it could not link ([`Report`], its signature checked,
    /// `error` at most [`REPORT_MAX`] bytes).
    async fn reported(&self, _node: &NodeId, _error: &str) {}
}

async fn send_ctl(sink: &mut FrameSink, m: &str, p: Value) -> Result<(), Refused> {
    let text = Envelope::ctl(format!("hs_{m}"), m, 0, p)
        .encode()
        .map_err(|e| refused("internal", e.detail))?;
    sink.send(Frame::Text(text))
        .await
        .map_err(|e| refused("offline", e.to_string()))
}

async fn recv_ctl(stream: &mut FrameStream, want: &str) -> Result<Value, Refused> {
    recv_ctl_of(stream, &[want]).await.map(|(_, p)| p)
}

/// The next control frame, one of `want`: which, and its payload.
async fn recv_ctl_of(stream: &mut FrameStream, want: &[&str]) -> Result<(String, Value), Refused> {
    let want0 = want[0];
    match stream.next().await {
        Some(Ok(Frame::Text(t))) => match Envelope::decode(&t) {
            Some(e)
                if e.v == UPGRADE_PROTOCOL && e.t == Kind::Ctl && want.contains(&e.m.as_str()) =>
            {
                Ok((e.m, e.p))
            }
            Some(e) if e.v == UPGRADE_PROTOCOL && e.t == Kind::Ctl && e.m == upgrade::ERROR => {
                let error: upgrade::Error = serde_json::from_value(e.p)
                    .map_err(|_| refused("invalid", "malformed bootstrap error"))?;
                let code = serde_json::to_value(error.code).unwrap();
                Err(refused(code.as_str().unwrap(), error.detail))
            }
            _ => Err(refused("invalid", format!("expected {want0}"))),
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
    let kind =
        serde_json::from_value(Value::String(r.code.clone())).unwrap_or(upgrade::Code::Internal);
    let error = upgrade::Error::new(kind, r.detail.clone());
    let _ = send_ctl(sink, upgrade::ERROR, serde_json::to_value(error).unwrap()).await;
    let _ = sink.send(Frame::Close(code, r.code.clone())).await;
    let _ = sink.close().await;
}

async fn receive_hello(
    sink: &mut FrameSink,
    stream: &mut FrameStream,
    admission: &dyn Admission,
    challenge: &Challenge,
) -> Result<(Hello, Admitted), Refused> {
    let (m, p) = recv_ctl_of(stream, &[upgrade::HELLO, upgrade::REPORT]).await?;
    if m == upgrade::REPORT {
        return Err(take_report(sink, admission, challenge, p).await);
    }
    let hello: Hello =
        serde_json::from_value(p).map_err(|_| refused("invalid", "malformed hello"))?;
    if hello.upgrade_protocol != UPGRADE_PROTOCOL {
        return Err(refused("unsupported", "no supported bootstrap protocol"));
    }
    let node = NodeId::new(hello.node_id.clone())
        .map_err(|_| refused(DENIED, "this node may not connect"))?;
    let denied = || refused(DENIED, "this node may not connect");
    let internal = |e: String| {
        // the store's text stays in the Hub's log
        tracing::warn!("node handshake: {e}");
        refused(INTERNAL, "the hub could not decide; dial again")
    };
    let key = admission
        .key(&node)
        .await
        .map_err(internal)?
        .ok_or_else(denied)?;
    let sig = upgrade::decode_hex::<64>(&hello.sig).ok_or_else(denied)?;
    if !identity::verify(&key, &hello_message(challenge, &hello), &sig) {
        return Err(denied());
    }
    let admitted = admission
        .admit(&hello)
        .await
        .map_err(internal)?
        .ok_or_else(denied)?;
    Ok((hello, admitted))
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
    accept_version(sink, stream, hub_id, admission, hub, pins, PROTOCOL).await
}

/// Test seam for a Hub built with a different ordinary protocol, without global state.
#[doc(hidden)]
#[allow(clippy::too_many_arguments)]
pub async fn accept_version(
    sink: &mut FrameSink,
    stream: &mut FrameStream,
    hub_id: &str,
    admission: &dyn Admission,
    hub: Option<&Identity>,
    pins: &[String],
    proto: u32,
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
            proto,
            upgrade_protocol: UPGRADE_PROTOCOL,
            hub_time: crate::now_ms(),
        };
        send_ctl(
            sink,
            upgrade::CHALLENGE,
            serde_json::to_value(&challenge).unwrap_or_default(),
        )
        .await?;
        let (hello, admitted) = receive_hello(sink, stream, admission, &challenge).await?;
        let mut welcome = Welcome {
            generation: admitted.generation,
            proto,
            upgrade_protocol: UPGRADE_PROTOCOL,
            mode: Mode::between(proto, hello.proto),
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
            upgrade::WELCOME,
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
        && e.code != REPORTED
    {
        refuse(sink, e).await;
    }
    r
}

/// A [`Report`] in place of a hello: noted when its signature holds, answered `noted`, closed. The
/// refusal to return: [`REPORTED`] once noted (the link ends there), else why it was not.
async fn take_report(
    sink: &mut FrameSink,
    admission: &dyn Admission,
    challenge: &Challenge,
    p: Value,
) -> Refused {
    let Ok(report) = serde_json::from_value::<Report>(p) else {
        return refused("invalid", "malformed report");
    };
    if report.error.len() > REPORT_MAX {
        return refused("invalid", "report too long");
    }
    let denied = || refused(DENIED, "this node may not connect");
    let Ok(node) = NodeId::new(report.node_id.clone()) else {
        return denied();
    };
    let key = match admission.key(&node).await {
        Ok(Some(k)) => k,
        Ok(None) => return denied(),
        Err(e) => {
            tracing::warn!("node report: {e}");
            return refused(INTERNAL, "the hub could not decide; dial again");
        }
    };
    let ok = upgrade::decode_hex::<64>(&report.sig)
        .is_some_and(|sig| identity::verify(&key, &report_message(challenge, &report), &sig));
    if !ok {
        return denied();
    }
    admission.reported(&node, &report.error).await;
    let _ = send_ctl(
        sink,
        upgrade::NOTED,
        serde_json::to_value(upgrade::Noted {}).unwrap(),
    )
    .await;
    let _ = sink
        .send(Frame::Close(upgrade::close::GOODBYE, REPORTED.into()))
        .await;
    let _ = sink.close().await;
    refused(REPORTED, report.error)
}

fn check_challenge(challenge: &Challenge, hub_id: Option<&str>) -> Result<(), Refused> {
    if hub_id.is_some_and(|want| want != challenge.hub_id) {
        return Err(refused(DENIED, "not the hub this node paired with"));
    }
    if challenge.upgrade_protocol != UPGRADE_PROTOCOL {
        return Err(refused("unsupported", "no supported bootstrap protocol"));
    }
    Ok(())
}

fn check_welcome(
    challenge: &Challenge,
    hello: &Hello,
    welcome: &Welcome,
    hub_key: Option<[u8; 32]>,
) -> Result<(), Refused> {
    if welcome.proto != challenge.proto
        || welcome.upgrade_protocol != UPGRADE_PROTOCOL
        || welcome.mode != Mode::between(hello.proto, challenge.proto)
    {
        return Err(refused(
            "invalid",
            "welcome does not confirm the authenticated protocol mode",
        ));
    }
    let signed = welcome_key(challenge, hello, welcome)?;
    if hub_key.is_some_and(|want| signed != Some(want)) {
        return Err(refused(DENIED, "not the hub this node paired with"));
    }
    Ok(())
}

/// The Node's side of a [`Report`]: answers the challenge with `error` (clipped to [`REPORT_MAX`])
/// instead of a hello, and waits for the Hub to say it noted it. `hub_id`: the Hub this Node paired
/// with; a challenge from any other is refused.
pub async fn report(
    sink: &mut FrameSink,
    stream: &mut FrameStream,
    node: &NodeId,
    identity: &Identity,
    hub_id: Option<&str>,
    error: &str,
) -> Result<(), Refused> {
    let run = async {
        let p = recv_ctl(stream, upgrade::CHALLENGE).await?;
        let challenge: Challenge =
            serde_json::from_value(p).map_err(|_| refused("invalid", "malformed challenge"))?;
        check_challenge(&challenge, hub_id)?;
        let mut r = Report {
            node_id: node.to_string(),
            error: clip_report(error).to_string(),
            sig: String::new(),
        };
        r.sig = hex::encode(identity.sign(&report_message(&challenge, &r)));
        send_ctl(
            sink,
            upgrade::REPORT,
            serde_json::to_value(&r).unwrap_or_default(),
        )
        .await?;
        let noted = recv_ctl(stream, upgrade::NOTED).await?;
        serde_json::from_value::<upgrade::Noted>(noted)
            .map_err(|_| refused("invalid", "malformed report acknowledgement"))?;
        Ok(())
    };
    match tokio::time::timeout(BUDGET, run).await {
        Ok(r) => r,
        Err(_) => Err(refused("offline", "handshake too slow")),
    }
}

/// What the Node says about itself in its hello.
pub struct NodeIntro<'a> {
    pub node: &'a NodeId,
    pub identity: &'a Identity,
    pub incarnation: u64,
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
    proto: u32,
) -> Result<(Challenge, Welcome), Refused> {
    let run = async {
        let p = recv_ctl(stream, upgrade::CHALLENGE).await?;
        let challenge: Challenge =
            serde_json::from_value(p).map_err(|_| refused("invalid", "malformed challenge"))?;
        check_challenge(&challenge, intro.hub_id)?;
        let mut hello = Hello {
            node_id: intro.node.to_string(),
            incarnation: intro.incarnation,
            proto,
            upgrade_protocol: UPGRADE_PROTOCOL,
            sig: String::new(),
        };
        hello.sig = hex::encode(intro.identity.sign(&hello_message(&challenge, &hello)));
        send_ctl(
            sink,
            upgrade::HELLO,
            serde_json::to_value(&hello).unwrap_or_default(),
        )
        .await?;
        let p = recv_ctl(stream, upgrade::WELCOME).await?;
        let welcome: Welcome =
            serde_json::from_value(p).map_err(|_| refused("invalid", "malformed welcome"))?;
        check_welcome(&challenge, &hello, &welcome, intro.hub_key)?;
        Ok((challenge, welcome))
    };
    match tokio::time::timeout(BUDGET, run).await {
        Ok(r) => r,
        Err(_) => Err(refused("offline", "handshake too slow")),
    }
}
