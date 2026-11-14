//! The durable digest packet queue.
//!
//! A packet is `queued` when a window is cut, `leased` to one digest run (`batch_id`, until `lease_until`),
//! then `done`, or back to `queued` when the run failed (its finished chunks stay finished, so a retry only
//! redoes the rest), and `failed` after [`Batching::max_attempts`]. A lease that runs out returns the packet to
//! the queue. A batch is due every [`Batching::every_ms`] while packets wait, or as soon as
//! [`Batching::packets`] are waiting; higher priority goes first.
//!
//! The batching knobs are the workspace setting `digest` (JSON): `every_min`, `packets`, `chunk_tokens`,
//! `overlap`, `context`, `backlog_days`, `lease_min`, `max_attempts`, `enabled`.

use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use steve_store::{StoreError, Tx, uid};

type Result<T> = std::result::Result<T, StoreError>;

/// The workspace's batching settings.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Batching {
    /// The module runs (windows are cut) at all.
    pub enabled: bool,
    pub every_min: i64,
    pub packets: i64,
    /// A chunk's token budget.
    pub chunk_tokens: i64,
    /// Lines a chunk repeats from the one before.
    pub overlap: usize,
    /// Messages a window carries from the one before, for context.
    pub context: usize,
    /// Without a cursor: `0` processes all stored messages; a positive value limits initial lookback.
    /// No connector history is fetched.
    pub backlog_days: i64,
    pub lease_min: i64,
    pub max_attempts: i64,
}

impl Default for Batching {
    fn default() -> Self {
        Self {
            enabled: true,
            every_min: 30,
            packets: 20,
            chunk_tokens: 6000,
            overlap: 2,
            context: 3,
            backlog_days: 0,
            lease_min: 120,
            max_attempts: 3,
        }
    }
}

impl Batching {
    pub fn every_ms(&self) -> i64 {
        self.every_min.clamp(1, 7 * 24 * 60) * 60_000
    }
    pub fn lease_ms(&self) -> i64 {
        self.lease_min.clamp(1, 7 * 24 * 60) * 60_000
    }
}

pub fn batching(c: &Connection, ws: &str) -> Result<Batching> {
    let v: Option<String> = c
        .prepare_cached("SELECT value FROM settings WHERE workspace_id = ? AND key = 'digest'")?
        .query_row([ws], |r| r.get(0))
        .optional()?;
    Ok(v.and_then(|v| serde_json::from_str(&v).ok())
        .unwrap_or_default())
}

/// A packet to write.
pub struct NewPacket<'a> {
    pub ws: &'a str,
    pub conv: &'a str,
    pub seq: i64,
    pub kind: &'a str,
    pub trigger: &'a str,
    pub first_id: &'a str,
    pub last_id: &'a str,
    pub first_at: i64,
    pub last_at: i64,
    pub count: i64,
    pub priority: i64,
    pub facts: &'a Value,
    pub candidates: &'a Value,
}

/// Writes a packet and its chunks; returns its id.
pub fn insert(tx: &Tx, p: &NewPacket, chunks: &[crate::chunk::Chunk], now: i64) -> Result<String> {
    let id = uid("dpk");
    let tokens: i64 = chunks.iter().map(|c| c.tokens).sum();
    tx.execute(
        "INSERT INTO digest_packets (id, workspace_id, conv, seq, kind, trigger, first_id, last_id, first_at, last_at,
            count, tokens, priority, facts, candidates, created, updated)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        params![
            id,
            p.ws,
            p.conv,
            p.seq,
            p.kind,
            p.trigger,
            p.first_id,
            p.last_id,
            p.first_at,
            p.last_at,
            p.count,
            tokens,
            p.priority,
            p.facts.to_string(),
            p.candidates.to_string(),
            now,
            now
        ],
    )?;
    for c in chunks {
        tx.execute(
            "INSERT INTO digest_chunks (packet_id, n, first_pos, last_pos, overlap, tokens, text, updated) VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
            params![id, c.n, c.first_pos, c.last_pos, c.overlap, c.tokens, c.text, now],
        )?;
    }
    crate::event(tx, p.ws, &id);
    Ok(id)
}

