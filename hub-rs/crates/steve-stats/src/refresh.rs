//! Keeping the derived tables current (stats.md §2.6): the `item_stats` refresh every 2 seconds and the
//! full `rebuild` of `stat_daily` from the detail rows (needed after `stats.tz` changes, and after the
//! backfill).

use std::collections::{BTreeMap, HashMap};
use std::time::Duration;

use chrono_tz::Tz;
use rusqlite::{Connection, OptionalExtension, params};
use steve_store::{Result, Store, now};

use crate::items;

/// Items recomputed per pass at most.
pub const BATCH: usize = 200;
const EVERY: Duration = Duration::from_secs(2);

/// The workspace's `stats.tz` (a JSON string in `settings`); `None` = the Hub's own zone.
pub fn tz_of(c: &Connection, ws: &str) -> Result<Option<Tz>> {
    let v: Option<String> = c
        .prepare_cached("SELECT value FROM settings WHERE workspace_id = ? AND key = 'stats.tz'")?
        .query_row([ws], |r| r.get(0))
        .optional()?;
    let name = v.and_then(|v| serde_json::from_str::<String>(&v).ok());
    Ok(crate::day::tz(name.as_deref()))
}

/// The `item_stats` refresher. Besides the rows flagged dirty by the writers, it picks up items whose
/// decisions were created or resolved since its last pass (decisions are written by the core, which
/// does not know about statistics).
#[derive(Debug, Default)]
pub struct Refresher {
    since: i64,
}

impl Refresher {
    pub fn new() -> Self {
        Self::default()
    }

    /// One pass: flags items with new decision activity, then recomputes up to `limit` dirty rows in
    /// one write transaction. Returns how many rows were recomputed.
    pub fn tick(&mut self, store: &Store, limit: usize) -> Result<usize> {
        let t = now();
        let since = self.since;
        // Do not join the single-writer queue just to discover there is no work. Keep the
        // overlap on this path too: a decision committed after the read snapshot must be
        // visible to the next pass. A positive hint is rechecked below in the transaction.
        if !store.stats().refresh_pending(since)? {
            self.since = t - 1_000;
            return Ok(0);
        }
        let n = store.transaction(|c| {
            if since > 0 {
                let ids: Vec<String> = c
                    .prepare_cached(
                        "SELECT DISTINCT item_id FROM decisions WHERE item_id IS NOT NULL AND (created >= ?1 OR resolved >= ?1)",
                    )?
                    .query_map([since], |r| r.get(0))?
                    .collect::<rusqlite::Result<_>>()?;
                for id in ids {
                    flag(c, &id)?;
                }
            }
            let ids: Vec<(String, String)> = c
                .prepare_cached("SELECT item_id, workspace_id FROM item_stats WHERE dirty = 1 LIMIT ?")?
                .query_map([limit as i64], |r| Ok((r.get(0)?, r.get(1)?)))?
                .collect::<rusqlite::Result<_>>()?;
            let mut wss: Vec<&str> = ids.iter().map(|x| x.1.as_str()).collect();
            wss.sort_unstable();
            wss.dedup();
            for ws in wss {
                // goes out after the commit; the Hub coalesces these per workspace
                c.publish(steve_store::stats_changed(ws, None));
            }
            for (id, _) in &ids {
                items::refresh_in(c, id, t)?;
            }
            Ok::<_, steve_store::StoreError>(ids.len())
        })?;
        // a little overlap, so a decision written during this pass is seen by the next one
        self.since = t - 1_000;
        Ok(n)
    }

    /// Runs forever, one pass every 2 seconds (blocking work off the async threads).
    pub async fn run(mut self, store: Store) {
        let mut tick = tokio::time::interval(EVERY);
        loop {
            tick.tick().await;
            let s = store.clone();
            let mut me = std::mem::take(&mut self);
            match tokio::task::spawn_blocking(move || {
                let r = me.tick(&s, BATCH);
                (me, r)
            })
            .await
            {
                Ok((me, r)) => {
                    self = me;
                    if let Err(e) = r {
                        tracing::error!("stats refresh {e}");
                    }
                }
                Err(e) => tracing::error!("stats refresh task {e}"),
            }
        }
    }
}

