//! What the statistics page reads besides the summary: ranges, the trend series, the
//! per-item table and drill-down, a session's totals, the non-token breakdowns, and the labels of keys.
//! Everything reads the cube and `item_stats` through their indexes; per-item reads go by `item_id`.

use std::collections::{BTreeMap, HashMap};

use chrono::{Days, NaiveDate};
use serde::Serialize;
use serde_json::{Value, json};
use steve_store::statistics::{
    BotReads, DecisionReads, Entity, ItemHistory, ItemStats, MarkReads, NodeReads, SessionReads,
    TokenGroup, TurnReads, UsageReads,
};
use steve_store::{Read, Result, Tokens};

use crate::day::{day, hour, hours};
use crate::items::{self, ItemFacts};
use crate::metrics::{self, Daily, P, Query, Summary, bounds, percentile, report_scope};
use crate::prices::PriceBook;
use crate::refresh::timezone as tz_of;

/* ── ranges ── */

/// Longest custom range, in days.
pub const MAX_DAYS: i64 = 366;

fn date(s: &str) -> Option<NaiveDate> {
    NaiveDate::parse_from_str(s, "%Y-%m-%d").ok()
}

fn fmt(d: NaiveDate) -> String {
    d.format("%Y-%m-%d").to_string()
}

/// `range=today|7d|30d|custom` → `(from, to)`, both inclusive local days; `today` is the workspace's
/// current local day. `Err` names the bad parameter (`range`, `from` or `to`).
pub fn range(
    kind: &str,
    from: Option<&str>,
    to: Option<&str>,
    today: &str,
) -> std::result::Result<(String, String), &'static str> {
    let t = date(today).ok_or("range")?;
    let back = |n: u64| t.checked_sub_days(Days::new(n)).map(fmt).ok_or("range");
    match kind {
        "today" => Ok((today.into(), today.into())),
        "7d" => Ok((back(6)?, today.into())),
        "30d" => Ok((back(29)?, today.into())),
        "custom" => {
            let f = from.and_then(date).ok_or("from")?;
            let l = to.and_then(date).ok_or("to")?;
            let span = (l - f).num_days() + 1;
            if !(1..=MAX_DAYS).contains(&span) {
                return Err("from");
            }
            Ok((fmt(f), fmt(l)))
        }
        _ => Err("range"),
    }
}

/// The range of the same length right before `from..=to`.
pub fn prev(from: &str, to: &str) -> Option<(String, String)> {
    let (f, l) = (date(from)?, date(to)?);
    let n = (l - f).num_days() + 1;
    let pl = f.checked_sub_days(Days::new(1))?;
    let pf = f.checked_sub_days(Days::new(n as u64))?;
    Some((fmt(pf), fmt(pl)))
}

/// The local days `from..=to`.
pub fn days(from: &str, to: &str) -> Vec<String> {
    let (Some(mut d), Some(l)) = (date(from), date(to)) else {
        return vec![];
    };
    let mut out = vec![];
    while d <= l && out.len() <= MAX_DAYS as usize {
        out.push(fmt(d));
        match d.checked_add_days(Days::new(1)) {
            Some(n) => d = n,
            None => break,
        }
    }
    out
}

/// Today in the workspace's zone.
pub fn today(c: &mut impl Read, ws: &str, now: i64) -> Result<String> {
    Ok(day(now, tz_of(c, ws)?))
}

/* ── labels ── */

/// Names of the fixed keys (sources, purposes, harnesses, fail reasons, decision kinds), in the
/// request's language.
pub fn fixed_label(key: &str) -> Option<String> {
    let spec = steve_acp::adapters::spec(key, None);
    if spec.builtin {
        return Some(spec.name);
    }
    source_label(key).or_else(|| outcome_label(key))
}

fn source_label(key: &str) -> Option<String> {
    Some(match key {
        "agent" => steve_i18n::t!("stats.label.agent"),
        "gateway" => steve_i18n::t!("stats.label.gateway"),
        "turn" => steve_i18n::t!("stats.label.turn"),
        "autonomous" => steve_i18n::t!("stats.label.autonomous"),
        "route" => steve_i18n::t!("stats.label.route"),
        "chat" => steve_i18n::t!("stats.label.chat"),
        "research" => steve_i18n::t!("stats.label.research"),
        "title" => steve_i18n::t!("stats.label.title"),
        "draft" => steve_i18n::t!("stats.label.draft"),
        "extract" => steve_i18n::t!("stats.label.extract"),
        "compose" => steve_i18n::t!("stats.label.compose"),
        "judge" => steve_i18n::t!("stats.label.judge"),
        "mail" => steve_i18n::t!("stats.label.mail"),
        "disclosure" => steve_i18n::t!("stats.label.disclosure"),
        "intent" => steve_i18n::t!("stats.label.intent"),
        "flow" => steve_i18n::t!("stats.label.flow"),
        "digest" => steve_i18n::t!("stats.label.digest"),
        "other" => steve_i18n::t!("stats.label.other"),
        _ => return None,
    })
}

fn outcome_label(key: &str) -> Option<String> {
    Some(match key {
        "stall" => steve_i18n::t!("stats.label.stall"),
        "error" => steve_i18n::t!("stats.label.error"),
        "restart" => steve_i18n::t!("stats.label.restart"),
        "release" => steve_i18n::t!("stats.label.release"),
        "accept" => steve_i18n::t!("stats.label.accept"),
        "ask" => steve_i18n::t!("stats.label.ask"),
        "approve" => steve_i18n::t!("stats.label.approve"),
        "fail" => steve_i18n::t!("stats.label.fail"),
        "propose" => steve_i18n::t!("stats.label.propose"),
        "confirm" => steve_i18n::t!("stats.label.confirm"),
        _ => return None,
    })
}

/// Bot, project, Node, session, and declared harness names of a workspace, for labelling keys.
#[derive(Debug, Default)]
pub struct Names {
    bots: HashMap<String, String>,
    projects: HashMap<String, String>,
    nodes: HashMap<String, String>,
    sessions: HashMap<String, String>,
    harnesses: HashMap<String, String>,
}

impl Names {
    pub fn load(c: &mut impl Read, ws: &str) -> Result<Names> {
        Ok(Names {
            bots: BotReads(c).names()?.into_iter().collect(),
            projects: ItemHistory(c).project_names(ws)?.into_iter().collect(),
            nodes: NodeReads(c).names()?.into_iter().collect(),
            sessions: SessionReads(c).titles(ws)?.into_iter().collect(),
            harnesses: HashMap::new(),
        })
    }

    /// Adds display names for custom/declared harness adapters.
    pub fn with_harnesses(mut self, harnesses: HashMap<String, String>) -> Self {
        self.harnesses = harnesses;
        self
    }

