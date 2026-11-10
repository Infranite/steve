//! The merge queue (059): targets, entries, batches, trains, the Node steps they took, and their
//! statistics. The rules live in steve-core `mq::queue` and `mq::train`; the state is these rows.
//! Writers take the transaction they are part of, so an event's rows change together. The converter
//! of the 031 lanes ([`convert_031`]) is here too: it is the store's own data.

use rusqlite::{Connection, OptionalExtension, params};
use serde::Serialize;

use crate::{Result, now, uid};

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct MqTarget {
    pub id: String,
    pub upstream: String,
    pub branch: String,
    pub mode: Option<String>,
    pub mode_evidence: Option<String>,
    pub node_id: Option<String>,
    pub granted_by: Option<String>,
    pub granted_at: Option<i64>,
    pub policy: String,
    pub let_through: bool,
    pub next_batch: i64,
    pub next_train: i64,
    pub legacy_lane: Option<String>,
    pub created: i64,
    pub updated: i64,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct MqEntry {
    pub id: String,
    pub target_id: String,
    pub workspace_id: Option<String>,
    pub item_id: Option<String>,
    pub cycle: Option<i64>,
    pub key: String,
    /// Keys it comes after (JSON array).
    pub after: String,
    pub head: String,
    pub branch: Option<String>,
    pub consent: Option<String>,
    pub node_id: Option<String>,
    pub state: String,
    pub batch_no: Option<i64>,
    pub train_no: Option<i64>,
    pub landed_commit: Option<String>,
    pub landed_at: Option<i64>,
    pub release_now: bool,
    /// Its pull request, when it went to the host's merge queue.
    pub pr: Option<i64>,
    pub reason: Option<String>,
    pub legacy_id: Option<String>,
    pub arrived: i64,
    pub updated: i64,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct MqBatch {
    pub target_id: String,
    pub no: i64,
    pub parent: Option<i64>,
    pub root: i64,
    pub state: String,
    pub step: Option<String>,
    pub base: Option<String>,
    pub tip: Option<String>,
    /// Entry ids in order (JSON array).
    pub members: String,
    pub checks: Option<String>,
    pub pr: Option<String>,
    pub outcome: Option<String>,
    pub reruns: i64,
    pub opened: i64,
    pub ended: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct MqTrain {
    pub target_id: String,
    pub no: i64,
    pub state: String,
    pub now: bool,
    pub tip: Option<String>,
    pub version: Option<String>,
    pub tag: Option<String>,
    pub decision_id: Option<String>,
    pub opened: i64,
    pub last: i64,
    pub closes_at: Option<i64>,
    pub ended: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct MqStep {
    pub operation_id: String,
    pub target_id: String,
    pub batch_no: Option<i64>,
    pub train_no: Option<i64>,
    pub kind: String,
    pub state: String,
    pub result: Option<String>,
    pub created: i64,
    pub updated: i64,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct MqStat {
    pub id: i64,
    pub target_id: String,
    pub at: i64,
    pub kind: String,
    pub batch_no: Option<i64>,
    pub train_no: Option<i64>,
    pub data: serde_json::Value,
}

fn target(r: &rusqlite::Row<'_>) -> rusqlite::Result<MqTarget> {
    Ok(MqTarget {
        id: r.get("id")?,
        upstream: r.get("upstream")?,
        branch: r.get("branch")?,
        mode: r.get("mode")?,
        mode_evidence: r.get("mode_evidence")?,
        node_id: r.get("node_id")?,
        granted_by: r.get("granted_by")?,
        granted_at: r.get("granted_at")?,
        policy: r.get("policy")?,
        let_through: r.get::<_, i64>("let_through")? != 0,
        next_batch: r.get("next_batch")?,
        next_train: r.get("next_train")?,
        legacy_lane: r.get("legacy_lane")?,
        created: r.get("created")?,
        updated: r.get("updated")?,
    })
}

fn entry(r: &rusqlite::Row<'_>) -> rusqlite::Result<MqEntry> {
    Ok(MqEntry {
        id: r.get("id")?,
        target_id: r.get("target_id")?,
        workspace_id: r.get("workspace_id")?,
        item_id: r.get("item_id")?,
        cycle: r.get("cycle")?,
        key: r.get("key")?,
        after: r.get("after")?,
        head: r.get("head")?,
        branch: r.get("branch")?,
        consent: r.get("consent")?,
        node_id: r.get("node_id")?,
        state: r.get("state")?,
        batch_no: r.get("batch_no")?,
        train_no: r.get("train_no")?,
        landed_commit: r.get("landed_commit")?,
        landed_at: r.get("landed_at")?,
        release_now: r.get::<_, i64>("release_now")? != 0,
        pr: r.get("pr")?,
        reason: r.get("reason")?,
        legacy_id: r.get("legacy_id")?,
        arrived: r.get("arrived")?,
        updated: r.get("updated")?,
    })
}

fn batch(r: &rusqlite::Row<'_>) -> rusqlite::Result<MqBatch> {
    Ok(MqBatch {
        target_id: r.get("target_id")?,
        no: r.get("no")?,
        parent: r.get("parent")?,
        root: r.get("root")?,
        state: r.get("state")?,
        step: r.get("step")?,
        base: r.get("base")?,
        tip: r.get("tip")?,
        members: r.get("members")?,
        checks: r.get("checks")?,
        pr: r.get("pr")?,
        outcome: r.get("outcome")?,
        reruns: r.get("reruns")?,
        opened: r.get("opened")?,
        ended: r.get("ended")?,
    })
}

fn train(r: &rusqlite::Row<'_>) -> rusqlite::Result<MqTrain> {
    Ok(MqTrain {
        target_id: r.get("target_id")?,
        no: r.get("no")?,
        state: r.get("state")?,
        now: r.get::<_, i64>("now")? != 0,
        tip: r.get("tip")?,
        version: r.get("version")?,
        tag: r.get("tag")?,
        decision_id: r.get("decision_id")?,
        opened: r.get("opened")?,
        last: r.get("last")?,
        closes_at: r.get("closes_at")?,
        ended: r.get("ended")?,
    })
}

fn step(r: &rusqlite::Row<'_>) -> rusqlite::Result<MqStep> {
    Ok(MqStep {
        operation_id: r.get("operation_id")?,
        target_id: r.get("target_id")?,
        batch_no: r.get("batch_no")?,
        train_no: r.get("train_no")?,
        kind: r.get("kind")?,
        state: r.get("state")?,
        result: r.get("result")?,
        created: r.get("created")?,
        updated: r.get("updated")?,
    })
}

fn stat(r: &rusqlite::Row<'_>) -> rusqlite::Result<MqStat> {
    let raw: String = r.get("data")?;
    Ok(MqStat {
        id: r.get("id")?,
        target_id: r.get("target_id")?,
        at: r.get("at")?,
        kind: r.get("kind")?,
        batch_no: r.get("batch_no")?,
        train_no: r.get("train_no")?,
        data: serde_json::from_str(&raw).unwrap_or(serde_json::Value::Null),
    })
}

// ── targets ──

/// The target (`upstream`, `branch`), made on first use.
pub fn target_for(c: &Connection, upstream: &str, branch: &str) -> Result<MqTarget> {
    let t = now();
    c.execute(
        "INSERT OR IGNORE INTO mq_targets (id, upstream, branch, created, updated) VALUES (?, ?, ?, ?, ?)",
        params![uid("mq"), upstream, branch, t, t],
    )?;
    Ok(c.query_row(
        "SELECT * FROM mq_targets WHERE upstream = ? AND branch = ?",
        params![upstream, branch],
        target,
    )?)
}

pub fn target_get(c: &Connection, id: &str) -> Result<Option<MqTarget>> {
    Ok(
        c.query_row("SELECT * FROM mq_targets WHERE id = ?", [id], target)
            .optional()?,
    )
}

pub fn target_by(c: &Connection, upstream: &str, branch: &str) -> Result<Option<MqTarget>> {
    Ok(c.query_row(
        "SELECT * FROM mq_targets WHERE upstream = ? AND branch = ?",
        params![upstream, branch],
        target,
    )
    .optional()?)
}

pub fn targets_all(c: &Connection) -> Result<Vec<MqTarget>> {
    let mut st = c.prepare_cached("SELECT * FROM mq_targets ORDER BY created, id")?;
    let v = st.query_map([], target)?.collect::<rusqlite::Result<_>>()?;
    Ok(v)
}

pub fn target_policy_set(c: &Connection, id: &str, policy: &str) -> Result<()> {
    c.execute(
        "UPDATE mq_targets SET policy = ?, updated = ? WHERE id = ?",
        params![policy, now(), id],
    )?;
    Ok(())
}

/// Allowed to merge into it, by `by` (once: an earlier grant stays).
pub fn target_grant(c: &Connection, id: &str, by: &str) -> Result<()> {
    let t = now();
    c.execute(
        "UPDATE mq_targets SET granted_by = coalesce(granted_by, ?), granted_at = coalesce(granted_at, ?), updated = ?
          WHERE id = ?",
        params![by, t, t, id],
    )?;
    Ok(())
}

pub fn target_mode_set(c: &Connection, id: &str, mode: Option<&str>, evidence: &str) -> Result<()> {
    c.execute(
        "UPDATE mq_targets SET mode = ?, mode_evidence = ?, updated = ? WHERE id = ?",
        params![mode, evidence, now(), id],
    )?;
    Ok(())
}

pub fn target_node_set(c: &Connection, id: &str, node: Option<&str>) -> Result<()> {
    c.execute(
        "UPDATE mq_targets SET node_id = ?, updated = ? WHERE id = ?",
        params![node, now(), id],
    )?;
    Ok(())
}

/// The queue-wide counters of the state machines.
pub fn target_counters_set(
    c: &Connection,
    id: &str,
    next_batch: i64,
    next_train: i64,
    let_through: bool,
) -> Result<()> {
    c.execute(
        "UPDATE mq_targets SET next_batch = max(next_batch, ?), next_train = max(next_train, ?), let_through = ?,
            updated = ? WHERE id = ?",
        params![next_batch, next_train, let_through as i64, now(), id],
    )?;
    Ok(())
}

// ── entries ──

pub struct NewEntry<'a> {
    pub target_id: &'a str,
    pub workspace_id: Option<&'a str>,
    pub item_id: Option<&'a str>,
    pub cycle: Option<i64>,
    /// `None`: the item, else the entry itself.
    pub key: Option<&'a str>,
    pub after: &'a [String],
    pub head: &'a str,
    pub branch: Option<&'a str>,
    pub consent: Option<&'a str>,
    pub node_id: Option<&'a str>,
}

pub fn entry_add(c: &Connection, e: &NewEntry<'_>) -> Result<MqEntry> {
    let (id, t) = (uid("me"), now());
    let key = e.key.or(e.item_id).unwrap_or(&id).to_string();
    let after = serde_json::to_string(e.after)?;
    c.execute(
        "INSERT INTO mq_entries (id, target_id, workspace_id, item_id, cycle, key, after, head, branch, consent, node_id,
            state, arrived, updated)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, 'queued', ?, ?)",
        params![
            id, e.target_id, e.workspace_id, e.item_id, e.cycle, key, after, e.head, e.branch, e.consent, e.node_id, t,
            t
        ],
    )?;
    Ok(c.query_row("SELECT * FROM mq_entries WHERE id = ?", [&id], entry)?)
}

pub fn entry_get(c: &Connection, id: &str) -> Result<Option<MqEntry>> {
    Ok(
        c.query_row("SELECT * FROM mq_entries WHERE id = ?", [id], entry)
            .optional()?,
    )
}

/// The item's entry still in a queue or a train (queued, batched, landed), if any.
pub fn entry_live_of_item(c: &Connection, item: &str) -> Result<Option<MqEntry>> {
    Ok(c.query_row(
        "SELECT * FROM mq_entries WHERE item_id = ? AND state IN ('queued', 'batched', 'landed')
          ORDER BY arrived DESC, rowid DESC LIMIT 1",
        [item],
        entry,
    )
    .optional()?)
}

/// The item's newest entry, whatever became of it.
pub fn entry_last_of_item(c: &Connection, item: &str) -> Result<Option<MqEntry>> {
    Ok(c.query_row(
        "SELECT * FROM mq_entries WHERE item_id = ? ORDER BY arrived DESC, rowid DESC LIMIT 1",
        [item],
        entry,
    )
    .optional()?)
}

/// What the queue of `target` holds: its queued and batched entries, the returned ones they come
/// after, and the landed ones not released (the train's), oldest first.
pub fn entries_live(c: &Connection, target: &str) -> Result<Vec<MqEntry>> {
    let mut st = c.prepare_cached(
        "SELECT * FROM mq_entries e WHERE target_id = ?1 AND (
            state IN ('queued', 'batched', 'landed')
            OR (state = 'returned' AND EXISTS (
                  SELECT 1 FROM mq_entries w, json_each(w.after) a
                   WHERE w.target_id = ?1 AND w.state IN ('queued', 'batched') AND a.value = e.key)
                AND NOT EXISTS (
                  SELECT 1 FROM mq_entries n WHERE n.target_id = ?1 AND n.key = e.key AND n.arrived > e.arrived)))
          ORDER BY arrived, rowid",
    )?;
    let v = st
        .query_map([target], entry)?
        .collect::<rusqlite::Result<_>>()?;
    Ok(v)
}

/// The entries of `target` (newest first, at most `limit`): what the queue page lists.
pub fn entries_recent(c: &Connection, target: &str, limit: i64) -> Result<Vec<MqEntry>> {
    let mut st = c.prepare_cached(
        "SELECT * FROM mq_entries WHERE target_id = ? ORDER BY arrived DESC, rowid DESC LIMIT ?",
    )?;
    let v = st
        .query_map(params![target, limit], entry)?
        .collect::<rusqlite::Result<_>>()?;
    Ok(v)
}

/// An entry's state and where it is; `reason` only when given.
pub fn entry_set(
    c: &Connection,
    id: &str,
    state: &str,
    batch_no: Option<i64>,
    train_no: Option<i64>,
    reason: Option<&str>,
) -> Result<()> {
    c.execute(
        "UPDATE mq_entries SET state = ?, batch_no = ?, train_no = ?, reason = coalesce(?, reason), updated = ?
          WHERE id = ?",
        params![state, batch_no, train_no, reason, now(), id],
    )?;
    Ok(())
}

pub fn entry_landed(c: &Connection, id: &str, commit: Option<&str>, at: i64) -> Result<()> {
    c.execute(
        "UPDATE mq_entries SET landed_commit = coalesce(?, landed_commit), landed_at = coalesce(landed_at, ?),
            updated = ? WHERE id = ?",
        params![commit, at, now(), id],
    )?;
    Ok(())
}

/// The entry went to the host's merge queue as pull request `pr`.
pub fn entry_pr_set(c: &Connection, id: &str, pr: i64) -> Result<()> {
    c.execute(
        "UPDATE mq_entries SET pr = ?, state = 'batched', updated = ? WHERE id = ?",
        params![pr, now(), id],
    )?;
    Ok(())
}

pub fn entry_release_now(c: &Connection, id: &str, on: bool) -> Result<()> {
    c.execute(
        "UPDATE mq_entries SET release_now = ?, updated = ? WHERE id = ?",
        params![on as i64, now(), id],
    )?;
    Ok(())
}

// ── batches ──

/// Write `b` as it is now (made the first time).
pub fn batch_put(c: &Connection, b: &MqBatch) -> Result<()> {
    c.execute(
        "INSERT INTO mq_batches (target_id, no, parent, root, state, step, base, tip, members, checks, pr, outcome,
            reruns, opened, ended)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
         ON CONFLICT (target_id, no) DO UPDATE SET parent = excluded.parent, root = excluded.root,
            state = excluded.state, step = excluded.step, base = excluded.base, tip = excluded.tip,
            members = excluded.members, checks = excluded.checks, pr = excluded.pr, outcome = excluded.outcome,
            reruns = excluded.reruns, ended = excluded.ended",
        params![
            b.target_id,
            b.no,
            b.parent,
            b.root,
            b.state,
            b.step,
            b.base,
            b.tip,
            b.members,
            b.checks,
            b.pr,
            b.outcome,
            b.reruns,
            b.opened,
            b.ended
        ],
    )?;
    Ok(())
}

