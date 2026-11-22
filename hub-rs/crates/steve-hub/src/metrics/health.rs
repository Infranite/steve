//! The Hub's health at a glance (`GET /api/system/health`): a few figures over the last window,
//! each with its limit, computed from two snapshots of the recorder (now, and the oldest of the
//! window). A figure past its limit is also a warning; the client words it. A figure without enough
//! samples in the window to mean anything has no value and is never a warning.
use std::collections::{BTreeSet, HashMap};

use serde::Serialize;

const GIB: f64 = (1u64 << 30) as f64;
/// Ratios need this many events in the window before they say anything.
const MIN_LOOKUPS: f64 = 100.0;
const MIN_CALLS: f64 = 5.0;

/// The recorder's exposition at one moment.
pub(crate) struct Snapshot {
    /// Seconds since the recorder was installed.
    at: f64,
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

impl Snapshot {
    /// The Prometheus text format: `name{k="v",…} value` lines; comments and malformed lines are
    /// skipped.
    pub(crate) fn parse(text: &str, at: f64) -> Snapshot {
        let series: Vec<Series> = text.lines().filter_map(series).collect();
        let by_key = series.iter().map(|s| (s.key.clone(), s.value)).collect();
        Snapshot { at, series, by_key }
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

#[derive(Debug, Serialize)]
pub(crate) struct Health {
    pub window_seconds: f64,
    pub checks: Vec<Check>,
    /// The checks past their limit.
    pub warnings: Vec<Check>,
}

/// `num / den` once `den` reaches `min`.
fn ratio(num: f64, den: f64, min: f64) -> Option<f64> {
    (den >= min && den > 0.0).then(|| num / den)
}

/// The health between `base` (`None`: the recorder's start) and `now`.
pub(crate) fn assess(now: &Snapshot, base: Option<&Snapshot>) -> Health {
    let w = Window { now, base };
    let all = |_: &Series| true;
    let secs = w.seconds();
    let hits = w.delta("steve_cache_requests_total", &|s| {
        s.label("result") == Some("hit")
    });
    let lookups = w.delta("steve_cache_requests_total", &|s| {
        s.label("result") != Some("off")
    });
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
        Check::new(
            "backup_destination_space",
            "bytes",
            w.now("steve_backup_destination_available_bytes", &all),
        )
        .min(GIB),
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
    ];
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
        window_seconds: secs,
        checks,
        warnings,
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
    fn figures_are_of_the_window_and_past_their_limit_are_warnings() {
        let base = Snapshot::parse(
            r#"
steve_http_request_duration_seconds_bucket{route="/a",le="0.1"} 100
steve_http_request_duration_seconds_bucket{route="/a",le="1"} 100
steve_http_request_duration_seconds_bucket{route="/a",le="+Inf"} 100
steve_process_disk_written_bytes 1000
steve_cache_requests_total{space="s",result="hit"} 10
steve_cache_requests_total{space="s",result="miss"} 0
steve_node_commands_total{node="n1",method="start_turn",result="ok"} 50
steve_node_process_disk_written_bytes{node="n1"} 900000000000
"#,
            0.0,
        );
        // the last 600 s: 100 requests of which 10 under 0.1 s and 90 under 1 s, a Node that
        // restarted (its total went down), a plugin label value with an escaped quote
        let now = Snapshot::parse(
            r#"
# HELP steve_http_request_duration_seconds Time to response headers.
steve_http_request_duration_seconds_bucket{route="/a",le="0.1"} 110
steve_http_request_duration_seconds_bucket{route="/a",le="1"} 200
steve_http_request_duration_seconds_bucket{route="/a",le="+Inf"} 200
steve_process_disk_written_bytes 7000000001000
steve_cache_requests_total{space="s",result="hit"} 40
steve_cache_requests_total{space="s",result="miss"} 170
steve_node_link_up{node="n1"} 1
steve_node_commands_total{node="n1",method="start_turn",result="ok"} 54
steve_node_commands_total{node="n1",method="start_turn",result="offline"} 6
steve_node_process_disk_written_bytes{node="n1"} 1000
steve_plugin_restarts_total{plugin="p\"1"} 1
"#,
            600.0,
        );
        let h = assess(&now, Some(&base));
        assert_eq!(h.window_seconds, 600.0);
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
        // no samples: no value, no warning
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
    fn without_a_window_yet_the_figures_count_from_the_start() {
        let now = Snapshot::parse("steve_process_disk_written_bytes 86400\n", 60.0);
        let h = assess(&now, None);
        assert_eq!(
            check(&h, "hub_disk_writes", None).value,
            Some(86400.0 * 1440.0)
        );
        assert!(h.warnings.is_empty(), "{h:?}");
    }
}