    /// The label of `key` in dimension `dim` (`model`, `bot`, `project`, `node`, `session`, `source`, `purpose`, …).
    pub fn label(&self, dim: &str, key: &str) -> String {
        match (dim, key) {
            ("bot", "") => steve_i18n::t!("stats.label.no_bot"),
            ("project", "") => steve_i18n::t!("stats.label.no_project"),
            ("model", "") => steve_i18n::t!("stats.label.unknown_model"),
            ("node", "") => steve_i18n::t!("stats.label.unattributed_node"),
            ("node", "local") => self
                .nodes
                .get("local")
                .filter(|s| !s.is_empty())
                .cloned()
                .unwrap_or_else(|| steve_i18n::t!("stats.label.local_node")),
            ("node", k) => self
                .nodes
                .get(k)
                .filter(|s| !s.is_empty())
                .cloned()
                .unwrap_or_else(|| k.into()),
            ("session", "") => steve_i18n::t!("stats.label.no_session"),
            ("session", k) => self
                .sessions
                .get(k)
                .filter(|s| !s.is_empty())
                .cloned()
                .unwrap_or_else(|| k.into()),
            ("bot", k) => self.bots.get(k).cloned().unwrap_or_else(|| k.into()),
            ("project", k) => self.projects.get(k).cloned().unwrap_or_else(|| k.into()),
            ("model", k) | ("day", k) => k.into(),
            ("harness", k) => self
                .harnesses
                .get(k)
                .cloned()
                .or_else(|| fixed_label(k))
                .unwrap_or_else(|| k.into()),
            (_, k) => fixed_label(k)
                .or_else(|| self.harnesses.get(k).cloned())
                .unwrap_or_else(|| k.into()),
        }
    }
}

/* ── series ── */

/// `metric` of `GET /stats/series`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Metric {
    Tokens,
    Cost,
    Done,
    Created,
    Wip,
    Lead,
    HitRate,
    Turns,
    /// Milliseconds agents spent in turns, each turn counted whole in the bucket it ended in (the cube's
    /// `turn_ms`, so the buckets add up to the summary's `usage.turn_ms`).
    TurnMs,
    /// Milliseconds a person was waited on: per item, the time any of its cards for a person was open
    /// (an item's `wait_ms`), cut at the bucket edges; a card with no item counts on its own.
    WaitMs,
    Decisions,
    /// Distinct pieces of work an agent turned on: an item counts once per bucket, and a conversation with no
    /// item is a piece of its own (the sessions page's activity heatmap).
    Active,
}

impl Metric {
    pub fn parse(s: &str) -> Option<Metric> {
        Some(match s {
            "tokens" => Metric::Tokens,
            "cost" => Metric::Cost,
            "done" => Metric::Done,
            "created" => Metric::Created,
            "wip" => Metric::Wip,
            "lead" => Metric::Lead,
            "hit_rate" => Metric::HitRate,
            "turns" => Metric::Turns,
            "turn_ms" => Metric::TurnMs,
            "wait_ms" => Metric::WaitMs,
            "decisions" => Metric::Decisions,
            "active" => Metric::Active,
            _ => return None,
        })
    }

    /// Whether `group` (`none|model|bot|project|source|purpose|token_type`) applies to this metric.
    pub fn allows(&self, group: &str) -> bool {
        match self {
            Metric::Tokens | Metric::Cost => true,
            Metric::HitRate | Metric::Turns | Metric::TurnMs => group != "token_type",
            Metric::Done | Metric::Created | Metric::Active | Metric::WaitMs => {
                matches!(group, "none" | "bot" | "project")
            }
            Metric::Wip | Metric::Lead | Metric::Decisions => group == "none",
        }
    }
}

/// One line of a chart.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Line {
    pub key: String,
    pub label: String,
    /// One value per bucket; `null` = nothing to measure that day (a rate or a time).
    pub values: Vec<Option<f64>>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Series {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub coverage: Option<Vec<crate::coverage::Coverage>>,
    /// `day` (`YYYY-MM-DD`) or `hour` (`YYYY-MM-DDTHH:00`, the local hours of a one-day range).
    pub bucket: &'static str,
    pub buckets: Vec<String>,
    pub series: Vec<Line>,
    /// Hours only: each bucket's `[start, end)` in Unix ms (a skipped hour is empty, a repeated one 2 h long).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub spans: Option<Vec<[i64; 2]>>,
    /// Hours only: the bucket that holds now, still running; the ones after it are `null` (not yet), not 0.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub current: Option<usize>,
}

/// How a series is cut.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Bucket {
    /// One bucket per local day of the range.
    Day,
    /// The 24 local hours of a one-day range, as they stand at `now` (Unix ms).
    Hour { now: i64 },
}

/// Lines kept before the rest are folded into “其他”.
pub const MAX_LINES: usize = 6;

/// Per-key, per-day numerators and denominators (`den` is only used by rates).
#[derive(Default)]
struct Acc {
    keys: BTreeMap<String, Vec<(f64, f64)>>,
}

impl Acc {
    fn add(&mut self, key: &str, i: usize, n: usize, num: f64, den: f64) {
        let v = self
            .keys
            .entry(key.to_string())
            .or_insert_with(|| vec![(0.0, 0.0); n]);
        v[i].0 += num;
        v[i].1 += den;
    }

    /// Lines, biggest first, at most [`MAX_LINES`] (the smallest folded into `other`).
    fn lines(self, rate: bool, names: &Names, dim: &str, order: &[&str]) -> Vec<Line> {
        let mut keys: Vec<(String, Vec<(f64, f64)>)> = self.keys.into_iter().collect();
        if order.is_empty() {
            let weight =
                |v: &Vec<(f64, f64)>| v.iter().map(|x| if rate { x.1 } else { x.0 }).sum::<f64>();
            keys.sort_by(|a, b| weight(&b.1).total_cmp(&weight(&a.1)).then(a.0.cmp(&b.0)));
        } else {
            keys.sort_by_key(|k| order.iter().position(|o| *o == k.0).unwrap_or(usize::MAX));
        }
        if keys.len() > MAX_LINES {
            let rest = keys.split_off(MAX_LINES - 1);
            let n = rest.first().map_or(0, |r| r.1.len());
            let mut sum = vec![(0.0, 0.0); n];
            for (_, v) in rest {
                for (i, x) in v.into_iter().enumerate() {
                    sum[i].0 += x.0;
                    sum[i].1 += x.1;
                }
            }
            keys.push(("other".into(), sum));
        }
        keys.into_iter()
            .map(|(k, v)| Line {
                label: if k == "other" {
                    steve_i18n::t!("stats.label.other")
                } else {
                    names.label(dim, &k)
                },
                key: k,
                values: v
                    .into_iter()
                    .map(|(num, den)| {
                        if rate {
                            (den > 0.0).then(|| num / den)
                        } else {
                            Some(num)
                        }
                    })
                    .collect(),
            })
            .collect()
    }
}