/// A packet as stored.
#[derive(Debug, Clone, Serialize)]
pub struct Packet {
    pub id: String,
    pub ws: String,
    pub conv: String,
    pub seq: i64,
    pub kind: String,
    pub trigger: String,
    pub first_id: String,
    pub last_id: String,
    pub first_at: i64,
    pub last_at: i64,
    pub count: i64,
    pub tokens: i64,
    pub priority: i64,
    pub facts: Value,
    pub candidates: Value,
    pub state: String,
    pub batch_id: Option<String>,
    pub lease_until: Option<i64>,
    pub attempts: i64,
    pub error: Option<String>,
    pub created: i64,
    pub updated: i64,
}

const COLS: &str = "id, workspace_id, conv, seq, kind, trigger, first_id, last_id, first_at, last_at, count, tokens, priority,
    facts, candidates, state, batch_id, lease_until, attempts, error, created, updated";

fn row(r: &rusqlite::Row) -> rusqlite::Result<Packet> {
    let j = |s: String| serde_json::from_str(&s).unwrap_or(Value::Null);
    Ok(Packet {
        id: r.get(0)?,
        ws: r.get(1)?,
        conv: r.get(2)?,
        seq: r.get(3)?,
        kind: r.get(4)?,
        trigger: r.get(5)?,
        first_id: r.get(6)?,
        last_id: r.get(7)?,
        first_at: r.get(8)?,
        last_at: r.get(9)?,
        count: r.get(10)?,
        tokens: r.get(11)?,
        priority: r.get(12)?,
        facts: j(r.get(13)?),
        candidates: j(r.get(14)?),
        state: r.get(15)?,
        batch_id: r.get(16)?,
        lease_until: r.get(17)?,
        attempts: r.get(18)?,
        error: r.get(19)?,
        created: r.get(20)?,
        updated: r.get(21)?,
    })
}

pub fn get(c: &Connection, ws: &str, id: &str) -> Result<Option<Packet>> {
    Ok(c.prepare_cached(&format!(
        "SELECT {COLS} FROM digest_packets WHERE workspace_id = ? AND id = ?"
    ))?
    .query_row(params![ws, id], row)
    .optional()?)
}

/// Packets of `ws` in `state` (`None`: any), the most urgent first; `conv` narrows to one conversation.
pub fn list(
    c: &Connection,
    ws: &str,
    state: Option<&str>,
    conv: Option<&str>,
    limit: usize,
) -> Result<Vec<Packet>> {
    let sql = format!(
        "SELECT {COLS} FROM digest_packets WHERE workspace_id = ?1 AND (?2 IS NULL OR state = ?2) AND (?3 IS NULL OR conv = ?3)
          ORDER BY CASE state WHEN 'leased' THEN 0 WHEN 'queued' THEN 1 WHEN 'failed' THEN 2 ELSE 3 END,
                   priority DESC, created DESC LIMIT {limit}"
    );
    Ok(c.prepare_cached(&sql)?
        .query_map(params![ws, state, conv], row)?
        .collect::<rusqlite::Result<_>>()?)
}

/// One chunk as stored.
#[derive(Debug, Clone, Serialize)]
pub struct ChunkRow {
    pub n: i64,
    pub first_pos: i64,
    pub last_pos: i64,
    pub overlap: i64,
    pub tokens: i64,
    pub text: String,
    pub state: String,
    pub attempts: i64,
    pub error: Option<String>,
}

pub fn chunks(c: &Connection, packet: &str) -> Result<Vec<ChunkRow>> {
    Ok(c
        .prepare_cached(
            "SELECT n, first_pos, last_pos, overlap, tokens, text, state, attempts, error FROM digest_chunks WHERE packet_id = ? ORDER BY n",
        )?
        .query_map([packet], |r| {
            Ok(ChunkRow {
                n: r.get(0)?,
                first_pos: r.get(1)?,
                last_pos: r.get(2)?,
                overlap: r.get(3)?,
                tokens: r.get(4)?,
                text: r.get(5)?,
                state: r.get(6)?,
                attempts: r.get(7)?,
                error: r.get(8)?,
            })
        })?
        .collect::<rusqlite::Result<_>>()?)
}

