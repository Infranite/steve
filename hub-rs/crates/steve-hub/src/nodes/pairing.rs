//! Pairing and the owner's controls.
//!
//! - `POST /api/nodes/pairings` (signed in): a one-time code `stv1-…` (128 random bits), shown once,
//!   valid 10 minutes. Only its sha256 is stored: the code is high-entropy, so no pepper is needed.
//! - `POST /node/pair {code, pubkey, name, sig}`: the Node proves it holds the key it registers
//!   (`sig` over `"steve-pair-v1" ‖ sha256(code) ‖ pubkey`); the claim is one atomic update. Wrong,
//!   used and expired codes all get the same `denied`, and claims are rate-limited per code (see [`Limiter`]).
//!   The Node is `pending` until its owner confirms the fingerprint the CLI printed.
//! - `GET /api/nodes`, `POST /api/nodes/{id}/confirm|pause|resume|rename|revoke|limit|context`: the owner's only.

use std::sync::Arc;
use std::time::Duration;
use steve_runtime::policy::{Limiter as RateLimiter, RateLimit};

use axum::Json;
use axum::extract::{Path, State};
use axum::http::HeaderMap;
use axum::response::{IntoResponse, Response};
use serde::Deserialize;
use serde_json::{Value, json};
use steve_modules::Caller;
use steve_node_wire::identity;
use steve_store::StoreError;

use super::{NodeService, Peer, now};
use crate::error::json_response;

pub const CODE_TTL_MS: i64 = 10 * 60 * 1000;

/// Token buckets for `/node/pair`, per code and one for all. Not per address: every remote claim
/// comes through the proxy on the Hub's machine with the same loopback peer, so a per-address
/// bucket would be one bucket that anyone could drain. Codes carry 128 bits, so brute force is no
/// concern; the per-code bucket stops hammering one code, and the global one, generous enough not
/// to starve real claims, only bounds the work garbage claims cost.
pub struct Limiter {
    buckets: RateLimiter<String>,
    global: RateLimiter<()>,
}
const PER_CODE: RateLimit = RateLimit::Gcra {
    units: std::num::NonZeroU32::MIN,
    per: Duration::from_secs(10),
    burst: 5,
};
const GLOBAL: RateLimit = RateLimit::Gcra {
    units: std::num::NonZeroU32::MIN,
    per: Duration::from_millis(20),
    burst: 200,
};
impl Default for Limiter {
    fn default() -> Self {
        Self {
            buckets: RateLimiter::new(PER_CODE),
            global: RateLimiter::new(GLOBAL),
        }
    }
}
impl Limiter {
    /// Global admission is consumed first, even when this particular code is exhausted.
    pub fn take(&self, code_hash: &str) -> bool {
        self.global.check(&()).is_ok() && self.buckets.check(&code_hash.to_owned()).is_ok()
    }
    #[cfg(test)]
    fn with_clock(clock: Arc<dyn steve_runtime::Clock>) -> Self {
        Self {
            buckets: RateLimiter::with_clock(PER_CODE, clock.clone()),
            global: RateLimiter::with_clock(GLOBAL, clock),
        }
    }
}

fn base32(b: &[u8]) -> String {
    const A: &[u8] = b"abcdefghijklmnopqrstuvwxyz234567";
    let (mut out, mut acc, mut bits) = (String::new(), 0u32, 0);
    for &x in b {
        acc = (acc << 8) | x as u32;
        bits += 8;
        while bits >= 5 {
            bits -= 5;
            out.push(A[((acc >> bits) & 31) as usize] as char);
        }
    }
    if bits > 0 {
        out.push(A[((acc << (5 - bits)) & 31) as usize] as char);
    }
    out
}

pub fn code_hash(code: &str) -> String {
    hex::encode(identity::sha256(code.trim().as_bytes()))
}

fn err(status: u16, e: &str) -> Response {
    json_response(status, &json!({ "error": e }))
}

fn denied() -> Response {
    err(403, "denied")
}

async fn blocking<T: Send + 'static>(
    f: impl FnOnce() -> Result<T, StoreError> + Send + 'static,
) -> Result<T, Box<Response>> {
    match tokio::task::spawn_blocking(f).await {
        Ok(Ok(v)) => Ok(v),
        Ok(Err(e)) => {
            tracing::error!("nodes: {e}");
            Err(Box::new(err(500, "internal")))
        }
        Err(_) => Err(Box::new(err(500, "internal"))),
    }
}