fn group_key<'a>(d: &'a Daily, group: &str) -> &'a str {
    match group {
        "model" => &d.model,
        "harness" => &d.harness,
        "bot" => &d.bot_id,
        "project" => &d.project_id,
        "node" => &d.node_id,
        "session" => &d.session_id,
        "source" => &d.source,
        "purpose" => &d.purpose,
        _ => "total",
    }
}

const TOKEN_TYPES: [&str; 4] = ["input", "output", "cache_read", "cache_write"];

fn token_parts(d: &Daily) -> [i64; 4] {
    [d.input, d.output, d.cache_read, d.cache_write]
}

/// `GET /stats/series`: one value per local day of `q`, or per local hour when `bucket` is [`Bucket::Hour`]
/// (the caller checks `q.from == q.to`). Hours count the same events as days, by the hour they happened in,
/// so a day's hours add up to the day. The caller checks `metric.allows(group)`.
pub fn series(
    c: &mut impl Read,
    q: &Query,
    metric: Metric,
    group: &str,
    bucket: Bucket,
) -> Result<Series> {
    series_with_harnesses(c, q, metric, group, bucket, &HashMap::new())
}

/// Like [`series`], with custom/declared harness display names for `group=harness` / source labels.
pub fn series_with_harnesses(
    c: &mut impl Read,
    q: &Query,
    metric: Metric,
    group: &str,
    bucket: Bucket,
    harnesses: &HashMap<String, String>,
) -> Result<Series> {
    let tz = tz_of(c, &q.workspace_id)?;
    let spans = match bucket {
        Bucket::Day => None,
        Bucket::Hour { .. } => Some(hours(&q.from, tz).unwrap_or_default()),
    };
    let buckets: Vec<String> = match &spans {
        Some(h) => h.iter().map(|x| x.0.clone()).collect(),
        None => days(&q.from, &q.to),
    };
    let mut coverage = vec![crate::coverage::Coverage::default(); buckets.len()];
    let mut series = lines(
        c,
        q,
        metric,
        group,
        &buckets,
        spans.as_deref(),
        tz,
        harnesses,
        &mut coverage,
    )?;
    let mut current = None;
    if let (Bucket::Hour { now }, Some(h)) = (bucket, &spans) {
        for (i, (_, start, end)) in h.iter().enumerate() {
            if *start > now {
                for l in &mut series {
                    l.values[i] = None;
                }
            } else if now < *end {
                current = Some(i);
            }
        }
    }
    Ok(Series {
        coverage: matches!(metric, Metric::Tokens | Metric::Cost).then_some(coverage),
        bucket: if spans.is_some() { "hour" } else { "day" },
        buckets,
        series,
        spans: spans.map(|h| h.iter().map(|x| [x.1, x.2]).collect()),
        current,
    })
}

/// The lines of [`series`] over `buckets` (the days, or the hours with their `spans`).
#[allow(clippy::too_many_arguments)]
fn lines(
    c: &mut impl Read,
    q: &Query,
    metric: Metric,
    group: &str,
    buckets: &[String],
    spans: Option<&[(String, i64, i64)]>,
    tz: Option<chrono_tz::Tz>,
    harnesses: &HashMap<String, String>,
    coverage: &mut [crate::coverage::Coverage],
) -> Result<Vec<Line>> {
    let n = buckets.len();
    let index: HashMap<&str, usize> = buckets
        .iter()
        .enumerate()
        .map(|(i, d)| (d.as_str(), i))
        .collect();

    let names = Names::load(c, &q.workspace_id)?.with_harnesses(harnesses.clone());
    let mut acc = Acc::default();
    let mut observed: Option<Observed> = None;

    let mut rate = false;
    let mut order: &[&str] = &[];
    let scope = BucketScope {
        q,
        group,
        buckets,
        spans,
        tz,
        index: &index,
        n,
    };
    match metric {
        Metric::Tokens | Metric::Cost | Metric::HitRate | Metric::Turns | Metric::TurnMs => {
            collect_usage_lines(c, &scope, metric, &mut acc, coverage, &mut observed)?;
            rate = metric == Metric::HitRate;
            if group == "token_type" {
                order = &TOKEN_TYPES;
                // every type is a line even when 0, so the stack keeps its order
                for t in TOKEN_TYPES {
                    acc.keys
                        .entry(t.into())
                        .or_insert_with(|| vec![(0.0, 0.0); n]);
                }
            }
        }
        Metric::Done | Metric::Created => {
            if collect_item_counts(c, &scope, metric, &mut acc)?.is_none() {
                return Ok(vec![]);
            }
        }
        Metric::Active => {
            if collect_active_work(c, &scope, &mut acc)?.is_none() {
                return Ok(vec![]);
            }
        }
        Metric::WaitMs => {
            if collect_waiting_time(c, &scope, &mut acc)?.is_none() {
                return Ok(vec![]);
            }
        }
        Metric::Wip => collect_work_in_progress(c, &scope, &mut acc)?,
        Metric::Decisions => {
            if collect_decision_counts(c, &scope, &mut acc)?.is_none() {
                return Ok(vec![]);
            }
        }
        Metric::Lead => return lead_percentiles(c, &scope),
    }
    Ok(finish_series(
        acc, observed, rate, order, &scope, &names, metric,
    ))
}

type Observed = std::collections::HashSet<(String, usize)>;

struct BucketScope<'a> {
    q: &'a Query,
    group: &'a str,
    buckets: &'a [String],
    spans: Option<&'a [(String, i64, i64)]>,
    tz: Option<chrono_tz::Tz>,
    index: &'a HashMap<&'a str, usize>,
    n: usize,
}

impl BucketScope<'_> {
    fn bucket_of(&self, ts: i64) -> String {
        if self.spans.is_some() {
            hour(ts, self.tz)
        } else {
            day(ts, self.tz)
        }
    }
}

fn series_total_label(m: Metric) -> String {
    match m {
        Metric::Tokens => steve_i18n::t!("stats.total.tokens"),
        Metric::Cost => steve_i18n::t!("stats.total.cost"),
        Metric::Done => steve_i18n::t!("stats.total.done"),
        Metric::Created => steve_i18n::t!("stats.total.created"),
        Metric::Wip => steve_i18n::t!("stats.total.wip"),
        Metric::Lead => steve_i18n::t!("stats.total.lead"),
        Metric::HitRate => steve_i18n::t!("stats.total.hit_rate"),
        Metric::Turns => steve_i18n::t!("stats.total.turns"),
        Metric::TurnMs => steve_i18n::t!("stats.total.turn_ms"),
        Metric::WaitMs => steve_i18n::t!("stats.total.wait_ms"),
        Metric::Decisions => steve_i18n::t!("stats.total.decisions"),
        Metric::Active => steve_i18n::t!("stats.total.active"),
    }
}

