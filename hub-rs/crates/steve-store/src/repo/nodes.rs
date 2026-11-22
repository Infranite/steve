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
    /// Whether its owner's sessions load the owner's personal harness configuration on this
    /// machine ([`set_owner_context`]); `None` follows the Hub's default ([`owner_context_default`]).
    pub owner_context: Option<bool>,
    /// `auto` | `notify` | `off`, as its owner set it; `None`: the Hub's default
    /// ([`upgrade_default`]).
    pub upgrade_policy: Option<String>,
    /// What its machine is, as it last said (`node.host`, JSON; [`set_host`]).
    #[serde(skip)]
    pub host: Option<String>,
    /// How its latest link reached the Hub (JSON; [`set_link`]).
    #[serde(skip)]
    pub link: Option<String>,
}

const COLS: &str = "node_id,owner,name,pubkey,fingerprint,state,generation,incarnation,local,created,last_seen,revoked_at,revoked_by,max_sessions,owner_context,upgrade_policy,host,link";

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
        owner_context: r.get::<_, Option<i64>>(14)?.map(|v| v != 0),
        upgrade_policy: r.get(15)?,
        host: r.get(16)?,
        link: r.get(17)?,
    })
}

/// Tells whoever shows `node_id` that it changed (G5: `{type:"node", id, user}`, to its owner), after
/// the commit. Nothing for a Node that is not there.
pub fn changed(tx: &Tx<'_>, node_id: &str) -> Result<()> {
    let owner: Option<String> = tx
        .query_row("SELECT owner FROM nodes WHERE node_id=?", [node_id], |r| {
            r.get(0)
        })
        .optional()?;
    if let Some(user) = owner {
        tx.publish(steve_types::DomainEvent::from(
            steve_types::DevicesEvent::Node {
                id: node_id.into(),
                user: user.into(),
            },
        ));
    }
    Ok(())
}

/// `true` (and the Node's owner told, [`changed`]) when `n` rows were written.
fn told(tx: &Tx<'_>, node_id: &str, n: usize) -> Result<bool> {
    if n == 1 {
        changed(tx, node_id)?;
    }
    Ok(n == 1)
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
    changed(tx, n.node_id)?;
    Ok(Some(owner))
}

/// `from` → `to` for the owner's Node (confirm: pending → active, pause, resume).
pub fn set_state(tx: &Tx<'_>, node_id: &str, owner: &str, from: &str, to: &str) -> Result<bool> {
    let n = tx.execute(
        "UPDATE nodes SET state=?4 WHERE node_id=?1 AND owner=?2 AND state=?3",
        params![node_id, owner, from, to],
    )?;
    told(tx, node_id, n)
}

/// Revoked for good, and the generation moves on so that no live link of it may act again.
pub fn revoke(tx: &Tx<'_>, node_id: &str, owner: &str, now: i64) -> Result<bool> {
    let n = tx.execute(
        "UPDATE nodes SET state='revoked', revoked_at=?3, revoked_by=?2, generation=generation+1
         WHERE node_id=?1 AND owner=?2 AND state!='revoked'",
        params![node_id, owner, now],
    )?;
    told(tx, node_id, n)
}

pub fn rename(tx: &Tx<'_>, node_id: &str, owner: &str, name: &str) -> Result<bool> {
    let n = tx.execute(
        "UPDATE nodes SET name=?3 WHERE node_id=?1 AND owner=?2",
        params![node_id, owner, name],
    )?;
    told(tx, node_id, n)
}

/// The owner sets how many sessions their Node runs at once (1 to
/// [`crate::runtime::SESSIONS_MAX`]). False: not theirs, or out of range.
pub fn set_max_sessions(tx: &Tx<'_>, node_id: &str, owner: &str, n: i64) -> Result<bool> {
    if !(1..=crate::runtime::SESSIONS_MAX).contains(&n) {
        return Ok(false);
    }
    let n = tx.execute(
        "UPDATE nodes SET max_sessions=?3 WHERE node_id=?1 AND owner=?2 AND local=0 AND state!='revoked'",
        params![node_id, owner, n],
    )?;
    told(tx, node_id, n)
}