pub fn batch_get(c: &Connection, target: &str, no: i64) -> Result<Option<MqBatch>> {
    Ok(c.query_row(
        "SELECT * FROM mq_batches WHERE target_id = ? AND no = ?",
        params![target, no],
        batch,
    )
    .optional()?)
}

/// The batches a bisection still needs: every batch of a root with a live one.
pub fn batches_live(c: &Connection, target: &str) -> Result<Vec<MqBatch>> {
    let mut st = c.prepare_cached(
        "SELECT * FROM mq_batches WHERE target_id = ?1 AND root IN (
            SELECT root FROM mq_batches WHERE target_id = ?1 AND state IN ('running', 'waiting', 'unknown'))
          ORDER BY no",
    )?;
    let v = st
        .query_map([target], batch)?
        .collect::<rusqlite::Result<_>>()?;
    Ok(v)
}

/// The newest batches of `target` (at most `limit`), newest first.
pub fn batches_recent(c: &Connection, target: &str, limit: i64) -> Result<Vec<MqBatch>> {
    let mut st =
        c.prepare_cached("SELECT * FROM mq_batches WHERE target_id = ? ORDER BY no DESC LIMIT ?")?;
    let v = st
        .query_map(params![target, limit], batch)?
        .collect::<rusqlite::Result<_>>()?;
    Ok(v)
}