fn collect_usage_lines(
    c: &mut impl Read,
    scope: &BucketScope<'_>,
    metric: Metric,
    acc: &mut Acc,
    coverage: &mut [crate::coverage::Coverage],
    observed: &mut Option<Observed>,
) -> Result<()> {
    let &BucketScope {
        q,
        group,
        spans,
        index,
        ..
    } = scope;

    let rows = if group == "session" {
        metrics::by_session(c, q, spans.is_some())?
    } else if spans.is_some() {
        metrics::hourly(c, q)?
    } else {
        metrics::daily(c, q)?
    };
    let prices = metrics::book_of(c, &q.workspace_id, rows.iter().map(|r| r.model.as_str()))?;
    if matches!(metric, Metric::Tokens | Metric::Cost) {
        *observed = Some(Default::default());
    }
    for d in rows {
        let Some(&i) = index.get(d.day.as_str()) else {
            continue;
        };
        let key = group_key(&d, group);
        coverage[i].add(&d.coverage);
        if let Some(seen) = observed.as_mut() {
            if group == "token_type" {
                let fields = [
                    d.coverage.input,
                    d.coverage.output,
                    d.coverage.cache_read,
                    d.coverage.cache_write,
                ];
                for (field, value) in TOKEN_TYPES.iter().zip(fields) {
                    if value.is_some()
                        && (metric == Metric::Tokens || prices.quote(&d.model, &d.day).is_some())
                    {
                        seen.insert((field.to_string(), i));
                    }
                }
            } else if d.coverage.total.is_some()
                && (metric == Metric::Tokens || prices.quote(&d.model, &d.day).is_some())
            {
                seen.insert((key.to_string(), i));
            }
        }
        add_usage_values(acc, &d, metric, key, i, scope, &prices);
    }
    Ok(())
}

fn add_usage_values(
    acc: &mut Acc,
    d: &Daily,
    metric: Metric,
    key: &str,
    i: usize,
    scope: &BucketScope<'_>,
    prices: &PriceBook,
) {
    let group = scope.group;
    let n = scope.n;
    match metric {
        Metric::Tokens if group == "token_type" => {
            for (t, v) in TOKEN_TYPES.iter().zip(token_parts(&d)) {
                acc.add(t, i, n, v as f64, 0.0);
            }
        }
        Metric::Tokens => acc.add(key, i, n, d.total() as f64, 0.0),
        Metric::Cost => {
            let Some(p) = prices.quote(&d.model, &d.day) else {
                return;
            };
            if group == "token_type" {
                let per = [
                    p.price.input,
                    p.price.output,
                    p.price.cache_read,
                    p.price.cache_write,
                ];
                for ((t, v), pr) in TOKEN_TYPES.iter().zip(token_parts(&d)).zip(per) {
                    acc.add(t, i, n, v as f64 * pr / 1e6, 0.0);
                }
            } else {
                acc.add(key, i, n, p.price.cost(&d.tokens()), 0.0);
            }
        }
        Metric::HitRate => acc.add(key, i, n, d.cache_read as f64, d.hit_base as f64),
        Metric::TurnMs => acc.add(key, i, n, d.turn_ms as f64, 0.0),
        _ => acc.add(key, i, n, d.turns as f64, 0.0),
    }
}

fn collect_item_counts(
    c: &mut impl Read,
    scope: &BucketScope<'_>,
    metric: Metric,
    acc: &mut Acc,
) -> Result<Option<()>> {
    let &BucketScope {
        q,
        group,
        index,
        n,
        tz,
        ..
    } = scope;

    let Some((lo, hi)) = bounds(&q.from, &q.to, tz) else {
        return Ok(None);
    };
    let range = report_scope(q, lo, hi);
    let rows = if metric == Metric::Done {
        MarkReads(c).completed_at(range)?
    } else {
        ItemHistory(c).created_at(range)?
    };
    // done: an item counts once per day (per hour)
    let mut seen = std::collections::HashSet::new();
    for (id, ts, bot, project) in rows {
        let dd = scope.bucket_of(ts);
        let Some(&i) = index.get(dd.as_str()) else {
            continue;
        };
        if !seen.insert((id, i)) {
            continue;
        }
        let key = match group {
            "bot" => bot.as_str(),
            "project" => project.as_str(),
            _ => "total",
        };
        acc.add(key, i, n, 1.0, 0.0);
    }
    Ok(Some(()))
}

fn collect_active_work(
    c: &mut impl Read,
    scope: &BucketScope<'_>,
    acc: &mut Acc,
) -> Result<Option<()>> {
    let &BucketScope {
        q,
        group,
        index,
        n,
        tz,
        ..
    } = scope;

    let Some((lo, hi)) = bounds(&q.from, &q.to, tz) else {
        return Ok(None);
    };
    let rows = TurnReads(c).active_at(report_scope(q, lo, hi))?;
    let mut seen = std::collections::HashSet::new();
    for (work, ts, bot, project) in rows {
        let Some(&i) = index.get(scope.bucket_of(ts).as_str()) else {
            continue;
        };
        if !seen.insert((work, i)) {
            continue;
        }
        let key = match group {
            "bot" => bot.as_str(),
            "project" => project.as_str(),
            _ => "total",
        };
        acc.add(key, i, n, 1.0, 0.0);
    }
    Ok(Some(()))
}

fn collect_waiting_time(
    c: &mut impl Read,
    scope: &BucketScope<'_>,
    acc: &mut Acc,
) -> Result<Option<()>> {
    let &BucketScope {
        q,
        group,
        buckets,
        spans,
        n,
        tz,
        ..
    } = scope;

    let Some((lo, hi)) = bounds(&q.from, &q.to, tz) else {
        return Ok(None);
    };
    // each bucket's [start, end): the hours' own spans, or the local days'
    let edges: Vec<(i64, i64)> = match spans {
        Some(h) => h.iter().map(|x| (x.1, x.2)).collect(),
        None => buckets
            .iter()
            .map(|d| bounds(d, d, tz).unwrap_or((0, 0)))
            .collect(),
    };
    let now = steve_store::now();
    let rows = DecisionReads(c).waiting(report_scope(q, lo, hi))?;
    // per piece of work: its line and the spans a person was waited on
    let mut work: BTreeMap<String, (String, Vec<(i64, i64)>)> = BTreeMap::new();
    for (card, item, bot, project) in rows.into_iter().filter(|r| r.0.human()) {
        let key = match group {
            "bot" => bot,
            "project" => project,
            _ => "total".into(),
        };
        let w = work
            .entry(item.unwrap_or_else(|| format!("decision:{}", card.id)))
            .or_insert_with(|| (key, vec![]));
        w.1.push((card.created, card.resolved.unwrap_or(now).min(now)));
    }
    for (key, spans) in work.into_values() {
        for (i, (start, end)) in edges.iter().enumerate() {
            let ms = items::union_ms(spans.iter().copied(), *start, *end);
            if ms > 0 {
                acc.add(&key, i, n, ms as f64, 0.0);
            }
        }
    }
    Ok(Some(()))
}