/// The owner chooses whether their sessions on their Node load their personal harness
/// configuration there (`None`: as the Hub's default says). Any of their Nodes, the Hub's own
/// included. False: not theirs, or revoked.
pub fn set_owner_context(
    tx: &Tx<'_>,
    node_id: &str,
    owner: &str,
    on: Option<bool>,
) -> Result<bool> {
    let n = tx.execute(
        "UPDATE nodes SET owner_context=?3 WHERE node_id=?1 AND owner=?2 AND state!='revoked'",
        params![node_id, owner, on],
    )?;
    told(tx, node_id, n)
}

/// The setting that holds the Hub's default for every Node without a choice of its own (`on` |
/// `off`); it was each workspace's `ownerContext` setting until migration 095 moved it here.
const OWNER_CONTEXT_DEFAULT_KEY: &str = "node.owner_context";

/// Whether a session of `node`'s owner there loads the owner's personal harness configuration:
/// the Node's own choice, else the Hub's default ([`owner_context_default`]).
pub fn owner_context(c: &Connection, node: &str) -> Result<bool> {
    let own: Option<Option<i64>> = c
        .query_row(
            "SELECT owner_context FROM nodes WHERE node_id=?",
            [node],
            |r| r.get(0),
        )
        .optional()?;
    match own {
        Some(Some(v)) => Ok(v != 0),
        _ => owner_context_default(c),
    }
}

/// The Hub's default for Nodes without their own choice: on unless the owner turned it off.
pub fn owner_context_default(c: &Connection) -> Result<bool> {
    let v: Option<String> = c
        .query_row(
            "SELECT value FROM settings WHERE workspace_id='default' AND key=?",
            [OWNER_CONTEXT_DEFAULT_KEY],
            |r| r.get(0),
        )
        .optional()?;
    Ok(v.as_deref() != Some("off"))
}

pub fn set_owner_context_default(tx: &Tx<'_>, on: bool, now: i64) -> Result<()> {
    tx.execute(
        "INSERT INTO settings(workspace_id,key,value,updated) VALUES ('default',?1,?2,?3)
         ON CONFLICT(workspace_id,key) DO UPDATE SET value=excluded.value, updated=excluded.updated",
        params![OWNER_CONTEXT_DEFAULT_KEY, if on { "on" } else { "off" }, now],
    )?;
    Ok(())
}

/// What the Hub does when a Node runs another build than the Hub: upgrade it (`auto`), say so
/// (`notify`) or nothing (`off`).
pub const UPGRADE_POLICIES: &[&str] = &["auto", "notify", "off"];
/// The setting that holds the Hub's default policy.
const UPGRADE_DEFAULT_KEY: &str = "node.upgrade_policy";
/// Without a setting: say so, upgrade only when the owner asks.
pub const UPGRADE_DEFAULT: &str = "notify";

/// `policy` `None`: back to the Hub's default.
pub fn set_upgrade_policy(
    tx: &Tx<'_>,
    node_id: &str,
    owner: &str,
    policy: Option<&str>,
) -> Result<bool> {
    if policy.is_some_and(|p| !UPGRADE_POLICIES.contains(&p)) {
        return Ok(false);
    }
    let n = tx.execute(
        "UPDATE nodes SET upgrade_policy=?3 WHERE node_id=?1 AND owner=?2 AND local=0 AND state!='revoked'",
        params![node_id, owner, policy],
    )?;
    told(tx, node_id, n)
}

/// The Hub's default policy for Nodes without their own.
pub fn upgrade_default(c: &Connection) -> Result<String> {
    let v: Option<String> = c
        .query_row(
            "SELECT value FROM settings WHERE workspace_id='default' AND key=?",
            [UPGRADE_DEFAULT_KEY],
            |r| r.get(0),
        )
        .optional()?;
    Ok(v.filter(|p| UPGRADE_POLICIES.contains(&p.as_str()))
        .unwrap_or_else(|| UPGRADE_DEFAULT.into()))
}

pub fn set_upgrade_default(tx: &Tx<'_>, policy: &str, now: i64) -> Result<bool> {
    if !UPGRADE_POLICIES.contains(&policy) {
        return Ok(false);
    }
    tx.execute(
        "INSERT INTO settings(workspace_id,key,value,updated) VALUES ('default',?1,?2,?3)
         ON CONFLICT(workspace_id,key) DO UPDATE SET value=excluded.value, updated=excluded.updated",
        params![UPGRADE_DEFAULT_KEY, policy, now],
    )?;
    Ok(true)
}

