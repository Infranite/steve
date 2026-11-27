//! The Hub's health at a glance (`GET /api/system/health`): figures over `15m`, `1h`, or `24h`,
//! each with its limit, computed from retained snapshots of the Prometheus exposition. A figure
//! past its limit is also a warning; the client words it. A figure without enough samples in the
//! window to mean anything has no value and is never a warning.
use std::collections::{BTreeMap, BTreeSet, HashMap, VecDeque};
use std::sync::Arc;

use serde::Serialize;

const GIB: f64 = (1u64 << 30) as f64;
/// Ratios need this many events in the window before they say anything.
const MIN_LOOKUPS: f64 = 100.0;
const MIN_CALLS: f64 = 5.0;

/// Supported query windows for `/api/system/health`.
pub(crate) const WINDOW_15M_SECS: f64 = 15.0 * 60.0;
pub(crate) const WINDOW_1H_SECS: f64 = 60.0 * 60.0;
pub(crate) const WINDOW_24H_SECS: f64 = 24.0 * 60.0 * 60.0;

/// Minute snapshots retained for `15m` and `1h` windows (60 intervals + baseline).
const RECENT_CAPACITY: usize = 61;
/// Coarse 5-minute snapshot stride retained for the `24h` window (288 intervals + baseline).
const DAY_STRIDE_SECS: f64 = 5.0 * 60.0;
const DAY_CAPACITY: usize = 289;
/// Tolerance around snapshot boundaries so slight ticker jitter keeps the full window.
const RECENT_GRACE_SECS: f64 = 30.0;
const DAY_GRACE_SECS: f64 = 150.0;
/// Use 1-minute `recent` points for `24h` until the Hub has more than an hour of `day` span.
const DAY_SERIES_MIN_SPAN_SECS: f64 = 60.0 * 60.0;
/// Append the latest `recent` snapshot after the last `day` snapshot once at least this far ahead.
const DAY_TAIL_GAP_SECS: f64 = 30.0;

/// Window selector for `GET /api/system/health?window=...`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize)]
pub(crate) enum HealthWindow {
    #[default]
    #[serde(rename = "15m")]
    M15,
    #[serde(rename = "1h")]
    H1,
    #[serde(rename = "24h")]
    H24,
}

impl HealthWindow {
    pub(crate) fn parse(raw: &str) -> Option<Self> {
        match raw {
            "15m" => Some(Self::M15),
            "1h" => Some(Self::H1),
            "24h" => Some(Self::H24),
            _ => None,
        }
    }

    pub(crate) const fn target_seconds(self) -> f64 {
        match self {
            Self::M15 => WINDOW_15M_SECS,
            Self::H1 => WINDOW_1H_SECS,
            Self::H24 => WINDOW_24H_SECS,
        }
    }
}

/// One interval point in the golden-signals time series.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub(crate) struct SeriesPoint {
    /// Unix epoch milliseconds at the end of this interval.
    pub at: i64,
    pub http_p95: Option<f64>,
    pub write_lock_wait_p95: Option<f64>,
    pub hub_cpu: Option<f64>,
    pub hub_memory: Option<f64>,
}

/// The recorder's compacted exposition at one moment.
pub(crate) struct Snapshot {
    /// Seconds since the recorder was installed.
    at: f64,
    /// Unix epoch milliseconds when this snapshot was captured.
    wall_ms: i64,
    series: Vec<Series>,
    /// By the series' text (name and labels as rendered).
    by_key: HashMap<String, f64>,
}

struct Series {
    key: String,
    name: String,
    labels: Vec<(String, String)>,
    value: f64,
}

impl Series {
    fn label(&self, k: &str) -> Option<&str> {
        self.labels
            .iter()
            .find(|(n, _)| n == k)
            .map(|(_, v)| v.as_str())
    }
}

enum Compaction {
    /// Hub-wide histogram aggregated by `le` across all other labels.
    HubHistogram,
    /// Hub store write-lock histogram (`lock="store"`), aggregated by `le`.
    StoreLockHistogram,
    /// Cache requests aggregated into `result="hit"` and `result="miss"` (dropping `result="off"`).
    CacheRequests,
    /// Hub-wide scalar counter or gauge summed across labels (if any).
    HubScalar,
    /// Entity-scoped counter or gauge kept with original labels so per-subject resets remain intact.
    EntityExact,
}