/// A new one-time code made by `me`, and when it expires. `confirmed`: the Node it pairs is active
/// at once (the Hub's own remote install, see `install.rs`).
pub async fn new_code_until(
    store: &steve_store::Store,
    me: &str,
    confirmed: bool,
) -> Result<(String, i64), StoreError> {
    let raw = identity::random::<16>().map_err(|e| StoreError::Conflict(e.to_string()))?;
    let code = format!("stv1-{}", base32(&raw));
    let (hash, me, store) = (code_hash(&code), me.to_string(), store.clone());
    let t = now();
    let expires = t + CODE_TTL_MS;
    let what = if confirmed {
        "install_pairing_created"
    } else {
        "pairing_created"
    };
    tokio::task::spawn_blocking(move || {
        store.write(|tx| {
            steve_store::nodes::create_pairing_as(tx, &hash, &me, t, expires, confirmed)?;
            steve_store::nodes::audit(tx, None, Some(&me), what, None, t)
        })
    })
    .await
    .map_err(|e| StoreError::Conflict(e.to_string()))??;
    Ok((code, expires))
}

pub async fn new_code(
    store: &steve_store::Store,
    me: &str,
    confirmed: bool,
) -> Result<String, StoreError> {
    Ok(new_code_until(store, me, confirmed).await?.0)
}

pub async fn create(State(svc): State<Arc<NodeService>>, caller: Caller) -> Response {
    let (code, expires) = match new_code_until(&svc.store, &caller.me, false).await {
        Ok(v) => v,
        Err(e) => {
            tracing::error!("nodes: {e}");
            return err(500, "internal");
        }
    };
    json_response(
        200,
        &json!({ "code": code, "expires": expires, "pin": svc.pin, "hub_id": svc.hub_id }),
    )
}

#[derive(Deserialize)]
pub struct PairBody {
    code: String,
    pubkey: String,
    name: String,
    sig: String,
}

pub async fn claim(
    State(svc): State<Arc<NodeService>>,
    Peer(peer): Peer,
    headers: HeaderMap,
    Json(b): Json<PairBody>,
) -> Response {
    if !svc.ingress.allows(peer, &headers) {
        return axum::http::StatusCode::FORBIDDEN.into_response();
    }
    if !svc.limiter.take(&code_hash(&b.code)) {
        return err(429, "slow down");
    }
    let Some(pubkey) = hex::decode(&b.pubkey)
        .ok()
        .and_then(|v| <[u8; 32]>::try_from(v).ok())
    else {
        return denied();
    };
    let Ok(sig) = hex::decode(&b.sig) else {
        return denied();
    };
    let code = b.code.trim().to_string();
    if !identity::verify(&pubkey, &identity::pair_message(&code, &pubkey), &sig) {
        return denied();
    }
    let name: String = b
        .name
        .chars()
        .filter(|c| !c.is_control())
        .take(64)
        .collect();
    let name = if name.trim().is_empty() {
        "node".to_string()
    } else {
        name
    };
    let Ok(raw) = identity::random::<8>() else {
        return err(500, "internal");
    };
    let node_id = format!("n_{}", hex::encode(raw));
    let fingerprint = identity::fingerprint(&pubkey);
    let (store, hash, id) = (svc.store.clone(), code_hash(&code), node_id.clone());
    let owner = blocking(move || {
        store.write(|tx| {
            let n = steve_store::nodes::NewNode {
                node_id: &id,
                name: &name,
                pubkey: &pubkey,
                fingerprint: &fingerprint,
            };
            // a key that is already paired is refused like a bad code (by the key itself: a
            // fingerprint stored before M4 is shorter)
            if store.read(|r| {
                steve_store::node_directory::registered_key(r, &pubkey, Some(&fingerprint))
            })? {
                return Ok(None);
            }
            let owner = steve_store::nodes::claim_pairing(tx, &hash, &n, now())?;
            if owner.is_some() {
                steve_store::nodes::audit(tx, Some(&id), owner.as_deref(), "paired", None, now())?;
            }
            Ok(owner)
        })
    })
    .await;
    match owner {
        Ok(Some(_)) => json_response(
            200,
            &json!({
                "node_id": node_id,
                "hub_id": svc.hub_id,
                // the Node refuses welcomes this key did not sign (M1)
                "hub_key": svc.identity.get().map(|k| hex::encode(k.public())),
            }),
        ),
        Ok(None) => denied(),
        Err(r) => *r,
    }
}

#[derive(Deserialize)]
pub struct RotateBody {
    node_id: String,
    new_pubkey: String,
    /// [`identity::rotate_message`] signed by the key the Hub knows, and by the new one.
    sig_old: String,
    sig_new: String,
}