fn collect_work_in_progress(
    c: &mut impl Read,
    scope: &BucketScope<'_>,
    acc: &mut Acc,
) -> Result<()> {
    let &BucketScope {
        q, spans, index, n, ..
    } = scope;

    let wip = match spans {
        Some(h) => metrics::wip_at(c, q, h.iter().map(|x| (x.0.clone(), x.2)).collect())?,
        None => metrics::wip_by_day(c, q)?,
    };
    for (d, v) in wip {
        if let Some(&i) = index.get(d.as_str()) {
            acc.add("total", i, n, v as f64, 0.0);
        }
    }
    Ok(())
}

fn collect_decision_counts(
    c: &mut impl Read,
    scope: &BucketScope<'_>,
    acc: &mut Acc,
) -> Result<Option<()>> {
    let &BucketScope {
        q, index, n, tz, ..
    } = scope;

    let Some((lo, hi)) = bounds(&q.from, &q.to, tz) else {
        return Ok(None);
    };
    let ts = DecisionReads(c).creation_times(report_scope(q, lo, hi))?;
    for t in ts {
        if let Some(&i) = index.get(scope.bucket_of(t).as_str()) {
            acc.add("total", i, n, 1.0, 0.0);
        }
    }
    Ok(Some(()))
}

fn lead_percentiles(c: &mut impl Read, scope: &BucketScope<'_>) -> Result<Vec<Line>> {
    let &BucketScope {
        q, index, n, tz, ..
    } = scope;

    let Some((lo, hi)) = bounds(&q.from, &q.to, tz) else {
        return Ok(vec![]);
    };
    let rows = ItemStats(c).completed_leads(report_scope(q, lo, hi))?;
    let mut per: Vec<Vec<i64>> = vec![vec![]; n];
    for (at, lead) in rows {
        if let Some(&i) = index.get(scope.bucket_of(at).as_str()) {
            per[i].push(lead);
        }
    }
    let line = |key: &str, label: &str, q: f64| Line {
        key: key.into(),
        label: label.into(),
        values: per
            .iter()
            .map(|v| percentile(v, q).map(|x| x as f64))
            .collect(),
    };
    Ok(vec![
        line("p50", &steve_i18n::t!("stats.label.median"), 0.5),
        line("p85", "p85", 0.85),
    ])
}

fn finish_series(
    acc: Acc,
    observed: Option<Observed>,
    rate: bool,
    order: &[&str],
    scope: &BucketScope<'_>,
    names: &Names,
    metric: Metric,
) -> Vec<Line> {
    let group = scope.group;
    let n = scope.n;

    let dim = if group == "none" { "total" } else { group };
    let mut series = acc.lines(rate, names, dim, order);
    if group == "none" {
        if series.is_empty() {
            series.push(Line {
                key: "total".into(),
                label: String::new(),
                values: vec![if rate { None } else { Some(0.0) }; n],
            });
        }
        for l in &mut series {
            l.label = series_total_label(metric);
        }
    }
    if let Some(seen) = observed {
        let retained: std::collections::HashSet<_> = series.iter().map(|l| l.key.clone()).collect();
        for line in &mut series {
            for (i, value) in line.values.iter_mut().enumerate() {
                let known = if line.key == "other" {
                    seen.iter().any(|(k, j)| *j == i && !retained.contains(k))
                } else {
                    seen.contains(&(line.key.clone(), i))
                };
                if !known {
                    *value = None;
                }
            }
        }
    }
    series
}

/* ── items ── */

/// A row of `GET /stats/items`.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ItemRow {
    pub coverage: crate::coverage::Coverage,
    pub item_id: String,
    pub no: i64,
    pub title: String,
    pub status: String,
    pub bot_id: Option<String>,
    pub project_id: Option<String>,
    pub created: i64,
    pub done_at: Option<i64>,
    pub lead_ms: Option<i64>,
    pub cycle_ms: Option<i64>,
    pub wait_ms: Option<i64>,
    pub agent_ms: Option<i64>,
    pub turns: i64,
    pub reworks: i64,
    pub first_pass: Option<bool>,
    pub tokens: i64,
    pub est: f64,
    /// Tokens on models with no price, not in `est`.
    pub unpriced_tokens: i64,
    pub reported: f64,
}

/// `sort` of `GET /stats/items`.
pub const ITEM_SORTS: &[&str] = &["est", "tokens", "lead", "wait", "reworks", "done_at"];
/// `only` of `GET /stats/items`: the items behind `throughput.done` / `throughput.created`, for the drill-down
/// lists of the stats page (the same marks and the same `created` window, so the counts agree).
pub const ITEM_ONLY: &[&str] = &["done", "created"];

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ItemPage {
    pub rows: Vec<ItemRow>,
    pub total: usize,
    /// Pass back as `cursor` for the next page; `None` on the last one.
    pub next: Option<String>,
}

/// Items done in the range or with activity in it (created, a turn, a mark), sorted descending by `sort`
/// (missing values last), `limit` from `cursor` (an offset). `only` (one of [`ITEM_ONLY`]) narrows them to
/// the items done (a status mark to `done`) or created in the range. `hour` (0–23, a one-day range) narrows
/// the range to that local hour, the span the hourly series counts in.
pub fn items(
    c: &mut impl Read,
    q: &Query,
    sort: &str,
    only: Option<&str>,
    hour: Option<u32>,
    limit: usize,
    cursor: usize,
) -> Result<ItemPage> {
    let tz = tz_of(c, &q.workspace_id)?;
    let Some((lo, hi)) = window(q, hour, tz) else {
        return Ok(ItemPage {
            rows: vec![],
            total: 0,
            next: None,
        });
    };
    let mut prices = metrics::book_of(c, &q.workspace_id, [])?;
    let selected = ItemStats(c).item_views(report_scope(q, lo, hi), only)?;
    let mut rows = Vec::with_capacity(selected.len());
    for row in selected {
        let u = item_usage(c, &row.id, &mut prices)?;
        rows.push(ItemRow {
            coverage: u.coverage,
            item_id: row.id,
            no: row.no,
            title: row.title,
            status: row.status,
            bot_id: row.bot,
            project_id: row.project,
            created: row.created,
            done_at: row.done,
            lead_ms: row.lead,
            cycle_ms: row.cycle,
            wait_ms: row.wait,
            agent_ms: row.agent,
            turns: row.turns,
            reworks: row.reworks,
            first_pass: row.first_pass,
            tokens: u.total,
            est: u.est,
            unpriced_tokens: u.unpriced_tokens,
            reported: u.reported,
        });
    }
    let key = |r: &ItemRow| -> Option<f64> {
        match sort {
            "tokens" => Some(r.tokens as f64),
            "lead" => r.lead_ms.map(|v| v as f64),
            "wait" => r.wait_ms.map(|v| v as f64),
            "reworks" => Some(r.reworks as f64),
            "done_at" => r.done_at.map(|v| v as f64),
            _ => Some(r.est),
        }
    };
    rows.sort_by(|a, b| match (key(a), key(b)) {
        (Some(x), Some(y)) => y.total_cmp(&x).then(b.created.cmp(&a.created)),
        (Some(_), None) => std::cmp::Ordering::Less,
        (None, Some(_)) => std::cmp::Ordering::Greater,
        (None, None) => b.created.cmp(&a.created),
    });
    let total = rows.len();
    let end = (cursor + limit).min(total);
    let page = if cursor < total {
        rows[cursor..end].to_vec()
    } else {
        vec![]
    };
    Ok(ItemPage {
        rows: page,
        total,
        next: (end < total).then(|| end.to_string()),
    })
}