fn compaction_rule(name: &str) -> Option<Compaction> {
    match name {
        "steve_http_request_duration_seconds_bucket"
        | "steve_sse_delivery_seconds_bucket"
        | "steve_projection_delay_seconds_bucket"
        | "steve_node_ingest_seconds_bucket"
        | "steve_web_interaction_seconds_bucket" => Some(Compaction::HubHistogram),
        "steve_lock_wait_seconds_bucket" => Some(Compaction::StoreLockHistogram),
        "steve_cache_requests_total" => Some(Compaction::CacheRequests),
        "steve_projection_reconcile_diff_total"
        | "steve_node_ingest_events_sum"
        | "steve_process_cpu_seconds"
        | "steve_process_resident_bytes"
        | "steve_process_disk_written_bytes"
        | "steve_backup_destination_available_bytes"
        | "steve_backup_destination_required_bytes"
        | "steve_web_long_task_seconds_count" => Some(Compaction::HubScalar),
        "steve_job_panics_total"
        | "steve_job_up"
        | "steve_node_link_up"
        | "steve_node_journal_bytes"
        | "steve_node_commands_total"
        | "steve_node_link_disconnects_total"
        | "steve_node_event_backlog"
        | "steve_node_process_disk_written_bytes"
        | "steve_plugin_calls_total"
        | "steve_plugin_restarts_total"
        | "steve_plugin_inbound_backlog" => Some(Compaction::EntityExact),
        _ => None,
    }
}

impl Snapshot {
    /// Parse Prometheus exposition text and retain only the series needed by health assessment.
    /// Hub-wide histograms and counters are pre-aggregated so retaining 350 snapshots stays compact.
    pub(crate) fn parse(text: &str, at: f64, wall_ms: i64) -> Snapshot {
        let mut out: Vec<Series> = Vec::new();
        let mut hist_buckets: BTreeMap<(String, String), f64> = BTreeMap::new();
        let mut store_lock_buckets: BTreeMap<String, f64> = BTreeMap::new();
        let mut cache_hit: Option<f64> = None;
        let mut cache_miss: Option<f64> = None;
        let mut scalars: BTreeMap<String, f64> = BTreeMap::new();

        for s in text.lines().filter_map(series) {
            match compaction_rule(&s.name) {
                Some(Compaction::HubHistogram) => {
                    if let Some(le) = s.label("le").map(str::to_string) {
                        *hist_buckets.entry((s.name, le)).or_insert(0.0) += s.value;
                    }
                }
                Some(Compaction::StoreLockHistogram) => {
                    if s.label("lock") == Some("store")
                        && let Some(le) = s.label("le").map(str::to_string)
                    {
                        *store_lock_buckets.entry(le).or_insert(0.0) += s.value;
                    }
                }
                Some(Compaction::CacheRequests) => match s.label("result") {
                    Some("hit") => *cache_hit.get_or_insert(0.0) += s.value,
                    Some("off") => {}
                    _ => *cache_miss.get_or_insert(0.0) += s.value,
                },
                Some(Compaction::HubScalar) => {
                    scalars
                        .entry(s.name)
                        .and_modify(|v| *v += s.value)
                        .or_insert(s.value);
                }
                Some(Compaction::EntityExact) => out.push(s),
                None => {}
            }
        }

        for ((name, le), value) in hist_buckets {
            let key = format!("{name}{{le=\"{le}\"}}");
            out.push(Series {
                key,
                name,
                labels: vec![("le".to_string(), le)],
                value,
            });
        }
        for (le, value) in store_lock_buckets {
            out.push(Series {
                key: format!("steve_lock_wait_seconds_bucket{{lock=\"store\",le=\"{le}\"}}"),
                name: "steve_lock_wait_seconds_bucket".to_string(),
                labels: vec![
                    ("lock".to_string(), "store".to_string()),
                    ("le".to_string(), le),
                ],
                value,
            });
        }
        for (result, value) in [("hit", cache_hit), ("miss", cache_miss)] {
            if let Some(value) = value {
                out.push(Series {
                    key: format!("steve_cache_requests_total{{result=\"{result}\"}}"),
                    name: "steve_cache_requests_total".to_string(),
                    labels: vec![("result".to_string(), result.to_string())],
                    value,
                });
            }
        }
        for (name, value) in scalars {
            out.push(Series {
                key: name.clone(),
                name,
                labels: Vec::new(),
                value,
            });
        }

        let by_key = out.iter().map(|s| (s.key.clone(), s.value)).collect();
        Snapshot {
            at,
            wall_ms,
            series: out,
            by_key,
        }
    }
}

fn series(line: &str) -> Option<Series> {
    if line.starts_with('#') {
        return None;
    }
    let (key, value) = line.trim_end().rsplit_once(' ')?;
    let value = match value {
        "+Inf" => f64::INFINITY,
        v => v.parse().ok()?,
    };
    let (name, labels) = match key.split_once('{') {
        Some((name, rest)) => (name, labels(rest.strip_suffix('}')?)?),
        None => (key, vec![]),
    };
    Some(Series {
        key: key.to_string(),
        name: name.to_string(),
        labels,
        value,
    })
}