/// `POST /node/rotate` (M4): a Node replaces its key. Both keys sign the change (the old one says
/// it is this Node, the new one that the Node holds it), bound to this Hub; the Hub swaps the key
/// where the old one is still current, closes the link of the old key, and audits it. The Node
/// then connects with its new key. A Node that lost its key cannot do this: it is revoked and
/// paired again.
pub async fn rotate(
    State(svc): State<Arc<NodeService>>,
    Peer(peer): Peer,
    headers: HeaderMap,
    Json(b): Json<RotateBody>,
) -> Response {
    if !svc.ingress.allows(peer, &headers) {
        return axum::http::StatusCode::FORBIDDEN.into_response();
    }
    if !svc
        .limiter
        .take(&code_hash(&format!("rotate:{}", b.node_id)))
    {
        return err(429, "slow down");
    }
    let key = |h: &str| {
        hex::decode(h)
            .ok()
            .and_then(|v| <[u8; 32]>::try_from(v).ok())
    };
    let (Some(new), Ok(sig_old), Ok(sig_new)) = (
        key(&b.new_pubkey),
        hex::decode(&b.sig_old),
        hex::decode(&b.sig_new),
    ) else {
        return denied();
    };
    let (store, node) = (svc.store.clone(), b.node_id.clone());
    let old =
        blocking(move || store.read(|r| steve_store::node_directory::link_key(r, &node))).await;
    let old = match old {
        Ok(Some(k)) => match <[u8; 32]>::try_from(k.as_slice()) {
            Ok(k) => k,
            Err(_) => return denied(),
        },
        Ok(None) => return denied(),
        Err(r) => return *r,
    };
    let msg = identity::rotate_message(&svc.hub_id, &b.node_id, &old, &new);
    if new == old
        || !identity::verify(&old, &msg, &sig_old)
        || !identity::verify(&new, &msg, &sig_new)
    {
        return denied();
    }
    let fingerprint = identity::fingerprint(&new);
    let (store, node, fp) = (svc.store.clone(), b.node_id.clone(), fingerprint.clone());
    let swapped = blocking(move || {
        store.write(|tx| {
            let swapped = store
                .write(|w| steve_store::node_directory::rotate_key(w, &node, &old, &new, &fp))?;
            if swapped {
                steve_store::nodes::audit(tx, Some(&node), None, "key_rotated", Some(&fp), now())?;
            }
            Ok(swapped)
        })
    })
    .await;
    match swapped {
        Ok(true) => {
            // the link of the old key goes; the Node dials again with the new one
            let _g = svc.links.lock().await;
            svc.registry.kill(&b.node_id, "key rotated");
            json_response(200, &json!({ "fingerprint": fingerprint }))
        }
        Ok(false) => denied(),
        Err(r) => *r,
    }
}

/// How the Node's latest link reached the Hub (G1), with its latency while it is up.
fn link_view(svc: &NodeService, n: &steve_store::nodes::NodeRow, linked: bool) -> Value {
    let rtt = svc.registry.rtt(&n.node_id).filter(|_| linked);
    match n
        .link
        .as_deref()
        .and_then(|l| serde_json::from_str::<Value>(l).ok())
    {
        Some(mut l) => {
            l["latency_ms"] = json!(rtt.map(|r| r.0));
            l["latency_at"] = json!(rtt.map(|r| r.1));
            l
        }
        None => Value::Null,
    }
}

/// The Node's protocol and whether its link only serves upgrades; returns the latter.
fn protocol_view(svc: &NodeService, node: &str, v: &mut Value) -> bool {
    let upgrade_only = svc
        .registry
        .conn(node)
        .is_some_and(|c| !c.is_closed() && c.is_upgrade());
    v["protocol"] = json!(svc.fleet.protocol(node));
    v["protocol_mode"] = json!(if upgrade_only { "upgrade" } else { "normal" });
    let status = upgrade_only
        .then(|| svc.fleet.protocol_status(node))
        .flatten();
    v["protocol_status"] = json!(status.map(|(_, text)| text));
    v["protocol_older"] = json!(status.map(|(older, _)| older));
    upgrade_only
}