/// How deep and how large `GET /stats/tree/{id}` reads below its root.
pub const TREE_DEPTH: i64 = 8;
pub const TREE_MAX: usize = 200;

/// One item of an item's tree: who started it (`parent_id`, from the child's `parent.item`) and what it
/// cost by itself. A parent's own row does not include its children; the client sums the subtree.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct TreeNode {
    pub coverage: crate::coverage::Coverage,
    pub item_id: String,
    pub parent_id: Option<String>,
    pub depth: i64,
    pub tokens: i64,
    pub est: f64,
    /// Tokens on models without a price: `est` leaves them out.
    pub unpriced_tokens: i64,
    pub reported: f64,
    pub agent_ms: Option<i64>,
    pub first_turn: Option<i64>,
    pub done_at: Option<i64>,
    pub dropped_at: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Tree {
    pub nodes: Vec<TreeNode>,
    /// More than [`TREE_MAX`] items, or deeper than [`TREE_DEPTH`]: the rest is left out.
    pub truncated: bool,
}

/// `root` and the items started from it, breadth first, read from `item_stats` (refreshed in the
/// background, so a running item may lag by a refresh). The workspace is returned for the caller's
/// access check; `None` if the root does not exist.
pub fn tree(c: &mut impl Read, root: &str) -> Result<Option<(String, Tree)>> {
    let Some((ws, _)) = ItemHistory(c).created(root)? else {
        return Ok(None);
    };
    let mut prices = metrics::book_of(c, &ws, [])?;
    let selected = ItemStats(c).tree(&ws, root, TREE_DEPTH, TREE_MAX as u64 + 1)?;
    let deeper = selected.iter().any(|row| row.depth > TREE_DEPTH);
    let mut nodes = Vec::new();
    for row in selected.into_iter().filter(|row| row.depth <= TREE_DEPTH) {
        let u = item_usage(c, &row.id, &mut prices)?;
        nodes.push(TreeNode {
            coverage: u.coverage,
            item_id: row.id,
            parent_id: row.parent,
            depth: row.depth,
            tokens: u.total,
            est: u.est,
            unpriced_tokens: u.unpriced_tokens,
            reported: u.reported,
            agent_ms: row.agent,
            first_turn: row.first_turn,
            done_at: row.done,
            dropped_at: row.dropped,
        });
    }
    let truncated = deeper || nodes.len() > TREE_MAX;
    nodes.truncate(TREE_MAX);
    Ok(Some((ws, Tree { nodes, truncated })))
}

/// The `[lo, hi)` of a query in Unix ms, or of one local hour of its (one) day.
pub fn window(q: &Query, hour: Option<u32>, tz: Option<chrono_tz::Tz>) -> Option<(i64, i64)> {
    match hour {
        None => bounds(&q.from, &q.to, tz),
        Some(h) if q.from == q.to => hours(&q.from, tz)?.get(h as usize).map(|x| (x.1, x.2)),
        Some(_) => None,
    }
}

/// Token figures with their estimate.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct Usage {
    pub coverage: crate::coverage::Coverage,
    pub input: i64,
    pub output: i64,
    pub cache_read: i64,
    pub cache_write: i64,
    pub total: i64,
    pub hit_rate: Option<f64>,
    pub est: f64,
    pub reported: f64,
    /// Tokens of models without a price.
    pub unpriced_tokens: i64,
    /// Priced through a route whose billing is unknown (`kiro/`).
    pub rough: bool,
}

impl Usage {
    /// Adds one model's tokens; the caller sets `hit_rate` once all are in.
    fn add(&mut self, model: &str, day: &str, t: &Tokens, reported: f64, prices: &PriceBook) {
        let n = |v: Option<i64>| v.unwrap_or(0);
        let total = n(t.input) + n(t.output) + n(t.cache_read) + n(t.cache_write);
        self.input += n(t.input);
        self.output += n(t.output);
        self.cache_read += n(t.cache_read);
        self.cache_write += n(t.cache_write);
        self.total += total;
        self.reported += reported;
        match prices.quote(model, day) {
            Some(p) => {
                self.est += p.price.cost(t);
                self.rough |= p.via.is_some();
            }
            None => self.unpriced_tokens += total,
        }
    }
}

/// A turn of the drill-down.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct TurnOut {
    pub id: String,
    pub session_id: String,
    pub started: i64,
    pub ended: Option<i64>,
    pub stop_reason: Option<String>,
    pub attempt: i64,
    pub transient: bool,
    pub tools: i64,
    pub tool_errors: i64,
    pub usage: Usage,
}

pub use steve_store::statistics::ItemMark as MarkOut;

/// A decision of the drill-down.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct DecisionOut {
    pub id: String,
    pub kind: String,
    pub status: String,
    pub created: i64,
    pub resolved: Option<i64>,
    /// `me`, `steve`, or `null` while open or expired.
    pub by: Option<String>,
    pub choice: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ModelOut {
    pub model: String,
    pub label: String,
    #[serde(flatten)]
    pub usage: Usage,
}

/// `GET /stats/items/:id`.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ItemDetail {
    pub item_id: String,
    pub no: i64,
    pub title: String,
    pub status: String,
    pub facts: ItemFacts,
    pub usage: Usage,
    /// Whole-life time until now (or until done / dropped).
    pub age_ms: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub turns: Option<Vec<TurnOut>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub marks: Option<Vec<MarkOut>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub decisions: Option<Vec<DecisionOut>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub by_model: Option<Vec<ModelOut>>,
}

