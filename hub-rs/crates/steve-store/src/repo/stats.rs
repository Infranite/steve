//! Statistics writes (migration 15): agent turns, token records and item marks.
//!
//! Every write also updates the `stat_daily` cube and flags the item's `item_stats` row dirty in the same
//! transaction, so the summary never disagrees with the detail rows. The store knows nothing about time
//! zones: callers pass the local `day` (`steve_stats::day`), computed with the workspace's `stats.tz`.
//! Costs are never stored, only tokens and the adapters' own dollar figures (`cost_reported`).

use std::collections::BTreeMap;

use rusqlite::{Connection, OptionalExtension, params};

use super::{Cached, to_json};
use crate::{Result, Store, now, uid};

impl Store {
    pub fn stats(&self) -> Stats<'_> {
        Stats(self)
    }
}

pub struct Stats<'a>(&'a Store);

/// Token counts in the Anthropic sense (`input` excludes cache, `output` includes thinking).
/// `None` = not reported, which is not the same as 0 (it keeps the row out of the hit-rate base).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Tokens {
    pub input: Option<i64>,
    pub output: Option<i64>,
    pub cache_read: Option<i64>,
    pub cache_write: Option<i64>,
    /// Part of `output` reported separately (codex `thoughtTokens`); not added to the total again.
    pub reasoning: Option<i64>,
}

impl Tokens {
    /// The hit-rate denominator: only rows that reported the cache split count.
    fn hit_base(&self) -> i64 {
        match (self.input, self.cache_read) {
            (Some(i), Some(r)) => i + r + self.cache_write.unwrap_or(0),
            _ => 0,
        }
    }
}

/// One model's share of a turn.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ModelUsage {
    /// The model that answered; `""` when unknown.
    pub model: String,
    pub tokens: Tokens,
    /// The adapter's dollar figure for this turn (an increment, not the process total).
    pub cost_reported: Option<f64>,
    /// Kept for troubleshooting, e.g. `{"scope":"all"}`.
    pub detail: serde_json::Value,
}

/// A `session/prompt` about to be sent.
#[derive(Debug, Clone, Default)]
pub struct TurnBegin {
    pub workspace_id: String,
    pub session_id: String,
    pub item_id: Option<String>,
    pub bot_id: Option<String>,
    pub project_id: Option<String>,
    pub harness: String,
    /// `events.seq` of the turn's first user event; `None` for continuations and retries.
    pub user_seq: Option<i64>,
    /// 0 = first try, k = k-th automatic retry.
    pub attempt: i64,
    pub started: i64,
}

/// The answer (or error) to a turn.
#[derive(Debug, Clone, Default)]
pub struct TurnFinish {
    pub turn_id: String,
    pub ended: i64,
    /// Local day of `ended`.
    pub day: String,
    /// The model the turn's counters are booked under in `stat_daily` (normally the first usage row's).
    pub model: String,
    /// end_turn | cancelled | max_tokens | refusal | error | interrupted
    pub stop_reason: Option<String>,
    pub error: Option<String>,
    /// The gateway was briefly unavailable and the runner retries.
    pub transient: bool,
    pub tools: i64,
    pub tool_errors: i64,
    pub tool_kinds: BTreeMap<String, i64>,
    pub permissions: i64,
    pub usage: Vec<ModelUsage>,
}

/// A token record outside a turn: a gateway call, an autonomous adapter turn, a backfilled session.
#[derive(Debug, Clone)]
pub struct NewUsage {
    pub workspace_id: String,
    pub ts: i64,
    pub day: String,
    /// agent | gateway | backfill
    pub source: String,
    pub purpose: String,
    pub harness: String,
    pub turn_id: Option<String>,
    pub session_id: Option<String>,
    pub item_id: Option<String>,
    pub bot_id: Option<String>,
    pub project_id: Option<String>,
    pub usage: ModelUsage,
    pub duration_ms: Option<i64>,
    pub ok: bool,
    /// Models tried (gateway fallback).
    pub attempts: i64,
}