pub async fn list(State(svc): State<Arc<NodeService>>, caller: Caller) -> Response {
    let store = svc.store.clone();
    // each with how many of its turns run and wait, against its `max_sessions`
    let rows = match blocking(move || {
        store.read(|c| {
            let default = steve_store::nodes::upgrade_default(c)?;
            let rows = steve_store::nodes::list_for_owner(c, &caller.me)?
                .into_iter()
                .map(|n| {
                    let latest = steve_store::nodes::upgrade_latest(c, &n.node_id)?;
                    let removal = steve_store::nodes::removal_latest(c, &n.node_id)?;
                    Ok((
                        steve_store::runtime::session_room(c, &n.node_id)?,
                        n,
                        latest,
                        removal,
                    ))
                })
                .collect::<steve_store::Result<Vec<_>>>()?;
            Ok((default, rows))
        })
    })
    .await
    {
        Ok(r) => r,
        Err(r) => return *r,
    };
    let (default, rows) = rows;
    let up = svc.upgrades.get().cloned();
    let rm = svc.removals.get().cloned();
    let items: Vec<Value> = rows
        .into_iter()
        .map(|(room, n, latest, removal)| {
            let linked = svc.registry.current(&n.node_id).is_some();
            let mut v = serde_json::to_value(&n).unwrap_or(Value::Null);
            v["linked"] = json!(linked);
            v["running"] = json!(room.running);
            v["waiting"] = json!(room.waiting);
            // the limit in force: the Hub's own Node keeps it on the Hub's device row
            v["max_sessions"] = json!(room.max);
            // how its latest link reached the Hub (G1): kept while it is away (`linked` says
            // whether it is up); its latency only while it is
            v["link"] = link_view(&svc, &n, linked);
            // what its machine is, as it last said (G2)
            v["host"] = n
                .host
                .as_deref()
                .and_then(|h| serde_json::from_str::<Value>(h).ok())
                .unwrap_or(Value::Null);
            if !linked && let Some((at, why)) = svc.fleet.link_failure(&n.node_id) {
                v["link_error"] = json!({ "at": at, "error": why });
            }
            let upgrade_only = protocol_view(&svc, &n.node_id, &mut v);
            // from the key, at today's length: a Node paired before M4 stored 16 digits
            if let Ok(k) = <[u8; 32]>::try_from(n.pubkey.as_slice()) {
                v["fingerprint"] = json!(identity::fingerprint(&k));
            }
            if let Some(rm) = &rm {
                v["removal"] = rm.view(&n, linked, removal.as_ref());
                // asked in the background: the next list shows whether it can remove itself
                if linked && !upgrade_only && !n.local && rm.seen(&n.node_id).is_none() {
                    let (rm, id) = (rm.clone(), n.node_id.clone());
                    tokio::spawn(async move {
                        rm.look(&id).await;
                    });
                }
            }
            if let Some(up) = &up {
                v["upgrade"] = up.view(&n, &default, latest.as_ref());
                // asked in the background: the next list shows it
                if linked && !n.local && up.seen(&n.node_id).is_none() {
                    let (up, id) = (up.clone(), n.node_id.clone());
                    tokio::spawn(async move {
                        up.look(&id).await;
                    });
                }
            }
            v
        })
        .collect();
    json_response(
        200,
        &json!({
            "nodes": items,
            "upgrade_default": default,
            "hub_build": up.as_ref().and_then(|u| u.hub().cloned()),
        }),
    )
}

#[derive(Deserialize, Default)]
pub struct RenameBody {
    #[serde(default)]
    name: String,
    /// `limit`: how many sessions the Node runs at once (1–256).
    #[serde(default)]
    max_sessions: Option<i64>,
    /// `context`: whether the owner's sessions on the Node load the owner's personal harness
    /// configuration there: `on`, `off`, or `inherit` (the Hub's default, `/api/node-context`).
    #[serde(default)]
    owner_context: Option<String>,
    /// `upgrade_policy`: `auto` | `notify` | `off`, or empty for the Hub's default.
    #[serde(default)]
    policy: Option<String>,
}