/// One upgrade of a Node the Hub ran (`node_upgrades`).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct UpgradeRow {
    pub operation_id: String,
    pub node_id: String,
    pub version: String,
    pub commit_sha: String,
    pub target: String,
    pub from_version: Option<String>,
    /// The artifact id the Node fetches it by.
    pub artifact: String,
    /// The archive on the Hub's disk.
    #[serde(skip)]
    pub file: String,
    pub sha256: String,
    pub size: i64,
    pub requested_by: String,
    pub force: bool,
    /// `preparing` | `staging` | `staged` | `draining` | `switching` | `succeeded` | `failed` |
    /// `rolled_back` | `abandoned` | `unknown`.
    pub phase: String,
    pub error: Option<String>,
    pub created: i64,
    pub updated: i64,
}

/// Phases after which nothing more happens to an upgrade.
pub const UPGRADE_DONE: &[&str] = &["succeeded", "failed", "rolled_back", "abandoned", "unknown"];

const UCOLS: &str = "operation_id,node_id,version,commit_sha,target,from_version,artifact,file,sha256,size,requested_by,force,phase,error,created,updated";

fn upgrade_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<UpgradeRow> {
    Ok(UpgradeRow {
        operation_id: r.get(0)?,
        node_id: r.get(1)?,
        version: r.get(2)?,
        commit_sha: r.get(3)?,
        target: r.get(4)?,
        from_version: r.get(5)?,
        artifact: r.get(6)?,
        file: r.get(7)?,
        sha256: r.get(8)?,
        size: r.get(9)?,
        requested_by: r.get(10)?,
        force: r.get::<_, i64>(11)? != 0,
        phase: r.get(12)?,
        error: r.get(13)?,
        created: r.get(14)?,
        updated: r.get(15)?,
    })
}

/// A new upgrade, unless the Node has one unfinished (`false`).
pub fn upgrade_insert(tx: &Tx<'_>, u: &UpgradeRow) -> Result<bool> {
    let open: i64 = tx.query_row(
        &format!(
            "SELECT count(*) FROM node_upgrades WHERE node_id=? AND phase NOT IN ({})",
            UPGRADE_DONE
                .iter()
                .map(|p| format!("'{p}'"))
                .collect::<Vec<_>>()
                .join(",")
        ),
        [&u.node_id],
        |r| r.get(0),
    )?;
    if open > 0 {
        return Ok(false);
    }
    tx.execute(
        &format!("INSERT INTO node_upgrades ({UCOLS}) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16)"),
        params![
            u.operation_id,
            u.node_id,
            u.version,
            u.commit_sha,
            u.target,
            u.from_version,
            u.artifact,
            u.file,
            u.sha256,
            u.size,
            u.requested_by,
            u.force as i64,
            u.phase,
            u.error,
            u.created,
            u.updated
        ],
    )?;
    changed(tx, &u.node_id)?;
    Ok(true)
}

/// Moves an unfinished upgrade on; a finished one stays as it ended (`false`).
pub fn upgrade_phase(
    tx: &Tx<'_>,
    operation_id: &str,
    phase: &str,
    error: Option<&str>,
    now: i64,
) -> Result<bool> {
    let n = tx.execute(
        &format!(
            "UPDATE node_upgrades SET phase=?2, error=?3, updated=?4 WHERE operation_id=?1 AND phase NOT IN ({})",
            UPGRADE_DONE.iter().map(|p| format!("'{p}'")).collect::<Vec<_>>().join(",")
        ),
        params![operation_id, phase, error, now],
    )?;
    if n == 1 {
        let node: Option<String> = tx
            .query_row(
                "SELECT node_id FROM node_upgrades WHERE operation_id=?",
                [operation_id],
                |r| r.get(0),
            )
            .optional()?;
        if let Some(node) = node {
            changed(tx, &node)?;
        }
    }
    Ok(n == 1)
}