/// Creates or flags an item's `item_stats` row (tasks only).
pub(crate) fn flag(c: &Connection, item_id: &str) -> Result<()> {
    c.prepare_cached(
        "INSERT INTO item_stats (item_id, workspace_id, created, dirty, updated) SELECT id, workspace_id, created, 1, ?1 FROM items WHERE id = ?2 AND kind = 'task'
         ON CONFLICT (item_id) DO UPDATE SET dirty = 1",
    )?
    .execute(params![now(), item_id])?;
    Ok(())
}

/// Recomputes up to `limit` dirty rows; returns how many.
pub fn refresh_dirty_in(c: &Connection, limit: usize, at: i64) -> Result<usize> {
    let ids: Vec<String> = c
        .prepare_cached("SELECT item_id FROM item_stats WHERE dirty = 1 LIMIT ?")?
        .query_map([limit as i64], |r| r.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    for id in &ids {
        items::refresh_in(c, id, at)?;
    }
    Ok(ids.len())
}

/// Recomputes every dirty row (after a rebuild or the backfill).
pub fn refresh_all(store: &Store) -> Result<usize> {
    let mut total = 0;
    loop {
        let n = store.transaction(|c| refresh_dirty_in(c, 1_000, now()))?;
        total += n;
        if n == 0 {
            return Ok(total);
        }
    }
}

/// One `stat_daily` row's measures.
#[derive(Debug, Default, Clone)]
struct Cell {
    input: i64,
    output: i64,
    cache_read: i64,
    cache_write: i64,
    reasoning: i64,
    hit_base: i64,
    cost_reported: f64,
    calls: i64,
    failed_calls: i64,
    turns: i64,
    turn_ms: i64,
    turn_errors: i64,
    retries: i64,
    interrupted: i64,
    tools: i64,
    tool_errors: i64,
    tool_kinds: BTreeMap<String, i64>,
}

/// (day, source, purpose, harness, model, bot_id, project_id)
type CellKey = (String, String, String, String, String, String, String);

/// Rebuilds `stat_daily` of workspace `ws` from `usage_records` and `agent_turns`, re-cutting days in
/// the current `stats.tz` (and correcting `usage_records.day`), then flags every item of the workspace
/// for the refresh. One write transaction. Returns the number of `stat_daily` rows.
///
/// A turn is booked under the model of its first token record, else its session's, else its bot's
/// model, else `""` — the same order the live writer uses.
pub fn rebuild_in(c: &Connection, ws: &str) -> Result<usize> {
    let tz = tz_of(c, ws)?;
    let mut cells: HashMap<CellKey, Cell> = HashMap::new();
    let mut first_model: HashMap<String, String> = HashMap::new();
    let mut fix_day: Vec<(i64, String)> = Vec::new();
    {
        let mut st = c.prepare(
            "SELECT id, ts, day, source, purpose, harness, model, turn_id, bot_id, project_id, input, output, cache_read, cache_write,
                    reasoning, cost_reported, ok FROM usage_records WHERE workspace_id = ? ORDER BY id",
        )?;
        let mut rows = st.query([ws])?;
        while let Some(r) = rows.next()? {
            let id: i64 = r.get(0)?;
            let ts: i64 = r.get(1)?;
            let old: String = r.get(2)?;
            let day = crate::day::day(ts, tz);
            if day != old {
                fix_day.push((id, day.clone()));
            }
            let model: String = r.get(6)?;
            if let Some(turn) = r.get::<_, Option<String>>(7)? {
                first_model.entry(turn).or_insert_with(|| model.clone());
            }
            let key = (
                day,
                r.get(3)?,
                r.get(4)?,
                r.get(5)?,
                model,
                r.get::<_, Option<String>>(8)?.unwrap_or_default(),
                r.get::<_, Option<String>>(9)?.unwrap_or_default(),
            );
            let tok = |i: usize| r.get::<_, Option<i64>>(i);
            let (input, cache_read, cache_write) = (tok(10)?, tok(12)?, tok(13)?);
            let cell = cells.entry(key).or_default();
            cell.input += input.unwrap_or(0);
            cell.output += tok(11)?.unwrap_or(0);
            cell.cache_read += cache_read.unwrap_or(0);
            cell.cache_write += cache_write.unwrap_or(0);
            cell.reasoning += tok(14)?.unwrap_or(0);
            if let (Some(i), Some(cr)) = (input, cache_read) {
                cell.hit_base += i + cr + cache_write.unwrap_or(0);
            }
            cell.cost_reported += r.get::<_, Option<f64>>(15)?.unwrap_or(0.0);
            cell.calls += 1;
            cell.failed_calls += i64::from(!r.get::<_, bool>(16)?);
        }
    }
    {
        let mut st = c.prepare(
            "SELECT t.id, t.harness, t.bot_id, t.project_id, t.attempt, t.started, t.ended, t.stop_reason, t.transient, t.tools,
                    t.tool_errors, t.tool_kinds, COALESCE(NULLIF(s.model, ''), NULLIF(b.model, ''), '')
             FROM agent_turns t LEFT JOIN sessions s ON s.id = t.session_id LEFT JOIN bots b ON b.id = t.bot_id
             WHERE t.workspace_id = ? AND t.ended IS NOT NULL",
        )?;
        let mut rows = st.query([ws])?;
        while let Some(r) = rows.next()? {
            let id: String = r.get(0)?;
            let ended: i64 = r.get(6)?;
            let fallback: String = r.get(12)?;
            let model = first_model.get(&id).cloned().unwrap_or(fallback);
            let key = (
                crate::day::day(ended, tz),
                "agent".to_string(),
                "turn".to_string(),
                r.get(1)?,
                model,
                r.get::<_, Option<String>>(2)?.unwrap_or_default(),
                r.get::<_, Option<String>>(3)?.unwrap_or_default(),
            );
            let reason: Option<String> = r.get(7)?;
            let transient: bool = r.get(8)?;
            let attempt: i64 = r.get(4)?;
            let started: i64 = r.get(5)?;
            let cell = cells.entry(key).or_default();
            cell.turns += i64::from(attempt == 0);
            cell.turn_ms += (ended - started).max(0);
            cell.turn_errors += i64::from(reason.as_deref() == Some("error") && !transient);
            cell.retries += i64::from(transient);
            cell.interrupted += i64::from(reason.as_deref() == Some("interrupted"));
            cell.tools += r.get::<_, i64>(9)?;
            cell.tool_errors += r.get::<_, i64>(10)?;
            let kinds: BTreeMap<String, i64> =
                serde_json::from_str(&r.get::<_, String>(11)?).unwrap_or_default();
            for (k, n) in kinds {
                *cell.tool_kinds.entry(k).or_default() += n;
            }
        }
    }
    for (id, day) in fix_day {
        c.execute(
            "UPDATE usage_records SET day = ? WHERE id = ?",
            params![day, id],
        )?;
    }
    c.execute("DELETE FROM stat_daily WHERE workspace_id = ?", [ws])?;
    let mut ins = c.prepare(
        "INSERT INTO stat_daily (workspace_id, day, source, purpose, harness, model, bot_id, project_id, input, output, cache_read,
           cache_write, reasoning, hit_base, cost_reported, calls, failed_calls, turns, turn_ms, turn_errors, retries, interrupted,
           tools, tool_errors, tool_kinds)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
    )?;
    let n = cells.len();
    for ((day, source, purpose, harness, model, bot, project), x) in cells {
        ins.execute(params![
            ws,
            day,
            source,
            purpose,
            harness,
            model,
            bot,
            project,
            x.input,
            x.output,
            x.cache_read,
            x.cache_write,
            x.reasoning,
            x.hit_base,
            x.cost_reported,
            x.calls,
            x.failed_calls,
            x.turns,
            x.turn_ms,
            x.turn_errors,
            x.retries,
            x.interrupted,
            x.tools,
            x.tool_errors,
            serde_json::to_string(&x.tool_kinds)?
        ])?;
    }
    flag_workspace(c, ws)?;
    Ok(n)
}

/// Flags (creating where missing) the `item_stats` row of every task item of `ws`.
pub(crate) fn flag_workspace(c: &Connection, ws: &str) -> Result<()> {
    c.execute(
        "INSERT INTO item_stats (item_id, workspace_id, created, dirty, updated) SELECT id, workspace_id, created, 1, ?1 FROM items
         WHERE workspace_id = ?2 AND kind = 'task'
         ON CONFLICT (item_id) DO UPDATE SET dirty = 1",
        params![now(), ws],
    )?;
    Ok(())
}

/// `POST /api/stats/rebuild`: [`rebuild_in`] for `ws`, then every item row recomputed.
pub fn rebuild(store: &Store, ws: &str) -> Result<usize> {
    let n = store.transaction(|c| rebuild_in(c, ws))?;
    refresh_all(store)?;
    store.publish(steve_store::stats_changed(ws, None));
    Ok(n)
}