/// A structured point in an item's life (`item_marks`).
#[derive(Debug, Clone, Default)]
pub struct NewMark {
    pub workspace_id: String,
    pub item_id: String,
    pub ts: i64,
    /// status | deliver | fail | release
    pub kind: String,
    pub from_v: Option<String>,
    pub to_v: Option<String>,
    /// me | steve | yolo | <bot id>; `""` when the caller cannot tell
    pub actor: String,
    pub r#ref: Option<String>,
    pub detail: serde_json::Value,
    pub backfill: bool,
}

impl Stats<'_> {
    /// Inserts the open `agent_turns` row and returns its id (`tn_…`).
    pub fn turn_begin(&self, b: &TurnBegin) -> Result<String> {
        self.0.write(|w| {
            let id = uid("tn");
            w.c.run(
                "INSERT INTO agent_turns (id, workspace_id, session_id, item_id, bot_id, project_id, harness, user_seq, attempt, started) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
                params![id, b.workspace_id, b.session_id, b.item_id, b.bot_id, b.project_id, b.harness, b.user_seq, b.attempt, b.started],
            )?;
            Ok(id)
        })
    }

    /// Completes the turn, writes one `usage_records` row per model and books both into `stat_daily`.
    /// `false` (and nothing written) if the turn does not exist or was already finished, so a late
    /// second call cannot count a turn twice.
    pub fn turn_finish(&self, f: &TurnFinish) -> Result<bool> {
        self.0.write(|w| {
            let tx = w.tx()?;
            let ws = finish_in(&tx, f)?;
            tx.commit()?;
            if let Some(ws) = &ws {
                w.publish(stats_changed(ws, Some(&f.day)));
            }
            Ok(ws.is_some())
        })
    }

    /// Ends every open turn as `interrupted` (Hub restart) at its session's last event, never later than
    /// `ended` (the boot); `day` maps a workspace id and a time to the local day. Returns how many turns were closed.
    pub fn interrupt_open(&self, ended: i64, day: impl Fn(&str, i64) -> String) -> Result<usize> {
        self.0.write(|w| {
            let tx = w.tx()?;
            // the turn stopped when its session last wrote something, not when the Hub came back up
            let open: Vec<(String, String, i64)> = tx
                .prepare_cached(
                    "SELECT t.id, t.workspace_id, MAX(t.started, COALESCE((SELECT MAX(e.ts) FROM events e WHERE e.session_id = t.session_id AND e.ts <= ?1), t.started))
                     FROM agent_turns t WHERE t.ended IS NULL",
                )?
                .query_map([ended], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
                .collect::<rusqlite::Result<_>>()?;
            let mut wss: Vec<&str> = open.iter().map(|o| o.1.as_str()).collect();
            wss.sort_unstable();
            wss.dedup();
            for (id, ws, at) in &open {
                let at = (*at).min(ended);
                finish_in(
                    &tx,
                    &TurnFinish {
                        turn_id: id.clone(),
                        ended: at,
                        day: day(ws, at),
                        stop_reason: Some("interrupted".into()),
                        ..TurnFinish::default()
                    },
                )?;
            }
            tx.commit()?;
            for ws in wss {
                w.publish(stats_changed(ws, None));
            }
            Ok(open.len())
        })
    }

    /// Writes one token record and books it into `stat_daily`; returns the row id.
    pub fn usage_add(&self, u: &NewUsage) -> Result<i64> {
        self.0.write(|w| {
            let tx = w.tx()?;
            let id = usage_in(&tx, u)?;
            tx.commit()?;
            w.publish(stats_changed(&u.workspace_id, Some(&u.day)));
            Ok(id)
        })
    }

    /// Writes one item mark; returns the row id.
    pub fn mark_add(&self, m: &NewMark) -> Result<i64> {
        self.0.write(|w| {
            let tx = w.tx()?;
            let id = mark_in(&tx, m)?;
            tx.commit()?;
            w.publish(stats_changed(&m.workspace_id, None));
            Ok(id)
        })
    }

    /// A read-only hint for the refresher. Writers remain authoritative: new work arriving after
    /// this snapshot is handled on the next pass, and a positive hint must be rechecked in the
    /// refresh transaction. `since == 0` preserves the initial dirty-row-only pass.
    pub fn refresh_pending(&self, since: i64) -> Result<bool> {
        self.0.read(|c| {
            Ok(c.query_row(
                // one EXISTS per column, so each is a range on its own index (decisions_created_item,
                // decisions_resolved_item) and an idle poll reads nothing
                "SELECT EXISTS(SELECT 1 FROM item_stats WHERE dirty = 1) OR (?1 > 0 AND (
                   EXISTS(SELECT 1 FROM decisions WHERE item_id IS NOT NULL AND created >= ?1)
                   OR EXISTS(SELECT 1 FROM decisions WHERE item_id IS NOT NULL AND resolved >= ?1)))",
                [since],
                |r| r.get(0),
            )?)
        })
    }

    /// Flags the item for the `item_stats` refresh (e.g. after a decision about it changed).
    pub fn touch_item(&self, item_id: &str) -> Result<()> {
        self.0.write(|w| dirty_in(w.c, item_id))
    }
}