/// Marks an unfinished upgrade as forced (its owner asked to switch it now).
pub fn upgrade_force(tx: &Tx<'_>, operation_id: &str) -> Result<bool> {
    Ok(tx.execute(
        &format!(
            "UPDATE node_upgrades SET force=1 WHERE operation_id=?1 AND phase NOT IN ({})",
            UPGRADE_DONE
                .iter()
                .map(|p| format!("'{p}'"))
                .collect::<Vec<_>>()
                .join(",")
        ),
        params![operation_id],
    )? == 1)
}

pub fn upgrade_get(c: &Connection, operation_id: &str) -> Result<Option<UpgradeRow>> {
    Ok(c.query_row(
        &format!("SELECT {UCOLS} FROM node_upgrades WHERE operation_id=?"),
        [operation_id],
        upgrade_row,
    )
    .optional()?)
}

/// The Node's latest upgrade.
pub fn upgrade_latest(c: &Connection, node_id: &str) -> Result<Option<UpgradeRow>> {
    Ok(c.query_row(
        &format!(
            "SELECT {UCOLS} FROM node_upgrades WHERE node_id=? ORDER BY created DESC, rowid DESC LIMIT 1"
        ),
        [node_id],
        upgrade_row,
    )
    .optional()?)
}

/// Upgrades not finished (after a Hub restart: to settle, never to run again).
pub fn upgrades_open(c: &Connection) -> Result<Vec<UpgradeRow>> {
    let mut st = c.prepare(&format!(
        "SELECT {UCOLS} FROM node_upgrades WHERE phase NOT IN ({}) ORDER BY created",
        UPGRADE_DONE
            .iter()
            .map(|p| format!("'{p}'"))
            .collect::<Vec<_>>()
            .join(",")
    ))?;
    let v = st
        .query_map([], upgrade_row)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(v)
}

/// One removal of a Node from its machine its owner asked for (`node_removals`).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct RemovalRow {
    pub operation_id: String,
    pub node_id: String,
    pub requested_by: String,
    pub grace_secs: i64,
    /// `requested` | `draining` | `cancelling` | `ready` | `confirming` | `removed` |
    /// `unconfirmed` | `failed` | `manual`.
    pub phase: String,
    pub error: Option<String>,
    pub created: i64,
    pub updated: i64,
}

/// Phases after which nothing more happens to a removal: `removed` (the Node confirmed and was
/// unpaired), `unconfirmed` (it did not confirm: offline, or too slow), `failed` (it refused or
/// failed), `manual` (it cannot remove itself).
pub const REMOVAL_DONE: &[&str] = &["removed", "unconfirmed", "failed", "manual"];

const RCOLS: &str = "operation_id,node_id,requested_by,grace_secs,phase,error,created,updated";

fn removal_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<RemovalRow> {
    Ok(RemovalRow {
        operation_id: r.get(0)?,
        node_id: r.get(1)?,
        requested_by: r.get(2)?,
        grace_secs: r.get(3)?,
        phase: r.get(4)?,
        error: r.get(5)?,
        created: r.get(6)?,
        updated: r.get(7)?,
    })
}

fn removal_done_sql() -> String {
    REMOVAL_DONE
        .iter()
        .map(|p| format!("'{p}'"))
        .collect::<Vec<_>>()
        .join(",")
}

/// A new removal, unless the Node has one unfinished (`false`).
pub fn removal_insert(tx: &Tx<'_>, r: &RemovalRow) -> Result<bool> {
    let open: i64 = tx.query_row(
        &format!(
            "SELECT count(*) FROM node_removals WHERE node_id=? AND phase NOT IN ({})",
            removal_done_sql()
        ),
        [&r.node_id],
        |x| x.get(0),
    )?;
    if open > 0 {
        return Ok(false);
    }
    tx.execute(
        &format!("INSERT INTO node_removals ({RCOLS}) VALUES (?1,?2,?3,?4,?5,?6,?7,?8)"),
        params![
            r.operation_id,
            r.node_id,
            r.requested_by,
            r.grace_secs,
            r.phase,
            r.error,
            r.created,
            r.updated
        ],
    )?;
    changed(tx, &r.node_id)?;
    Ok(true)
}

