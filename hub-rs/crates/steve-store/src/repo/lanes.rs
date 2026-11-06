//! Merge lanes (031): one per target, its requests and units. The window's rules live in steve-core
//! `lane::batch`; this keeps its snapshot and the rows the API shows. Writers take the transaction they
//! are part of: a lane's state and its rows change together.

use rusqlite::{Connection, OptionalExtension, params};
use serde::Serialize;

use crate::{Result, now, uid};

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct LaneRow {
    pub id: String,
    pub upstream: String,
    pub branch: String,
    pub window_ms: Option<i64>,
    pub max_members: Option<i64>,
    /// The state machine's snapshot (JSON).
    pub state: String,
    pub created: i64,
    pub updated: i64,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct LaneRequest {
    pub id: String,
    pub lane_id: String,
    pub workspace_id: Option<String>,
    pub item_id: Option<String>,
    pub source: String,
    pub dir: String,
    pub branch: String,
    pub head: String,
    pub repo: Option<String>,
    pub arrived: i64,
    pub unit: Option<i64>,
    pub released_in: Option<i64>,
    pub state: String,
    pub reason: Option<String>,
    pub updated: i64,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct LaneUnit {
    pub lane_id: String,
    pub no: i64,
    pub state: String,
    pub branch: Option<String>,
    pub version: Option<String>,
    pub outcome: Option<String>,
    pub opened: i64,
    pub ended: Option<i64>,
}

fn lane(r: &rusqlite::Row<'_>) -> rusqlite::Result<LaneRow> {
    Ok(LaneRow {
        id: r.get("id")?,
        upstream: r.get("upstream")?,
        branch: r.get("branch")?,
        window_ms: r.get("window_ms")?,
        max_members: r.get("max_members")?,
        state: r.get("state")?,
        created: r.get("created")?,
        updated: r.get("updated")?,
    })
}

fn request(r: &rusqlite::Row<'_>) -> rusqlite::Result<LaneRequest> {
    Ok(LaneRequest {
        id: r.get("id")?,
        lane_id: r.get("lane_id")?,
        workspace_id: r.get("workspace_id")?,
        item_id: r.get("item_id")?,
        source: r.get("source")?,
        dir: r.get("dir")?,
        branch: r.get("branch")?,
        head: r.get("head")?,
        repo: r.get("repo")?,
        arrived: r.get("arrived")?,
        unit: r.get("unit")?,
        released_in: r.get("released_in")?,
        state: r.get("state")?,
        reason: r.get("reason")?,
        updated: r.get("updated")?,
    })
}

fn unit(r: &rusqlite::Row<'_>) -> rusqlite::Result<LaneUnit> {
    Ok(LaneUnit {
        lane_id: r.get("lane_id")?,
        no: r.get("no")?,
        state: r.get("state")?,
        branch: r.get("branch")?,
        version: r.get("version")?,
        outcome: r.get("outcome")?,
        opened: r.get("opened")?,
        ended: r.get("ended")?,
    })
}

/// The lane of a target, made on first use.
pub fn lane_for(c: &Connection, upstream: &str, branch: &str) -> Result<LaneRow> {
    let t = now();
    c.execute(
        "INSERT OR IGNORE INTO lanes (id, upstream, branch, created, updated) VALUES (?, ?, ?, ?, ?)",
        params![uid("ln"), upstream, branch, t, t],
    )?;
    Ok(c.query_row(
        "SELECT * FROM lanes WHERE upstream = ? AND branch = ?",
        params![upstream, branch],
        lane,
    )?)
}

pub fn lane_get(c: &Connection, id: &str) -> Result<Option<LaneRow>> {
    Ok(c.query_row("SELECT * FROM lanes WHERE id = ?", [id], lane)
        .optional()?)
}

pub fn lanes_all(c: &Connection) -> Result<Vec<LaneRow>> {
    let mut st = c.prepare_cached("SELECT * FROM lanes ORDER BY created")?;
    let v = st.query_map([], lane)?.collect::<rusqlite::Result<_>>()?;
    Ok(v)
}

/// Store the state machine's snapshot.
pub fn lane_save(c: &Connection, id: &str, state: &str) -> Result<()> {
    c.execute(
        "UPDATE lanes SET state = ?, updated = ? WHERE id = ?",
        params![state, now(), id],
    )?;
    Ok(())
}

/// A lane's limits (`None`: the default).
pub fn lane_policy_set(
    c: &Connection,
    id: &str,
    window_ms: Option<i64>,
    max_members: Option<i64>,
) -> Result<()> {
    c.execute(
        "UPDATE lanes SET window_ms = ?, max_members = ?, updated = ? WHERE id = ?",
        params![window_ms, max_members, now(), id],
    )?;
    Ok(())
}

pub struct NewRequest<'a> {
    pub lane_id: &'a str,
    pub workspace_id: Option<&'a str>,
    pub item_id: Option<&'a str>,
    pub source: &'a str,
    pub dir: &'a str,
    pub branch: &'a str,
    pub head: &'a str,
    pub repo: Option<&'a str>,
}

pub fn request_add(c: &Connection, r: &NewRequest<'_>) -> Result<LaneRequest> {
    let (id, t) = (uid("lr"), now());
    c.execute(
        "INSERT INTO lane_requests (id, lane_id, workspace_id, item_id, source, dir, branch, head, repo, arrived, state, updated)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, 'queued', ?)",
        params![id, r.lane_id, r.workspace_id, r.item_id, r.source, r.dir, r.branch, r.head, r.repo, t, t],
    )?;
    Ok(c.query_row("SELECT * FROM lane_requests WHERE id = ?", [&id], request)?)
}

pub fn request_get(c: &Connection, id: &str) -> Result<Option<LaneRequest>> {
    Ok(
        c.query_row("SELECT * FROM lane_requests WHERE id = ?", [id], request)
            .optional()?,
    )
}

/// The item's request still in a lane (not released or left), if any.
pub fn request_of_item(c: &Connection, item: &str) -> Result<Option<LaneRequest>> {
    Ok(c.query_row(
        "SELECT * FROM lane_requests WHERE item_id = ? AND state NOT IN ('released', 'left') ORDER BY arrived DESC LIMIT 1",
        [item],
        request,
    )
    .optional()?)
}

/// A lane's requests, oldest first (`open`: only those still in it).
pub fn requests_of(c: &Connection, lane: &str, open: bool) -> Result<Vec<LaneRequest>> {
    let sql = if open {
        "SELECT * FROM lane_requests WHERE lane_id = ? AND state NOT IN ('released', 'left') ORDER BY arrived"
    } else {
        "SELECT * FROM lane_requests WHERE lane_id = ? ORDER BY arrived"
    };
    let mut st = c.prepare_cached(sql)?;
    let v = st
        .query_map([lane], request)?
        .collect::<rusqlite::Result<_>>()?;
    Ok(v)
}

/// A request's state; `unit` / `released_in` / `reason` only when given.
pub fn request_set(
    c: &Connection,
    id: &str,
    state: &str,
    unit: Option<i64>,
    released_in: Option<i64>,
    reason: Option<&str>,
) -> Result<()> {
    c.execute(
        "UPDATE lane_requests SET state = ?, unit = coalesce(?, unit), released_in = coalesce(?, released_in),
            reason = coalesce(?, reason), updated = ? WHERE id = ?",
        params![state, unit, released_in, reason, now(), id],
    )?;
    Ok(())
}

/// A request's head moved (the item was reworked and delivered again before it merged).
pub fn request_head(c: &Connection, id: &str, head: &str) -> Result<()> {
    c.execute(
        "UPDATE lane_requests SET head = ?, updated = ? WHERE id = ?",
        params![head, now(), id],
    )?;
    Ok(())
}

pub fn unit_open(c: &Connection, lane: &str, no: i64, opened: i64) -> Result<()> {
    c.execute(
        "INSERT OR IGNORE INTO lane_units (lane_id, no, state, opened) VALUES (?, ?, 'collecting', ?)",
        params![lane, no, opened],
    )?;
    Ok(())
}

/// A unit's state; `branch` / `version` / `outcome` only when given; `ended` set with `done`.
pub fn unit_set(
    c: &Connection,
    lane: &str,
    no: i64,
    state: &str,
    branch: Option<&str>,
    version: Option<&str>,
    outcome: Option<&str>,
) -> Result<()> {
    let ended = (state == "done").then(now);
    c.execute(
        "UPDATE lane_units SET state = ?, branch = coalesce(?, branch), version = coalesce(?, version),
            outcome = coalesce(?, outcome), ended = coalesce(?, ended) WHERE lane_id = ? AND no = ?",
        params![state, branch, version, outcome, ended, lane, no],
    )?;
    Ok(())
}

/// The newest card of `kind` about a batch (`subject`: `lane:<lane>:<unit>`): its status and result.
pub fn batch_card(
    c: &Connection,
    subject: &str,
    kind: &str,
) -> Result<Option<(String, serde_json::Value)>> {
    Ok(c
        .query_row(
            "SELECT status, coalesce(result, 'null') FROM decisions
              WHERE subject_type = 'delivery_batch' AND subject_id = ? AND kind = ? ORDER BY created DESC LIMIT 1",
            params![subject, kind],
            |r| {
                let raw: String = r.get(1)?;
                Ok((r.get::<_, String>(0)?, serde_json::from_str(&raw).unwrap_or(serde_json::Value::Null)))
            },
        )
        .optional()?)
}

/// The text of a batch's approval card while it is still open (`subject`: `lane:<lane>:<unit>`): a
/// request joined the batch before you answered. `false` when no card of it is open.
pub fn batch_card_text(
    tx: &crate::Tx<'_>,
    subject: &str,
    question: &str,
    draft: &str,
) -> Result<bool> {
    let id: Option<String> = tx
        .query_row(
            "SELECT id FROM decisions WHERE subject_type = 'delivery_batch' AND subject_id = ?
              AND kind = 'approve' AND status = 'open' ORDER BY created DESC LIMIT 1",
            [subject],
            |r| r.get(0),
        )
        .optional()?;
    let Some(id) = id else {
        return Ok(false);
    };
    tx.execute(
        "UPDATE decisions SET question = ?, draft = ? WHERE id = ?",
        params![question, draft, id],
    )?;
    tx.publish(crate::ChangeEvent::Decision {
        id,
        item_id: Some(None),
    });
    Ok(true)
}

/// Marks the newest resolved card of a batch (`subject`, `kind`) as acted on: recovery after a
/// restart redoes only answers not acted on yet.
pub fn batch_card_acted(c: &Connection, subject: &str, kind: &str) -> Result<()> {
    c.execute(
        "UPDATE decisions SET result = json_set(coalesce(result, '{}'), '$.acted', json('true'))
          WHERE id = (SELECT id FROM decisions WHERE subject_type = 'delivery_batch' AND subject_id = ?
                       AND kind = ? AND status = 'resolved' ORDER BY created DESC LIMIT 1)",
        params![subject, kind],
    )?;
    Ok(())
}

pub fn units_of(c: &Connection, lane: &str) -> Result<Vec<LaneUnit>> {
    let mut st = c.prepare_cached("SELECT * FROM lane_units WHERE lane_id = ? ORDER BY no")?;
    let v = st
        .query_map([lane], unit)?
        .collect::<rusqlite::Result<_>>()?;
    Ok(v)
}