/// `k="v",k2="v2"` with `\\`, `\"` and `\n` escaped in values.
fn labels(mut s: &str) -> Option<Vec<(String, String)>> {
    let mut out = vec![];
    while !s.is_empty() {
        let (k, rest) = s.split_once("=\"")?;
        let mut v = String::new();
        let mut chars = rest.char_indices();
        let end = loop {
            match chars.next()? {
                (i, '"') => break i,
                (_, '\\') => match chars.next()?.1 {
                    'n' => v.push('\n'),
                    c => v.push(c),
                },
                (_, c) => v.push(c),
            }
        };
        out.push((k.trim_start_matches(',').to_string(), v));
        s = rest[end + 1..].trim_start_matches(',');
    }
    Some(out)
}

/// What changed between the window's start and now.
struct Window<'a> {
    now: &'a Snapshot,
    base: Option<&'a Snapshot>,
}

impl Window<'_> {
    fn seconds(&self) -> f64 {
        self.now.at - self.base.map_or(0.0, |b| b.at)
    }

    fn matching<'s>(
        &'s self,
        name: &'s str,
        keep: &'s dyn Fn(&Series) -> bool,
    ) -> impl Iterator<Item = &'s Series> + 's {
        self.now
            .series
            .iter()
            .filter(move |s| s.name == name && keep(s))
    }

    /// How much a total grew; one that went down (its source restarted) grew by what it is now.
    fn growth(&self, s: &Series) -> f64 {
        match self.base.and_then(|b| b.by_key.get(&s.key)) {
            Some(before) if *before <= s.value => s.value - before,
            _ => s.value,
        }
    }

    fn delta(&self, name: &str, keep: &dyn Fn(&Series) -> bool) -> f64 {
        self.matching(name, keep).map(|s| self.growth(s)).sum()
    }

    fn now(&self, name: &str, keep: &dyn Fn(&Series) -> bool) -> Option<f64> {
        self.matching(name, keep)
            .map(|s| s.value)
            .reduce(|a, b| a + b)
    }

    /// The `q` quantile of a histogram's samples in the window, interpolated within its bucket as
    /// Prometheus' `histogram_quantile` does; `None` without samples.
    fn quantile(&self, name: &str, q: f64, keep: &dyn Fn(&Series) -> bool) -> Option<f64> {
        let bucket = format!("{name}_bucket");
        let mut by_le: Vec<(f64, f64)> = vec![];
        for s in self.matching(&bucket, keep) {
            let le = match s.label("le")? {
                "+Inf" => f64::INFINITY,
                v => v.parse().ok()?,
            };
            let n = self.growth(s);
            match by_le.iter_mut().find(|(l, _)| *l == le) {
                Some((_, c)) => *c += n,
                None => by_le.push((le, n)),
            }
        }
        by_le.sort_by(|a, b| a.0.total_cmp(&b.0));
        let total = by_le.last()?.1;
        if total <= 0.0 {
            return None;
        }
        let rank = q * total;
        let (mut lower, mut below) = (0.0, 0.0);
        for (le, count) in by_le {
            if count >= rank {
                if le.is_infinite() {
                    return Some(lower);
                }
                let share = if count > below {
                    (rank - below) / (count - below)
                } else {
                    1.0
                };
                return Some(lower + (le - lower) * share);
            }
            (lower, below) = (le, count);
        }
        Some(lower)
    }

    fn per_day(&self, growth: f64) -> Option<f64> {
        let s = self.seconds();
        (s > 0.0).then(|| growth / s * 86_400.0)
    }

    /// The values a label takes in any of `names` now.
    fn subjects(&self, label: &str, names: &[&str]) -> BTreeSet<String> {
        self.now
            .series
            .iter()
            .filter(|s| names.contains(&s.name.as_str()))
            .filter_map(|s| s.label(label).map(str::to_string))
            .collect()
    }
}

/// One figure and its limit.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub(crate) struct Check {
    pub key: &'static str,
    /// The Node or plugin it is about.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub subject: Option<String>,
    /// `None`: not enough samples in the window.
    pub value: Option<f64>,
    /// `seconds`, `ratio`, `bytes`, `bytes_per_day`, `per_second`, `per_minute`, `count`, `up`.
    pub unit: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub min: Option<f64>,
}

