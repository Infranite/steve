//! Pairing and the owner's controls.
//!
//! - `POST /api/nodes/pairings` (signed in): a one-time code `stv1-…` (128 random bits), shown once,
//!   valid 10 minutes. Only its sha256 is stored: the code is high-entropy, so no pepper is needed.
//! - `POST /node/pair {code, pubkey, name, sig}`: the Node proves it holds the key it registers
//!   (`sig` over `"steve-pair-v1" ‖ sha256(code) ‖ pubkey`); the claim is one atomic update. Wrong,
//!   used and expired codes all get the same `denied`, and claims are rate-limited per code (see [`Limiter`]).
//!   The Node is `pending` until its owner confirms the fingerprint the CLI printed.
//! - `GET /api/nodes`, `POST /api/nodes/{id}/confirm|pause|resume|rename|revoke`: the owner's only.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Instant;

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
    buckets: Mutex<HashMap<String, (f64, Instant)>>,
    global: Mutex<(f64, Instant)>,
}

const PER_CODE: (f64, f64) = (5.0, 0.1);
const GLOBAL: (f64, f64) = (200.0, 50.0);

impl Default for Limiter {
    fn default() -> Self {
        Limiter {
            buckets: Mutex::new(HashMap::new()),
            global: Mutex::new((GLOBAL.0, Instant::now())),
        }
    }
}

/// Refills `(tokens, at)` at `rate` up to `burst` and takes one if there is one.
fn take_one(b: &mut (f64, Instant), (burst, rate): (f64, f64)) -> bool {
    let now = Instant::now();
    b.0 = (b.0 + now.duration_since(b.1).as_secs_f64() * rate).min(burst);
    b.1 = now;
    if b.0 >= 1.0 {
        b.0 -= 1.0;
        true
    } else {
        false
    }
}

impl Limiter {
    /// One claim of the code whose hash is `code_hash`.
    pub fn take(&self, code_hash: &str) -> bool {
        if !take_one(&mut self.global.lock().unwrap(), GLOBAL) {
            return false;
        }
        let mut b = self.buckets.lock().unwrap();
        if b.len() > 10_000 {
            // a full bucket is the same as none: dropping those forgets nothing, while a code
            // being hammered keeps its count (M5: clearing all reset it too)
            let now = Instant::now();
            b.retain(|_, (tokens, at)| {
                *tokens + now.duration_since(*at).as_secs_f64() * PER_CODE.1 < PER_CODE.0
            });
            if b.len() > 10_000 {
                b.clear();
            }
        }
        let e = b
            .entry(code_hash.to_string())
            .or_insert((PER_CODE.0, Instant::now()));
        take_one(e, PER_CODE)
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
        store.transaction(|tx| {
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
        store.transaction(|tx| {
            let n = steve_store::nodes::NewNode {
                node_id: &id,
                name: &name,
                pubkey: &pubkey,
                fingerprint: &fingerprint,
            };
            // a key that is already paired is refused like a bad code (the UNIQUE fingerprint)
            let exists: bool = tx.query_row(
                "SELECT EXISTS(SELECT 1 FROM nodes WHERE fingerprint=?)",
                [&fingerprint],
                |r| r.get(0),
            )?;
            if exists {
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
        Ok(Some(_)) => json_response(200, &json!({ "node_id": node_id, "hub_id": svc.hub_id })),
        Ok(None) => denied(),
        Err(r) => *r,
    }
}

pub async fn list(State(svc): State<Arc<NodeService>>, caller: Caller) -> Response {
    let store = svc.store.clone();
    let rows =
        match blocking(move || store.reader(|c| steve_store::nodes::list_for_owner(c, &caller.me)))
            .await
        {
            Ok(r) => r,
            Err(r) => return *r,
        };
    let items: Vec<Value> = rows
        .into_iter()
        .map(|n| {
            let linked = svc.registry.current(&n.node_id).is_some();
            let mut v = serde_json::to_value(&n).unwrap_or(Value::Null);
            v["linked"] = json!(linked);
            v
        })
        .collect();
    json_response(200, &json!({ "nodes": items }))
}

#[derive(Deserialize, Default)]
pub struct RenameBody {
    #[serde(default)]
    name: String,
}

pub async fn act(
    State(svc): State<Arc<NodeService>>,
    caller: Caller,
    Path((id, action)): Path<(String, String)>,
    body: Option<Json<RenameBody>>,
) -> Response {
    let (store, me, node) = (svc.store.clone(), caller.me.clone(), id.clone());
    let name = body.map(|b| b.0.name).unwrap_or_default();
    let action2 = action.clone();
    let done = blocking(move || {
        store.transaction(|tx| {
            use steve_store::nodes::*;
            let ok = match action2.as_str() {
                "confirm" => set_state(tx, &node, &me, "pending", "active")?,
                "pause" => set_state(tx, &node, &me, "active", "paused")?,
                "resume" => set_state(tx, &node, &me, "paused", "active")?,
                "revoke" => revoke(tx, &node, &me, now())?,
                "rename" => {
                    let n: String = name.chars().filter(|c| !c.is_control()).take(64).collect();
                    !n.trim().is_empty() && rename(tx, &node, &me, &n)?
                }
                _ => return Ok(None),
            };
            if ok {
                audit(tx, Some(&node), Some(&me), &action2, None, now())?;
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
            let _g = svc.links.lock().await;
            match action.as_str() {
                "revoke" => {
                    svc.registry.kill(&id, "revoked");
                    if let Some(rt) = svc.fleet.runtime_for(&id) {
                        rt.set_paused(true);
                    }
                }
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

    /// M5: many garbage codes no longer reset the count of the one being hammered.
    #[test]
    fn garbage_codes_do_not_reset_a_hammered_one() {
        let l = Limiter::default();
        for _ in 0..PER_CODE.0 as usize {
            assert!(l.take("target"));
        }
        assert!(!l.take("target"));
        {
            let mut b = l.buckets.lock().unwrap();
            let now = Instant::now();
            for i in 0..10_001 {
                b.insert(format!("junk{i}"), (PER_CODE.0, now));
            }
        }
        *l.global.lock().unwrap() = (GLOBAL.0, Instant::now());
        assert!(l.take("junk-new"));
        assert!(!l.take("target"), "still out of tries");
    }
}