/// Moves an unfinished removal on; a finished one stays as it ended (`false`).
pub fn removal_phase(
    tx: &Tx<'_>,
    operation_id: &str,
    phase: &str,
    error: Option<&str>,
    now: i64,
) -> Result<bool> {
    let node: Option<String> = tx
        .query_row(
            &format!(
                "UPDATE node_removals SET phase=?2, error=?3, updated=?4
                 WHERE operation_id=?1 AND phase NOT IN ({}) RETURNING node_id",
                removal_done_sql()
            ),
            params![operation_id, phase, error, now],
            |r| r.get(0),
        )
        .optional()?;
    if let Some(node) = &node {
        changed(tx, node)?;
    }
    Ok(node.is_some())
}

pub fn removal_get(c: &Connection, operation_id: &str) -> Result<Option<RemovalRow>> {
    Ok(c.query_row(
        &format!("SELECT {RCOLS} FROM node_removals WHERE operation_id=?"),
        [operation_id],
        removal_row,
    )
    .optional()?)
}

/// The Node's latest removal.
pub fn removal_latest(c: &Connection, node_id: &str) -> Result<Option<RemovalRow>> {
    Ok(c.query_row(
        &format!(
            "SELECT {RCOLS} FROM node_removals WHERE node_id=? ORDER BY created DESC, rowid DESC LIMIT 1"
        ),
        [node_id],
        removal_row,
    )
    .optional()?)
}

/// Removals not finished (after a Hub restart: to settle, never to run on).
pub fn removals_open(c: &Connection) -> Result<Vec<RemovalRow>> {
    let mut st = c.prepare(&format!(
        "SELECT {RCOLS} FROM node_removals WHERE phase NOT IN ({}) ORDER BY created",
        removal_done_sql()
    ))?;
    let v = st
        .query_map([], removal_row)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(v)
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

/// The authenticated connection was observed at `now`. Only a close notifies its owner.
pub fn touch(tx: &Tx<'_>, node_id: &str, generation: u64, now: i64, closed: bool) -> Result<()> {
    let n = tx.execute(
        "UPDATE nodes SET last_seen=MAX(COALESCE(last_seen,0),?2)
         WHERE node_id=?1 AND generation=?3 AND state IN ('active','paused')",
        params![node_id, now, generation as i64],
    )?;
    if closed {
        told(tx, node_id, n)?;
    }
    Ok(())
}

/// What the Node's machine is (`node.host`, JSON), as it just said; its owner is told when it
/// differs from what was kept.
pub fn set_host(tx: &Tx<'_>, node_id: &str, host: &str) -> Result<bool> {
    let n = tx.execute(
        "UPDATE nodes SET host=?2 WHERE node_id=?1 AND host IS NOT ?2",
        params![node_id, host],
    )?;
    told(tx, node_id, n)
}

/// How the Node's latest link reached the Hub (JSON), kept while the Node is away.
pub fn set_link(tx: &Tx<'_>, node_id: &str, link: &str) -> Result<bool> {
    Ok(tx.execute(
        "UPDATE nodes SET link=?2 WHERE node_id=?1",
        params![node_id, link],
    )? == 1)
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

/// Session token ledger per Node, for the current minute and the workspace's local day.
pub fn session_tokens(
    c: &Connection,
    workspace: &str,
    today: &str,
    minute_day: &str,
    minute: i64,
    now: i64,
) -> Result<std::collections::BTreeMap<String, (i64, i64)>> {
    // Reasoning is already included in output. The indexed day range also covers a minute
    // spanning midnight. Model calls without a session have no session Node to attribute to.
    let mut stmt = c.prepare_cached(
        "WITH ledger AS (
            SELECT r.node_id, u.ts, u.day,
                   COALESCE(u.input,0) + COALESCE(u.output,0) +
                   COALESCE(u.cache_read,0) + COALESCE(u.cache_write,0) AS tokens
            FROM usage_records u JOIN runtime_sessions r ON r.session_id=u.session_id
            WHERE u.workspace_id=?1 AND u.day BETWEEN ?3 AND ?2 AND u.ts<=?5
         )
         SELECT node_id, SUM(CASE WHEN ts>?4 THEN tokens ELSE 0 END),
                         SUM(CASE WHEN day=?2 THEN tokens ELSE 0 END)
         FROM ledger GROUP BY node_id",
    )?;
    let rows = stmt.query_map(params![workspace, today, minute_day, minute, now], |r| {
        Ok((r.get(0)?, (r.get(1)?, r.get(2)?)))
    })?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
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