impl Check {
    fn new(key: &'static str, unit: &'static str, value: Option<f64>) -> Check {
        Check {
            key,
            subject: None,
            value,
            unit,
            max: None,
            min: None,
        }
    }
    fn max(self, max: f64) -> Check {
        Check {
            max: Some(max),
            ..self
        }
    }
    fn min(self, min: f64) -> Check {
        Check {
            min: Some(min),
            ..self
        }
    }
    fn of(self, subject: &str) -> Check {
        Check {
            subject: Some(subject.to_string()),
            ..self
        }
    }
    fn failing(&self) -> bool {
        self.value
            .is_some_and(|v| self.max.is_some_and(|m| v > m) || self.min.is_some_and(|m| v < m))
    }
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct Health {
    pub window: HealthWindow,
    pub window_seconds: f64,
    pub sampled_at: i64,
    pub checks: Vec<Check>,
    /// The checks past their limit.
    pub warnings: Vec<Check>,
    /// Time series for the four golden signals over the selected window.
    pub series: Vec<SeriesPoint>,
}

/// Retained snapshot rings covering 15m/1h (1-minute resolution) and 24h (5-minute resolution).
#[derive(Default)]
pub(crate) struct HealthHistory {
    recent: VecDeque<Arc<Snapshot>>,
    day: VecDeque<Arc<Snapshot>>,
}

impl HealthHistory {
    pub(crate) fn push(&mut self, snap: Snapshot) {
        let snap = Arc::new(snap);
        let push_day = self
            .day
            .back()
            .is_none_or(|last| snap.at - last.at >= DAY_STRIDE_SECS - 1.0);
        if push_day {
            if self.day.len() == DAY_CAPACITY {
                self.day.pop_front();
            }
            self.day.push_back(Arc::clone(&snap));
        }
        if self.recent.len() == RECENT_CAPACITY {
            self.recent.pop_front();
        }
        self.recent.push_back(snap);
    }

    pub(crate) fn report(&self, window: HealthWindow) -> Option<Health> {
        let now = self.recent.back()?;
        let slice = self.slice_for(window, now);
        if slice.len() < 2 {
            return Some(assess(now, None, window, vec![point_of(now, None)]));
        }
        let base = Some(slice[0].as_ref());
        let series = (1..slice.len())
            .map(|idx| point_of(&slice[idx], Some(&slice[idx - 1])))
            .collect();
        Some(assess(now, base, window, series))
    }