/// The step a running batch is at, and what it found.
pub fn batch_step(
    c: &Connection,
    target: &str,
    no: i64,
    step: Option<&str>,
    base: Option<&str>,
    tip: Option<&str>,
) -> Result<()> {
    c.execute(
        "UPDATE mq_batches SET step = ?, base = coalesce(?, base), tip = coalesce(?, tip)
          WHERE target_id = ? AND no = ?",
        params![step, base, tip, target, no],
    )?;
    Ok(())
}

pub fn batch_result(
    c: &Connection,
    target: &str,
    no: i64,
    checks: Option<&str>,
    pr: Option<&str>,
    outcome: Option<&str>,
) -> Result<()> {
    c.execute(
        "UPDATE mq_batches SET checks = coalesce(?, checks), pr = coalesce(?, pr), outcome = coalesce(?, outcome)
          WHERE target_id = ? AND no = ?",
        params![checks, pr, outcome, target, no],
    )?;
    Ok(())
}

// ── trains ──

pub fn train_put(c: &Connection, t: &MqTrain) -> Result<()> {
    c.execute(
        "INSERT INTO mq_trains (target_id, no, state, now, tip, version, tag, decision_id, opened, last, closes_at, ended)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
         ON CONFLICT (target_id, no) DO UPDATE SET state = excluded.state, now = excluded.now, tip = excluded.tip,
            version = excluded.version, tag = excluded.tag, decision_id = excluded.decision_id,
            opened = excluded.opened, last = excluded.last, closes_at = excluded.closes_at, ended = excluded.ended",
        params![
            t.target_id,
            t.no,
            t.state,
            t.now as i64,
            t.tip,
            t.version,
            t.tag,
            t.decision_id,
            t.opened,
            t.last,
            t.closes_at,
            t.ended
        ],
    )?;
    Ok(())
}

