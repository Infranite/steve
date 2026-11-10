//! The metrics over a range of local days. Usage, spend and agent counters read the
//! daily cube (`stat_daily`); flow, time and quality read `item_stats`, `item_marks` and `decisions`
//! through their indexes. Costs are estimated here, from tokens and the prices of each day
//! ([`PriceBook`]: the table, then Models.dev as it was that day).
//!
//! Filters: `project` and `bot` apply to everything (through the item, turn or record); `model` and
//! `source` only to the token and turn figures, which are the only ones that have them.

use std::collections::BTreeMap;

use chrono::{Days, Local, NaiveDate, TimeZone};
use chrono_tz::Tz;
use rusqlite::{Connection, params};
use serde::Serialize;
use serde_json::Value;
use steve_store::{Result, Tokens};

use crate::catalog::{self, SyncState};
use crate::items::ATTEMPTED;
use crate::prices::{Price, PriceBook, PriceSource, default_prices};
use crate::refresh::tz_of;

/// WIP statuses (T4).
pub const WIP: &[&str] = &["wait", "run", "block", "pause", "ship"];
/// Fail reasons (Q3), in display order.
pub const FAIL_REASONS: &[&str] = &["stall", "error", "restart", "release", "other"];

/// What a metrics call is about.
#[derive(Debug, Clone, Default)]
pub struct Query {
    pub workspace_id: String,
    /// First local day, `YYYY-MM-DD`, inclusive.
    pub from: String,
    /// Last local day, inclusive.
    pub to: String,
    pub project: Option<String>,
    pub bot: Option<String>,
    pub model: Option<String>,
    pub source: Option<String>,
}

/// The price table in force: `stats.prices` of the workspace, else the defaults.
pub fn prices_of(c: &Connection, ws: &str) -> Result<Vec<Price>> {
    let v: Option<String> = c
        .query_row(
            "SELECT value FROM settings WHERE workspace_id = ? AND key = 'stats.prices'",
            [ws],
            |r| r.get(0),
        )
        .ok();
    Ok(v.and_then(|v| serde_json::from_str::<Vec<Price>>(&v).ok())
        .unwrap_or_else(default_prices))
}

/// The prices in force for a read of `ws`: its table and the Models.dev catalog, days cut in its zone.
pub fn book_of<'c>(c: &'c Connection, ws: &str) -> Result<PriceBook<'c>> {
    Ok(PriceBook::new(prices_of(c, ws)?).with_catalog(c, tz_of(c, ws)?))
}

/// Unix ms of local midnight starting `day` in `tz` (the Hub's zone for `None`); `None` for a bad date.
pub fn day_start(day: &str, tz: Option<Tz>) -> Option<i64> {
    let d = NaiveDate::parse_from_str(day, "%Y-%m-%d").ok()?;
    let midnight = d.and_hms_opt(0, 0, 0)?;
    let ts = match tz {
        Some(z) => z
            .from_local_datetime(&midnight)
            .earliest()?
            .timestamp_millis(),
        None => Local
            .from_local_datetime(&midnight)
            .earliest()?
            .timestamp_millis(),
    };
    Some(ts)
}

/// `[from 00:00, (to + 1) 00:00)` in Unix ms.
pub fn bounds(from: &str, to: &str, tz: Option<Tz>) -> Option<(i64, i64)> {
    let next = NaiveDate::parse_from_str(to, "%Y-%m-%d")
        .ok()?
        .checked_add_days(Days::new(1))?
        .format("%Y-%m-%d")
        .to_string();
    Some((day_start(from, tz)?, day_start(&next, tz)?))
}

/// Nearest-rank percentile of unsorted values (`q` in 0..=1).
pub fn percentile(v: &[i64], q: f64) -> Option<i64> {
    if v.is_empty() {
        return None;
    }
    let mut s = v.to_vec();
    s.sort_unstable();
    let rank = ((q * s.len() as f64).ceil() as usize).clamp(1, s.len());
    Some(s[rank - 1])
}

fn avg(v: &[i64]) -> Option<f64> {
    (!v.is_empty()).then(|| v.iter().sum::<i64>() as f64 / v.len() as f64)
}

fn ratio(a: i64, b: i64) -> Option<f64> {
    (b > 0).then(|| a as f64 / b as f64)
}

/* ── the daily cube ── */

/// A `stat_daily` row.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Daily {
    pub day: String,
    pub source: String,
    pub purpose: String,
    pub harness: String,
    pub model: String,
    pub bot_id: String,
    pub project_id: String,
    pub input: i64,
    pub output: i64,
    pub cache_read: i64,
    pub cache_write: i64,
    pub reasoning: i64,
    pub hit_base: i64,
    pub cost_reported: f64,
    pub calls: i64,
    pub failed_calls: i64,
    pub turns: i64,
    pub turn_ms: i64,
    pub turn_errors: i64,
    pub retries: i64,
    pub interrupted: i64,
    pub tools: i64,
    pub tool_errors: i64,
    pub tool_kinds: BTreeMap<String, i64>,
}

impl Daily {
    pub fn tokens(&self) -> Tokens {
        Tokens {
            input: Some(self.input),
            output: Some(self.output),
            cache_read: Some(self.cache_read),
            cache_write: Some(self.cache_write),
            reasoning: Some(self.reasoning),
        }
    }
    /// U6 of the row.
    pub fn total(&self) -> i64 {
        self.input + self.output + self.cache_read + self.cache_write
    }
}