/// Leases that ran out go back to the queue. Returns how many.
pub fn release_expired(tx: &Tx, ws: &str, now: i64) -> Result<usize> {
    let b = batching(tx, ws)?;
    Ok(tx.execute(
        "UPDATE digest_packets SET state = CASE WHEN attempts >= ?3 THEN 'failed' ELSE 'queued' END,
            batch_id = NULL, lease_until = NULL, error = 'lease expired', updated = ?2
          WHERE workspace_id = ?1 AND state = 'leased' AND lease_until <= ?2",
        params![ws, now, b.max_attempts.max(1)],
    )?)
}

/// Why a batch is due now (`packets` waiting, or the oldest waited `every`), `None` when it is not.
pub fn due(c: &Connection, ws: &str, now: i64, b: &Batching) -> Result<Option<&'static str>> {
    let (n, oldest): (i64, Option<i64>) = c
        .prepare_cached("SELECT count(*), min(created) FROM digest_packets WHERE workspace_id = ? AND state = 'queued'")?
        .query_row([ws], |r| Ok((r.get(0)?, r.get(1)?)))?;
    Ok(if n == 0 {
        None
    } else if n >= b.packets.max(1) {
        Some("packets")
    } else if oldest.is_some_and(|o| now - o >= b.every_ms()) {
        Some("every")
    } else {
        None
    })
}

/// Leases up to `max` queued packets of `ws` to a new batch: `(batch id, packet ids)`, the most urgent first.
pub fn lease(
    tx: &Tx,
    ws: &str,
    now: i64,
    b: &Batching,
    max: usize,
) -> Result<(String, Vec<String>)> {
    lease_matching(tx, ws, now, b, max, |_| Ok(true))
}

/// Lease only packets the consumer can still read. The predicate is evaluated before leasing,
/// so a source that became private neither spends attempts nor blocks other conversations.
pub fn lease_matching(
    tx: &Tx,
    ws: &str,
    now: i64,
    b: &Batching,
    max: usize,
    mut visible: impl FnMut(&Packet) -> Result<bool>,
) -> Result<(String, Vec<String>)> {
    release_expired(tx, ws, now)?;
    let batch = uid("dbt");
    let mut st = tx.prepare_cached(
        "SELECT p.id FROM digest_packets p WHERE p.workspace_id = ? AND p.state = 'queued'
            AND NOT EXISTS (SELECT 1 FROM digest_packets older WHERE older.workspace_id = p.workspace_id
                AND older.conv = p.conv AND older.seq < p.seq AND older.state <> 'done')
         ORDER BY p.priority DESC, p.created, p.conv, p.seq",
    )?;
    // Stream candidates until enough readable packets are found. A fixed pre-filter limit would
    // let many now-private sources permanently starve readable conversations behind them.
    let ids = st.query_map([ws], |r| r.get::<_, String>(0))?;
    let mut selected = Vec::new();
    for id in ids {
        if selected.len() >= max.min(500) {
            break;
        }
        let id = id?;
        let Some(packet) = get(tx, ws, &id)? else {
            continue;
        };
        if visible(&packet)? {
            selected.push(id);
        }
    }
    let ids = selected;
    for id in &ids {
        tx.execute(
            "UPDATE digest_packets SET state = 'leased', attempts = attempts + 1, batch_id = ?, lease_until = ?, updated = ? WHERE id = ?",
            params![batch, now + b.lease_ms(), now, id],
        )?;
        crate::event(tx, ws, id);
    }
    Ok((batch, ids))
}

/// A stale/expired lease cannot acknowledge a later worker's work.
fn owns(c: &Connection, p: &Packet, now: i64) -> Result<()> {
    let valid: bool = c.query_row(
        "SELECT EXISTS(SELECT 1 FROM digest_packets WHERE id = ? AND workspace_id = ?
         AND state = 'leased' AND batch_id = ? AND lease_until > ?)",
        params![p.id, p.ws, p.batch_id, now],
        |r| r.get(0),
    )?;
    if !valid {
        return Err(StoreError::Hook("digest lease no longer owned".into()));
    }
    Ok(())
}

/// Renew an owned lease between chunks, including when a large window spans sessions.
pub fn renew(tx: &Tx, p: &Packet, b: &Batching, now: i64) -> Result<()> {
    owns(tx, p, now)?;
    tx.execute(
        "UPDATE digest_packets SET lease_until = ?, updated = ? WHERE id = ?",
        params![now + b.lease_ms(), now, p.id],
    )?;
    Ok(())
}