    fn slice_for(&self, window: HealthWindow, now: &Arc<Snapshot>) -> Vec<Arc<Snapshot>> {
        let use_day = matches!(window, HealthWindow::H24)
            && self.day.len() >= 2
            && self
                .day
                .front()
                .is_some_and(|first| now.at - first.at >= DAY_SERIES_MIN_SPAN_SECS);
        if use_day {
            let cutoff = WINDOW_24H_SECS + DAY_GRACE_SECS;
            let mut out: Vec<Arc<Snapshot>> = self
                .day
                .iter()
                .filter(|s| now.at - s.at <= cutoff)
                .cloned()
                .collect();
            if out
                .last()
                .is_some_and(|last| now.at - last.at >= DAY_TAIL_GAP_SECS)
            {
                out.push(Arc::clone(now));
            }
            return out;
        }
        let cutoff = window.target_seconds() + RECENT_GRACE_SECS;
        self.recent
            .iter()
            .filter(|s| now.at - s.at <= cutoff)
            .cloned()
            .collect()
    }
}

/// `num / den` once `den` reaches `min`.
fn ratio(num: f64, den: f64, min: f64) -> Option<f64> {
    (den >= min && den > 0.0).then(|| num / den)
}

/// Compute one interval's golden-signal point between `base` and `now`.
pub(crate) fn point_of(now: &Snapshot, base: Option<&Snapshot>) -> SeriesPoint {
    let w = Window { now, base };
    let all = |_: &Series| true;
    let secs = w.seconds();
    SeriesPoint {
        at: now.wall_ms,
        http_p95: w.quantile("steve_http_request_duration_seconds", 0.95, &all),
        write_lock_wait_p95: w.quantile("steve_lock_wait_seconds", 0.95, &|s| {
            s.label("lock") == Some("store")
        }),
        hub_cpu: (secs > 0.0).then(|| w.delta("steve_process_cpu_seconds", &all) / secs),
        hub_memory: w.now("steve_process_resident_bytes", &all),
    }
}

/// Single-snapshot health fallback before two periodic snapshots have been captured.
pub(crate) fn assess_single(now: &Snapshot, window: HealthWindow) -> Health {
    assess(now, None, window, vec![point_of(now, None)])
}

/// The health between `base` (`None`: the recorder's start) and `now`.
pub(crate) fn assess(
    now: &Snapshot,
    base: Option<&Snapshot>,
    window: HealthWindow,
    series: Vec<SeriesPoint>,
) -> Health {
    let w = Window { now, base };
    let all = |_: &Series| true;
    let secs = w.seconds();
    let hits = w.delta("steve_cache_requests_total", &|s| {
        s.label("result") == Some("hit")
    });
    let lookups = w.delta("steve_cache_requests_total", &|s| {
        s.label("result") != Some("off")
    });
    // Backup planning samples free space and the space for staging and repacking the current DB.
    let mut backup_space = Check::new(
        "backup_destination_space",
        "bytes",
        w.now("steve_backup_destination_available_bytes", &all)
            .filter(|bytes| bytes.is_finite()),
    );
    backup_space.min = w
        .now("steve_backup_destination_required_bytes", &all)
        .filter(|bytes| bytes.is_finite() && *bytes > 0.0);
    let mut checks = vec![
        Check::new(
            "http_p95",
            "seconds",
            w.quantile("steve_http_request_duration_seconds", 0.95, &all),
        )
        .max(0.5),
        Check::new(
            "write_lock_wait_p95",
            "seconds",
            w.quantile("steve_lock_wait_seconds", 0.95, &|s| {
                s.label("lock") == Some("store")
            }),
        )
        .max(0.1),
        Check::new(
            "sse_delivery_p95",
            "seconds",
            w.quantile("steve_sse_delivery_seconds", 0.95, &all),
        )
        .max(0.5),
        Check::new(
            "patch_delay_p95",
            "seconds",
            w.quantile("steve_projection_delay_seconds", 0.95, &all),
        )
        .max(0.5),
        Check::new(
            "reconcile_diff",
            "count",
            Some(w.delta("steve_projection_reconcile_diff_total", &all)),
        )
        .max(0.0),
        Check::new(
            "ingest_p95",
            "seconds",
            w.quantile("steve_node_ingest_seconds", 0.95, &all),
        )
        .max(0.25),
        Check::new(
            "ingest_rate",
            "per_second",
            (secs > 0.0).then(|| w.delta("steve_node_ingest_events_sum", &all) / secs),
        ),
        Check::new(
            "cache_hit_ratio",
            "ratio",
            ratio(hits, lookups, MIN_LOOKUPS),
        )
        .min(0.5),
        Check::new(
            "hub_cpu",
            "ratio",
            (secs > 0.0).then(|| w.delta("steve_process_cpu_seconds", &all) / secs),
        )
        .max(0.8),
        Check::new(
            "hub_memory",
            "bytes",
            w.now("steve_process_resident_bytes", &all),
        )
        .max(2.0 * GIB),
        Check::new(
            "hub_disk_writes",
            "bytes_per_day",
            w.per_day(w.delta("steve_process_disk_written_bytes", &all)),
        )
        .max(5.0 * GIB),
        backup_space,
        Check::new(
            "web_interaction_p75",
            "seconds",
            w.quantile("steve_web_interaction_seconds", 0.75, &all),
        )
        .max(0.2),
        Check::new(
            "web_long_tasks",
            "per_minute",
            (secs > 0.0).then(|| w.delta("steve_web_long_task_seconds_count", &all) / secs * 60.0),
        )
        .max(10.0),
        Check::new(
            "job_panics",
            "count",
            Some(w.delta("steve_job_panics_total", &all)),
        )
        .max(0.0),
        Check::new(
            "jobs_down",
            "count",
            Some(
                w.matching("steve_job_up", &all)
                    .filter(|s| s.value < 1.0)
                    .count() as f64,
            ),
        )
        .max(0.0),
    ];
    let jobs = w.subjects("job", &["steve_job_up", "steve_job_panics_total"]);
    for job in &jobs {
        let of = |s: &Series| s.label("job") == Some(job.as_str());
        let up = w.now("steve_job_up", &of);
        let panics = w.delta("steve_job_panics_total", &of);
        if up.is_some_and(|v| v < 1.0) {
            checks.push(Check::new("job_up", "up", up).min(1.0).of(job));
        }
        if panics > 0.0 {
            checks.push(
                Check::new("job_panics", "count", Some(panics))
                    .max(0.0)
                    .of(job),
            );
        }
    }
    let nodes = w.subjects(
        "node",
        &[
            "steve_node_link_up",
            "steve_node_journal_bytes",
            "steve_node_commands_total",
        ],
    );
    for node in &nodes {
        let of = |s: &Series| s.label("node") == Some(node.as_str());
        let commands = w.delta("steve_node_commands_total", &of);
        let failed = w.delta("steve_node_commands_total", &|s| {
            of(s) && s.label("result") != Some("ok")
        });
        let written = w
            .now("steve_node_process_disk_written_bytes", &of)
            .and_then(|_| w.per_day(w.delta("steve_node_process_disk_written_bytes", &of)));
        checks.extend([
            Check::new("node_link", "up", w.now("steve_node_link_up", &of)).of(node),
            Check::new(
                "node_link_drops",
                "count",
                w.now("steve_node_link_disconnects_total", &of)
                    .map(|_| w.delta("steve_node_link_disconnects_total", &of)),
            )
            .max(3.0)
            .of(node),
            Check::new(
                "node_command_failures",
                "ratio",
                ratio(failed, commands, MIN_CALLS),
            )
            .max(0.2)
            .of(node),
            Check::new(
                "node_backlog",
                "count",
                w.now("steve_node_event_backlog", &of),
            )
            .max(10_000.0)
            .of(node),
            Check::new("node_disk_writes", "bytes_per_day", written)
                .max(5.0 * GIB)
                .of(node),
        ]);
    }
    let plugins = w.subjects(
        "plugin",
        &[
            "steve_plugin_calls_total",
            "steve_plugin_restarts_total",
            "steve_plugin_inbound_backlog",
        ],
    );
    for plugin in &plugins {
        let of = |s: &Series| s.label("plugin") == Some(plugin.as_str());
        let calls = w.delta("steve_plugin_calls_total", &of);
        let failed = w.delta("steve_plugin_calls_total", &|s| {
            of(s) && s.label("result") != Some("ok")
        });
        checks.extend([
            Check::new("plugin_errors", "ratio", ratio(failed, calls, MIN_CALLS))
                .max(0.2)
                .of(plugin),
            Check::new(
                "plugin_restarts",
                "count",
                Some(w.delta("steve_plugin_restarts_total", &of)),
            )
            .max(2.0)
            .of(plugin),
            Check::new(
                "plugin_backlog",
                "count",
                w.now("steve_plugin_inbound_backlog", &of),
            )
            .max(100.0)
            .of(plugin),
        ]);
    }
    let warnings = checks.iter().filter(|c| c.failing()).cloned().collect();
    Health {
        window,
        window_seconds: secs,
        sampled_at: now.wall_ms,
        checks,
        warnings,
        series,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn check<'a>(h: &'a Health, key: &str, subject: Option<&str>) -> &'a Check {
        h.checks
            .iter()
            .find(|c| c.key == key && c.subject.as_deref() == subject)
            .unwrap_or_else(|| panic!("{key} {subject:?} in {h:?}"))
    }