/// The cube rows of the query's days and filters.
pub fn daily(c: &Connection, q: &Query) -> Result<Vec<Daily>> {
    Ok(c
        .prepare_cached(
            "SELECT day, source, purpose, harness, model, bot_id, project_id, input, output, cache_read, cache_write, reasoning,
                    hit_base, cost_reported, calls, failed_calls, turns, turn_ms, turn_errors, retries, interrupted, tools,
                    tool_errors, tool_kinds
             FROM stat_daily WHERE workspace_id = ?1 AND day >= ?2 AND day <= ?3
               AND (?4 IS NULL OR project_id = ?4) AND (?5 IS NULL OR bot_id = ?5)
               AND (?6 IS NULL OR model = ?6) AND (?7 IS NULL OR source = ?7)",
        )?
        .query_map(
            params![q.workspace_id, q.from, q.to, q.project, q.bot, q.model, q.source],
            |r| {
                Ok(Daily {
                    day: r.get(0)?,
                    source: r.get(1)?,
                    purpose: r.get(2)?,
                    harness: r.get(3)?,
                    model: r.get(4)?,
                    bot_id: r.get(5)?,
                    project_id: r.get(6)?,
                    input: r.get(7)?,
                    output: r.get(8)?,
                    cache_read: r.get(9)?,
                    cache_write: r.get(10)?,
                    reasoning: r.get(11)?,
                    hit_base: r.get(12)?,
                    cost_reported: r.get(13)?,
                    calls: r.get(14)?,
                    failed_calls: r.get(15)?,
                    turns: r.get(16)?,
                    turn_ms: r.get(17)?,
                    turn_errors: r.get(18)?,
                    retries: r.get(19)?,
                    interrupted: r.get(20)?,
                    tools: r.get(21)?,
                    tool_errors: r.get(22)?,
                    tool_kinds: serde_json::from_str(&r.get::<_, String>(23)?).unwrap_or_default(),
                })
            },
        )?
        .collect::<rusqlite::Result<_>>()?)
}

/// The cube rows of a one-day query (`from == to`) cut by local hour (ST-43): the same measures, read from
/// the detail rows the cube is built from (`usage_records` by their `day`, `agent_turns` by the day they
/// ended, as `refresh::rebuild_in` does), with `day` set to the [`crate::day::hour`] bucket. So the hours of
/// a day add up to that day's cube rows. Rows of hours outside the day (a stale `day`) are left out.
pub fn hourly(c: &Connection, q: &Query) -> Result<Vec<Daily>> {
    let tz = tz_of(c, &q.workspace_id)?;
    let Some((lo, hi)) = bounds(&q.from, &q.to, tz) else {
        return Ok(vec![]);
    };
    let hour = |ts: i64| {
        let h = crate::day::hour(ts, tz);
        h.starts_with(q.from.as_str()).then_some(h)
    };
    let keep = |source: &str, model: &str| {
        q.source.as_deref().is_none_or(|s| s == source)
            && q.model.as_deref().is_none_or(|m| m == model)
    };
    type Key = (String, String, String, String, String, String, String);
    fn cell(cells: &mut BTreeMap<Key, Daily>, k: Key) -> &mut Daily {
        cells.entry(k.clone()).or_insert_with(|| Daily {
            day: k.0,
            source: k.1,
            purpose: k.2,
            harness: k.3,
            model: k.4,
            bot_id: k.5,
            project_id: k.6,
            ..Daily::default()
        })
    }
    let mut cells: BTreeMap<Key, Daily> = BTreeMap::new();
    // a turn ended before `agent_turns.model` was kept is booked under the model of its first token
    // record (ts = the turn's end, so in this day)
    let mut first_model: BTreeMap<String, String> = BTreeMap::new();
    {
        let mut st = c.prepare_cached(
            "SELECT ts, source, purpose, harness, model, turn_id, COALESCE(bot_id, ''), COALESCE(project_id, ''), input, output,
                    cache_read, cache_write, reasoning, cost_reported, ok
             FROM usage_records WHERE workspace_id = ?1 AND day >= ?2 AND day <= ?3
               AND (?4 IS NULL OR project_id = ?4) AND (?5 IS NULL OR bot_id = ?5) ORDER BY id",
        )?;
        let mut rows = st.query(params![q.workspace_id, q.from, q.to, q.project, q.bot])?;
        while let Some(r) = rows.next()? {
            let model: String = r.get(4)?;
            if let Some(turn) = r.get::<_, Option<String>>(5)? {
                first_model.entry(turn).or_insert_with(|| model.clone());
            }
            let source: String = r.get(1)?;
            let Some(h) = hour(r.get(0)?).filter(|_| keep(&source, &model)) else {
                continue;
            };
            let k = (h, source, r.get(2)?, r.get(3)?, model, r.get(6)?, r.get(7)?);
            let d = cell(&mut cells, k);
            let tok = |i: usize| r.get::<_, Option<i64>>(i);
            let (input, cache_read, cache_write) = (tok(8)?, tok(10)?, tok(11)?);
            d.input += input.unwrap_or(0);
            d.output += tok(9)?.unwrap_or(0);
            d.cache_read += cache_read.unwrap_or(0);
            d.cache_write += cache_write.unwrap_or(0);
            d.reasoning += tok(12)?.unwrap_or(0);
            if let (Some(i), Some(cr)) = (input, cache_read) {
                d.hit_base += i + cr + cache_write.unwrap_or(0);
            }
            d.cost_reported += r.get::<_, Option<f64>>(13)?.unwrap_or(0.0);
            d.calls += 1;
            d.failed_calls += i64::from(!r.get::<_, bool>(14)?);
        }
    }
    let mut st = c.prepare_cached(
        "SELECT t.id, t.harness, COALESCE(t.bot_id, ''), COALESCE(t.project_id, ''), t.attempt, t.started, t.ended, t.stop_reason,
                t.transient, t.tools, t.tool_errors, t.tool_kinds, COALESCE(NULLIF(s.model, ''), NULLIF(b.model, ''), ''), t.model
         FROM agent_turns t LEFT JOIN sessions s ON s.id = t.session_id LEFT JOIN bots b ON b.id = t.bot_id
         WHERE t.workspace_id = ?1 AND t.started < ?3 AND t.ended >= ?2 AND t.ended < ?3
           AND (?4 IS NULL OR t.project_id = ?4) AND (?5 IS NULL OR t.bot_id = ?5)",
    )?;
    let mut rows = st.query(params![q.workspace_id, lo, hi, q.project, q.bot])?;
    while let Some(r) = rows.next()? {
        let id: String = r.get(0)?;
        let ended: i64 = r.get(6)?;
        // the model the live writer booked the turn under; older turns as `refresh::rebuild_in` does
        let model = match r.get::<_, Option<String>>(13)? {
            Some(m) => m,
            None => first_model.get(&id).cloned().unwrap_or(r.get(12)?),
        };
        let Some(h) = hour(ended).filter(|_| keep("agent", &model)) else {
            continue;
        };
        let k = (
            h,
            "agent".into(),
            "turn".into(),
            r.get(1)?,
            model,
            r.get(2)?,
            r.get(3)?,
        );
        let d = cell(&mut cells, k);
        let reason: Option<String> = r.get(7)?;
        let transient: bool = r.get(8)?;
        d.turns += i64::from(r.get::<_, i64>(4)? == 0);
        d.turn_ms += (ended - r.get::<_, i64>(5)?).max(0);
        d.turn_errors += i64::from(reason.as_deref() == Some("error") && !transient);
        d.retries += i64::from(transient);
        d.interrupted += i64::from(reason.as_deref() == Some("interrupted"));
        d.tools += r.get::<_, i64>(9)?;
        d.tool_errors += r.get::<_, i64>(10)?;
        let kinds: BTreeMap<String, i64> =
            serde_json::from_str(&r.get::<_, String>(11)?).unwrap_or_default();
        for (k, n) in kinds {
            *d.tool_kinds.entry(k).or_default() += n;
        }
    }
    Ok(cells.into_values().collect())
}