/// Records one chunk's progress. Finished chunks are immutable across retries.
pub fn chunk_done(
    tx: &Tx,
    p: &Packet,
    n: i64,
    ok: bool,
    error: Option<&str>,
    now: i64,
) -> Result<bool> {
    owns(tx, p, now)?;
    Ok(tx.execute(
        "UPDATE digest_chunks SET state = ?, attempts = attempts + 1, error = ?, updated = ?
         WHERE packet_id = ? AND n = ? AND state <> 'done'",
        params![if ok { "done" } else { "failed" }, error, now, p.id, n],
    )? > 0)
}

/// A packet completes only when every chunk completed. A failed attempt retains completed chunks;
/// exhausting the attempt limit (including crashed/expired leases) leaves an inspectable failed packet.
pub fn packet_done(
    tx: &Tx,
    p: &Packet,
    ok: bool,
    error: Option<&str>,
    b: &Batching,
    now: i64,
) -> Result<String> {
    owns(tx, p, now)?;
    if ok {
        let left: i64 = tx.query_row(
            "SELECT count(*) FROM digest_chunks WHERE packet_id = ? AND state <> 'done'",
            [&p.id],
            |r| r.get(0),
        )?;
        if left != 0 {
            return Err(StoreError::Hook("digest chunks are unfinished".into()));
        }
    }
    let attempts: i64 = tx.query_row(
        "SELECT attempts FROM digest_packets WHERE id = ?",
        [&p.id],
        |r| r.get(0),
    )?;
    let state = if ok {
        "done"
    } else if attempts >= b.max_attempts.max(1) {
        "failed"
    } else {
        "queued"
    };
    tx.execute(
        "UPDATE digest_packets SET state = ?, error = ?, batch_id = NULL, lease_until = NULL, updated = ? WHERE id = ?",
        params![state, error, now, p.id],
    )?;
    if !ok {
        tx.execute("UPDATE digest_chunks SET state = 'queued', updated = ? WHERE packet_id = ? AND state = 'failed'", params![now, p.id])?;
    }
    crate::event(tx, &p.ws, &p.id);
    Ok(state.into())
}

/// An explicit operator retry unblocks a failed conversation without losing finished chunks.
/// It resets the attempt budget, not the conversation cursor or its message receipts.
pub fn retry(tx: &Tx, ws: &str, id: &str, now: i64) -> Result<bool> {
    let changed = tx.execute(
        "UPDATE digest_packets SET state = 'queued', attempts = 0, error = NULL, updated = ?
         WHERE workspace_id = ? AND id = ? AND state = 'failed'",
        params![now, ws, id],
    )? > 0;
    if changed {
        tx.execute("UPDATE digest_chunks SET state = 'queued', error = NULL, updated = ? WHERE packet_id = ? AND state <> 'done'", params![now, id])?;
        crate::event(tx, ws, id);
    }
    Ok(changed)
}

/// Counts by state, and the oldest waiting packet.
pub fn stats(c: &Connection, ws: &str, conv: Option<&str>) -> Result<Value> {
    let mut out = json!({"queued": 0, "leased": 0, "done": 0, "failed": 0});
    let mut st = c.prepare_cached(
        "SELECT state, count(*), sum(tokens) FROM digest_packets WHERE workspace_id = ?1 AND (?2 IS NULL OR conv = ?2) GROUP BY state",
    )?;
    let mut tokens = 0i64;
    for r in st.query_map(params![ws, conv], |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, i64>(1)?,
            r.get::<_, Option<i64>>(2)?,
        ))
    })? {
        let (s, n, t) = r?;
        if s == "queued" || s == "leased" {
            tokens += t.unwrap_or(0);
        }
        out[s] = json!(n);
    }
    out["waiting_tokens"] = json!(tokens);
    out["oldest_queued"] = json!(
        c.prepare_cached(
            "SELECT min(created) FROM digest_packets WHERE workspace_id = ?1 AND state = 'queued' AND (?2 IS NULL OR conv = ?2)"
        )?
        .query_row(params![ws, conv], |r| r.get::<_, Option<i64>>(0))?
    );
    Ok(out)
}