/// The `fail` mark's reason code for a fail card's text: `restart` (Hub restart),
/// `stall` (the agent stopped without delivering or asking), `error` (the session failed), `release`
/// (anything about a release), else `other`. Shared by the live hooks and the backfill.
pub fn fail_reason(why: &str) -> &'static str {
    let w = why.trim_start();
    if w.starts_with("Hub 重启") {
        "restart"
    } else if w.starts_with("它停下了") || w.starts_with("Agent 停下来了") {
        "stall"
    } else if w.starts_with("会话出错") || w.starts_with("开不了会话") {
        "error"
    } else if w.contains("发布") {
        "release"
    } else {
        "other"
    }
}

/// [`Stats::mark_add`] inside a transaction the caller holds (item status changes, slice ST3).
pub(crate) fn mark_in(c: &Connection, m: &NewMark) -> Result<i64> {
    c.run(
        "INSERT INTO item_marks (workspace_id, item_id, ts, kind, from_v, to_v, actor, ref, detail, backfill) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        params![m.workspace_id, m.item_id, m.ts, m.kind, m.from_v, m.to_v, m.actor, m.r#ref, detail_json(&m.detail)?, m.backfill],
    )?;
    let id = c.last_insert_rowid();
    dirty_in(c, &m.item_id)?;
    Ok(id)
}

/// The `stats` change event of a workspace.
pub fn stats_changed(ws: &str, day: Option<&str>) -> crate::ChangeEvent {
    crate::ChangeEvent::Stats {
        ws: ws.to_string(),
        day: day.map(str::to_string),
    }
}

