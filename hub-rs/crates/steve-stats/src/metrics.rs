//! The metrics over a range of local days. Usage, spend and agent counters read the
//! daily cube (`stat_daily`); flow, time and quality read `item_stats`, `item_marks` and `decisions`
//! through their indexes. Costs are estimated here, from tokens and the prices of each day
//! ([`PriceBook`]: the table, then Models.dev as it was that day).
//!
//! Filters: `project` and `bot` apply to everything (through the item, turn or record); `model` and
//! `source` only to the token and turn figures, which are the only ones that have them.

use std::collections::BTreeMap;

use chrono::{Days, NaiveDate};
use chrono_tz::Tz;
use serde::Serialize;
use steve_store::{Result, Tokens};

use crate::catalog::{self, SyncState};
use crate::items::ATTEMPTED;
use crate::prices::{Price, PriceBook, PriceSource, default_prices};
use crate::refresh::{add_turn, add_usage, timezone as tz_of};
use steve_store::statistics::{
    Catalog, CompletedItem, DailyFilter, DailyKey, DailyRows, DecisionReads, ItemHistory,
    ItemStats, MarkReads, Measures, ReportScope, SessionReads, StatsSettings, TimelineReads,
    TurnFacts, TurnReads, UsageFacts, UsageReads, YoloReads,
};
use steve_store::{ItemReads, Read};

/// WIP statuses (T4).
pub const WIP: &[&str] = &["active", "paused", "closing"];
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
pub fn prices_of(c: &mut impl Read, ws: &str) -> Result<Vec<Price>> {
    Ok(StatsSettings(c)
        .prices(ws)?
        .and_then(|v| serde_json::from_value(v).ok())
        .unwrap_or_else(default_prices))
}

/// Configured prices plus the catalog versions of models used by this report.
pub fn book_of<'a>(
    c: &mut impl Read,
    ws: &str,
    models: impl IntoIterator<Item = &'a str>,
) -> Result<PriceBook> {
    let table = prices_of(c, ws)?;
    let tz = tz_of(c, ws)?;
    PriceBook::new(table).with_catalog(c, tz, models)
}

/// Unix ms of local midnight starting `day` in `tz` (the Hub's zone for `None`); `None` for a bad date.
pub fn day_start(day: &str, tz: Option<Tz>) -> Option<i64> {
    let d = NaiveDate::parse_from_str(day, "%Y-%m-%d").ok()?;
    let midnight = d.and_hms_opt(0, 0, 0)?;
    crate::day::resolved(tz).instant(midnight).ok()
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
    pub coverage: crate::coverage::Coverage,
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
pub fn daily(c: &mut impl Read, q: &Query) -> Result<Vec<Daily>> {
    let mut rows = DailyRows(c)
        .range(daily_filter(q))?
        .into_iter()
        .map(|(key, measures)| daily_row(key, measures))
        .collect();
    attach_coverage(&mut rows, crate::coverage::cells(c, q)?);
    Ok(rows)
}

pub(crate) fn daily_filter(q: &Query) -> DailyFilter<'_> {
    DailyFilter {
        ws: &q.workspace_id,
        from: &q.from,
        to: &q.to,
        project: q.project.as_deref(),
        bot: q.bot.as_deref(),
        model: q.model.as_deref(),
        source: q.source.as_deref(),
    }
}

pub(crate) fn report_scope(q: &Query, lo: i64, hi: i64) -> ReportScope<'_> {
    ReportScope {
        ws: &q.workspace_id,
        lo,
        hi,
        project: q.project.as_deref(),
        bot: q.bot.as_deref(),
    }
}

fn daily_row(
    (day, source, purpose, harness, model, bot_id, project_id): DailyKey,
    m: Measures,
) -> Daily {
    let Measures {
        input,
        output,
        cache_read,
        cache_write,
        reasoning,
        hit_base,
        cost_reported,
        calls,
        failed_calls,
        turns,
        turn_ms,
        turn_errors,
        retries,
        interrupted,
        tools,
        tool_errors,
        tool_kinds,
    } = m;
    Daily {
        day,
        source,
        purpose,
        harness,
        model,
        bot_id,
        project_id,
        input,
        output,
        cache_read,
        cache_write,
        reasoning,
        hit_base,
        cost_reported,
        calls,
        failed_calls,
        turns,
        turn_ms,
        turn_errors,
        retries,
        interrupted,
        tools,
        tool_errors,
        tool_kinds,
        coverage: Default::default(),
    }
}

fn attach_coverage(
    rows: &mut Vec<Daily>,
    mut cells: BTreeMap<crate::coverage::Key, crate::coverage::Coverage>,
) {
    for d in rows.iter_mut() {
        let key = (
            d.day.clone(),
            d.source.clone(),
            d.purpose.clone(),
            d.harness.clone(),
            d.model.clone(),
            d.bot_id.clone(),
            d.project_id.clone(),
        );
        d.coverage = cells.remove(&key).unwrap_or_default();
    }
    for ((day, source, purpose, harness, model, bot_id, project_id), coverage) in cells {
        rows.push(Daily {
            day,
            source,
            purpose,
            harness,
            model,
            bot_id,
            project_id,
            coverage,
            ..Daily::default()
        });
    }
}

