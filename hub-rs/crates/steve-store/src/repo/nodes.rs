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
    /// How many sessions it runs at once, as its owner set it ([`crate::runtime::session_room`]).
    pub max_sessions: i64,
}

const COLS: &str = "node_id,owner,name,pubkey,fingerprint,state,generation,incarnation,local,created,last_seen,revoked_at,revoked_by,max_sessions";

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
        max_sessions: r.get(13)?,
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
    create_pairing_as(tx, code_hash, created_by, now, expires, false)
}

/// `confirmed`: the Node this code pairs is active at once (the Hub's own remote install, which
/// its owner authorised), not pending until the fingerprint is confirmed.
pub fn create_pairing_as(
    tx: &Tx<'_>,
    code_hash: &str,
    created_by: &str,
    now: i64,
    expires: i64,
    confirmed: bool,
) -> Result<()> {
    tx.execute(
        "INSERT INTO node_pairings(code_hash,created_by,created,expires,confirmed) VALUES (?,?,?,?,?)",
        params![code_hash, created_by, now, expires, confirmed],
    )?;
    Ok(())
}

pub struct NewNode<'a> {
    pub node_id: &'a str,
    pub name: &'a str,
    pub pubkey: &'a [u8; 32],
    pub fingerprint: &'a str,
}

/// Claims a code and registers the Node as `pending` (`active` for a code made `confirmed`), owned
/// by whoever made the code. One atomic update: a wrong, used or expired code all give `None`, and
/// two claims of one code never both win.
pub fn claim_pairing(
    tx: &Tx<'_>,
    code_hash: &str,
    n: &NewNode<'_>,
    now: i64,
) -> Result<Option<String>> {
    let claimed: Option<(String, bool)> = tx
        .query_row(
            "UPDATE node_pairings SET claimed_at=?1, node_id=?2
             WHERE code_hash=?3 AND claimed_at IS NULL AND expires>?1 RETURNING created_by, confirmed",
            params![now, n.node_id, code_hash],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    let Some((owner, confirmed)) = claimed else {
        return Ok(None);
    };
    let state = if confirmed { "active" } else { "pending" };
    tx.execute(
        "INSERT INTO nodes(node_id,owner,name,pubkey,fingerprint,state,created) VALUES (?,?,?,?,?,?,?)",
        params![n.node_id, owner, n.name, &n.pubkey[..], n.fingerprint, state, now],
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

/// The owner sets how many sessions their Node runs at once (1 to
/// [`crate::runtime::SESSIONS_MAX`]). False: not theirs, or out of range.
pub fn set_max_sessions(tx: &Tx<'_>, node_id: &str, owner: &str, n: i64) -> Result<bool> {
    if !(1..=crate::runtime::SESSIONS_MAX).contains(&n) {
        return Ok(false);
    }
    Ok(tx.execute(
        "UPDATE nodes SET max_sessions=?3 WHERE node_id=?1 AND owner=?2 AND local=0 AND state!='revoked'",
        params![node_id, owner, n],
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

/// How long a Node's vitals are kept (B3).
pub const VITALS_KEEP_MS: i64 = 24 * 60 * 60 * 1000;

/// One sample of a Node's vitals (`data`: the Node's own JSON), and what is older than a day goes.
pub fn vitals_put(
    tx: &Tx<'_>,
    node_id: &str,
    at: i64,
    rtt_ms: Option<i64>,
    data: &str,
) -> Result<()> {
    tx.execute(
        "INSERT INTO node_vitals(node_id,at,rtt_ms,data) VALUES (?,?,?,?)",
        params![node_id, at, rtt_ms, data],
    )?;
    tx.execute(
        "DELETE FROM node_vitals WHERE node_id=? AND at<?",
        params![node_id, at - VITALS_KEEP_MS],
    )?;
    Ok(())
}

/// A Node's samples since `since`, oldest first: (at, rtt_ms, data).
pub fn vitals_since(
    c: &rusqlite::Connection,
    node_id: &str,
    since: i64,
) -> Result<Vec<(i64, Option<i64>, String)>> {
    let mut st = c.prepare(
        "SELECT at, rtt_ms, data FROM node_vitals WHERE node_id=? AND at>=? ORDER BY at",
    )?;
    let rows = st
        .query_map(params![node_id, since], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

/// A tag as stored: trimmed, lower case, letters, digits, `-`, `_`, `.` and `:` only, at most 40
/// characters; `None` for anything else.
pub fn tag_of(raw: &str) -> Option<String> {
    let t = raw.trim().to_lowercase();
    (!t.is_empty()
        && t.chars().count() <= 40
        && t.chars()
            .all(|c| c.is_alphanumeric() || matches!(c, '-' | '_' | '.' | ':')))
    .then_some(t)
}

/// Every Node's tags, by Node.
pub fn all_tags(c: &Connection) -> Result<std::collections::BTreeMap<String, Vec<String>>> {
    let mut st = c.prepare("SELECT node_id, tag FROM node_tags ORDER BY node_id, tag")?;
    let mut out: std::collections::BTreeMap<String, Vec<String>> = Default::default();
    for r in st.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))? {
        let (n, t) = r?;
        out.entry(n).or_default().push(t);
    }
    Ok(out)
}

/// Replaces `node_id`'s tags (the valid ones of `tags`); returns what is stored now.
pub fn set_tags(tx: &Tx<'_>, node_id: &str, tags: &[String]) -> Result<Vec<String>> {
    tx.execute("DELETE FROM node_tags WHERE node_id = ?", [node_id])?;
    let mut kept: Vec<String> = tags.iter().filter_map(|t| tag_of(t)).collect();
    kept.sort();
    kept.dedup();
    for t in &kept {
        tx.execute(
            "INSERT INTO node_tags(node_id, tag) VALUES (?, ?)",
            params![node_id, t],
        )?;
    }
    Ok(kept)
}

/// The Nodes that carry `tag`.
pub fn with_tag(c: &Connection, tag: &str) -> Result<Vec<String>> {
    let mut st = c.prepare("SELECT node_id FROM node_tags WHERE tag = ? ORDER BY node_id")?;
    let v = st
        .query_map([tag], |r| r.get(0))?
        .collect::<rusqlite::Result<Vec<String>>>()?;
    Ok(v)
}

/// The tag `item_id` asks of the Node its sessions run on.
pub fn item_tag(c: &Connection, item_id: &str) -> Result<Option<String>> {
    Ok(c.query_row(
        "SELECT tag FROM item_node_tags WHERE item_id = ?",
        [item_id],
        |r| r.get(0),
    )
    .optional()?)
}

/// Sets (`Some`) or clears (`None`) the tag `item_id` asks for; `false` for an invalid tag.
pub fn set_item_tag(tx: &Tx<'_>, item_id: &str, tag: Option<&str>) -> Result<bool> {
    match tag {
        None => {
            tx.execute("DELETE FROM item_node_tags WHERE item_id = ?", [item_id])?;
            Ok(true)
        }
        Some(raw) => {
            let Some(t) = tag_of(raw) else {
                return Ok(false);
            };
            tx.execute(
                "INSERT INTO item_node_tags(item_id, tag) VALUES (?, ?) ON CONFLICT(item_id) DO UPDATE SET tag = excluded.tag",
                params![item_id, t],
            )?;
            Ok(true)
        }
    }
}