/// Most turns in a drill-down.
pub const MAX_TURNS: i64 = 500;

fn item_tokens(c: &mut impl Read, item_id: &str) -> Result<Vec<(String, TokenGroup)>> {
    UsageReads(c).groups(Entity::Item(item_id), true)
}

fn item_usage(c: &mut impl Read, id: &str, prices: &mut PriceBook) -> Result<Usage> {
    let tokens = item_tokens(c, id)?;
    prices.load(c, tokens.iter().map(|(_, r)| r.0.as_str()))?;
    let mut u = Usage::default();
    let mut base = 0;
    for (_, (model, day, t, hb, rep)) in tokens {
        u.add(&model, &day, &t, rep, prices);
        base += hb;
    }
    u.hit_rate = (base > 0).then(|| u.cache_read as f64 / base as f64);
    u.coverage = crate::coverage::entity(c, "item", id)?;
    Ok(u)
}

/// The drill-down of one item, its facts computed now (not the possibly stale `item_stats` row);
/// `brief` leaves out the turns, marks, decisions and models. `None` if the item does not exist.
pub fn item_detail(
    c: &mut impl Read,
    item_id: &str,
    now: i64,
    brief: bool,
) -> Result<Option<(String, ItemDetail)>> {
    let Some(head) = ItemHistory(c).head(item_id)? else {
        return Ok(None);
    };
    let Some((_, input)) = items::load(c, item_id)? else {
        return Ok(None);
    };
    let facts = items::compute(&input, now);
    let tokens = item_tokens(c, item_id)?;
    let prices = metrics::book_of(c, &head.ws, tokens.iter().map(|(_, r)| r.0.as_str()))?;
    let steve_store::statistics::ItemHead {
        ws,
        no,
        title,
        status,
    } = head;
    let mut usage = Usage::default();
    let mut base = 0;
    let mut per_model: BTreeMap<String, (Usage, i64)> = BTreeMap::new();
    let mut per_turn: HashMap<String, (Usage, i64)> = HashMap::new();
    for (turn, (model, day, t, hb, rep)) in &tokens {
        usage.add(model, day, t, *rep, &prices);
        base += hb;
        let m = per_model.entry(model.clone()).or_default();
        m.0.add(model, day, t, *rep, &prices);
        m.1 += hb;
        if !turn.is_empty() {
            let x = per_turn.entry(turn.clone()).or_default();
            x.0.add(model, day, t, *rep, &prices);
            x.1 += hb;
        }
    }
    let rate = |u: &mut Usage, b: i64| u.hit_rate = (b > 0).then(|| u.cache_read as f64 / b as f64);
    rate(&mut usage, base);
    usage.coverage = crate::coverage::entity(c, "item", item_id)?;
    let end = facts.done_at.or(facts.dropped_at).unwrap_or(now);
    let mut d = ItemDetail {
        item_id: item_id.into(),
        no,
        title,
        status,
        age_ms: (end - facts.created).max(0),
        facts,
        usage,
        turns: None,
        marks: None,
        decisions: None,
        by_model: None,
    };
    if brief {
        return Ok(Some((ws, d)));
    }
    populate_item_details(c, item_id, &mut d, per_model, per_turn, rate)?;
    Ok(Some((ws, d)))
}

fn populate_item_details(
    c: &mut impl Read,
    item_id: &str,
    d: &mut ItemDetail,
    per_model: BTreeMap<String, (Usage, i64)>,
    mut per_turn: HashMap<String, (Usage, i64)>,
    rate: impl Fn(&mut Usage, i64),
) -> Result<()> {
    let turns: Vec<TurnOut> = TurnReads(c)
        .details(item_id, MAX_TURNS as u64)?
        .into_iter()
        .map(|r| TurnOut {
            id: r.id,
            session_id: r.session,
            started: r.started,
            ended: r.ended,
            stop_reason: r.stop_reason,
            attempt: r.attempt,
            transient: r.transient,
            tools: r.tools,
            tool_errors: r.tool_errors,
            usage: Usage::default(),
        })
        .map(|mut t| -> Result<_> {
            if let Some((mut u, b)) = per_turn.remove(&t.id) {
                rate(&mut u, b);
                t.usage = u;
            }
            t.usage.coverage = crate::coverage::entity(c, "turn", &t.id)?;
            Ok(t)
        })
        .collect::<Result<_>>()?;
    let marks = MarkReads(c).for_item(item_id)?;
    let decisions = items::decisions_of(c, item_id)?
        .into_iter()
        .map(|x| DecisionOut {
            by: x.settled_by().map(|s| s.as_str().to_string()),
            choice: x.choice().map(str::to_string),
            id: x.id,
            kind: x.kind,
            status: x.status,
            created: x.created,
            resolved: x.resolved,
        })
        .collect();
    let mut by_model: Vec<ModelOut> = per_model
        .into_iter()
        .map(|(model, (mut u, b))| {
            rate(&mut u, b);
            ModelOut {
                label: if model.is_empty() {
                    steve_i18n::t!("stats.label.unknown_model")
                } else {
                    model.clone()
                },
                model,
                usage: u,
            }
        })
        .collect();
    for m in &mut by_model {
        m.usage.coverage = crate::coverage::model(c, item_id, &m.model)?;
    }
    by_model.sort_by(|a, b| {
        b.usage
            .est
            .total_cmp(&a.usage.est)
            .then(b.usage.total.cmp(&a.usage.total))
    });
    d.turns = Some(turns);
    d.marks = Some(marks);
    d.decisions = Some(decisions);
    d.by_model = Some(by_model);
    Ok(())
}

/// `GET /stats/sessions/:id`: a session's turns and tokens (the session page's top bar).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct SessionTotals {
    pub session_id: String,
    pub turns: i64,
    pub usage: Usage,
}

/// `None` if the session does not exist; also returns its workspace.
pub fn session(c: &mut impl Read, session_id: &str) -> Result<Option<(String, SessionTotals)>> {
    let Some(ws) = SessionReads(c).workspace(session_id)? else {
        return Ok(None);
    };
    let rows = UsageReads(c).groups(Entity::Session(session_id), false)?;
    let prices = metrics::book_of(c, &ws, rows.iter().map(|(_, r)| r.0.as_str()))?;
    let mut usage = Usage::default();
    let mut base = 0;
    for (_, (model, day, t, hb, rep)) in &rows {
        usage.add(model, day, t, *rep, &prices);
        base += hb;
    }
    usage.hit_rate = (base > 0).then(|| usage.cache_read as f64 / base as f64);
    usage.coverage = crate::coverage::entity(c, "session", session_id)?;
    let turns = TurnReads(c).session_count(session_id)?;
    Ok(Some((
        ws,
        SessionTotals {
            session_id: session_id.into(),
            turns,
            usage,
        },
    )))
}