/// The cube rows of a one-day query (`from == to`) cut by local hour (ST-43): the same measures, read from
/// the detail rows the cube is built from (`usage_records` by their `day`, `agent_turns` by the day they
/// ended, as `refresh::rebuild_in` does), with `day` set to the [`crate::day::hour`] bucket. So the hours of
/// a day add up to that day's cube rows. Rows of hours outside the day (a stale `day`) are left out.
pub fn hourly(c: &mut impl Read, q: &Query) -> Result<Vec<Daily>> {
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
    let mut cells = BTreeMap::<DailyKey, Measures>::new();
    let mut first = BTreeMap::<String, String>::new();
    let f = daily_filter(q);
    UsageFacts(c).scan(f.ws, Some((f.from, f.to)), f.project, f.bot, |u| {
        if let Some(id) = &u.turn {
            first.entry(id.clone()).or_insert_with(|| u.model.clone());
        }
        if let Some(h) = hour(u.at).filter(|_| keep(&u.source, &u.model)) {
            let key = (
                h,
                u.source.clone(),
                u.purpose.clone(),
                u.harness.clone(),
                u.model.clone(),
                u.bot.clone(),
                u.project.clone(),
            );
            add_usage(cells.entry(key).or_default(), &u);
        }
        Ok(())
    })?;
    TurnFacts(c).scan_ended(f.ws, Some((lo, hi)), f.project, f.bot, |t| {
        let model = t
            .model
            .clone()
            .or_else(|| first.get(&t.id).cloned())
            .unwrap_or_else(|| t.fallback_model.clone());
        if let Some(h) = hour(t.ended).filter(|_| keep("agent", &model)) {
            let key = (
                h,
                "agent".into(),
                "turn".into(),
                t.harness.clone(),
                model,
                t.bot.clone(),
                t.project.clone(),
            );
            add_turn(cells.entry(key).or_default(), &t);
        }
        Ok(())
    })?;
    let mut rows = cells
        .into_iter()
        .map(|(key, measures)| daily_row(key, measures))
        .collect();
    attach_coverage(&mut rows, crate::coverage::hourly_cells(c, q)?);
    Ok(rows)
}

/// Token and spend totals of a set of cube rows.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct UsageTotals {
    pub coverage: crate::coverage::Coverage,
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
        self.coverage.add(&d.coverage);
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
    /// Q2: rework verdicts given in the range ("验收返工").
    pub reworks: i64,
    pub reworks_per_done: Option<f64>,
    /// Q2 inside work graphs: unit attempts that were not the unit's first ("单元重试"), next to
    /// `reworks` ([`crate::workflow::Workflow::retries`]).
    pub unit_retries: i64,
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
    /// Work graphs, deliveries and replies ([`crate::workflow`]).
    pub workflow: crate::workflow::Workflow,
    /// U8: sessions whose context is over 80 % full now.
    pub context_over_80: i64,
    pub since: Since,
}

/// An `item_marks` row in the range: (item, ts, from_v, to_v).

/// `(item filter SQL, …)`: `?2`/`?3` are the bounds, `?4` project, `?5` bot on alias `i`.

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

/// The tokens of an item's `{model: [in, out, cr, cw]}` on models with no price on `day`: left out of
/// [`item_est`], so an estimate next to them is a lower bound.
pub fn item_unpriced(models: &BTreeMap<String, [i64; 4]>, prices: &PriceBook, day: &str) -> i64 {
    models
        .iter()
        .filter(|(m, n)| {
            let t = Tokens {
                input: Some(n[0]),
                output: Some(n[1]),
                cache_read: Some(n[2]),
                cache_write: Some(n[3]),
                reasoning: None,
            };
            prices.cost(m, day, &t).is_none()
        })
        .map(|(_, n)| n.iter().sum::<i64>())
        .sum()
}