    #[test]
    fn health_window_parses_supported_values_and_rejects_unknown() {
        assert_eq!(HealthWindow::parse("15m"), Some(HealthWindow::M15));
        assert_eq!(HealthWindow::parse("1h"), Some(HealthWindow::H1));
        assert_eq!(HealthWindow::parse("24h"), Some(HealthWindow::H24));
        for bad in ["", "5m", "2d", "15M", "all"] {
            assert_eq!(HealthWindow::parse(bad), None, "{bad}");
        }
    }

    #[test]
    fn figures_are_of_the_window_and_past_their_limit_are_warnings() {
        let base = Snapshot::parse(
            r#"
steve_http_request_duration_seconds_bucket{route="/a",le="0.1"} 100
steve_http_request_duration_seconds_bucket{route="/a",le="1"} 100
steve_http_request_duration_seconds_bucket{route="/a",le="+Inf"} 100
steve_lock_wait_seconds_bucket{lock="store",caller="a",le="0.05"} 10
steve_lock_wait_seconds_bucket{lock="store",caller="a",le="+Inf"} 10
steve_lock_wait_seconds_bucket{lock="core",caller="b",le="0.05"} 999
steve_lock_wait_seconds_bucket{lock="core",caller="b",le="+Inf"} 999
steve_process_disk_written_bytes 1000
steve_cache_requests_total{space="s",result="hit"} 10
steve_cache_requests_total{space="s",result="miss"} 0
steve_cache_requests_total{space="s",result="off"} 500
steve_node_commands_total{node="n1",method="start_turn",result="ok"} 50
steve_node_process_disk_written_bytes{node="n1"} 900000000000
steve_unrelated_metric{foo="bar"} 42
"#,
            0.0,
            1_000_000,
        );
        // the last 600 s: 100 requests across routes /a and /b (10 under 0.1 s, 90 under 1 s),
        // core lock ignored, cache off ignored, Node restarted (total went down), escaped plugin quote
        let now = Snapshot::parse(
            r#"
# HELP steve_http_request_duration_seconds Time to response headers.
steve_http_request_duration_seconds_bucket{route="/a",le="0.1"} 105
steve_http_request_duration_seconds_bucket{route="/a",le="1"} 150
steve_http_request_duration_seconds_bucket{route="/a",le="+Inf"} 150
steve_http_request_duration_seconds_bucket{route="/b",le="0.1"} 5
steve_http_request_duration_seconds_bucket{route="/b",le="1"} 50
steve_http_request_duration_seconds_bucket{route="/b",le="+Inf"} 50
steve_lock_wait_seconds_bucket{lock="store",caller="a",le="0.05"} 10
steve_lock_wait_seconds_bucket{lock="store",caller="a",le="+Inf"} 10
steve_lock_wait_seconds_bucket{lock="core",caller="b",le="0.05"} 1000
steve_lock_wait_seconds_bucket{lock="core",caller="b",le="+Inf"} 2000
steve_process_disk_written_bytes 7000000001000
steve_cache_requests_total{space="s",result="hit"} 40
steve_cache_requests_total{space="s",result="miss"} 170
steve_cache_requests_total{space="s",result="off"} 900
steve_node_link_up{node="n1"} 1
steve_node_commands_total{node="n1",method="start_turn",result="ok"} 54
steve_node_commands_total{node="n1",method="start_turn",result="offline"} 6
steve_node_process_disk_written_bytes{node="n1"} 1000
steve_plugin_restarts_total{plugin="p\"1"} 1
"#,
            600.0,
            1_600_000,
        );
        let h = assess(&now, Some(&base), HealthWindow::M15, vec![]);
        assert_eq!(h.window, HealthWindow::M15);
        assert_eq!(h.window_seconds, 600.0);
        assert_eq!(h.sampled_at, 1_600_000);
        // rank 95 of 100 falls 85/90 of the way through (0.1, 1]
        let p95 = check(&h, "http_p95", None).value.unwrap();
        assert!((p95 - (0.1 + 0.9 * 85.0 / 90.0)).abs() < 1e-9, "{p95}");
        assert_eq!(check(&h, "cache_hit_ratio", None).value, Some(30.0 / 200.0));
        assert_eq!(
            check(&h, "hub_disk_writes", None).value,
            Some(7e12 / 600.0 * 86_400.0)
        );
        assert_eq!(
            check(&h, "node_command_failures", Some("n1")).value,
            Some(0.6)
        );
        assert_eq!(
            check(&h, "node_disk_writes", Some("n1")).value,
            Some(1000.0 / 600.0 * 86_400.0)
        );
        assert_eq!(check(&h, "plugin_restarts", Some("p\"1")).value, Some(1.0));
        // store lock had 0 samples in the window (core lock was ignored): no value, no warning
        assert_eq!(check(&h, "write_lock_wait_p95", None).value, None);
        let mut warned: Vec<_> = h
            .warnings
            .iter()
            .map(|c| (c.key, c.subject.as_deref()))
            .collect();
        warned.sort();
        assert_eq!(
            warned,
            [
                ("cache_hit_ratio", None),
                ("http_p95", None),
                ("hub_disk_writes", None),
                ("node_command_failures", Some("n1")),
            ]
        );
    }