/// Finishes an open turn; returns its workspace, or `None` if it was not open.
fn finish_in(c: &Connection, f: &TurnFinish) -> Result<Option<String>> {
    struct Turn {
        ws: String,
        session: String,
        item: Option<String>,
        bot: Option<String>,
        project: Option<String>,
        harness: String,
        attempt: i64,
        started: i64,
    }
    let t = c
        .prepare_cached(
            "SELECT workspace_id, session_id, item_id, bot_id, project_id, harness, attempt, started FROM agent_turns WHERE id = ? AND ended IS NULL",
        )?
        .query_row([&f.turn_id], |r| {
            Ok(Turn {
                ws: r.get(0)?,
                session: r.get(1)?,
                item: r.get(2)?,
                bot: r.get(3)?,
                project: r.get(4)?,
                harness: r.get(5)?,
                attempt: r.get(6)?,
                started: r.get(7)?,
            })
        })
        .optional()?;
    let Some(t) = t else { return Ok(None) };
    let kinds = to_json(&f.tool_kinds)?;
    c.run(
        "UPDATE agent_turns SET ended = ?, stop_reason = ?, error = ?, transient = ?, tools = ?, tool_errors = ?, tool_kinds = ?, permissions = ? WHERE id = ?",
        params![f.ended, f.stop_reason, f.error, f.transient, f.tools, f.tool_errors, kinds, f.permissions, f.turn_id],
    )?;
    let reason = f.stop_reason.as_deref();
    let key = Key {
        ws: &t.ws,
        day: &f.day,
        source: "agent",
        purpose: "turn",
        harness: &t.harness,
        model: &f.model,
        bot: t.bot.as_deref().unwrap_or(""),
        project: t.project.as_deref().unwrap_or(""),
    };
    upsert_daily(
        c,
        &key,
        &Delta {
            turns: i64::from(t.attempt == 0),
            turn_ms: (f.ended - t.started).max(0),
            turn_errors: i64::from(reason == Some("error") && !f.transient),
            retries: i64::from(f.transient),
            interrupted: i64::from(reason == Some("interrupted")),
            tools: f.tools,
            tool_errors: f.tool_errors,
            tool_kinds: Some(&f.tool_kinds),
            ..Delta::default()
        },
    )?;
    for u in &f.usage {
        usage_in(
            c,
            &NewUsage {
                workspace_id: t.ws.clone(),
                ts: f.ended,
                day: f.day.clone(),
                source: "agent".into(),
                purpose: "turn".into(),
                harness: t.harness.clone(),
                turn_id: Some(f.turn_id.clone()),
                session_id: Some(t.session.clone()),
                item_id: t.item.clone(),
                bot_id: t.bot.clone(),
                project_id: t.project.clone(),
                usage: u.clone(),
                duration_ms: None,
                ok: true,
                attempts: 1,
            },
        )?;
    }
    if let Some(item) = &t.item {
        dirty_in(c, item)?;
    }
    Ok(Some(t.ws))
}

fn usage_in(c: &Connection, u: &NewUsage) -> Result<i64> {
    let k = &u.usage.tokens;
    c.run(
        "INSERT INTO usage_records (workspace_id, ts, day, source, purpose, harness, model, turn_id, session_id, item_id, bot_id, project_id, input, output, cache_read, cache_write, reasoning, cost_reported, duration_ms, ok, attempts, detail) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        params![
            u.workspace_id, u.ts, u.day, u.source, u.purpose, u.harness, u.usage.model, u.turn_id, u.session_id,
            u.item_id, u.bot_id, u.project_id, k.input, k.output, k.cache_read, k.cache_write, k.reasoning,
            u.usage.cost_reported, u.duration_ms, u.ok, u.attempts, detail_json(&u.usage.detail)?
        ],
    )?;
    let id = c.last_insert_rowid();
    let key = Key {
        ws: &u.workspace_id,
        day: &u.day,
        source: &u.source,
        purpose: &u.purpose,
        harness: &u.harness,
        model: &u.usage.model,
        bot: u.bot_id.as_deref().unwrap_or(""),
        project: u.project_id.as_deref().unwrap_or(""),
    };
    upsert_daily(
        c,
        &key,
        &Delta {
            input: k.input.unwrap_or(0),
            output: k.output.unwrap_or(0),
            cache_read: k.cache_read.unwrap_or(0),
            cache_write: k.cache_write.unwrap_or(0),
            reasoning: k.reasoning.unwrap_or(0),
            hit_base: k.hit_base(),
            cost_reported: u.usage.cost_reported.unwrap_or(0.0),
            calls: 1,
            failed_calls: i64::from(!u.ok),
            ..Delta::default()
        },
    )?;
    if let Some(item) = &u.item_id {
        dirty_in(c, item)?;
    }
    Ok(id)
}

/// The `stat_daily` primary key.
struct Key<'a> {
    ws: &'a str,
    day: &'a str,
    source: &'a str,
    purpose: &'a str,
    harness: &'a str,
    model: &'a str,
    bot: &'a str,
    project: &'a str,
}