pub fn train_get(c: &Connection, target: &str, no: i64) -> Result<Option<MqTrain>> {
    Ok(c.query_row(
        "SELECT * FROM mq_trains WHERE target_id = ? AND no = ?",
        params![target, no],
        train,
    )
    .optional()?)
}

/// The trains not over (open, waiting for you, releasing, failed, unknown, halted).
pub fn trains_live(c: &Connection, target: &str) -> Result<Vec<MqTrain>> {
    let mut st = c.prepare_cached(
        "SELECT * FROM mq_trains WHERE target_id = ? AND state NOT IN ('released', 'dropped') ORDER BY no",
    )?;
    let v = st
        .query_map([target], train)?
        .collect::<rusqlite::Result<_>>()?;
    Ok(v)
}

pub fn trains_recent(c: &Connection, target: &str, limit: i64) -> Result<Vec<MqTrain>> {
    let mut st =
        c.prepare_cached("SELECT * FROM mq_trains WHERE target_id = ? ORDER BY no DESC LIMIT ?")?;
    let v = st
        .query_map(params![target, limit], train)?
        .collect::<rusqlite::Result<_>>()?;
    Ok(v)
}

/// The entries of train `no`, in the order they landed.
pub fn train_members(c: &Connection, target: &str, no: i64) -> Result<Vec<MqEntry>> {
    let mut st = c.prepare_cached(
        "SELECT * FROM mq_entries WHERE target_id = ? AND train_no = ? ORDER BY landed_at, arrived, rowid",
    )?;
    let v = st
        .query_map(params![target, no], entry)?
        .collect::<rusqlite::Result<_>>()?;
    Ok(v)
}