/// Every metric for `q`, `None` for a malformed day.
pub fn summary(c: &mut impl Read, q: &Query) -> Result<Option<Summary>> {
    let tz = tz_of(c, &q.workspace_id)?;
    let Some((lo, hi)) = bounds(&q.from, &q.to, tz) else {
        return Ok(None);
    };
    let rows = daily(c, q)?;
    let mut prices = book_of(
        c,
        &q.workspace_id,
        rows.iter().map(|row| row.model.as_str()),
    )?;
    let ws = &q.workspace_id;
    let scope = report_scope(q, lo, hi);

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
    sp.catalog = Catalog(c).state(catalog::SOURCE)?;
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
    let created = ItemHistory(c).created_sources(scope)?;
    th.created = created.len() as i64;
    for source in created {
        let key = source.get(1).cloned().unwrap_or_default();
        *th.created_by_source.entry(key).or_default() += 1;
    }
    let status = MarkReads(c).changes(scope, "status")?;
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
    th.done = distinct("completed");
    th.dropped = distinct("cancelled");
    th.drop_rate = ratio(th.dropped, th.done + th.dropped);
    th.net = th.created - th.done - th.dropped;
    th.wip = ItemHistory(c).open_tasks(scope, WIP)?;
    let delivers = MarkReads(c).changes(scope, "deliver")?;
    th.delivered = delivers.len() as i64;
    th.delivered_auto = delivers
        .iter()
        .filter(|m| m.3.as_deref() == Some("auto"))
        .count() as i64;
    th.delivered_manual = th.delivered - th.delivered_auto;

    /* items done in the range (item_stats) */
    let done = ItemStats(c).completed(scope)?;
    prices.load(
        c,
        done.iter()
            .flat_map(|row| row.models.keys().map(String::as_str)),
    )?;
    let col =
        |f: fn(&CompletedItem) -> Option<i64>| -> Vec<i64> { done.iter().filter_map(f).collect() };
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
    let releases = MarkReads(c).changes(scope, "release")?;
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
    let judged = ItemStats(c).first_passes(scope)?;
    qu.first_pass_n = judged.len() as i64;
    qu.first_pass = ratio(
        judged.iter().filter(|b| **b).count() as i64,
        qu.first_pass_n,
    );
    let decisions = DecisionReads(c).created(scope)?;
    qu.reworks = DecisionReads(c).reworks(scope)?;
    qu.reworks_per_done = ratio(qu.reworks, s.throughput.done);
    s.workflow = crate::workflow::of(c, q, lo, hi)?;
    let qu = &mut s.quality;
    qu.unit_retries = s.workflow.retries;
    let fails = MarkReads(c).changes(scope, "fail")?;
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
            finished += i64::from(
                ItemReads(c).item_outcome_or_phase(ws, id)?.as_deref() == Some("completed"),
            );
        }
        qu.stalled_then_done = ratio(finished, stalled.len() as i64);
    }
    qu.rollbacks = rel("rolled_back");
    qu.release_unknown = rel("unknown");
    qu.turn_error_rate = ratio(s.usage.turn_errors, s.usage.turns);
    qu.reopened = status
        .iter()
        .filter(|m| {
            matches!(m.2.as_deref(), Some("completed" | "cancelled" | "failed"))
                && !matches!(m.3.as_deref(), Some("completed" | "cancelled" | "failed"))
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
    hu.messages = TimelineReads(c).owner_messages(scope)?;
    hu.per_done = ratio(by_person + hu.messages, s.throughput.done);
    hu.yolo = Some(yolo(c, scope, hu.decisions)?);

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
    let turns = TurnReads(c).completed_in(scope)?;
    ag.turn_ms = P::of(
        &turns
            .iter()
            .map(|t| (t.ended - t.started).max(0))
            .collect::<Vec<_>>(),
    );
    ag.permissions = turns.iter().map(|t| t.permissions).sum();
    ag.cancelled = turns
        .iter()
        .filter(|t| t.reason.as_deref() == Some("cancelled"))
        .count() as i64;
    let retried: Vec<_> = turns.iter().filter(|t| t.attempt > 0).collect();
    ag.retry_success = ratio(
        retried
            .iter()
            .filter(|t| !t.transient && t.reason.as_deref() != Some("error"))
            .count() as i64,
        retried.len() as i64,
    );
    (ag.gateway_fallbacks, ag.gateway_failed) = UsageReads(c).gateway_outcomes(daily_filter(q))?;

    s.context_over_80 = SessionReads(c).close_to_limit(ws)?;
    let mut usage = UsageReads(c);
    s.since = Since {
        tokens: usage.earliest(ws, &["agent", "gateway"])?,
        gateway: usage.earliest(ws, &["gateway"])?,
    };
    Ok(Some(s))
}

fn yolo(c: &mut impl Read, scope: ReportScope<'_>, decisions: i64) -> Result<Yolo> {
    let rows = YoloReads(c).actions(scope)?;
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
        if undone {
            y.undone += 1;
        }
    }
    y.rate = ratio(y.decided, decisions);
    Ok(y)
}

/// WIP at the end of each day from `from` to `to` (T4 trend), replaying status marks. Days before an
/// item's first mark count it by its creation.
pub fn wip_by_day(c: &mut impl Read, q: &Query) -> Result<Vec<(String, i64)>> {
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
pub fn wip_at(
    c: &mut impl Read,
    q: &Query,
    ends: Vec<(String, i64)>,
) -> Result<Vec<(String, i64)>> {
    let Some((_, end)) = ends.last() else {
        return Ok(Vec::new());
    };
    let marks: Vec<_> = MarkReads(c)
        .changes(report_scope(q, i64::MIN, *end), "status")?
        .into_iter()
        .map(|(id, at, _, state)| (id, at, state.unwrap_or_default()))
        .collect();
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