/// Token and spend totals of a set of cube rows.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct UsageTotals {
    pub input: i64,
    pub output: i64,
    pub cache_read: i64,
    pub cache_write: i64,
    pub reasoning: i64,
    pub total: i64,
    /// U5: Σ cache_read ÷ Σ hit_base; `None` when nothing reported the cache split.
    pub hit_rate: Option<f64>,
    /// S1, dollars, priced tokens only.
    pub est: f64,
    /// S2: the adapters' own figures.
    pub reported: f64,
    /// S4: tokens of models without a price.
    pub unpriced_tokens: i64,
    /// S6: Σ cache_read × (input price − cache read price).
    pub saved_by_cache: f64,
    pub calls: i64,
    pub failed_calls: i64,
    pub turns: i64,
    pub turn_ms: i64,
    pub turn_errors: i64,
    pub retries: i64,
    pub interrupted: i64,
    pub tools: i64,
    pub tool_errors: i64,
    #[serde(skip)]
    hit_base: i64,
}

impl UsageTotals {
    fn add(&mut self, d: &Daily, prices: &PriceBook) {
        self.input += d.input;
        self.output += d.output;
        self.cache_read += d.cache_read;
        self.cache_write += d.cache_write;
        self.reasoning += d.reasoning;
        self.total += d.total();
        self.hit_base += d.hit_base;
        self.hit_rate = (self.hit_base > 0).then(|| self.cache_read as f64 / self.hit_base as f64);
        self.reported += d.cost_reported;
        match prices.quote(&d.model, &d.day) {
            Some(p) => {
                self.est += p.price.cost(&d.tokens());
                self.saved_by_cache +=
                    d.cache_read as f64 * (p.price.input - p.price.cache_read) / 1e6;
            }
            None => self.unpriced_tokens += d.total(),
        }
        self.calls += d.calls;
        self.failed_calls += d.failed_calls;
        self.turns += d.turns;
        self.turn_ms += d.turn_ms;
        self.turn_errors += d.turn_errors;
        self.retries += d.retries;
        self.interrupted += d.interrupted;
        self.tools += d.tools;
        self.tool_errors += d.tool_errors;
    }

    fn of<'a>(rows: impl IntoIterator<Item = &'a Daily>, prices: &PriceBook) -> Self {
        let mut t = UsageTotals::default();
        for d in rows {
            t.add(d, prices);
        }
        t
    }
}

/// Dimensions of [`breakdown`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum By {
    Model,
    Bot,
    Project,
    Source,
    Purpose,
    Harness,
    Day,
}

impl By {
    pub fn parse(s: &str) -> Option<By> {
        Some(match s {
            "model" => By::Model,
            "bot" => By::Bot,
            "project" => By::Project,
            "source" => By::Source,
            "purpose" => By::Purpose,
            "harness" => By::Harness,
            "day" => By::Day,
            _ => return None,
        })
    }
    fn key<'a>(&self, d: &'a Daily) -> &'a str {
        match self {
            By::Model => &d.model,
            By::Bot => &d.bot_id,
            By::Project => &d.project_id,
            By::Source => &d.source,
            By::Purpose => &d.purpose,
            By::Harness => &d.harness,
            By::Day => &d.day,
        }
    }
}

/// One row of a breakdown.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct Row {
    pub key: String,
    #[serde(flatten)]
    pub usage: UsageTotals,
}

/// Usage and spend per value of `by` (S/U/A metrics grouped), most expensive first; the rows add up
/// to [`summary`]'s usage for the same query.
pub fn breakdown(rows: &[Daily], by: By, prices: &PriceBook) -> Vec<Row> {
    let mut groups: BTreeMap<&str, Vec<&Daily>> = BTreeMap::new();
    for d in rows {
        groups.entry(by.key(d)).or_default().push(d);
    }
    let mut out: Vec<Row> = groups
        .into_iter()
        .map(|(k, v)| Row {
            key: k.to_string(),
            usage: UsageTotals::of(v, prices),
        })
        .collect();
    out.sort_by(|a, b| {
        b.usage
            .est
            .total_cmp(&a.usage.est)
            .then(b.usage.total.cmp(&a.usage.total))
    });
    out
}

/// The harness and models each purpose actually ran on (the purpose breakdown's "which model"): per
/// purpose, `(harness, model, calls + turns)` busiest first. Rows with no model (a call that reported
/// nothing) are left out.
pub fn models_by_purpose(rows: &[Daily]) -> BTreeMap<String, Vec<(String, String, i64)>> {
    let mut m: BTreeMap<String, BTreeMap<(String, String), i64>> = BTreeMap::new();
    for d in rows.iter().filter(|d| !d.model.is_empty()) {
        *m.entry(d.purpose.clone())
            .or_default()
            .entry((d.harness.clone(), d.model.clone()))
            .or_default() += d.calls + d.turns;
    }
    m.into_iter()
        .map(|(p, ms)| {
            let mut v: Vec<(String, String, i64)> =
                ms.into_iter().map(|((h, m), n)| (h, m, n)).collect();
            v.sort_by(|a, b| b.2.cmp(&a.2).then(a.1.cmp(&b.1)).then(a.0.cmp(&b.0)));
            (p, v)
        })
        .collect()
}