pub async fn act(
    State(svc): State<Arc<NodeService>>,
    caller: Caller,
    Path((id, action)): Path<(String, String)>,
    body: Option<Json<RenameBody>>,
) -> Response {
    let (store, me, node) = (svc.store.clone(), caller.me.clone(), id.clone());
    let (name, max_sessions, choice, policy) = body
        .map(|b| (b.0.name, b.0.max_sessions, b.0.owner_context, b.0.policy))
        .unwrap_or_default();
    let policy = policy.filter(|p| !p.is_empty());
    let detail_policy = policy.clone();
    let action2 = action.clone();
    let done = blocking(move || {
        store.write(|tx| {
            use steve_store::nodes::*;
            let ok = match action2.as_str() {
                "confirm" => set_state(tx, &node, &me, "pending", "active")?,
                "pause" => set_state(tx, &node, &me, "active", "paused")?,
                "resume" => set_state(tx, &node, &me, "paused", "active")?,
                "revoke" => revoke(tx, &node, &me, now())?,
                "limit" => match max_sessions {
                    Some(n) => set_max_sessions(tx, &node, &me, n)?,
                    None => false,
                },
                "context" => match choice.as_deref() {
                    Some("on") => set_owner_context(tx, &node, &me, Some(true))?,
                    Some("off") => set_owner_context(tx, &node, &me, Some(false))?,
                    Some("inherit") => set_owner_context(tx, &node, &me, None)?,
                    _ => false,
                },
                "upgrade_policy" => set_upgrade_policy(tx, &node, &me, policy.as_deref())?,
                "rename" => {
                    let n: String = name.chars().filter(|c| !c.is_control()).take(64).collect();
                    !n.trim().is_empty() && rename(tx, &node, &me, &n)?
                }
                _ => return Ok(None),
            };
            if ok {
                let detail = match action2.as_str() {
                    "limit" => max_sessions.map(|n| n.to_string()),
                    "context" => choice.clone(),
                    "upgrade_policy" => {
                        Some(detail_policy.clone().unwrap_or_else(|| "default".into()))
                    }
                    _ => None,
                };
                audit(
                    tx,
                    Some(&node),
                    Some(&me),
                    &action2,
                    detail.as_deref(),
                    now(),
                )?;
            }
            Ok(Some(ok))
        })
    })
    .await;
    match done {
        Err(r) => *r,
        Ok(None) => err(404, "not found"),
        // not the owner's, or not in a state that allows it: one answer
        Ok(Some(false)) => err(409, "not possible"),
        Ok(Some(true)) => {
            // after the commit, under the lock a link is installed with: a link being installed
            // either saw this change in the store or is in the registry by now
            if action == "revoke" {
                svc.revoked(&id).await;
                return json_response(200, &json!({ "ok": true }));
            }
            let _g = svc.links.lock().await;
            match action.as_str() {
                "pause" | "resume" => {
                    let paused = action == "pause";
                    if let Some(c) = svc.registry.conn(&id) {
                        c.notify("paused", json!({"paused": paused}));
                    }
                    if let Some(rt) = svc.fleet.runtime_for(&id) {
                        rt.set_paused(paused);
                    }
                }
                _ => {}
            }
            json_response(200, &json!({ "ok": true }))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rate_table_preserves_burst_refill_and_atomic_rejection() {
        for (quota, burst, unit_ms) in [(PER_CODE, 5, 10_000), (GLOBAL, 200, 20)] {
            let clock = Arc::new(steve_runtime::FakeClock::new(0));
            let bucket = RateLimiter::with_clock(quota, clock.clone());
            for _ in 0..burst {
                assert!(bucket.check(&()).is_ok());
            }
            for (ms, allowed) in [
                (0, false),
                (unit_ms - 1, false),
                (unit_ms, true),
                (unit_ms, false),
                (unit_ms * 2, true),
            ] {
                clock.set(ms);
                assert_eq!(bucket.check(&()).is_ok(), allowed, "{quota:?} at {ms}");
            }
            // governor keeps a key for one more replenish interval before it is fresh again
            clock.advance(unit_ms * (burst + 1));
            assert!(bucket.is_empty());
        }
    }
    #[test]
    fn garbage_codes_do_not_reset_a_hammered_one() {
        let clock = Arc::new(steve_runtime::FakeClock::new(0));
        let l = Limiter::with_clock(clock.clone());
        for _ in 0..5 {
            assert!(l.take("target"));
        }
        assert!(!l.take("target"));
        // Inactive keys are evicted, without clearing the still-exhausted target.
        for i in 0..10_001 {
            l.buckets.check(&format!("junk{i}")).unwrap();
        }
        assert!(l.take("junk-new"));
        assert!(!l.take("target"));
        clock.advance(60_000);
        assert!(l.buckets.is_empty());
    }
    #[test]
    fn code_rejection_still_consumes_global_capacity() {
        let clock = Arc::new(steve_runtime::FakeClock::new(0));
        let l = Limiter::with_clock(clock.clone());
        for n in 0..200 {
            assert_eq!(l.take("code"), n < 5);
        }
        assert!(!l.take("fresh"));
        clock.advance(20);
        assert!(l.take("fresh"));
    }
}