// ── steps ──

/// Record a Node step (made the first time; later its state and result).
pub fn step_put(c: &Connection, s: &MqStep) -> Result<()> {
    let t = now();
    c.execute(
        "INSERT INTO mq_steps (operation_id, target_id, batch_no, train_no, kind, state, result, created, updated)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)
         ON CONFLICT (operation_id) DO UPDATE SET state = excluded.state,
            result = coalesce(excluded.result, mq_steps.result), updated = excluded.updated",
        params![
            s.operation_id,
            s.target_id,
            s.batch_no,
            s.train_no,
            s.kind,
            s.state,
            s.result,
            t,
            t
        ],
    )?;
    Ok(())
}

/// The steps of batch `no` (`train` false) or train `no`, in order.
pub fn steps_of(c: &Connection, target: &str, no: i64, train: bool) -> Result<Vec<MqStep>> {
    let sql = if train {
        "SELECT * FROM mq_steps WHERE target_id = ? AND train_no = ? ORDER BY created, rowid"
    } else {
        "SELECT * FROM mq_steps WHERE target_id = ? AND batch_no = ? ORDER BY created, rowid"
    };
    let mut st = c.prepare_cached(sql)?;
    let v = st
        .query_map(params![target, no], step)?
        .collect::<rusqlite::Result<_>>()?;
    Ok(v)
}