/// Batched cumulative `(tokens, estimated_or_reported_cost_usd)` for a set of session IDs in `ws`.
pub fn session_summaries(
    c: &mut impl Read,
    ws: &str,
    ids: &[String],
) -> Result<HashMap<String, (i64, Option<f64>)>> {
    if ids.is_empty() {
        return Ok(HashMap::new());
    }
    let rows = UsageReads(c).for_sessions(ws, ids)?;
    if rows.is_empty() {
        return Ok(HashMap::new());
    }
    let prices = metrics::book_of(c, ws, rows.iter().map(|(_, r)| r.0.as_str()))?;
    let mut by_session: HashMap<String, (i64, f64, f64, bool)> = HashMap::new();
    for (sid, (model, day, t, _hb, rep)) in &rows {
        let total = t.input.unwrap_or(0)
            + t.output.unwrap_or(0)
            + t.cache_read.unwrap_or(0)
            + t.cache_write.unwrap_or(0);
        let entry = by_session.entry(sid.clone()).or_default();
        entry.0 += total;
        entry.2 += *rep;
        if let Some(usd) = prices.cost(model, day, t) {
            entry.1 += usd;
            entry.3 = true;
        }
    }
    Ok(by_session
        .into_iter()
        .map(|(sid, (tokens, est, rep, priced))| {
            let cost = if priced {
                Some(est)
            } else if rep > 0.0 {
                Some(rep)
            } else {
                None
            };
            (sid, (tokens, cost))
        })
        .collect())
}

/* ── the breakdowns that are not token rows ── */

/// `by=tool|decision_kind|fail_reason` rows, from a summary of the same query.
pub fn count_rows(s: &Summary, by: &str) -> Vec<Value> {
    let mut rows: Vec<Value> = match by {
        // ACP kinds and `mcp:<tool>` make up the total; `tool:<name>` is the same calls by tool name
        "tool" => s
            .agents
            .tool_kinds
            .iter()
            .map(|(k, n)| {
                let (dim, name) = match k.split_once(':') {
                    Some(("tool", n)) => ("tool", n),
                    Some(("mcp", n)) => ("mcp", n),
                    _ => ("kind", k.as_str()),
                };
                json!({"key": k, "label": name, "dim": dim, "count": n})
            })
            .collect(),
        "decision_kind" => {
            let mut per: BTreeMap<&str, i64> = BTreeMap::new();
            for (k, n) in &s.human.by_kind {
                *per.entry(k.split('/').next().unwrap_or(k)).or_default() += n;
            }
            per.into_iter()
                .map(|(k, n)| {
                    let resp = s.human.response_by_kind.get(k).cloned().unwrap_or_else(|| P::of(&[]));
                    json!({"key": k, "label": fixed_label(k).unwrap_or_else(|| k.to_string()), "count": n, "response": resp,
                           "subjects": s.human.by_kind.iter().filter(|(x, _)| x.split('/').next() == Some(k))
                               .map(|(x, n)| (x.split_once('/').map_or("", |p| p.1).to_string(), *n)).collect::<BTreeMap<_, _>>()})
                })
                .collect()
        }
        "fail_reason" => s
            .quality
            .fails
            .iter()
            .map(|(k, n)| json!({"key": k, "label": fixed_label(k).unwrap_or_else(|| k.to_string()), "count": n}))
            .collect(),
        _ => vec![],
    };
    rows.sort_by(|a, b| b["count"].as_i64().cmp(&a["count"].as_i64()));
    rows
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn labels_in_english() {
        let en = |f: fn() -> String| steve_i18n::scope(steve_i18n::EN, async move { f() });
        assert_eq!(en(|| fixed_label("chat").unwrap()).await, "Chat answers");
        assert_eq!(fixed_label("chat").as_deref(), Some("对话回答"));
        assert_eq!(en(|| Names::default().label("bot", "")).await, "No bot");
        assert_eq!(Names::default().label("bot", ""), "无 Bot");
        assert_eq!(fixed_label("nope"), None);
    }

    #[test]
    fn ranges() {
        let t = "2026-11-01";
        assert_eq!(range("today", None, None, t), Ok((t.into(), t.into())));
        assert_eq!(
            range("7d", None, None, t),
            Ok(("2026-10-26".into(), t.into()))
        );
        assert_eq!(
            range("30d", None, None, t),
            Ok(("2026-10-03".into(), t.into()))
        );
        assert_eq!(range("custom", None, Some(t), t), Err("from"));
        assert_eq!(range("custom", Some("2026-11-01"), None, t), Err("to"));
        assert_eq!(
            range("custom", Some("2025-11-01"), Some("2026-11-01"), t),
            Ok(("2025-11-01".into(), t.into()))
        );
        // 367 days
        assert_eq!(
            range("custom", Some("2025-10-31"), Some("2026-11-01"), t),
            Err("from")
        );
        assert_eq!(
            range("custom", Some("2026-11-02"), Some("2026-11-01"), t),
            Err("from")
        );
        assert_eq!(range("90d", None, None, t), Err("range"));
    }

    #[test]
    fn prev_is_as_long_and_right_before() {
        assert_eq!(
            prev("2026-10-26", "2026-11-01"),
            Some(("2026-10-19".into(), "2026-10-25".into()))
        );
        assert_eq!(
            prev("2026-11-01", "2026-11-01"),
            Some(("2026-10-31".into(), "2026-10-31".into()))
        );
        for (f, l) in [("2026-10-03", "2026-11-01"), ("2026-02-20", "2026-03-05")] {
            let (pf, pl) = prev(f, l).unwrap_or_default();
            assert_eq!(days(&pf, &pl).len(), days(f, l).len());
        }
        assert_eq!(
            days("2026-10-30", "2026-11-02"),
            vec!["2026-10-30", "2026-10-31", "2026-11-01", "2026-11-02"]
        );
    }

    #[test]
    fn folding_keeps_six_lines() {
        let mut a = Acc::default();
        for (i, k) in ["a", "b", "c", "d", "e", "f", "g", "h"].iter().enumerate() {
            a.add(k, 0, 2, (10 - i) as f64, 0.0);
            a.add(k, 1, 2, 1.0, 0.0);
        }
        let lines = a.lines(false, &Names::default(), "model", &[]);
        assert_eq!(lines.len(), MAX_LINES);
        assert_eq!(lines[0].key, "a");
        let other = &lines[5];
        assert_eq!(
            (other.key.as_str(), other.label.as_str()),
            ("other", "其他")
        );
        // f + g + h = 5 + 4 + 3 on day 0, 3 × 1 on day 1
        assert_eq!(other.values, vec![Some(12.0), Some(3.0)]);
    }
}