/// Tool calls per bucket (A3): ACP kinds and `mcp:<tool>`; `tool:<name>` buckets are per tool name.
pub fn tool_kinds(rows: &[Daily]) -> BTreeMap<String, i64> {
    let mut m = BTreeMap::new();
    for d in rows {
        for (k, n) in &d.tool_kinds {
            *m.entry(k.clone()).or_default() += n;
        }
    }
    m
}

/* ── the summary ── */

#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct P {
    pub p50: Option<i64>,
    pub p85: Option<i64>,
    pub avg: Option<f64>,
    pub sum: i64,
    pub n: usize,
}

impl P {
    pub fn of(v: &[i64]) -> P {
        P {
            p50: percentile(v, 0.5),
            p85: percentile(v, 0.85),
            avg: avg(v),
            sum: v.iter().sum(),
            n: v.len(),
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct Spend {
    pub est: f64,
    pub reported: f64,
    pub unpriced_tokens: i64,
    /// S4 per model.
    pub unpriced: BTreeMap<String, i64>,
    pub saved_by_cache: f64,
    /// S5: estimate per source (agent / gateway).
    pub by_source: BTreeMap<String, f64>,
    /// S5: the gateway estimate per purpose.
    pub gateway_by_purpose: BTreeMap<String, f64>,
    /// Priced tokens per price source (`table`, `models_dev`).
    pub priced_by: BTreeMap<String, i64>,
    /// The Models.dev sync behind the catalog prices.
    pub catalog: SyncState,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct Throughput {
    pub created: i64,
    /// T1 by `items.source[1]` (chat, manual, signal…).
    pub created_by_source: BTreeMap<String, i64>,
    pub done: i64,
    pub dropped: i64,
    /// T3 rate: dropped ÷ (done + dropped).
    pub drop_rate: Option<f64>,
    pub wip: i64,
    pub delivered: i64,
    pub delivered_auto: i64,
    pub delivered_manual: i64,
    /// T6: created − done − dropped.
    pub net: i64,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct Times {
    /// D1 over items done in the range ("平均完成时间" = `lead.avg`).
    pub lead: P,
    pub cycle: P,
    /// D3: first delivery − first turn.
    pub first_deliver: P,
    pub wait: P,
    pub agent: P,
    /// D6: lead − wait − agent, floored at 0.
    pub other: P,
    /// D7.
    pub releases: i64,
    pub released: i64,
    pub release_success_rate: Option<f64>,
    pub not_released: i64,
    /// D8: released − accepted.
    pub accept_to_release: P,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct Quality {
    /// Q1: first verdicts that passed ÷ first verdicts, over items first delivered in the range.
    pub first_pass: Option<f64>,
    pub first_pass_n: i64,
    /// Q2: rework verdicts given in the range.
    pub reworks: i64,
    pub reworks_per_done: Option<f64>,
    /// Q3 by reason.
    pub fails: BTreeMap<String, i64>,
    /// Q4: items with a stall in the range that are done now ÷ those items.
    pub stalled_then_done: Option<f64>,
    /// Q6.
    pub rollbacks: i64,
    pub release_unknown: i64,
    /// Q5: error turns ÷ turns (retried transient errors excluded).
    pub turn_error_rate: Option<f64>,
    /// Q7: `done→run` / `drop→run` (any reopening status change out of done or drop).
    pub reopened: i64,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct Yolo {
    pub decided: i64,
    pub declined: i64,
    pub declined_by_reason: BTreeMap<String, i64>,
    pub undone: i64,
    pub rate: Option<f64>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct Human {
    /// H1: decisions created in the range.
    pub decisions: i64,
    /// H1 by `kind/subject_type`.
    pub by_kind: BTreeMap<String, i64>,
    /// H2: resolved by a person, `resolved − created`.
    pub response: P,
    pub response_by_kind: BTreeMap<String, P>,
    /// H3.
    pub expired: i64,
    /// H4; `None` before YOLO exists (no `yolo_actions` table).
    pub yolo: Option<Yolo>,
    /// H5: messages the user added to items.
    pub messages: i64,
    /// H6: (person-resolved decisions + messages) ÷ done items.
    pub per_done: Option<f64>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct Agents {
    /// A1 (first attempts).
    pub turns: i64,
    /// A2 over turns that started in the range.
    pub turn_ms: P,
    pub tools: i64,
    pub tool_error_rate: Option<f64>,
    pub tool_kinds: BTreeMap<String, i64>,
    /// A5: transient gateway errors that were retried, and the share of retries that went through.
    pub retries: i64,
    pub retry_success: Option<f64>,
    /// A6.
    pub gateway_fallbacks: i64,
    pub gateway_failed: i64,
    /// A7.
    pub permissions: i64,
    /// A8.
    pub interrupted: i64,
    pub cancelled: i64,
    /// A9.
    pub tokens_per_turn: Option<f64>,
    pub tools_per_turn: Option<f64>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct PerItem {
    /// U7 over items done in the range: whole-life tokens.
    pub tokens: P,
    /// S3: whole-life estimated dollars (median, average, the 3 most expensive).
    pub est_p50: Option<f64>,
    pub est_avg: Option<f64>,
    pub est_top: Vec<(String, f64)>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct Since {
    /// First day with token detail (agent or gateway records).
    pub tokens: Option<String>,
    pub gateway: Option<String>,
}

/// Every metric for one range.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct Summary {
    pub from: String,
    pub to: String,
    pub tz: String,
    pub usage: UsageTotals,
    pub spend: Spend,
    pub throughput: Throughput,
    pub time: Times,
    pub quality: Quality,
    pub human: Human,
    pub agents: Agents,
    pub per_item: PerItem,
    /// U8: sessions whose context is over 80 % full now.
    pub context_over_80: i64,
    pub since: Since,
}

/// An `item_marks` row in the range: (item, ts, from_v, to_v).
type MarkHit = (String, i64, Option<String>, Option<String>);

/// `(item filter SQL, …)`: `?2`/`?3` are the bounds, `?4` project, `?5` bot on alias `i`.
const ITEM_FILTER: &str = "(?4 IS NULL OR i.project_id = ?4) AND (?5 IS NULL OR i.bot_id = ?5)";

/// The estimated dollars of an item's `{model: [in, out, cr, cw]}`, priced as on `day` (the day it
/// was done, or today: the per-model sums do not keep their days).
pub fn item_est(models: &BTreeMap<String, [i64; 4]>, prices: &PriceBook, day: &str) -> f64 {
    models
        .iter()
        .filter_map(|(m, n)| {
            prices.cost(
                m,
                day,
                &Tokens {
                    input: Some(n[0]),
                    output: Some(n[1]),
                    cache_read: Some(n[2]),
                    cache_write: Some(n[3]),
                    reasoning: None,
                },
            )
        })
        .sum()
}

/// Every metric for `q`, `None` for a malformed day.
pub fn summary(c: &Connection, q: &Query) -> Result<Option<Summary>> {
    let tz = tz_of(c, &q.workspace_id)?;
    let Some((lo, hi)) = bounds(&q.from, &q.to, tz) else {
        return Ok(None);
    };
    let prices = book_of(c, &q.workspace_id)?;
    let rows = daily(c, q)?;
    let ws = &q.workspace_id;
    let p = params![ws, lo, hi, q.project, q.bot];

    let mut s = Summary {
        from: q.from.clone(),
        to: q.to.clone(),
        tz: tz
            .map(|z| z.name().to_string())
            .unwrap_or_else(|| "local".into()),
        usage: UsageTotals::of(&rows, &prices),
        ..Summary::default()
    };

    /* spend */
    let sp = &mut s.spend;
    sp.est = s.usage.est;
    sp.reported = s.usage.reported;
    sp.unpriced_tokens = s.usage.unpriced_tokens;
    sp.saved_by_cache = s.usage.saved_by_cache;
    sp.catalog = catalog::state(c, catalog::SOURCE)?;
    for d in &rows {
        let quote = prices.quote(&d.model, &d.day);
        let est = quote.as_ref().map_or(0.0, |p| p.price.cost(&d.tokens()));
        match &quote {
            None if d.total() > 0 => *sp.unpriced.entry(d.model.clone()).or_default() += d.total(),
            Some(q) if d.total() > 0 => {
                let k = match q.source {
                    PriceSource::Table => "table",
                    PriceSource::ModelsDev => "models_dev",
                };
                *sp.priced_by.entry(k.into()).or_default() += d.total();
            }
            _ => {}
        }
        *sp.by_source.entry(d.source.clone()).or_default() += est;
        if d.source == "gateway" {
            *sp.gateway_by_purpose.entry(d.purpose.clone()).or_default() += est;
        }
    }

    /* throughput */
    let th = &mut s.throughput;
    let created: Vec<String> = c
        .prepare_cached(&format!(
            "SELECT i.source FROM items i WHERE i.workspace_id = ?1 AND i.kind = 'task' AND i.created >= ?2 AND i.created < ?3 AND {ITEM_FILTER}"
        ))?
        .query_map(p, |r| r.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    th.created = created.len() as i64;
    for src in created {
        let key = serde_json::from_str::<Vec<String>>(&src)
            .ok()
            .and_then(|v| v.get(1).cloned())
            .unwrap_or_default();
        *th.created_by_source.entry(key).or_default() += 1;
    }
    let marks_to = |kind: &str| -> Result<Vec<MarkHit>> {
        Ok(c.prepare_cached(&format!(
            "SELECT m.item_id, m.ts, m.from_v, m.to_v FROM item_marks m JOIN items i ON i.id = m.item_id
             WHERE m.workspace_id = ?1 AND m.kind = ?6 AND m.ts >= ?2 AND m.ts < ?3 AND {ITEM_FILTER} ORDER BY m.ts"
        ))?
        .query_map(params![ws, lo, hi, q.project, q.bot, kind], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
        })?
        .collect::<rusqlite::Result<_>>()?)
    };
    let status = marks_to("status")?;
    let distinct = |to: &str| {
        let mut ids: Vec<&str> = status
            .iter()
            .filter(|m| m.3.as_deref() == Some(to))
            .map(|m| m.0.as_str())
            .collect();
        ids.sort_unstable();
        ids.dedup();
        ids.len() as i64
    };
    th.done = distinct("done");
    th.dropped = distinct("drop");
    th.drop_rate = ratio(th.dropped, th.done + th.dropped);
    th.net = th.created - th.done - th.dropped;
    th.wip = c.query_row(
        &format!(
            "SELECT COUNT(*) FROM items i WHERE i.workspace_id = ?1 AND i.kind = 'task' AND i.archived = 0
               AND i.status IN ('wait', 'run', 'block', 'pause', 'ship') AND {}",
            ITEM_FILTER.replace("?4", "?2").replace("?5", "?3")
        ),
        params![ws, q.project, q.bot],
        |r| r.get(0),
    )?;
    let delivers = marks_to("deliver")?;
    th.delivered = delivers.len() as i64;
    th.delivered_auto = delivers
        .iter()
        .filter(|m| m.3.as_deref() == Some("auto"))
        .count() as i64;
    th.delivered_manual = th.delivered - th.delivered_auto;

    /* items done in the range (item_stats) */
    struct Done {
        id: String,
        lead: Option<i64>,
        cycle: Option<i64>,
        first: Option<i64>,
        wait: Option<i64>,
        agent: Option<i64>,
        accepted: Option<i64>,
        tokens: i64,
        models: BTreeMap<String, [i64; 4]>,
        done_at: i64,
    }
    let done: Vec<Done> = c
        .prepare_cached(&format!(
            "SELECT s.item_id, s.lead_ms, s.cycle_ms, s.first_deliver - s.first_turn, s.wait_ms, s.agent_ms, s.accepted,
                    s.input + s.output + s.cache_read + s.cache_write, s.models, s.done_at
             FROM item_stats s JOIN items i ON i.id = s.item_id
             WHERE s.workspace_id = ?1 AND s.done_at >= ?2 AND s.done_at < ?3 AND {ITEM_FILTER}"
        ))?
        .query_map(p, |r| {
            Ok(Done {
                id: r.get(0)?,
                lead: r.get(1)?,
                cycle: r.get(2)?,
                first: r.get(3)?,
                wait: r.get(4)?,
                agent: r.get(5)?,
                accepted: r.get(6)?,
                tokens: r.get(7)?,
                models: serde_json::from_str(&r.get::<_, String>(8)?).unwrap_or_default(),
                done_at: r.get(9)?,
            })
        })?
        .collect::<rusqlite::Result<_>>()?;
    let col = |f: fn(&Done) -> Option<i64>| -> Vec<i64> { done.iter().filter_map(f).collect() };
    let t = &mut s.time;
    t.lead = P::of(&col(|d| d.lead));
    t.cycle = P::of(&col(|d| d.cycle));
    t.first_deliver = P::of(&col(|d| d.first.filter(|v| *v >= 0)));
    t.wait = P::of(&col(|d| d.wait));
    t.agent = P::of(&col(|d| d.agent));
    t.other = P::of(&col(|d| {
        d.lead
            .map(|l| (l - d.wait.unwrap_or(0) - d.agent.unwrap_or(0)).max(0))
    }));
    let releases = marks_to("release")?;
    let rel = |v: &str| {
        releases
            .iter()
            .filter(|m| m.3.as_deref() == Some(v))
            .count() as i64
    };
    t.released = rel("released");
    t.releases = ATTEMPTED.iter().map(|v| rel(v)).sum();
    t.release_success_rate = ratio(t.released, t.releases);
    t.not_released = rel("not_released");
    let accepted: BTreeMap<&str, i64> = done
        .iter()
        .filter_map(|d| Some((d.id.as_str(), d.accepted?)))
        .collect();
    let to_release: Vec<i64> = releases
        .iter()
        .filter(|m| m.3.as_deref() == Some("released"))
        .filter_map(|m| Some(m.1 - accepted.get(m.0.as_str())?))
        .filter(|v| *v >= 0)
        .collect();
    t.accept_to_release = P::of(&to_release);

    /* per completed item */
    s.per_item.tokens = P::of(&done.iter().map(|d| d.tokens).collect::<Vec<_>>());
    let mut ests: Vec<(String, f64)> = done
        .iter()
        .map(|d| {
            let day = crate::day(d.done_at, tz);
            (d.id.clone(), item_est(&d.models, &prices, &day))
        })
        .collect();
    if !ests.is_empty() {
        let mut v: Vec<f64> = ests.iter().map(|e| e.1).collect();
        v.sort_by(f64::total_cmp);
        let rank = ((0.5 * v.len() as f64).ceil() as usize).clamp(1, v.len());
        s.per_item.est_p50 = Some(v[rank - 1]);
        s.per_item.est_avg = Some(v.iter().sum::<f64>() / v.len() as f64);
    }
    ests.sort_by(|a, b| b.1.total_cmp(&a.1));
    ests.truncate(3);
    s.per_item.est_top = ests;

    /* quality */
    let qu = &mut s.quality;
    let fp: Vec<Option<bool>> = c
        .prepare_cached(&format!(
            "SELECT s.first_pass FROM item_stats s JOIN items i ON i.id = s.item_id
             WHERE s.workspace_id = ?1 AND s.first_deliver >= ?2 AND s.first_deliver < ?3 AND {ITEM_FILTER}"
        ))?
        .query_map(p, |r| r.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    let judged: Vec<bool> = fp.into_iter().flatten().collect();
    qu.first_pass_n = judged.len() as i64;
    qu.first_pass = ratio(
        judged.iter().filter(|b| **b).count() as i64,
        qu.first_pass_n,
    );
    let decisions = decisions_in(c, ws, lo, hi, q)?;
    qu.reworks = c.query_row(
        &format!(
            "SELECT COUNT(*) FROM decisions d JOIN items i ON i.id = d.item_id
             WHERE d.workspace_id = ?1 AND d.kind = 'accept' AND d.resolved >= ?2 AND d.resolved < ?3
               AND json_extract(d.result, '$.choice') = 'rework' AND {ITEM_FILTER}"
        ),
        p,
        |r| r.get(0),
    )?;
    qu.reworks_per_done = ratio(qu.reworks, s.throughput.done);
    let fails = marks_to("fail")?;
    for r in FAIL_REASONS {
        qu.fails.insert((*r).into(), 0);
    }
    for m in &fails {
        *qu.fails
            .entry(m.3.clone().unwrap_or_else(|| "other".into()))
            .or_default() += 1;
    }
    let mut stalled: Vec<&str> = fails
        .iter()
        .filter(|m| m.3.as_deref() == Some("stall"))
        .map(|m| m.0.as_str())
        .collect();
    stalled.sort_unstable();
    stalled.dedup();
    if !stalled.is_empty() {
        let mut finished = 0;
        for id in &stalled {
            let st: String =
                c.query_row("SELECT status FROM items WHERE id = ?", [id], |r| r.get(0))?;
            finished += i64::from(st == "done");
        }
        qu.stalled_then_done = ratio(finished, stalled.len() as i64);
    }
    qu.rollbacks = rel("rolled_back");
    qu.release_unknown = rel("unknown");
    qu.turn_error_rate = ratio(s.usage.turn_errors, s.usage.turns);
    qu.reopened = status
        .iter()
        .filter(|m| {
            matches!(m.2.as_deref(), Some("done") | Some("drop"))
                && !matches!(m.3.as_deref(), Some("done") | Some("drop"))
        })
        .count() as i64;

    /* human */
    let hu = &mut s.human;
    hu.decisions = decisions.len() as i64;
    let mut response = Vec::new();
    let mut by_kind_resp: BTreeMap<String, Vec<i64>> = BTreeMap::new();
    let mut by_person = 0;
    for (d, subject) in &decisions {
        *hu.by_kind
            .entry(format!("{}/{subject}", d.kind))
            .or_default() += 1;
        if d.status == "expired" {
            hu.expired += 1;
        }
        if d.status == "resolved"
            && !d.by_yolo()
            && let Some(r) = d.resolved
        {
            by_person += 1;
            response.push(r - d.created);
            by_kind_resp
                .entry(d.kind.clone())
                .or_default()
                .push(r - d.created);
        }
    }
    hu.response = P::of(&response);
    hu.response_by_kind = by_kind_resp
        .iter()
        .map(|(k, v)| (k.clone(), P::of(v)))
        .collect();
    hu.messages = c.query_row(
        &format!(
            "SELECT COUNT(*) FROM timeline t JOIN items i ON i.id = t.item_id
             WHERE i.workspace_id = ?1 AND t.kind = 'me' AND t.actor = 'me' AND t.ts >= ?2 AND t.ts < ?3 AND {ITEM_FILTER}"
        ),
        p,
        |r| r.get(0),
    )?;
    hu.per_done = ratio(by_person + hu.messages, s.throughput.done);
    hu.yolo = yolo(c, ws, lo, hi, hu.decisions)?;

    /* agents */
    let ag = &mut s.agents;
    ag.turns = s.usage.turns;
    ag.tools = s.usage.tools;
    ag.tool_error_rate = ratio(s.usage.tool_errors, s.usage.tools);
    ag.tool_kinds = tool_kinds(&rows);
    ag.retries = s.usage.retries;
    ag.interrupted = s.usage.interrupted;
    ag.tokens_per_turn =
        (ag.turns > 0 && s.usage.total > 0).then(|| s.usage.total as f64 / ag.turns as f64);
    ag.tools_per_turn = (ag.turns > 0).then(|| ag.tools as f64 / ag.turns as f64);
    let turns: Vec<(i64, i64, Option<String>, i64, bool, i64)> = c
        .prepare_cached(
            "SELECT t.started, t.ended, t.stop_reason, t.attempt, t.transient, t.permissions FROM agent_turns t
             WHERE t.workspace_id = ?1 AND t.started >= ?2 AND t.started < ?3 AND t.ended IS NOT NULL
               AND (?4 IS NULL OR t.project_id = ?4) AND (?5 IS NULL OR t.bot_id = ?5)",
        )?
        .query_map(p, |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?))
        })?
        .collect::<rusqlite::Result<_>>()?;
    ag.turn_ms = P::of(&turns.iter().map(|t| (t.1 - t.0).max(0)).collect::<Vec<_>>());
    ag.permissions = turns.iter().map(|t| t.5).sum();
    ag.cancelled = turns
        .iter()
        .filter(|t| t.2.as_deref() == Some("cancelled"))
        .count() as i64;
    let retried: Vec<_> = turns.iter().filter(|t| t.3 > 0).collect();
    ag.retry_success = ratio(
        retried
            .iter()
            .filter(|t| !t.4 && t.2.as_deref() != Some("error"))
            .count() as i64,
        retried.len() as i64,
    );
    let (fallbacks, failed): (i64, i64) = c.query_row(
        "SELECT COALESCE(SUM(attempts > 1), 0), COALESCE(SUM(ok = 0), 0) FROM usage_records
         WHERE workspace_id = ?1 AND source = 'gateway' AND day >= ?2 AND day <= ?3
           AND (?4 IS NULL OR project_id = ?4) AND (?5 IS NULL OR bot_id = ?5)",
        params![ws, q.from, q.to, q.project, q.bot],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    ag.gateway_fallbacks = fallbacks;
    ag.gateway_failed = failed;

    /* context, since */
    s.context_over_80 = c.query_row(
        "SELECT COUNT(*) FROM sessions WHERE workspace_id = ? AND source = 'Steve' AND size > 0 AND used * 5 > size * 4
           AND status IN ('running', 'starting', 'idle')",
        [ws],
        |r| r.get(0),
    )?;
    s.since = Since {
        tokens: c.query_row(
            "SELECT MIN(day) FROM usage_records WHERE workspace_id = ? AND source IN ('agent', 'gateway')",
            [ws],
            |r| r.get(0),
        )?,
        gateway: c.query_row(
            "SELECT MIN(day) FROM usage_records WHERE workspace_id = ? AND source = 'gateway'",
            [ws],
            |r| r.get(0),
        )?,
    };
    Ok(Some(s))
}

/// Decisions created in the range (with their subject type), item filters applied when given.
fn decisions_in(
    c: &Connection,
    ws: &str,
    lo: i64,
    hi: i64,
    q: &Query,
) -> Result<Vec<(crate::items::DecisionRow, String)>> {
    let filtered = q.project.is_some() || q.bot.is_some();
    let sql = if filtered {
        format!(
            "SELECT d.id, d.kind, d.status, d.created, d.resolved, d.result, d.subject_type FROM decisions d JOIN items i ON i.id = d.item_id
             WHERE d.workspace_id = ?1 AND d.created >= ?2 AND d.created < ?3 AND {ITEM_FILTER}"
        )
    } else {
        "SELECT d.id, d.kind, d.status, d.created, d.resolved, d.result, d.subject_type FROM decisions d
         WHERE d.workspace_id = ?1 AND d.created >= ?2 AND d.created < ?3 AND (?4 IS NULL) AND (?5 IS NULL)"
            .to_string()
    };
    Ok(c.prepare_cached(&sql)?
        .query_map(params![ws, lo, hi, q.project, q.bot], |r| {
            Ok((crate::items::decision_row(r)?, r.get(6)?))
        })?
        .collect::<rusqlite::Result<_>>()?)
}

/// H4 from `yolo_actions` (migration 14); `None` while that table does not exist.
fn yolo(c: &Connection, ws: &str, lo: i64, hi: i64, decisions: i64) -> Result<Option<Yolo>> {
    let exists: bool = c.query_row(
        "SELECT EXISTS (SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'yolo_actions')",
        [],
        |r| r.get(0),
    )?;
    if !exists {
        return Ok(None);
    }
    let rows: Vec<(String, String, Option<Value>)> = c
        .prepare_cached(
            "SELECT outcome, reason, undone FROM yolo_actions WHERE workspace_id = ? AND created >= ? AND created < ?",
        )?
        .query_map(params![ws, lo, hi], |r| {
            Ok((
                r.get(0)?,
                r.get(1)?,
                r.get::<_, Option<i64>>(2)?.map(Value::from),
            ))
        })?
        .collect::<rusqlite::Result<_>>()?;
    let mut y = Yolo::default();
    for (outcome, reason, undone) in rows {
        match outcome.as_str() {
            "decided" => y.decided += 1,
            "declined" => {
                y.declined += 1;
                *y.declined_by_reason.entry(reason).or_default() += 1;
            }
            _ => {}
        }
        if undone.is_some() {
            y.undone += 1;
        }
    }
    y.rate = ratio(y.decided, decisions);
    Ok(Some(y))
}

/// WIP at the end of each day from `from` to `to` (T4 trend), replaying status marks. Days before an
/// item's first mark count it by its creation.
pub fn wip_by_day(c: &Connection, q: &Query) -> Result<Vec<(String, i64)>> {
    let tz = tz_of(c, &q.workspace_id)?;
    let Some(_) = bounds(&q.from, &q.to, tz) else {
        return Ok(vec![]);
    };
    let mut ends = Vec::new();
    let mut day = NaiveDate::parse_from_str(&q.from, "%Y-%m-%d").ok();
    let last = NaiveDate::parse_from_str(&q.to, "%Y-%m-%d").ok();
    while let (Some(d), Some(l)) = (day, last) {
        if d > l {
            break;
        }
        let Some(end) = d
            .checked_add_days(Days::new(1))
            .and_then(|n| day_start(&n.format("%Y-%m-%d").to_string(), tz))
        else {
            break;
        };
        ends.push((d.format("%Y-%m-%d").to_string(), end));
        day = d.checked_add_days(Days::new(1));
    }
    wip_at(c, q, ends)
}

/// WIP just before each `(bucket, end)` instant (ascending), replaying status marks: [`wip_by_day`] and the
/// hours of a day.
pub fn wip_at(c: &Connection, q: &Query, ends: Vec<(String, i64)>) -> Result<Vec<(String, i64)>> {
    let marks: Vec<(String, i64, String)> = c
        .prepare_cached(
            "SELECT m.item_id, m.ts, COALESCE(m.to_v, '') FROM item_marks m JOIN items i ON i.id = m.item_id
             WHERE m.workspace_id = ?1 AND m.kind = 'status' AND (?2 IS NULL OR i.project_id = ?2) AND (?3 IS NULL OR i.bot_id = ?3)
             ORDER BY m.ts, m.id",
        )?
        .query_map(params![q.workspace_id, q.project, q.bot], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?))
        })?
        .collect::<rusqlite::Result<_>>()?;
    let mut state: BTreeMap<&str, &str> = BTreeMap::new();
    let mut k = 0;
    Ok(ends
        .into_iter()
        .map(|(name, end)| {
            while k < marks.len() && marks[k].1 < end {
                state.insert(&marks[k].0, &marks[k].2);
                k += 1;
            }
            (
                name,
                state.values().filter(|s| WIP.contains(s)).count() as i64,
            )
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn percentiles() {
        assert_eq!(percentile(&[], 0.5), None);
        assert_eq!(percentile(&[5], 0.85), Some(5));
        assert_eq!(percentile(&[4, 1, 3, 2], 0.5), Some(2));
        assert_eq!(percentile(&[1, 2, 3, 4, 5, 6, 7, 8, 9, 10], 0.85), Some(9));
    }

    #[test]
    fn bounds_follow_the_zone() {
        let sh = crate::day::tz(Some("Asia/Shanghai"));
        let (lo, hi) = bounds("2026-11-01", "2026-11-01", sh).unwrap_or_default();
        assert_eq!(hi - lo, 86_400_000);
        assert_eq!(crate::day::day(lo, sh), "2026-11-01");
        assert_eq!(crate::day::day(hi - 1, sh), "2026-11-01");
        assert_eq!(crate::day::day(hi, sh), "2026-11-02");
        assert!(bounds("2026-13-01", "2026-11-01", sh).is_none());
    }

    fn row(model: &str, source: &str, i: i64, o: i64, cr: i64) -> Daily {
        Daily {
            model: model.into(),
            source: source.into(),
            input: i,
            output: o,
            cache_read: cr,
            hit_base: i + cr,
            calls: 1,
            ..Default::default()
        }
    }

    #[test]
    fn models_by_purpose_counts_calls_per_model() {
        let r = |p: &str, h: &str, m: &str, calls: i64| Daily {
            purpose: p.into(),
            harness: h.into(),
            model: m.into(),
            calls,
            ..Default::default()
        };
        let rows = vec![
            r("route", "claude-cli", "haiku", 3),
            r("route", "claude-cli", "sonnet", 1),
            r("route", "claude-cli", "haiku", 2),
            r("route", "codex-cli", "haiku", 1),
            r("chat", "codex-cli", "opus", 1),
            r("chat", "claude-cli", "", 4),
        ];
        let m = models_by_purpose(&rows);
        let s = |h: &str, m: &str, n: i64| (h.to_string(), m.to_string(), n);
        assert_eq!(
            m["route"],
            vec![
                s("claude-cli", "haiku", 5),
                s("codex-cli", "haiku", 1),
                s("claude-cli", "sonnet", 1)
            ]
        );
        assert_eq!(m["chat"], vec![s("codex-cli", "opus", 1)]);
    }

    #[test]
    fn breakdown_adds_up_and_prices_linearly() {
        let prices = PriceBook::new(default_prices());
        let rows = vec![
            row("claude-opus-5-5", "agent", 1_000_000, 100_000, 9_000_000),
            row("kiro/claude-opus-5-5", "agent", 10, 10, 0),
            row("gpt-9", "gateway", 500, 50, 0),
            row("claude-haiku-5-5", "gateway", 1_000, 10, 0),
        ];
        let all = UsageTotals::of(&rows, &prices);
        for by in [By::Model, By::Source, By::Bot] {
            let parts = breakdown(&rows, by, &prices);
            let est: f64 = parts.iter().map(|r| r.usage.est).sum();
            let total: i64 = parts.iter().map(|r| r.usage.total).sum();
            assert!((est - all.est).abs() < 1e-9);
            assert_eq!(total, all.total);
        }
        assert_eq!(all.unpriced_tokens, 550);
        // opus: 1M in × 4 + 0.1M out × 20 + 9M cache read × 0.2 = 4 + 2 + 1.8
        let opus = UsageTotals::of(&rows[..1], &prices);
        assert!((opus.est - 7.8).abs() < 1e-9);
        assert!((opus.saved_by_cache - 9.0 * 3.8).abs() < 1e-9);
        assert_eq!(opus.hit_rate, Some(0.9));
        // doubling every price doubles the estimate
        let double: Vec<Price> = prices
            .table()
            .iter()
            .map(|p| Price {
                input: p.input * 2.0,
                output: p.output * 2.0,
                cache_read: p.cache_read * 2.0,
                cache_write: p.cache_write * 2.0,
                ..p.clone()
            })
            .collect();
        let double = PriceBook::new(double);
        assert!((UsageTotals::of(&rows, &double).est - 2.0 * all.est).abs() < 1e-9);
    }
}