// ── statistics ──

pub fn stat_add(
    c: &Connection,
    target: &str,
    kind: &str,
    batch_no: Option<i64>,
    train_no: Option<i64>,
    data: &serde_json::Value,
) -> Result<()> {
    c.execute(
        "INSERT INTO mq_stats (target_id, at, kind, batch_no, train_no, data) VALUES (?, ?, ?, ?, ?, ?)",
        params![
            target,
            now(),
            kind,
            batch_no,
            train_no,
            serde_json::to_string(data)?
        ],
    )?;
    Ok(())
}

/// `target`'s facts since `since` (all targets with `None`), oldest first, at most `limit`.
pub fn stats_since(
    c: &Connection,
    target: Option<&str>,
    since: i64,
    limit: i64,
) -> Result<Vec<MqStat>> {
    let mut st = c.prepare_cached(
        "SELECT * FROM mq_stats WHERE (?1 IS NULL OR target_id = ?1) AND at >= ?2 ORDER BY at, id LIMIT ?3",
    )?;
    let v = st
        .query_map(params![target, since, limit], stat)?
        .collect::<rusqlite::Result<_>>()?;
    Ok(v)
}

// ── the 031 lanes ──

/// A 031 lane: id, upstream, branch, window, members, created.
type OldLane = (String, String, String, Option<i64>, Option<i64>, i64);
/// A 031 request: id, workspace, item, branch, head, unit, released in, state, arrived.
type OldRequest = (
    String,
    Option<String>,
    Option<String>,
    String,
    String,
    Option<i64>,
    Option<i64>,
    String,
    i64,
);

/// What [`convert_031`] did.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct Converted {
    pub targets: usize,
    pub queued: usize,
    pub landed: usize,
    pub failed_trains: usize,
    /// Batch approval cards no longer asked (their batches are merged by the queue instead).
    pub superseded: Vec<String>,
    /// The items told on their timeline.
    pub items: Vec<String>,
}