/// What one write adds to a `stat_daily` row.
#[derive(Default)]
struct Delta<'a> {
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
    tool_kinds: Option<&'a BTreeMap<String, i64>>,
}

fn upsert_daily(c: &Connection, k: &Key, d: &Delta) -> Result<()> {
    c.run(
        "INSERT INTO stat_daily (workspace_id, day, source, purpose, harness, model, bot_id, project_id, input, output, cache_read, cache_write, reasoning, hit_base, cost_reported, calls, failed_calls, turns, turn_ms, turn_errors, retries, interrupted, tools, tool_errors)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
         ON CONFLICT (workspace_id, day, source, purpose, harness, model, bot_id, project_id) DO UPDATE SET
           input = input + excluded.input, output = output + excluded.output,
           cache_read = cache_read + excluded.cache_read, cache_write = cache_write + excluded.cache_write,
           reasoning = reasoning + excluded.reasoning, hit_base = hit_base + excluded.hit_base,
           cost_reported = cost_reported + excluded.cost_reported, calls = calls + excluded.calls,
           failed_calls = failed_calls + excluded.failed_calls, turns = turns + excluded.turns,
           turn_ms = turn_ms + excluded.turn_ms, turn_errors = turn_errors + excluded.turn_errors,
           retries = retries + excluded.retries, interrupted = interrupted + excluded.interrupted,
           tools = tools + excluded.tools, tool_errors = tool_errors + excluded.tool_errors",
        params![
            k.ws, k.day, k.source, k.purpose, k.harness, k.model, k.bot, k.project, d.input, d.output,
            d.cache_read, d.cache_write, d.reasoning, d.hit_base, d.cost_reported, d.calls, d.failed_calls,
            d.turns, d.turn_ms, d.turn_errors, d.retries, d.interrupted, d.tools, d.tool_errors
        ],
    )?;
    let Some(kinds) = d.tool_kinds.filter(|m| !m.is_empty()) else {
        return Ok(());
    };
    // the per-kind counts are a JSON object, merged here rather than in SQL
    let pk = params![
        k.ws, k.day, k.source, k.purpose, k.harness, k.model, k.bot, k.project
    ];
    let cur: String = c.row(
        "SELECT tool_kinds FROM stat_daily WHERE workspace_id = ? AND day = ? AND source = ? AND purpose = ? AND harness = ? AND model = ? AND bot_id = ? AND project_id = ?",
        pk,
        |r| r.get(0),
    )?;
    let mut merged: BTreeMap<String, i64> = serde_json::from_str(&cur).unwrap_or_default();
    for (kind, n) in kinds {
        *merged.entry(kind.clone()).or_default() += n;
    }
    c.run(
        "UPDATE stat_daily SET tool_kinds = ? WHERE workspace_id = ? AND day = ? AND source = ? AND purpose = ? AND harness = ? AND model = ? AND bot_id = ? AND project_id = ?",
        params![
            to_json(&merged)?,
            k.ws,
            k.day,
            k.source,
            k.purpose,
            k.harness,
            k.model,
            k.bot,
            k.project
        ],
    )?;
    Ok(())
}

/// Creates or flags the item's `item_stats` row; an unknown item is ignored.
fn dirty_in(c: &Connection, item_id: &str) -> Result<()> {
    c.run(
        "INSERT INTO item_stats (item_id, workspace_id, created, dirty, updated) SELECT id, workspace_id, created, 1, ? FROM items WHERE id = ?
         ON CONFLICT (item_id) DO UPDATE SET dirty = 1",
        params![now(), item_id],
    )?;
    Ok(())
}

/// `{}` for a missing detail, so the column never holds `null`.
fn detail_json(v: &serde_json::Value) -> Result<String> {
    Ok(if v.is_null() {
        "{}".into()
    } else {
        to_json(v)?
    })
}
