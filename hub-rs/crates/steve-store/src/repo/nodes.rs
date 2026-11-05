//! Paired Nodes (N2): pairing codes kept only as their sha256, the Node's public key, its state and
//! the generation that fences its connections. Every write takes the caller's transaction.

use rusqlite::{Connection, OptionalExtension, params};
use serde::Serialize;

use crate::{Result, Tx};

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct NodeRow {
    pub node_id: String,
    pub owner: String,
    pub name: String,
    #[serde(skip)]
    pub pubkey: Vec<u8>,
    pub fingerprint: String,
    pub state: String,
    pub generation: i64,
    pub incarnation: Option<i64>,
    pub local: bool,
    pub created: i64,
    pub last_seen: Option<i64>,
    pub revoked_at: Option<i64>,
    pub revoked_by: Option<String>,
}

const COLS: &str = "node_id,owner,name,pubkey,fingerprint,state,generation,incarnation,local,created,last_seen,revoked_at,revoked_by";

fn row(r: &rusqlite::Row<'_>) -> rusqlite::Result<NodeRow> {
    Ok(NodeRow {
        node_id: r.get(0)?,
        owner: r.get(1)?,
        name: r.get(2)?,
        pubkey: r.get(3)?,
        fingerprint: r.get(4)?,
        state: r.get(5)?,
        generation: r.get(6)?,
        incarnation: r.get(7)?,
        local: r.get::<_, i64>(8)? != 0,
        created: r.get(9)?,
        last_seen: r.get(10)?,
        revoked_at: r.get(11)?,
        revoked_by: r.get(12)?,
    })
}

/// A one-time code for `created_by`, valid until `expires`.
pub fn create_pairing(
    tx: &Tx<'_>,
    code_hash: &str,
    created_by: &str,
    now: i64,
    expires: i64,
) -> Result<()> {
    tx.execute(
        "INSERT INTO node_pairings(code_hash,created_by,created,expires) VALUES (?,?,?,?)",
        params![code_hash, created_by, now, expires],
    )?;
    Ok(())
}

pub struct NewNode<'a> {
    pub node_id: &'a str,
    pub name: &'a str,
    pub pubkey: &'a [u8; 32],
    pub fingerprint: &'a str,
}

/// Claims a code and registers the Node as `pending`, owned by whoever made the code. One atomic
/// update: a wrong, used or expired code all give `None`, and two claims of one code never both win.
pub fn claim_pairing(
    tx: &Tx<'_>,
    code_hash: &str,
    n: &NewNode<'_>,
    now: i64,
) -> Result<Option<String>> {
    let owner: Option<String> = tx
        .query_row(
            "UPDATE node_pairings SET claimed_at=?1, node_id=?2
             WHERE code_hash=?3 AND claimed_at IS NULL AND expires>?1 RETURNING created_by",
            params![now, n.node_id, code_hash],
            |r| r.get(0),
        )
        .optional()?;
    let Some(owner) = owner else { return Ok(None) };
    tx.execute(
        "INSERT INTO nodes(node_id,owner,name,pubkey,fingerprint,state,created) VALUES (?,?,?,?,?,'pending',?)",
        params![n.node_id, owner, n.name, &n.pubkey[..], n.fingerprint, now],
    )?;
    Ok(Some(owner))
}

/// `from` → `to` for the owner's Node (confirm: pending → active, pause, resume).
pub fn set_state(tx: &Tx<'_>, node_id: &str, owner: &str, from: &str, to: &str) -> Result<bool> {
    Ok(tx.execute(
        "UPDATE nodes SET state=?4 WHERE node_id=?1 AND owner=?2 AND state=?3",
        params![node_id, owner, from, to],
    )? == 1)
}

/// Revoked for good, and the generation moves on so that no live link of it may act again.
pub fn revoke(tx: &Tx<'_>, node_id: &str, owner: &str, now: i64) -> Result<bool> {
    Ok(tx.execute(
        "UPDATE nodes SET state='revoked', revoked_at=?3, revoked_by=?2, generation=generation+1
         WHERE node_id=?1 AND owner=?2 AND state!='revoked'",
        params![node_id, owner, now],
    )? == 1)
}

pub fn rename(tx: &Tx<'_>, node_id: &str, owner: &str, name: &str) -> Result<bool> {
    Ok(tx.execute(
        "UPDATE nodes SET name=?3 WHERE node_id=?1 AND owner=?2",
        params![node_id, owner, name],
    )? == 1)
}

/// A new link of an active or paused Node: the next generation, the incarnation it reported, and
/// the one before (a change means the Node restarted). `None`: it may not connect.
pub struct Link {
    pub generation: i64,
    pub paused: bool,
    pub previous_incarnation: Option<i64>,
}

pub fn begin_link(tx: &Tx<'_>, node_id: &str, incarnation: i64, now: i64) -> Result<Option<Link>> {
    let prev: Option<Option<i64>> = tx
        .query_row(
            "SELECT incarnation FROM nodes WHERE node_id=? AND state IN ('active','paused')",
            [node_id],
            |r| r.get(0),
        )
        .optional()?;
    let Some(previous_incarnation) = prev else {
        return Ok(None);
    };
    let got: Option<(i64, String)> = tx
        .query_row(
            "UPDATE nodes SET generation=generation+1, incarnation=?2, last_seen=?3
             WHERE node_id=?1 AND state IN ('active','paused') RETURNING generation, state",
            params![node_id, incarnation, now],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    Ok(got.map(|(generation, state)| Link {
        generation,
        paused: state == "paused",
        previous_incarnation,
    }))
}

pub fn touch(tx: &Tx<'_>, node_id: &str, now: i64) -> Result<()> {
    tx.execute(
        "UPDATE nodes SET last_seen=? WHERE node_id=?",
        params![now, node_id],
    )?;
    Ok(())
}

pub fn get(c: &Connection, node_id: &str) -> Result<Option<NodeRow>> {
    Ok(c.query_row(
        &format!("SELECT {COLS} FROM nodes WHERE node_id=?"),
        [node_id],
        row,
    )
    .optional()?)
}

/// The generation and state the guards compare a link with.
pub fn link_state(c: &Connection, node_id: &str) -> Result<Option<(i64, String)>> {
    Ok(c.query_row(
        "SELECT generation, state FROM nodes WHERE node_id=?",
        [node_id],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )
    .optional()?)
}

pub fn list_for_owner(c: &Connection, owner: &str) -> Result<Vec<NodeRow>> {
    let mut st = c.prepare(&format!(
        "SELECT {COLS} FROM nodes WHERE owner=? ORDER BY created, node_id"
    ))?;
    let v = st
        .query_map([owner], row)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(v)
}

/// Every Node that may connect (active or paused), for the fleet.
pub fn list_live(c: &Connection) -> Result<Vec<NodeRow>> {
    let mut st = c.prepare(&format!(
        "SELECT {COLS} FROM nodes WHERE state IN ('active','paused') ORDER BY created, node_id"
    ))?;
    let v = st
        .query_map([], row)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(v)
}

/// `safe_detail`: nothing secret (never a code, never a key), nothing outside the Hub's own words.
pub fn audit(
    tx: &Tx<'_>,
    node_id: Option<&str>,
    actor: Option<&str>,
    what: &str,
    safe_detail: Option<&str>,
    now: i64,
) -> Result<()> {
    tx.execute(
        "INSERT INTO node_audit(at,node_id,actor,what,safe_detail) VALUES (?,?,?,?,?)",
        params![now, node_id, actor, what, safe_detail],
    )?;
    Ok(())
}