/// The 031 lanes into the queue, once each (a lane converted before is skipped: run it at every
/// boot). In the caller's transaction, so a lane converts whole or not at all:
///
/// - a lane: its target (window → `release_window_s`, members → `max_batch`; its never-set checks are
///   dropped), allowed to merge (`migrated-031`: you approved its batches), its batch numbers
///   continued;
/// - queued, collecting and merging requests: queued entries with their heads, the consent the item's
///   last passing acceptance. A merging unit's approval card is superseded: nothing was pushed
///   without its answer, and what was pushed is found on the target when it is integrated again;
/// - merged requests waiting for their release: landed, in a new open train;
/// - a unit whose release failed: a failed train, its card kept and pointed at the train;
/// - released or left requests and ended units: not converted (the 031 tables stay, read only).
///
/// `note`: the timeline line for each item converted.
pub fn convert_031(tx: &crate::Tx<'_>, note: &str) -> Result<Converted> {
    let mut out = Converted::default();
    let lanes: Vec<OldLane> = {
        let mut st = tx.prepare(
            "SELECT id, upstream, branch, window_ms, max_members, created FROM lanes
              WHERE id NOT IN (SELECT legacy_lane FROM mq_targets WHERE legacy_lane IS NOT NULL)
              ORDER BY created, id",
        )?;
        st.query_map([], |r| {
            Ok((
                r.get(0)?,
                r.get(1)?,
                r.get(2)?,
                r.get(3)?,
                r.get(4)?,
                r.get(5)?,
            ))
        })?
        .collect::<rusqlite::Result<_>>()?
    };
    let t = now();
    for (lane, upstream, branch, window_ms, max_members, _) in lanes {
        let target = target_for(tx, &upstream, &branch)?;
        let mut policy: serde_json::Map<String, serde_json::Value> =
            serde_json::from_str(&target.policy).unwrap_or_default();
        if let Some(w) = window_ms {
            policy
                .entry("release_window_s")
                .or_insert(serde_json::json!(w / 1000));
        }
        if let Some(m) = max_members {
            policy.entry("max_batch").or_insert(serde_json::json!(m));
        }
        let last_unit: i64 = tx.query_row(
            "SELECT coalesce(max(no), 0) FROM lane_units WHERE lane_id = ?",
            [&lane],
            |r| r.get(0),
        )?;
        tx.execute(
            "UPDATE mq_targets SET policy = ?, legacy_lane = ?, next_batch = max(next_batch, ?),
                granted_by = coalesce(granted_by, 'migrated-031'), granted_at = coalesce(granted_at, ?), updated = ?
              WHERE id = ?",
            params![
                serde_json::to_string(&policy)?,
                lane,
                last_unit,
                t,
                t,
                target.id
            ],
        )?;
        out.targets += 1;

        let reqs: Vec<OldRequest> = {
            let mut st = tx.prepare(
                "SELECT id, workspace_id, item_id, branch, head, unit, released_in, state, arrived FROM lane_requests
                  WHERE lane_id = ? AND state IN ('queued', 'collecting', 'merging', 'merged') ORDER BY arrived, rowid",
            )?;
            st.query_map([&lane], |r| {
                Ok((
                    r.get(0)?,
                    r.get(1)?,
                    r.get(2)?,
                    r.get(3)?,
                    r.get(4)?,
                    r.get(5)?,
                    r.get(6)?,
                    r.get(7)?,
                    r.get(8)?,
                ))
            })?
            .collect::<rusqlite::Result<_>>()?
        };
        // units whose release failed: one failed train each, their requests with it
        let failed: Vec<i64> = {
            let mut st = tx.prepare(
                "SELECT no FROM lane_units WHERE lane_id = ? AND state = 'release_failed' ORDER BY no",
            )?;
            st.query_map([&lane], |r| r.get(0))?
                .collect::<rusqlite::Result<_>>()?
        };
        let mut next_train = target.next_train;
        let mut train_of_unit: Vec<(i64, i64)> = vec![];
        for unit in &failed {
            next_train += 1;
            train_put(
                tx,
                &MqTrain {
                    target_id: target.id.clone(),
                    no: next_train,
                    state: "failed".into(),
                    now: false,
                    tip: None,
                    version: None,
                    tag: None,
                    decision_id: None,
                    opened: t,
                    last: t,
                    closes_at: None,
                    ended: None,
                },
            )?;
            tx.execute(
                "UPDATE decisions SET subject_type = 'mq_train', subject_id = ?
                  WHERE subject_type = 'delivery_batch' AND subject_id = ? AND kind = 'fail' AND status = 'open'",
                params![
                    format!("{}:{next_train}", target.id),
                    format!("lane:{lane}:{unit}")
                ],
            )?;
            train_of_unit.push((*unit, next_train));
            out.failed_trains += 1;
        }
        let mut open_train: Option<i64> = None;
        for (rid, ws, item, br, head, unit, released_in, state, arrived) in reqs {
            let consent: Option<String> = match &item {
                Some(i) => tx
                    .query_row(
                        "SELECT id FROM decisions WHERE item_id = ? AND kind = 'accept' AND status = 'resolved'
                            AND json_extract(coalesce(result, '{}'), '$.choice') = 'pass'
                          ORDER BY resolved DESC, created DESC LIMIT 1",
                        [i],
                        |r| r.get(0),
                    )
                    .optional()?,
                None => None,
            };
            let (st, train) = if state == "merged" {
                let failed_train = released_in
                    .and_then(|u| train_of_unit.iter().find(|(x, _)| *x == u).map(|(_, n)| *n));
                let train = match failed_train {
                    Some(n) => n,
                    None => match open_train {
                        Some(n) => n,
                        None => {
                            next_train += 1;
                            train_put(
                                tx,
                                &MqTrain {
                                    target_id: target.id.clone(),
                                    no: next_train,
                                    state: "open".into(),
                                    now: false,
                                    tip: None,
                                    version: None,
                                    tag: None,
                                    decision_id: None,
                                    opened: t,
                                    last: t,
                                    closes_at: None,
                                    ended: None,
                                },
                            )?;
                            open_train = Some(next_train);
                            next_train
                        }
                    },
                };
                out.landed += 1;
                ("landed", Some(train))
            } else {
                out.queued += 1;
                ("queued", None)
            };
            let id = uid("me");
            let key = item.clone().unwrap_or_else(|| id.clone());
            tx.execute(
                "INSERT OR IGNORE INTO mq_entries (id, target_id, workspace_id, item_id, key, head, branch, consent,
                    state, train_no, landed_commit, landed_at, legacy_id, arrived, updated)
                 VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
                params![
                    id,
                    target.id,
                    ws,
                    item,
                    key,
                    head,
                    (!br.is_empty()).then_some(br),
                    consent,
                    st,
                    train,
                    (st == "landed").then_some(head.clone()),
                    (st == "landed").then_some(t),
                    rid,
                    arrived,
                    t
                ],
            )?;
            // a merging unit's open approval card: the queue merges it instead
            if state == "merging"
                && let Some(u) = unit
            {
                let subject = format!("lane:{lane}:{u}");
                let open: Vec<String> = {
                    let mut st = tx.prepare(
                        "SELECT id FROM decisions WHERE subject_type = 'delivery_batch' AND subject_id = ?
                            AND kind = 'approve' AND status = 'open'",
                    )?;
                    st.query_map([&subject], |r| r.get(0))?
                        .collect::<rusqlite::Result<_>>()?
                };
                for d in open {
                    crate::decision_resolve_in(
                        tx,
                        &d,
                        &serde_json::json!({"superseded": "merge-queue"}),
                        "expired",
                    )?;
                    out.superseded.push(d);
                }
            }
            if let Some(i) = &item {
                crate::timeline_add_in(tx, i, "steve", "sys", note, None)?;
                out.items.push(i.clone());
            }
        }
        tx.execute(
            "UPDATE mq_targets SET next_train = max(next_train, ?) WHERE id = ?",
            params![next_train, target.id],
        )?;
    }
    Ok(out)
}