    #[test]
    fn backup_space_uses_the_sampled_requirement() {
        let sample = |required| {
            Snapshot::parse(
                &format!(
                    "steve_backup_destination_available_bytes 200\nsteve_backup_destination_required_bytes {required}\n"
                ),
                60.0,
                60_000,
            )
        };
        for (required, expected) in [("200", Some(200.0)), ("201", Some(201.0)), ("NaN", None)] {
            let health = assess_single(&sample(required), HealthWindow::M15);
            assert_eq!(
                check(&health, "backup_destination_space", None).min,
                expected
            );
            assert_eq!(
                health
                    .warnings
                    .iter()
                    .any(|check| check.key == "backup_destination_space"),
                required == "201"
            );
        }
    }

    #[test]
    fn without_a_window_yet_the_figures_count_from_the_start() {
        let now = Snapshot::parse("steve_process_disk_written_bytes 86400\n", 60.0, 60_000);
        let h = assess_single(&now, HealthWindow::M15);
        assert_eq!(
            check(&h, "hub_disk_writes", None).value,
            Some(86400.0 * 1440.0)
        );
        assert!(h.warnings.is_empty(), "{h:?}");
    }

    #[test]
    fn panicked_and_down_background_jobs_raise_explicit_warnings() {
        let base = Snapshot::parse(
            r#"
steve_job_up{job="calendar.sync"} 1
steve_job_up{job="mail.sync"} 1
steve_job_panics_total{job="mail.sync"} 1
"#,
            0.0,
            0,
        );
        let now = Snapshot::parse(
            r#"
steve_job_up{job="calendar.sync"} 1
steve_job_up{job="mail.sync"} 0
steve_job_panics_total{job="mail.sync"} 3
"#,
            60.0,
            60_000,
        );
        let h = assess(&now, Some(&base), HealthWindow::M15, vec![]);
        assert_eq!(check(&h, "job_panics", None).value, Some(2.0));
        assert_eq!(check(&h, "jobs_down", None).value, Some(1.0));
        assert_eq!(check(&h, "job_up", Some("mail.sync")).value, Some(0.0));
        assert_eq!(check(&h, "job_panics", Some("mail.sync")).value, Some(2.0));
        // Healthy jobs do not clutter the report.
        assert!(
            !h.checks
                .iter()
                .any(|c| c.subject.as_deref() == Some("calendar.sync"))
        );
        let mut warned: Vec<_> = h
            .warnings
            .iter()
            .map(|c| (c.key, c.subject.as_deref()))
            .collect();
        warned.sort();
        assert_eq!(
            warned,
            [
                ("job_panics", None),
                ("job_panics", Some("mail.sync")),
                ("job_up", Some("mail.sync")),
                ("jobs_down", None),
            ]
        );
    }

