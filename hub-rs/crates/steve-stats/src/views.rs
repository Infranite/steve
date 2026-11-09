//! What the statistics page reads besides the summary: ranges, the trend series, the
//! per-item table and drill-down, a session's totals, the non-token breakdowns, and the labels of keys.
//! Everything reads the cube and `item_stats` through their indexes; per-item reads go by `item_id`.

use std::collections::{BTreeMap, HashMap};

use chrono::{Days, NaiveDate};
use rusqlite::{Connection, OptionalExtension, params};
use serde::Serialize;
use serde_json::{Value, json};
use steve_store::{Result, Tokens};

use crate::day::{day, hour, hours};
use crate::items::{self, ItemFacts};
use crate::metrics::{self, Daily, P, Query, Summary, bounds, item_est, percentile};
use crate::prices::PriceBook;
use crate::refresh::tz_of;

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
pub fn today(c: &Connection, ws: &str, now: i64) -> Result<String> {
    Ok(day(now, tz_of(c, ws)?))
}

/* ── labels ── */

/// Names of the fixed keys (sources, purposes, harnesses, fail reasons, decision kinds), in the
/// request's language.
pub fn fixed_label(key: &str) -> Option<String> {
    Some(match key {
        "agent" => steve_i18n::t!("stats.label.agent"),
        "gateway" => steve_i18n::t!("stats.label.gateway"),
        "backfill" => steve_i18n::t!("stats.label.backfill"),
        "turn" => steve_i18n::t!("stats.label.turn"),
        "autonomous" => steve_i18n::t!("stats.label.autonomous"),
        "route" => steve_i18n::t!("stats.label.route"),
        "chat" => steve_i18n::t!("stats.label.chat"),
        "research" => steve_i18n::t!("stats.label.research"),
        "title" => steve_i18n::t!("stats.label.title"),
        "draft" => steve_i18n::t!("stats.label.draft"),
        "extract" => steve_i18n::t!("stats.label.extract"),
        "compose" => steve_i18n::t!("stats.label.compose"),
        "mail" => steve_i18n::t!("stats.label.mail"),
        "disclosure" => steve_i18n::t!("stats.label.disclosure"),
        "intent" => steve_i18n::t!("stats.label.intent"),
        "flow" => steve_i18n::t!("stats.label.flow"),
        "yolo" => steve_i18n::t!("stats.label.yolo"),
        "digest" => steve_i18n::t!("stats.label.digest"),
        "other" => steve_i18n::t!("stats.label.other"),
        "claude-code" => steve_i18n::t!("stats.label.claude_code"),
        "codex" => steve_i18n::t!("stats.label.codex"),
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

/// Bot and project names of a workspace, for labelling keys.
#[derive(Debug, Default)]
pub struct Names {
    bots: HashMap<String, String>,
    projects: HashMap<String, String>,
}

impl Names {
    pub fn load(c: &Connection, ws: &str) -> Result<Names> {
        let pairs = |sql: &str| -> Result<HashMap<String, String>> {
            Ok(c.prepare_cached(sql)?
                .query_map([ws], |r| Ok((r.get(0)?, r.get(1)?)))?
                .collect::<rusqlite::Result<_>>()?)
        };
        Ok(Names {
            // bots are global (a bot joins many workspaces); a deleted one keeps its name
            bots: pairs("SELECT id, name FROM bots WHERE ?1 IS NOT NULL")?,
            projects: pairs("SELECT id, name FROM projects WHERE workspace_id = ?")?,
        })
    }

    /// The label of `key` in dimension `dim` (`model`, `bot`, `project`, `source`, `purpose`, …).
    pub fn label(&self, dim: &str, key: &str) -> String {
        match (dim, key) {
            ("bot", "") => steve_i18n::t!("stats.label.no_bot"),
            ("project", "") => steve_i18n::t!("stats.label.no_project"),
            ("model", "") => steve_i18n::t!("stats.label.unknown_model"),
            ("bot", k) => self.bots.get(k).cloned().unwrap_or_else(|| k.into()),
            ("project", k) => self.projects.get(k).cloned().unwrap_or_else(|| k.into()),
            ("model", k) | ("day", k) => k.into(),
            (_, k) => fixed_label(k).unwrap_or_else(|| k.into()),
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
    Decisions,
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
            "decisions" => Metric::Decisions,
            _ => return None,
        })
    }

    /// Whether `group` (`none|model|bot|project|source|purpose|token_type`) applies to this metric.
    pub fn allows(&self, group: &str) -> bool {
        match self {
            Metric::Tokens | Metric::Cost => true,
            Metric::HitRate | Metric::Turns => group != "token_type",
            Metric::Done | Metric::Created => matches!(group, "none" | "bot" | "project"),
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
        "bot" => &d.bot_id,
        "project" => &d.project_id,
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
    c: &Connection,
    q: &Query,
    metric: Metric,
    group: &str,
    bucket: Bucket,
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
    let mut series = lines(c, q, metric, group, &buckets, spans.as_deref(), tz)?;
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
        bucket: if spans.is_some() { "hour" } else { "day" },
        buckets,
        series,
        spans: spans.map(|h| h.iter().map(|x| [x.1, x.2]).collect()),
        current,
    })
}

/// The lines of [`series`] over `buckets` (the days, or the hours with their `spans`).
fn lines(
    c: &Connection,
    q: &Query,
    metric: Metric,
    group: &str,
    buckets: &[String],
    spans: Option<&[(String, i64, i64)]>,
    tz: Option<chrono_tz::Tz>,
) -> Result<Vec<Line>> {
    let n = buckets.len();
    let index: HashMap<&str, usize> = buckets
        .iter()
        .enumerate()
        .map(|(i, d)| (d.as_str(), i))
        .collect();
    let bucket_of = |ts: i64| {
        if spans.is_some() {
            hour(ts, tz)
        } else {
            day(ts, tz)
        }
    };
    let names = Names::load(c, &q.workspace_id)?;
    let mut acc = Acc::default();
    let total_label = |m: Metric| match m {
        Metric::Tokens => steve_i18n::t!("stats.total.tokens"),
        Metric::Cost => steve_i18n::t!("stats.total.cost"),
        Metric::Done => steve_i18n::t!("stats.total.done"),
        Metric::Created => steve_i18n::t!("stats.total.created"),
        Metric::Wip => steve_i18n::t!("stats.total.wip"),
        Metric::Lead => steve_i18n::t!("stats.total.lead"),
        Metric::HitRate => steve_i18n::t!("stats.total.hit_rate"),
        Metric::Turns => steve_i18n::t!("stats.total.turns"),
        Metric::Decisions => steve_i18n::t!("stats.total.decisions"),
    };
    let mut rate = false;
    let mut order: &[&str] = &[];
    match metric {
        Metric::Tokens | Metric::Cost | Metric::HitRate | Metric::Turns => {
            let prices = metrics::book_of(c, &q.workspace_id)?;
            let rows = if spans.is_some() {
                metrics::hourly(c, q)?
            } else {
                metrics::daily(c, q)?
            };
            for d in rows {
                let Some(&i) = index.get(d.day.as_str()) else {
                    continue;
                };
                let key = group_key(&d, group);
                match metric {
                    Metric::Tokens if group == "token_type" => {
                        for (t, v) in TOKEN_TYPES.iter().zip(token_parts(&d)) {
                            acc.add(t, i, n, v as f64, 0.0);
                        }
                    }
                    Metric::Tokens => acc.add(key, i, n, d.total() as f64, 0.0),
                    Metric::Cost => {
                        let Some(p) = prices.quote(&d.model, &d.day) else {
                            continue;
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
                    _ => acc.add(key, i, n, d.turns as f64, 0.0),
                }
            }
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
            let Some((lo, hi)) = bounds(&q.from, &q.to, tz) else {
                return Ok(vec![]);
            };
            let sql = if metric == Metric::Done {
                "SELECT DISTINCT m.item_id, m.ts, COALESCE(i.bot_id, ''), COALESCE(i.project_id, '') FROM item_marks m JOIN items i ON i.id = m.item_id
                 WHERE m.workspace_id = ?1 AND m.kind = 'status' AND m.to_v = 'done' AND m.ts >= ?2 AND m.ts < ?3
                   AND (?4 IS NULL OR i.project_id = ?4) AND (?5 IS NULL OR i.bot_id = ?5) ORDER BY m.ts"
            } else {
                "SELECT i.id, i.created, COALESCE(i.bot_id, ''), COALESCE(i.project_id, '') FROM items i
                 WHERE i.workspace_id = ?1 AND i.kind = 'task' AND i.created >= ?2 AND i.created < ?3
                   AND (?4 IS NULL OR i.project_id = ?4) AND (?5 IS NULL OR i.bot_id = ?5)"
            };
            let rows: Vec<(String, i64, String, String)> = c
                .prepare_cached(sql)?
                .query_map(params![q.workspace_id, lo, hi, q.project, q.bot], |r| {
                    Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
                })?
                .collect::<rusqlite::Result<_>>()?;
            // done: an item counts once per day (per hour)
            let mut seen = std::collections::HashSet::new();
            for (id, ts, bot, project) in rows {
                let dd = bucket_of(ts);
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
        }
        Metric::Wip => {
            let wip = match spans {
                Some(h) => metrics::wip_at(c, q, h.iter().map(|x| (x.0.clone(), x.2)).collect())?,
                None => metrics::wip_by_day(c, q)?,
            };
            for (d, v) in wip {
                if let Some(&i) = index.get(d.as_str()) {
                    acc.add("total", i, n, v as f64, 0.0);
                }
            }
        }
        Metric::Decisions => {
            let Some((lo, hi)) = bounds(&q.from, &q.to, tz) else {
                return Ok(vec![]);
            };
            let filtered = q.project.is_some() || q.bot.is_some();
            let sql = if filtered {
                "SELECT d.created FROM decisions d JOIN items i ON i.id = d.item_id WHERE d.workspace_id = ?1 AND d.created >= ?2 AND d.created < ?3
                   AND (?4 IS NULL OR i.project_id = ?4) AND (?5 IS NULL OR i.bot_id = ?5)"
            } else {
                "SELECT d.created FROM decisions d WHERE d.workspace_id = ?1 AND d.created >= ?2 AND d.created < ?3 AND ?4 IS NULL AND ?5 IS NULL"
            };
            let ts: Vec<i64> = c
                .prepare_cached(sql)?
                .query_map(params![q.workspace_id, lo, hi, q.project, q.bot], |r| {
                    r.get(0)
                })?
                .collect::<rusqlite::Result<_>>()?;
            for t in ts {
                if let Some(&i) = index.get(bucket_of(t).as_str()) {
                    acc.add("total", i, n, 1.0, 0.0);
                }
            }
        }
        Metric::Lead => {
            let Some((lo, hi)) = bounds(&q.from, &q.to, tz) else {
                return Ok(vec![]);
            };
            let rows: Vec<(i64, i64)> = c
                .prepare_cached(
                    "SELECT s.done_at, s.lead_ms FROM item_stats s JOIN items i ON i.id = s.item_id
                     WHERE s.workspace_id = ?1 AND s.done_at >= ?2 AND s.done_at < ?3 AND s.lead_ms IS NOT NULL
                       AND (?4 IS NULL OR i.project_id = ?4) AND (?5 IS NULL OR i.bot_id = ?5)",
                )?
                .query_map(params![q.workspace_id, lo, hi, q.project, q.bot], |r| Ok((r.get(0)?, r.get(1)?)))?
                .collect::<rusqlite::Result<_>>()?;
            let mut per: Vec<Vec<i64>> = vec![vec![]; n];
            for (at, lead) in rows {
                if let Some(&i) = index.get(bucket_of(at).as_str()) {
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
            return Ok(vec![
                line("p50", &steve_i18n::t!("stats.label.median"), 0.5),
                line("p85", "p85", 0.85),
            ]);
        }
    }
    let dim = if group == "none" { "total" } else { group };
    let mut series = acc.lines(rate, &names, dim, order);
    if group == "none" {
        if series.is_empty() {
            series.push(Line {
                key: "total".into(),
                label: String::new(),
                values: vec![if rate { None } else { Some(0.0) }; n],
            });
        }
        for l in &mut series {
            l.label = total_label(metric);
        }
    }
    Ok(series)
}

/* ── items ── */

/// A row of `GET /stats/items`.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ItemRow {
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
    c: &Connection,
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
    let prices = metrics::book_of(c, &q.workspace_id)?;
    let mut rows: Vec<ItemRow> = c
        .prepare_cached(
            "SELECT i.id, i.no, i.title, i.status, i.bot_id, i.project_id, i.created, s.done_at, s.lead_ms, s.cycle_ms, s.wait_ms,
                    s.agent_ms, s.turns, s.reworks, s.first_pass, s.input + s.output + s.cache_read + s.cache_write, s.models, s.cost_reported
             FROM item_stats s JOIN items i ON i.id = s.item_id
             WHERE s.workspace_id = ?1 AND i.kind = 'task' AND (?4 IS NULL OR i.project_id = ?4) AND (?5 IS NULL OR i.bot_id = ?5)
               AND ((s.done_at >= ?2 AND s.done_at < ?3) OR (i.created >= ?2 AND i.created < ?3)
                 OR EXISTS (SELECT 1 FROM agent_turns t WHERE t.item_id = i.id AND t.started >= ?2 AND t.started < ?3)
                 OR EXISTS (SELECT 1 FROM item_marks m WHERE m.item_id = i.id AND m.ts >= ?2 AND m.ts < ?3))
               AND (?6 IS NULL
                 OR (?6 = 'done' AND EXISTS (SELECT 1 FROM item_marks m WHERE m.item_id = i.id AND m.kind = 'status' AND m.to_v = 'done' AND m.ts >= ?2 AND m.ts < ?3))
                 OR (?6 = 'created' AND i.created >= ?2 AND i.created < ?3))",
        )?
        .query_map(params![q.workspace_id, lo, hi, q.project, q.bot, only], |r| {
            let models: BTreeMap<String, [i64; 4]> =
                serde_json::from_str(&r.get::<_, String>(16)?).unwrap_or_default();
            let done_at: Option<i64> = r.get(7)?;
            let as_of = done_at.map_or_else(|| prices.today().to_string(), |t| day(t, tz));
            Ok(ItemRow {
                item_id: r.get(0)?,
                no: r.get(1)?,
                title: r.get(2)?,
                status: r.get(3)?,
                bot_id: r.get(4)?,
                project_id: r.get(5)?,
                created: r.get(6)?,
                done_at: r.get(7)?,
                lead_ms: r.get(8)?,
                cycle_ms: r.get(9)?,
                wait_ms: r.get(10)?,
                agent_ms: r.get(11)?,
                turns: r.get(12)?,
                reworks: r.get(13)?,
                first_pass: r.get(14)?,
                tokens: r.get(15)?,
                est: item_est(&models, &prices, &as_of),
                reported: r.get(17)?,
            })
        })?
        .collect::<rusqlite::Result<_>>()?;
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

/// A mark of the drill-down.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct MarkOut {
    pub ts: i64,
    pub kind: String,
    pub from_v: Option<String>,
    pub to_v: Option<String>,
    pub actor: String,
    pub r#ref: Option<String>,
    pub backfill: bool,
}

/// A decision of the drill-down.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct DecisionOut {
    pub id: String,
    pub kind: String,
    pub status: String,
    pub created: i64,
    pub resolved: Option<i64>,
    /// `me`, `yolo`, or `null` while open or expired.
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
    /// Only token records from the backfill (session-level adapter dollars, a lower bound).
    pub backfill_only: bool,
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

type TokenRow = (String, String, Tokens, i64, f64, String);

/// The item's token records summed per (turn, model): `(turn or "", (model, tokens, hit base, reported, source))`.
fn item_tokens(c: &Connection, item_id: &str) -> Result<Vec<(String, TokenRow)>> {
    Ok(c.prepare_cached(
        "SELECT COALESCE(turn_id, ''), model, day, SUM(input), SUM(output), SUM(cache_read), SUM(cache_write),
                COALESCE(SUM(CASE WHEN input IS NOT NULL AND cache_read IS NOT NULL THEN input + cache_read + COALESCE(cache_write, 0) END), 0),
                COALESCE(SUM(cost_reported), 0), MIN(source)
         FROM usage_records WHERE item_id = ? GROUP BY turn_id, model, day",
    )?
    .query_map([item_id], |r| {
        Ok((
            r.get(0)?,
            (
                r.get(1)?,
                r.get(2)?,
                Tokens {
                    input: r.get(3)?,
                    output: r.get(4)?,
                    cache_read: r.get(5)?,
                    cache_write: r.get(6)?,
                    reasoning: None,
                },
                r.get(7)?,
                r.get(8)?,
                r.get(9)?,
            ),
        ))
    })?
    .collect::<rusqlite::Result<_>>()?)
}

/// The drill-down of one item, its facts computed now (not the possibly stale `item_stats` row);
/// `brief` leaves out the turns, marks, decisions and models. `None` if the item does not exist.
pub fn item_detail(
    c: &Connection,
    item_id: &str,
    now: i64,
    brief: bool,
) -> Result<Option<(String, ItemDetail)>> {
    let head: Option<(String, i64, String, String)> = c
        .prepare_cached("SELECT workspace_id, no, title, status FROM items WHERE id = ?")?
        .query_row([item_id], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
        })
        .optional()?;
    let Some((ws, no, title, status)) = head else {
        return Ok(None);
    };
    let Some((_, input)) = items::load(c, item_id)? else {
        return Ok(None);
    };
    let facts = items::compute(&input, now);
    let prices = metrics::book_of(c, &ws)?;
    let tokens = item_tokens(c, item_id)?;
    let mut usage = Usage::default();
    let mut base = 0;
    let mut per_model: BTreeMap<String, (Usage, i64)> = BTreeMap::new();
    let mut per_turn: HashMap<String, (Usage, i64)> = HashMap::new();
    let mut backfill_only = !tokens.is_empty();
    for (turn, (model, day, t, hb, rep, source)) in &tokens {
        backfill_only &= source == "backfill";
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
    let end = facts.done_at.or(facts.dropped_at).unwrap_or(now);
    let mut d = ItemDetail {
        item_id: item_id.into(),
        no,
        title,
        status,
        age_ms: (end - facts.created).max(0),
        facts,
        usage,
        backfill_only,
        turns: None,
        marks: None,
        decisions: None,
        by_model: None,
    };
    if brief {
        return Ok(Some((ws, d)));
    }
    let turns: Vec<TurnOut> = c
        .prepare_cached(
            "SELECT id, session_id, started, ended, stop_reason, attempt, transient, tools, tool_errors FROM agent_turns
             WHERE item_id = ? ORDER BY started LIMIT ?",
        )?
        .query_map(params![item_id, MAX_TURNS], |r| {
            Ok(TurnOut {
                id: r.get(0)?,
                session_id: r.get(1)?,
                started: r.get(2)?,
                ended: r.get(3)?,
                stop_reason: r.get(4)?,
                attempt: r.get(5)?,
                transient: r.get(6)?,
                tools: r.get(7)?,
                tool_errors: r.get(8)?,
                usage: Usage::default(),
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?
        .into_iter()
        .map(|mut t| {
            if let Some((mut u, b)) = per_turn.remove(&t.id) {
                rate(&mut u, b);
                t.usage = u;
            }
            t
        })
        .collect();
    let marks: Vec<MarkOut> = c
        .prepare_cached(
            "SELECT ts, kind, from_v, to_v, actor, ref, backfill FROM item_marks WHERE item_id = ? ORDER BY ts, id",
        )?
        .query_map([item_id], |r| {
            Ok(MarkOut {
                ts: r.get(0)?,
                kind: r.get(1)?,
                from_v: r.get(2)?,
                to_v: r.get(3)?,
                actor: r.get(4)?,
                r#ref: r.get(5)?,
                backfill: r.get(6)?,
            })
        })?
        .collect::<rusqlite::Result<_>>()?;
    let decisions = items::decisions_of(c, item_id)?
        .into_iter()
        .map(|x| DecisionOut {
            by: (x.status == "resolved")
                .then(|| if x.by_yolo() { "yolo" } else { "me" }.to_string()),
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
    Ok(Some((ws, d)))
}

/// `GET /stats/sessions/:id`: a session's turns and tokens (the session page's top bar).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct SessionTotals {
    pub session_id: String,
    pub turns: i64,
    pub usage: Usage,
}

/// `None` if the session does not exist; also returns its workspace.
pub fn session(c: &Connection, session_id: &str) -> Result<Option<(String, SessionTotals)>> {
    let ws: Option<String> = c
        .prepare_cached("SELECT workspace_id FROM sessions WHERE id = ?")?
        .query_row([session_id], |r| r.get(0))
        .optional()?;
    let Some(ws) = ws else { return Ok(None) };
    let prices = metrics::book_of(c, &ws)?;
    let rows: Vec<TokenRow> = c
        .prepare_cached(
            "SELECT model, day, SUM(input), SUM(output), SUM(cache_read), SUM(cache_write),
                    COALESCE(SUM(CASE WHEN input IS NOT NULL AND cache_read IS NOT NULL THEN input + cache_read + COALESCE(cache_write, 0) END), 0),
                    COALESCE(SUM(cost_reported), 0), MIN(source)
             FROM usage_records WHERE session_id = ? GROUP BY model, day",
        )?
        .query_map([session_id], |r| {
            Ok((
                r.get(0)?,
                r.get(1)?,
                Tokens {
                    input: r.get(2)?,
                    output: r.get(3)?,
                    cache_read: r.get(4)?,
                    cache_write: r.get(5)?,
                    reasoning: None,
                },
                r.get(6)?,
                r.get(7)?,
                r.get(8)?,
            ))
        })?
        .collect::<rusqlite::Result<_>>()?;
    let mut usage = Usage::default();
    let mut base = 0;
    for (model, day, t, hb, rep, _) in &rows {
        usage.add(model, day, t, *rep, &prices);
        base += hb;
    }
    usage.hit_rate = (base > 0).then(|| usage.cache_read as f64 / base as f64);
    let turns: i64 = c.query_row(
        "SELECT COUNT(*) FROM agent_turns WHERE session_id = ? AND attempt = 0",
        [session_id],
        |r| r.get(0),
    )?;
    Ok(Some((
        ws,
        SessionTotals {
            session_id: session_id.into(),
            turns,
            usage,
        },
    )))
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