    #[test]
    fn history_serves_windows_and_golden_series_across_startup_hour_and_day() {
        let mut history = HealthHistory::default();
        assert!(history.report(HealthWindow::M15).is_none());

        let snap_at = |min: usize| {
            let secs = (min * 60) as f64;
            let wall_ms = (min as i64) * 60_000;
            // Every minute adds 10 HTTP requests (except minute 5..=6 which are idle for gap test)
            // and 15 CPU seconds (25% core utilization), and sets memory to (100 + min) MiB.
            let http_count = if min < 5 {
                min * 10
            } else if min <= 6 {
                40
            } else {
                (min - 2) * 10
            };
            let lock_count = min;
            let cpu = secs * 0.25;
            let mem = (100 + min) * (1024 * 1024);
            Snapshot::parse(
                &format!(
                    "steve_http_request_duration_seconds_bucket{{route=\"/a\",le=\"0.1\"}} {http_count}\n\
                     steve_http_request_duration_seconds_bucket{{route=\"/a\",le=\"+Inf\"}} {http_count}\n\
                     steve_lock_wait_seconds_bucket{{lock=\"store\",le=\"0.05\"}} {lock_count}\n\
                     steve_lock_wait_seconds_bucket{{lock=\"store\",le=\"+Inf\"}} {lock_count}\n\
                     steve_process_cpu_seconds {cpu}\n\
                     steve_process_resident_bytes {mem}\n"
                ),
                secs,
                wall_ms,
            )
        };

        // First snapshot alone falls back to single-point assessment.
        history.push(snap_at(1));
        let single = history.report(HealthWindow::M15).unwrap();
        assert_eq!(single.series.len(), 1);
        assert_eq!(single.series[0].at, 60_000);
        assert_eq!(single.window_seconds, 60.0);

        // Push up to 30 minutes (less than DAY_SERIES_MIN_SPAN_SECS): 24h falls back to 1m points.
        for min in 2..=30 {
            history.push(snap_at(min));
        }
        let early_24h = history.report(HealthWindow::H24).unwrap();
        assert_eq!(early_24h.window, HealthWindow::H24);
        assert_eq!(early_24h.window_seconds, 29.0 * 60.0);
        assert_eq!(early_24h.series.len(), 29);

        let r15 = history.report(HealthWindow::M15).unwrap();
        assert_eq!(r15.window_seconds, 15.0 * 60.0);
        assert_eq!(r15.series.len(), 15);

        // Push up to 120 minutes: 1h has 60 points (recent capacity = 61), 24h switches to 5m day ring.
        for min in 31..=120 {
            history.push(snap_at(min));
        }
        assert_eq!(history.recent.len(), RECENT_CAPACITY);

        let r1h = history.report(HealthWindow::H1).unwrap();
        assert_eq!(r1h.window, HealthWindow::H1);
        assert_eq!(r1h.window_seconds, 60.0 * 60.0);
        assert_eq!(r1h.series.len(), 60);
        assert!((r1h.series.last().unwrap().hub_cpu.unwrap() - 0.25).abs() < 1e-9);

        let r24 = history.report(HealthWindow::H24).unwrap();
        assert_eq!(r24.window, HealthWindow::H24);
        // day starts at min 1, steps every 5m up to min 116, plus tail snap at min 120 (gap = 240s >= 30s)
        assert_eq!(r24.window_seconds, (120.0 - 1.0) * 60.0);
        assert_eq!(r24.sampled_at, 120 * 60_000);
        assert_eq!(r24.series.len(), 24);
        // Interval 1..=6 in 5m ring had minute 5..=6 idle, but minute 2..=4 had requests; verify gap in 1m series instead:
        let mut gap_hist = HealthHistory::default();
        for min in 4..=6 {
            gap_hist.push(snap_at(min));
        }
        let gap_r15 = gap_hist.report(HealthWindow::M15).unwrap();
        assert_eq!(gap_r15.series.len(), 2);
        // 4->5 and 5->6 both had 0 new HTTP requests while lock/CPU still reported.
        assert_eq!(gap_r15.series[0].http_p95, None);
        assert_eq!(gap_r15.series[1].http_p95, None);
        assert!(gap_r15.series[0].write_lock_wait_p95.is_some());

        // Verify 24h ring caps at DAY_CAPACITY over >24h of snapshots.
        for min in 121..=1500 {
            history.push(snap_at(min));
        }
        assert_eq!(history.day.len(), DAY_CAPACITY);
        let full_24 = history.report(HealthWindow::H24).unwrap();
        assert!(full_24.window_seconds >= WINDOW_24H_SECS - DAY_STRIDE_SECS);
        assert!(full_24.window_seconds <= WINDOW_24H_SECS + DAY_GRACE_SECS);
    }
}
