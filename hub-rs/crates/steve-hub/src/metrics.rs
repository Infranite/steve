//! Metrics, collected in memory from the Hub's start (S1). Exporting is optional: with
//! `STEVE_METRICS=prometheus` the Hub serves `/metrics` on `STEVE_METRICS_LISTEN`, the management
//! socket always serves the same text, and `/api/system/health` reads the same recorder. Every
//! metric is described in [`catalog`]; labels take bounded values only (route templates, methods,
//! results, reason codes, harnesses, plugin and Node ids), never a session, item or message id.
//! Hot paths use static keys or handles registered once; nothing here writes to the database.
pub(crate) mod backup;
mod catalog;
pub(crate) mod health;
pub(crate) mod nodes;

/// A handle registered on first use and kept for the process (the recorder is installed before
/// anything records): recording through it skips the registry lookup and allocates nothing, which a
/// `metrics::counter!("…")` call at a hot site does on every use.
macro_rules! once {
    (counter $name:literal) => {
        $crate::metrics::once!(@ metrics::Counter, metrics::counter!($name))
    };
    (gauge $name:literal) => {
        $crate::metrics::once!(@ metrics::Gauge, metrics::gauge!($name))
    };
    (histogram $name:literal) => {
        $crate::metrics::once!(@ metrics::Histogram, metrics::histogram!($name))
    };
    (@ $t:ty, $make:expr) => {{
        static HANDLE: std::sync::OnceLock<$t> = std::sync::OnceLock::new();
        HANDLE.get_or_init(|| $make)
    }};
}
pub(crate) use once;

use std::collections::{BTreeMap, HashMap, HashSet};
use std::fmt::Write as _;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock, RwLock};
use std::task::{Context, Poll};
use std::time::{Duration, Instant};

use axum::body::{Body, Bytes};
use axum::extract::{MatchedPath, Request, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use axum::{Router, routing::get};
use hyper::body::{Body as _, Frame, SizeHint};
use metrics_exporter_prometheus::{
    Matcher, PrometheusBuilder, PrometheusHandle, PrometheusRecorder,
};
use steve_store::Store;
use steve_types::devices::{NodeVitalsDay, NodeVitalsLatest, NodeVitalsPoint, VitalsTokens};

use crate::config::ConfigError;

const UPKEEP_EVERY: Duration = Duration::from_secs(5);
pub(crate) const CONTENT_TYPE: &str = "text/plain; version=0.0.4; charset=utf-8";

const DEFAULT_BUCKETS: &[f64] = &[
    0.00001, 0.000025, 0.00005, 0.0001, 0.00025, 0.0005, 0.001, 0.0025, 0.005, 0.01, 0.025, 0.05,
    0.1, 0.25, 0.5, 1.0, 2.5, 5.0, 10.0, 30.0, 100.0,
];
const COUNT_BUCKETS: &[f64] = &[
    0.0, 1.0, 2.0, 4.0, 8.0, 16.0, 32.0, 64.0, 128.0, 256.0, 512.0, 1024.0,
];
const LONG_BUCKETS: &[f64] = &[
    0.5, 1.0, 2.5, 5.0, 10.0, 30.0, 60.0, 120.0, 300.0, 600.0, 1800.0, 3600.0, 7200.0, 21600.0,
    86400.0, 604800.0,
];
const TOKEN_BUCKETS: &[f64] = &[
    4_000.0,
    16_000.0,
    32_000.0,
    64_000.0,
    128_000.0,
    160_000.0,
    200_000.0,
    256_000.0,
    384_000.0,
    512_000.0,
    768_000.0,
    1_000_000.0,
];

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Listen {
    Tcp(SocketAddr),
    Unix(PathBuf),
}

impl Listen {
    pub fn parse(raw: &str) -> Result<Self, ConfigError> {
        let invalid = || ConfigError::Invalid {
            var: "STEVE_METRICS_LISTEN",
            msg: "expected a loopback IP:port or unix:<absolute path>".into(),
        };
        if let Some(path) = raw.strip_prefix("unix:") {
            let path = PathBuf::from(path);
            return if path.is_absolute()
                && path.file_name().is_some()
                && !raw.as_bytes().contains(&0)
            {
                Ok(Self::Unix(path))
            } else {
                Err(invalid())
            };
        }
        let addr: SocketAddr = raw.parse().map_err(|_| invalid())?;
        if !addr.ip().is_loopback() {
            return Err(invalid());
        }
        Ok(Self::Tcp(addr))
    }
}

fn atomic_f64_add(bits: &AtomicU64, delta: f64) {
    let mut cur = bits.load(Ordering::Relaxed);
    loop {
        let next = (f64::from_bits(cur) + delta).to_bits();
        match bits.compare_exchange_weak(cur, next, Ordering::Relaxed, Ordering::Relaxed) {
            Ok(_) => return,
            Err(actual) => cur = actual,
        }
    }
}

struct DynCounter(AtomicU64);

impl metrics::CounterFn for DynCounter {
    fn increment(&self, value: u64) {
        self.0.fetch_add(value, Ordering::Relaxed);
    }
    fn absolute(&self, value: u64) {
        self.0.fetch_max(value, Ordering::Relaxed);
    }
}

struct DynGauge(AtomicU64);

impl DynGauge {
    fn new() -> Self {
        Self(AtomicU64::new(0.0_f64.to_bits()))
    }
    fn get(&self) -> f64 {
        f64::from_bits(self.0.load(Ordering::Relaxed))
    }
}

impl metrics::GaugeFn for DynGauge {
    fn increment(&self, value: f64) {
        atomic_f64_add(&self.0, value);
    }
    fn decrement(&self, value: f64) {
        atomic_f64_add(&self.0, -value);
    }
    fn set(&self, value: f64) {
        self.0.store(value.to_bits(), Ordering::Relaxed);
    }
}

struct BucketHistogram {
    bounds: &'static [f64],
    buckets: Box<[AtomicU64]>,
    sum_bits: AtomicU64,
    count: AtomicU64,
}

impl BucketHistogram {
    fn new(bounds: &'static [f64]) -> Self {
        let mut buckets = Vec::with_capacity(bounds.len() + 1);
        buckets.resize_with(bounds.len() + 1, || AtomicU64::new(0));
        Self {
            bounds,
            buckets: buckets.into_boxed_slice(),
            sum_bits: AtomicU64::new(0.0_f64.to_bits()),
            count: AtomicU64::new(0),
        }
    }
}

impl metrics::HistogramFn for BucketHistogram {
    fn record(&self, value: f64) {
        if value.is_nan() {
            return;
        }
        let idx = self.bounds.partition_point(|&b| b < value);
        self.buckets[idx].fetch_add(1, Ordering::Relaxed);
        self.count.fetch_add(1, Ordering::Relaxed);
        atomic_f64_add(&self.sum_bits, value);
    }
}

/// Dynamic series owned by entities that can be revoked or uninstalled (`node`, `plugin`).
/// Storing them separately from `PrometheusRecorder` allows immediate removal on revocation so
/// deleted Nodes or uninstalled plugins never leak unbounded series in memory.
#[derive(Default)]
struct DynSeries {
    counters: RwLock<HashMap<metrics::Key, Arc<DynCounter>>>,
    gauges: RwLock<HashMap<metrics::Key, Arc<DynGauge>>>,
    histograms: RwLock<HashMap<metrics::Key, Arc<BucketHistogram>>>,
    descriptions: RwLock<HashMap<String, String>>,
}

fn is_removable(key: &metrics::Key) -> bool {
    key.labels().any(|l| matches!(l.key(), "node" | "plugin"))
}

fn buckets_for(name: &str) -> &'static [f64] {
    if catalog::COUNTS.contains(&name) {
        COUNT_BUCKETS
    } else if catalog::LONG.contains(&name) {
        LONG_BUCKETS
    } else if catalog::TOKENS.contains(&name) {
        TOKEN_BUCKETS
    } else {
        DEFAULT_BUCKETS
    }
}

fn escape_label_value(out: &mut String, val: &str) {
    for ch in val.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            c => out.push(c),
        }
    }
}

fn format_labels(key: &metrics::Key, extra: Option<(&str, &str)>) -> String {
    let mut out = String::new();
    let mut first = true;
    for l in key.labels() {
        if first {
            out.push('{');
            first = false;
        } else {
            out.push(',');
        }
        out.push_str(l.key());
        out.push_str("=\"");
        escape_label_value(&mut out, l.value());
        out.push('"');
    }
    if let Some((ek, ev)) = extra {
        if first {
            out.push('{');
            first = false;
        } else {
            out.push(',');
        }
        out.push_str(ek);
        out.push_str("=\"");
        escape_label_value(&mut out, ev);
        out.push('"');
    }
    if !first {
        out.push('}');
    }
    out
}

fn write_f64(out: &mut String, v: f64) {
    if v.is_nan() {
        out.push_str("NaN");
    } else if v == f64::INFINITY {
        out.push_str("+Inf");
    } else if v == f64::NEG_INFINITY {
        out.push_str("-Inf");
    } else {
        let _ = write!(out, "{v}");
    }
}

impl DynSeries {
    fn remove_label(&self, label_key: &str, label_val: &str) {
        let keep = |k: &metrics::Key| {
            !k.labels()
                .any(|l| l.key() == label_key && l.value() == label_val)
        };
        self.counters
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .retain(|k, _| keep(k));
        self.gauges
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .retain(|k, _| keep(k));
        self.histograms
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .retain(|k, _| keep(k));
    }

    fn render(&self, out: &mut String) {
        let descs = self.descriptions.read().unwrap_or_else(|e| e.into_inner());
        // Counters
        {
            let map = self.counters.read().unwrap_or_else(|e| e.into_inner());
            let mut by_name: BTreeMap<&str, Vec<(String, u64)>> = BTreeMap::new();
            for (k, v) in map.iter() {
                by_name
                    .entry(k.name())
                    .or_default()
                    .push((format_labels(k, None), v.0.load(Ordering::Relaxed)));
            }
            for (name, mut rows) in by_name {
                rows.sort_by(|a, b| a.0.cmp(&b.0));
                if !out.is_empty() && !out.ends_with('\n') {
                    out.push('\n');
                }
                if let Some(help) = descs.get(name) {
                    let _ = writeln!(out, "# HELP {name} {help}");
                }
                let _ = writeln!(out, "# TYPE {name} counter");
                for (labels, val) in rows {
                    let _ = writeln!(out, "{name}{labels} {val}");
                }
            }
        }
        // Gauges
        {
            let map = self.gauges.read().unwrap_or_else(|e| e.into_inner());
            let mut by_name: BTreeMap<&str, Vec<(String, f64)>> = BTreeMap::new();
            for (k, v) in map.iter() {
                by_name
                    .entry(k.name())
                    .or_default()
                    .push((format_labels(k, None), v.get()));
            }
            for (name, mut rows) in by_name {
                rows.sort_by(|a, b| a.0.cmp(&b.0));
                if !out.is_empty() && !out.ends_with('\n') {
                    out.push('\n');
                }
                if let Some(help) = descs.get(name) {
                    let _ = writeln!(out, "# HELP {name} {help}");
                }
                let _ = writeln!(out, "# TYPE {name} gauge");
                for (labels, val) in rows {
                    let _ = write!(out, "{name}{labels} ");
                    write_f64(out, val);
                    out.push('\n');
                }
            }
        }
        // Histograms
        {
            let map = self.histograms.read().unwrap_or_else(|e| e.into_inner());
            let mut by_name: BTreeMap<&str, Vec<(&metrics::Key, String, &BucketHistogram)>> =
                BTreeMap::new();
            for (k, v) in map.iter() {
                by_name
                    .entry(k.name())
                    .or_default()
                    .push((k, format_labels(k, None), v.as_ref()));
            }
            for (name, mut rows) in by_name {
                rows.sort_by(|a, b| a.1.cmp(&b.1));
                if !out.is_empty() && !out.ends_with('\n') {
                    out.push('\n');
                }
                if let Some(help) = descs.get(name) {
                    let _ = writeln!(out, "# HELP {name} {help}");
                }
                let _ = writeln!(out, "# TYPE {name} histogram");
                for (key, base_labels, h) in rows {
                    let mut cumulative = 0u64;
                    for (idx, &bound) in h.bounds.iter().enumerate() {
                        cumulative += h.buckets[idx].load(Ordering::Relaxed);
                        let le = bound.to_string();
                        let labels = format_labels(key, Some(("le", &le)));
                        let _ = writeln!(out, "{name}_bucket{labels} {cumulative}");
                    }
                    cumulative += h.buckets[h.bounds.len()].load(Ordering::Relaxed);
                    let inf_labels = format_labels(key, Some(("le", "+Inf")));
                    let _ = writeln!(out, "{name}_bucket{inf_labels} {cumulative}");
                    let _ = write!(out, "{name}_sum{base_labels} ");
                    write_f64(out, f64::from_bits(h.sum_bits.load(Ordering::Relaxed)));
                    out.push('\n');
                    let _ = writeln!(
                        out,
                        "{name}_count{base_labels} {}",
                        h.count.load(Ordering::Relaxed)
                    );
                }
            }
        }
    }
}

pub(crate) struct HubRecorder {
    inner: PrometheusRecorder,
    dyn_series: Arc<DynSeries>,
}

#[derive(Clone)]
pub(crate) struct HubHandle {
    inner: PrometheusHandle,
    dyn_series: Arc<DynSeries>,
}

impl HubHandle {
    pub(crate) fn render(&self) -> String {
        let mut out = self.inner.render();
        self.dyn_series.render(&mut out);
        out
    }

    fn run_upkeep(&self) {
        self.inner.run_upkeep();
    }

    fn remove_label(&self, key: &str, value: &str) {
        self.dyn_series.remove_label(key, value);
    }
}

impl HubRecorder {
    pub(crate) fn handle(&self) -> HubHandle {
        HubHandle {
            inner: self.inner.handle(),
            dyn_series: self.dyn_series.clone(),
        }
    }
}

impl metrics::Recorder for HubRecorder {
    fn describe_counter(
        &self,
        key: metrics::KeyName,
        unit: Option<metrics::Unit>,
        description: metrics::SharedString,
    ) {
        self.dyn_series
            .descriptions
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .insert(key.as_str().to_owned(), description.as_ref().to_owned());
        self.inner.describe_counter(key, unit, description);
    }

    fn describe_gauge(
        &self,
        key: metrics::KeyName,
        unit: Option<metrics::Unit>,
        description: metrics::SharedString,
    ) {
        self.dyn_series
            .descriptions
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .insert(key.as_str().to_owned(), description.as_ref().to_owned());
        self.inner.describe_gauge(key, unit, description);
    }

    fn describe_histogram(
        &self,
        key: metrics::KeyName,
        unit: Option<metrics::Unit>,
        description: metrics::SharedString,
    ) {
        self.dyn_series
            .descriptions
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .insert(key.as_str().to_owned(), description.as_ref().to_owned());
        self.inner.describe_histogram(key, unit, description);
    }

    fn register_counter(
        &self,
        key: &metrics::Key,
        metadata: &metrics::Metadata<'_>,
    ) -> metrics::Counter {
        if !is_removable(key) {
            return self.inner.register_counter(key, metadata);
        }
        if let Some(c) = self
            .dyn_series
            .counters
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .get(key)
        {
            return metrics::Counter::from_arc(c.clone());
        }
        let c = self
            .dyn_series
            .counters
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .entry(key.clone())
            .or_insert_with(|| Arc::new(DynCounter(AtomicU64::new(0))))
            .clone();
        metrics::Counter::from_arc(c)
    }

    fn register_gauge(
        &self,
        key: &metrics::Key,
        metadata: &metrics::Metadata<'_>,
    ) -> metrics::Gauge {
        if !is_removable(key) {
            return self.inner.register_gauge(key, metadata);
        }
        if let Some(g) = self
            .dyn_series
            .gauges
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .get(key)
        {
            return metrics::Gauge::from_arc(g.clone());
        }
        let g = self
            .dyn_series
            .gauges
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .entry(key.clone())
            .or_insert_with(|| Arc::new(DynGauge::new()))
            .clone();
        metrics::Gauge::from_arc(g)
    }

    fn register_histogram(
        &self,
        key: &metrics::Key,
        metadata: &metrics::Metadata<'_>,
    ) -> metrics::Histogram {
        if !is_removable(key) {
            return self.inner.register_histogram(key, metadata);
        }
        if let Some(h) = self
            .dyn_series
            .histograms
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .get(key)
        {
            return metrics::Histogram::from_arc(h.clone());
        }
        let bounds = buckets_for(key.name());
        let h = self
            .dyn_series
            .histograms
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .entry(key.clone())
            .or_insert_with(|| Arc::new(BucketHistogram::new(bounds)))
            .clone();
        metrics::Histogram::from_arc(h)
    }
}

fn recorder() -> HubRecorder {
    // Explicit buckets give distributions without the exporter's time-window maintenance task.
    let mut b = PrometheusBuilder::new()
        .set_buckets(DEFAULT_BUCKETS)
        .expect("static nonempty buckets");
    for name in catalog::COUNTS {
        b = b
            .set_buckets_for_metric(Matcher::Full((*name).into()), COUNT_BUCKETS)
            .expect("static count buckets");
    }
    for name in catalog::LONG {
        b = b
            .set_buckets_for_metric(Matcher::Full((*name).into()), LONG_BUCKETS)
            .expect("static long buckets");
    }
    for name in catalog::TOKENS {
        b = b
            .set_buckets_for_metric(Matcher::Full((*name).into()), TOKEN_BUCKETS)
            .expect("static token buckets");
    }
    HubRecorder {
        inner: b.build_recorder(),
        dyn_series: Arc::new(DynSeries::default()),
    }
}

#[cfg(test)]
pub(crate) fn test_recorder() -> HubRecorder {
    recorder()
}

struct Installed {
    handle: HubHandle,
    started: Instant,
    history: Mutex<History>,
    backup_paths: Option<(PathBuf, PathBuf)>,
}

#[derive(Default)]
struct History {
    health: health::HealthHistory,
    nodes: nodes::History,
}

static INSTALLED: OnceLock<Installed> = OnceLock::new();

type PluginPidSource = Arc<dyn Fn() -> Vec<(String, u32)> + Send + Sync>;
static PLUGIN_PIDS: Mutex<Option<PluginPidSource>> = Mutex::new(None);
static SEEN_PLUGINS: Mutex<Option<HashSet<String>>> = Mutex::new(None);

/// Registers the running plugin PID provider so `sample_process` collects per-plugin CPU/RSS/IO.
pub(crate) fn set_plugin_pids(source: impl Fn() -> Vec<(String, u32)> + Send + Sync + 'static) {
    *PLUGIN_PIDS.lock().unwrap_or_else(|e| e.into_inner()) = Some(Arc::new(source));
}

/// Drops all in-memory Prometheus series and vitals history for a revoked Node.
pub(crate) fn remove_node(id: &str) {
    let Some(i) = INSTALLED.get() else { return };
    i.handle.remove_label("node", id);
    i.history
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .nodes
        .remove(id);
}

/// Drops all in-memory Prometheus series for an uninstalled/removed plugin.
pub(crate) fn remove_plugin(id: &str) {
    if let Some(seen) = SEEN_PLUGINS
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .as_mut()
    {
        seen.remove(id);
    }
    if let Some(i) = INSTALLED.get() {
        i.handle.remove_label("plugin", id);
    }
}

/// Installs the in-memory recorder, before anything that records starts. Collection is always on;
/// a second call (or another recorder already in place) changes nothing.
pub fn install() {
    install_recorder(None);
}

pub(crate) fn install_with_backup(home: PathBuf, database: PathBuf) {
    install_recorder(Some((home, database)));
}

fn install_recorder(backup_paths: Option<(PathBuf, PathBuf)>) {
    let recorder = recorder();
    let handle = recorder.handle();
    if let Err(e) = metrics::set_global_recorder(recorder) {
        tracing::warn!("metrics recorder unavailable: {e}");
        return;
    }
    catalog::describe();
    steve_store::instrumentation::enable_lock_metrics();
    steve_store::instrumentation::register_known_harnesses(steve_harness_spec::builtin_ids());
    let installed = INSTALLED.get_or_init(|| Installed {
        handle,
        started: Instant::now(),
        history: Mutex::default(),
        backup_paths,
    });
    // build_recorder starts no exporter upkeep: fold buffered histogram samples here.
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(UPKEEP_EVERY);
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            tick.tick().await;
            installed.handle.run_upkeep();
        }
    });
}

/// Capture one interval. The Node readings already carry their Hub receipt timestamps. Both the
/// typed history and exported gauges are updated from the same observations in this pass.
pub(crate) async fn record_minute(observations: Vec<nodes::Observation>) {
    let Some(i) = INSTALLED.get() else { return };
    if let Some((home, database)) = &i.backup_paths {
        backup::collect(home, database).await;
    }
    let now_ms = crate::nodes::now();
    let mut history = i.history.lock().unwrap_or_else(|e| e.into_inner());
    history.nodes.record(observations, now_ms);
    let text = i.render();
    let point = health::Snapshot::parse(&text, i.started.elapsed().as_secs_f64(), now_ms);
    history.health.push(point);
}

pub(crate) fn latest_node(id: &str, since: i64) -> Option<NodeVitalsLatest> {
    let history = INSTALLED
        .get()?
        .history
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    history.nodes.latest(id, since).cloned()
}

/// Live CPU, memory, and disk-write snapshots across all recently sampled Nodes, keyed by session id.
pub(crate) fn latest_session_vitals(
    since: i64,
) -> std::collections::HashMap<String, steve_types::devices::NodeSessionVitals> {
    let Some(i) = INSTALLED.get() else {
        return std::collections::HashMap::new();
    };
    let history = i.history.lock().unwrap_or_else(|e| e.into_inner());
    history.nodes.active_session_vitals(since)
}

pub fn node_days(
    ids: impl IntoIterator<Item = String>,
    now: i64,
    mut tokens: BTreeMap<String, (i64, i64)>,
    mut costs: BTreeMap<String, (f64, f64)>,
) -> BTreeMap<String, NodeVitalsDay> {
    let Some(i) = INSTALLED.get() else {
        return BTreeMap::new();
    };
    let history = i.history.lock().unwrap_or_else(|e| e.into_inner());
    ids.into_iter()
        .filter_map(|id| {
            let (per_minute, today) = tokens.remove(&id).unwrap_or_default();
            let (total_cost_usd, today_cost_usd) = costs.remove(&id).unwrap_or_default();
            let day = history.nodes.day(
                &id,
                now,
                VitalsTokens {
                    per_minute,
                    today,
                    total_cost_usd,
                    today_cost_usd,
                },
            )?;
            Some((id, day))
        })
        .collect()
}

impl Installed {
    fn render(&self) -> String {
        sample_process();
        self.handle.render()
    }

    fn snapshot(&self) -> health::Snapshot {
        health::Snapshot::parse(
            &self.render(),
            self.started.elapsed().as_secs_f64(),
            crate::nodes::now(),
        )
    }
}

/// The exposition text now (this process sampled first); `None` before [`install`].
pub fn render() -> Option<String> {
    INSTALLED.get().map(Installed::render)
}

/// The health of the selected window (since the start until a window has passed).
pub(crate) fn health(window: health::HealthWindow) -> Option<health::Health> {
    let i = INSTALLED.get()?;
    {
        let history = i.history.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(report) = history.health.report(window) {
            return Some(report);
        }
    }
    Some(health::assess_single(&i.snapshot(), window))
}

/// The pages a web report may name (a bounded label covering every nav destination plus detail
/// views); anything else is `other`.
pub(crate) const WEB_PAGES: [&str; 28] = [
    "chat",
    "inbox",
    "items",
    "item",
    "projects",
    "calendar",
    "todos",
    "feed",
    "channels",
    "mail",
    "contacts",
    "knowledge",
    "integrations",
    "bots",
    "sessions",
    "session",
    "auto",
    "capabilities",
    "signals",
    "stats",
    "me",
    "devices",
    "accounts",
    "vault",
    "settings",
    "publications",
    "github",
    "other",
];

/// Bounded JS error kinds accepted for `steve_web_errors_total`.
pub(crate) const WEB_ERROR_KINDS: [&str; 2] = ["error", "unhandledrejection"];
/// Bounded SSE reconnect reasons accepted for `steve_web_sse_reconnects_total`.
pub(crate) const WEB_SSE_RECONNECT_REASONS: [&str; 3] = ["error", "stale", "wake"];

/// Bounded menu item keys accepted for `steve_web_nav_opens_total` (`POST /api/system/web-perf`).
pub(crate) const WEB_NAV_ITEMS: &[&str] = &[
    "chat",
    "inbox",
    "items",
    "projects",
    "calendar",
    "todos",
    "feed",
    "channels",
    "mail",
    "contacts",
    "knowledge",
    "integrations",
    "bots",
    "sessions",
    "auto",
    "capabilities",
    "signals",
    "stats",
    "me",
    "devices",
    "accounts",
    "vault",
    "settings",
    "publications",
    "github",
];

/// What a web client measured on `page` since its last report (`POST /api/system/web-perf`): its
/// long tasks, the latency of each interaction and of merging each patch frame, in seconds, plus
/// menu item open counts (`steve_web_nav_opens_total`), JS error counts, and SSE reconnect counts.
/// Returns the bounded nav opens recorded so the caller can also aggregate them by workspace local
/// day in `telemetry_daily`.
#[allow(clippy::too_many_arguments)]
pub(crate) fn web_report(
    page: &str,
    long_tasks: &[f64],
    interactions: &[f64],
    patches: &[f64],
    snapshots: &[f64],
    transcripts: &[f64],
    dom_nodes: Option<u64>,
    nav_opens: &BTreeMap<String, u32>,
    errors: &BTreeMap<String, u32>,
    sse_reconnects: &BTreeMap<String, u32>,
) -> Vec<(&'static str, u32)> {
    let page = WEB_PAGES
        .into_iter()
        .find(|p| *p == page)
        .unwrap_or("other");
    metrics::counter!("steve_web_reports_total").increment(1);
    if let Some(n) = dom_nodes {
        metrics::gauge!("steve_web_dom_nodes", "page" => page).set(n as f64);
    }
    for (name, xs) in [
        ("steve_web_long_task_seconds", long_tasks),
        ("steve_web_interaction_seconds", interactions),
        ("steve_web_patch_apply_seconds", patches),
        ("steve_web_snapshot_seconds", snapshots),
        ("steve_web_transcript_open_seconds", transcripts),
    ] {
        if xs.is_empty() {
            continue;
        }
        let h = metrics::histogram!(name, "page" => page);
        xs.iter().for_each(|s| h.record(*s));
    }
    let mut accepted = Vec::new();
    for (item, &count) in nav_opens {
        if count == 0 {
            continue;
        }
        if let Some(&known) = WEB_NAV_ITEMS.iter().find(|k| **k == item.as_str()) {
            metrics::counter!("steve_web_nav_opens_total", "item" => known)
                .increment(u64::from(count));
            accepted.push((known, count));
        }
    }
    for (kind, &count) in errors {
        if count == 0 {
            continue;
        }
        if let Some(&known) = WEB_ERROR_KINDS.iter().find(|k| **k == kind.as_str()) {
            metrics::counter!("steve_web_errors_total", "page" => page, "kind" => known)
                .increment(u64::from(count));
        }
    }
    for (reason, &count) in sse_reconnects {
        if count == 0 {
            continue;
        }
        if let Some(&known) = WEB_SSE_RECONNECT_REASONS
            .iter()
            .find(|r| **r == reason.as_str())
        {
            metrics::counter!("steve_web_sse_reconnects_total", "reason" => known)
                .increment(u64::from(count));
        }
    }
    accepted
}

/// This process's and running plugin child processes' resource usage, sampled when somebody reads
/// the metrics.
fn sample_process() {
    sample_plugins();
    let Some(u) = steve_runtime::process::this() else {
        metrics::counter!("steve_metrics_collection_errors_total", "source" => "process")
            .increment(1);
        return;
    };
    metrics::gauge!("steve_process_cpu_seconds").set(u.cpu_seconds);
    metrics::gauge!("steve_process_resident_bytes").set(u.resident_bytes as f64);
    for (gauge, v) in [
        (metrics::gauge!("steve_process_open_fds"), u.open_fds),
        (metrics::gauge!("steve_process_threads"), u.threads),
        (
            metrics::gauge!("steve_process_disk_read_bytes"),
            u.disk_read_bytes,
        ),
        (
            metrics::gauge!("steve_process_disk_written_bytes"),
            u.disk_written_bytes,
        ),
    ] {
        if let Some(v) = v {
            gauge.set(v as f64);
        }
    }
}

fn sample_plugins() {
    let source = PLUGIN_PIDS
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clone();
    let Some(source) = source else { return };
    let pids = source();
    let mut active: BTreeMap<String, (usize, Option<steve_runtime::process::Usage>)> =
        BTreeMap::new();
    for (plugin, pid) in pids {
        let entry = active.entry(plugin).or_insert((0, None));
        entry.0 += 1;
        if let Some(usage) = steve_runtime::process::of_pid(pid) {
            entry.1 = Some(match entry.1.take() {
                Some(sum) => sum.add(usage),
                None => usage,
            });
        }
    }
    let mut seen_guard = SEEN_PLUGINS.lock().unwrap_or_else(|e| e.into_inner());
    let seen = seen_guard.get_or_insert_with(HashSet::new);
    for (plugin, (count, usage)) in &active {
        seen.insert(plugin.clone());
        let p = plugin.clone();
        metrics::gauge!("steve_plugin_processes", "plugin" => p.clone()).set(*count as f64);
        if let Some(u) = usage {
            metrics::gauge!("steve_plugin_process_cpu_seconds", "plugin" => p.clone())
                .set(u.cpu_seconds);
            metrics::gauge!("steve_plugin_process_resident_bytes", "plugin" => p.clone())
                .set(u.resident_bytes as f64);
            for (name, v) in [
                ("steve_plugin_process_open_fds", u.open_fds),
                ("steve_plugin_process_threads", u.threads),
                ("steve_plugin_process_disk_read_bytes", u.disk_read_bytes),
                (
                    "steve_plugin_process_disk_written_bytes",
                    u.disk_written_bytes,
                ),
            ] {
                if let Some(v) = v {
                    metrics::gauge!(name, "plugin" => p.clone()).set(v as f64);
                }
            }
        }
    }
    let gone: Vec<String> = seen
        .iter()
        .filter(|plugin| !active.contains_key(*plugin))
        .cloned()
        .collect();
    for plugin in gone {
        seen.remove(&plugin);
        metrics::gauge!("steve_plugin_processes", "plugin" => plugin.clone()).set(0.0);
        for name in [
            "steve_plugin_process_cpu_seconds",
            "steve_plugin_process_resident_bytes",
            "steve_plugin_process_open_fds",
            "steve_plugin_process_threads",
            "steve_plugin_process_disk_read_bytes",
            "steve_plugin_process_disk_written_bytes",
        ] {
            metrics::gauge!(name, "plugin" => plugin.clone()).set(0.0);
        }
    }
}

fn node_figures(node: &str, latest: &NodeVitalsLatest, point: NodeVitalsPoint) {
    let node = node.to_string();
    let set = |name: &'static str, v: Option<f64>| {
        // An unavailable value must not leave the previous gauge looking current.
        metrics::gauge!(name, "node" => node.clone()).set(v.unwrap_or(f64::NAN));
    };
    set(
        "steve_node_sample_timestamp_seconds",
        Some(latest.at as f64 / 1_000.0),
    );
    set("steve_node_cpu_load_ratio", point.load);
    set(
        "steve_node_memory_total_bytes",
        latest.mem_total.map(|n| n as f64),
    );
    set(
        "steve_node_memory_available_bytes",
        latest.mem_available.map(|n| n as f64),
    );
    set("steve_node_memory_used_ratio", point.mem);
    set(
        "steve_node_rtt_seconds",
        point.rtt.map(|ms| ms as f64 / 1_000.0),
    );
    set(
        "steve_node_network_received_bytes_per_second",
        point.network.map(|v| v.rx_bytes_per_second),
    );
    set(
        "steve_node_network_sent_bytes_per_second",
        point.network.map(|v| v.tx_bytes_per_second),
    );
    let process_written = latest
        .process
        .as_ref()
        .and_then(|p| p.disk_written_bytes)
        .unwrap_or(0);
    let total_written = process_written.saturating_add(latest.adapter_disk_written_bytes);
    set("steve_node_disk_written_bytes", Some(total_written as f64));
    set(
        "steve_node_adapter_disk_written_bytes",
        Some(latest.adapter_disk_written_bytes as f64),
    );
}

/// A Node's own meters, as its vitals brought them. `own_process`: the Node runs in another process
/// (in the Hub's own, its process figures are the Hub's and are not repeated).
pub(crate) fn node_meters(node: &str, m: &steve_node_api::NodeMeters, own_process: bool) {
    let node = node.to_string();
    let set = |name: &'static str, v: f64| metrics::gauge!(name, "node" => node.clone()).set(v);
    set("steve_node_journal_bytes", m.journal_bytes as f64);
    set("steve_node_journal_file_bytes", m.journal_file_bytes as f64);
    set("steve_node_journal_commits", m.journal_commits as f64);
    set("steve_node_event_backlog", m.event_backlog as f64);
    set(
        "steve_node_event_backlog_bytes",
        m.event_backlog_bytes as f64,
    );
    set("steve_node_adapters", f64::from(m.adapters));
    set("steve_node_adapter_cpu_seconds", m.adapter_cpu_seconds);
    set(
        "steve_node_adapter_resident_bytes",
        m.adapter_resident_bytes as f64,
    );
    let network = |bytes: fn(&steve_node_api::NetworkBytes) -> u64| {
        if m.network.is_empty() {
            f64::NAN
        } else {
            m.network.values().map(|n| bytes(n) as f64).sum()
        }
    };
    set("steve_node_network_received_bytes", network(|n| n.received));
    set("steve_node_network_sent_bytes", network(|n| n.sent));
    let Some(p) = m.process.as_ref().filter(|_| own_process) else {
        return;
    };
    set("steve_node_process_cpu_seconds", p.cpu_seconds);
    set("steve_node_process_resident_bytes", p.resident_bytes as f64);
    for (name, v) in [
        ("steve_node_process_open_fds", p.open_fds),
        ("steve_node_process_threads", p.threads),
        ("steve_node_process_disk_read_bytes", p.disk_read_bytes),
        (
            "steve_node_process_disk_written_bytes",
            p.disk_written_bytes,
        ),
    ] {
        if let Some(v) = v {
            set(name, v as f64);
        }
    }
}

/// Serves the configured export, independently of the business listener. An export that cannot
/// start never stops the Hub; collection goes on regardless.
pub async fn start(st: crate::api::AppState) -> Option<String> {
    let Some(listen) = &st.cfg.metrics else {
        return None;
    };
    let Some(modules) = &st.modules else {
        tracing::warn!("metrics export unavailable: no access store");
        return None;
    };
    let Some(handle) = INSTALLED.get().map(|i| i.handle.clone()) else {
        tracing::warn!("metrics export unavailable: no recorder");
        return None;
    };
    enum Bound {
        Tcp(tokio::net::TcpListener),
        Unix(tokio::net::UnixListener),
    }
    let listener = match listen {
        Listen::Tcp(addr) => tokio::net::TcpListener::bind(addr).await.map(Bound::Tcp),
        Listen::Unix(path) => unix_listener(path).map(Bound::Unix),
    };
    let listener = match listener {
        Ok(listener) => listener,
        Err(e) => {
            tracing::warn!("metrics listener unavailable: {e}");
            return None;
        }
    };
    let addr = match &listener {
        Bound::Tcp(listener) => listener.local_addr().ok()?.to_string(),
        Bound::Unix(listener) => format!(
            "unix:{}",
            listener.local_addr().ok()?.as_pathname()?.display()
        ),
    };
    let app = router(modules.store.clone(), handle);
    tokio::spawn(async move {
        let result = match listener {
            Bound::Tcp(listener) => axum::serve(listener, app).await,
            Bound::Unix(listener) => axum::serve(listener, app).await,
        };
        if let Err(e) = result {
            tracing::warn!("metrics listener stopped: {e}");
        }
    });
    Some(addr)
}

fn unix_listener(path: &std::path::Path) -> std::io::Result<tokio::net::UnixListener> {
    use std::os::unix::fs::PermissionsExt;
    let listener = tokio::net::UnixListener::bind(path)?;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    Ok(listener)
}

#[derive(Clone)]
struct Endpoint {
    store: Store,
    handle: HubHandle,
}

fn router(store: Store, handle: HubHandle) -> Router {
    Router::new()
        .route("/metrics", get(scrape))
        .with_state(Endpoint { store, handle })
}

async fn scrape(State(st): State<Endpoint>, headers: HeaderMap) -> Response {
    let Some(credential) = crate::access::bearer(&headers) else {
        return StatusCode::UNAUTHORIZED.into_response();
    };
    // Same credential validation as api::lookup_user; don't mark a client seen (which writes).
    let store = st.store.clone();
    let user = crate::api::blocking(move || {
        store
            .access()
            .client_user(&credential)
            .map_err(|e| crate::error::ApiError::internal(e.to_string()))
    })
    .await;
    match user {
        Ok(Some((user, _))) if user.role == "owner" => {}
        Ok(Some(_)) => return StatusCode::FORBIDDEN.into_response(),
        Ok(None) => return StatusCode::UNAUTHORIZED.into_response(),
        Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
    }
    sample_process();
    ([(header::CONTENT_TYPE, CONTENT_TYPE)], st.handle.render()).into_response()
}

struct HttpMetrics {
    requests: metrics::Counter,
    duration: metrics::Histogram,
    bytes: metrics::Counter,
}

/// Records HTTP request metrics without cloning `Arc` handles on the hot exact-size path.
/// Returns `Some(bytes_counter)` only for streaming bodies whose byte length is not known upfront.
fn record_http(
    route: &str,
    method: &'static str,
    status: &'static str,
    elapsed_secs: f64,
    exact_bytes: Option<u64>,
) -> Option<metrics::Counter> {
    // Only cold route registration allocates labels. MatchedPath never contains an entity ID.
    thread_local! {
        static HANDLES: std::cell::RefCell<HashMap<String, HashMap<(&'static str, &'static str), HttpMetrics>>> =
            std::cell::RefCell::new(HashMap::new());
    }
    HANDLES.with(|cache| {
        let mut cache = cache.borrow_mut();
        let by_route = match cache.get_mut(route) {
            Some(m) => m,
            None => cache.entry(route.to_owned()).or_default(),
        };
        let handles = by_route.entry((method, status)).or_insert_with(|| HttpMetrics {
            requests: metrics::counter!("steve_http_requests_total", "route" => route.to_owned(), "method" => method, "status" => status),
            duration: metrics::histogram!("steve_http_request_duration_seconds", "route" => route.to_owned(), "method" => method, "status" => status),
            bytes: metrics::counter!("steve_http_response_bytes_total", "route" => route.to_owned(), "method" => method, "status" => status),
        });
        handles.requests.increment(1);
        handles.duration.record(elapsed_secs);
        match exact_bytes {
            Some(n) => {
                handles.bytes.increment(n);
                None
            }
            None => Some(handles.bytes.clone()),
        }
    })
}

/// Zero-copy streaming body wrapper that counts data frame bytes while preserving trailers and errors.
struct CountedBody {
    inner: Body,
    bytes: metrics::Counter,
}

impl hyper::body::Body for CountedBody {
    type Data = Bytes;
    type Error = axum::Error;

    fn poll_frame(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Self::Data>, Self::Error>>> {
        let poll = Pin::new(&mut self.inner).poll_frame(cx);
        if let Poll::Ready(Some(Ok(ref frame))) = poll
            && let Some(data) = frame.data_ref()
        {
            self.bytes.increment(data.len() as u64);
        }
        poll
    }

    fn is_end_stream(&self) -> bool {
        self.inner.is_end_stream()
    }

    fn size_hint(&self) -> SizeHint {
        self.inner.size_hint()
    }
}

/// The header with the newest bus revision a request committed that can change a view.
pub const REV_HEADER: &str = "x-steve-rev";

/// Outermost request observation, including CORS/maintenance/auth short responses. Latency
/// ends at response headers; body bytes count exact sizes without wrapping and stream chunks
/// without losing trailers or errors.
pub async fn http(request: Request, next: Next) -> Response {
    let route = request.extensions().get::<MatchedPath>().cloned();
    // `mcp_gate` intercepts `/mcp*` ahead of axum's Router, so `MatchedPath` is absent there.
    let is_mcp = route.is_none() && request.uri().path().starts_with("/mcp");
    let method = match request.method().as_str() {
        "GET" => "GET",
        "POST" => "POST",
        "PUT" => "PUT",
        "DELETE" => "DELETE",
        "PATCH" => "PATCH",
        "HEAD" => "HEAD",
        "OPTIONS" => "OPTIONS",
        _ => "OTHER",
    };
    let start = Instant::now();
    let calls = steve_store::instrumentation::RequestCalls::default();
    let mut response = calls.scope(next.run(request)).await;
    // write then read: the newest revision this request committed that can change a view, so a
    // client can wait for the patch that is at least as new (`crate::projection`)
    if calls.rev() > 0 {
        response
            .headers_mut()
            .insert(REV_HEADER, axum::http::HeaderValue::from(calls.rev()));
    }
    let route = route
        .as_ref()
        .map(MatchedPath::as_str)
        .unwrap_or(if is_mcp { "/mcp" } else { "unmatched" });
    if response
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| {
            v.split(';')
                .next()
                .is_some_and(|v| v.trim() == "text/event-stream")
        })
    {
        inc_sse_connections(route);
        return response;
    }
    let status = match response.status().as_u16() / 100 {
        1 => "1xx",
        2 => "2xx",
        3 => "3xx",
        4 => "4xx",
        _ => "5xx",
    };
    record_store_calls(route, calls.count() as f64);
    let exact_bytes = response.body().size_hint().exact();
    let Some(bytes) = record_http(
        route,
        method,
        status,
        start.elapsed().as_secs_f64(),
        exact_bytes,
    ) else {
        return response;
    };
    let (parts, body) = response.into_parts();
    Response::from_parts(parts, Body::new(CountedBody { inner: body, bytes }))
}

/// Blocking API work keeps the request's storage counter alongside its existing language context.
pub(crate) fn carry<R>(f: impl FnOnce() -> R) -> impl FnOnce() -> R {
    steve_store::instrumentation::carry(f)
}

fn record_store_calls(route: &str, count: f64) {
    thread_local! {
        static HANDLES: std::cell::RefCell<HashMap<String, metrics::Histogram>> =
            std::cell::RefCell::new(HashMap::new());
    }
    HANDLES.with(|cache| {
        let mut cache = cache.borrow_mut();
        if let Some(handle) = cache.get(route) {
            handle.record(count);
            return;
        }
        let handle =
            metrics::histogram!("steve_http_store_calls_per_request", "route" => route.to_owned());
        handle.record(count);
        cache.insert(route.to_owned(), handle);
    });
}

fn inc_sse_connections(route: &str) {
    thread_local! {
        static HANDLES: std::cell::RefCell<HashMap<String, metrics::Counter>> =
            std::cell::RefCell::new(HashMap::new());
    }
    HANDLES.with(|cache| {
        let mut cache = cache.borrow_mut();
        if let Some(handle) = cache.get(route) {
            handle.increment(1);
            return;
        }
        let handle =
            metrics::counter!("steve_http_sse_connections_total", "route" => route.to_owned());
        handle.increment(1);
        cache.insert(route.to_owned(), handle);
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::Request as HttpRequest;
    use http_body_util::BodyExt;
    use steve_store::{
        NoPublisher,
        access::{NewClient, credential},
    };
    use tower::ServiceExt;

    fn runtime() -> tokio::runtime::Runtime {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
    }

    #[test]
    fn upkeep_retains_bucket_counts_across_unscraped_intervals() {
        let recorder = recorder();
        let handle = recorder.handle();
        metrics::with_local_recorder(&recorder, || {
            let histogram = metrics::histogram!("steve_lock_wait_seconds", "lock" => "store", "caller" => "upkeep-fixture:1");
            for _ in 0..3 {
                histogram.record(0.0025);
            }
            handle.run_upkeep();
            handle.run_upkeep();
            for _ in 0..2 {
                histogram.record(0.005);
            }
            handle.run_upkeep();
        });
        let text = handle.render();
        for (prefix, labels, value) in [
            ("steve_lock_wait_seconds_count{", None, " 5"),
            (
                "steve_lock_wait_seconds_bucket{",
                Some("le=\"0.0025\""),
                " 3",
            ),
            (
                "steve_lock_wait_seconds_bucket{",
                Some("le=\"0.005\""),
                " 5",
            ),
        ] {
            assert!(
                text.lines().any(|line| line.starts_with(prefix)
                    && line.contains("caller=\"upkeep-fixture:1\"")
                    && labels.is_none_or(|label| line.contains(label))
                    && line.ends_with(value)),
                "{text}"
            );
        }
    }

    #[test]
    fn addresses_are_loopback_or_absolute_unix_paths() {
        for address in [
            "127.0.0.1:9464",
            "[::1]:9464",
            "unix:/tmp/metrics-test.sock",
        ] {
            assert!(Listen::parse(address).is_ok(), "{address}");
        }
        for address in [
            "0.0.0.0:9464",
            "[::]:9464",
            "192.0.2.1:9464",
            "localhost:9464",
            "unix:",
            "unix:relative",
            "unix:/",
        ] {
            assert!(Listen::parse(address).is_err(), "{address}");
        }
    }

    #[test]
    fn unix_socket_is_private_and_existing_files_are_not_replaced() {
        runtime().block_on(async {
            use std::os::unix::fs::PermissionsExt;
            let tmp = tempfile::tempdir().unwrap();
            let path = tmp.path().join("metrics.sock");
            let listener = unix_listener(&path).unwrap();
            assert_eq!(path.metadata().unwrap().permissions().mode() & 0o777, 0o600);
            assert!(unix_listener(&path).is_err());
            drop(listener);
            let regular = tmp.path().join("regular");
            std::fs::write(&regular, "keep").unwrap();
            assert!(unix_listener(&regular).is_err());
            assert_eq!(std::fs::read_to_string(regular).unwrap(), "keep");
        });
    }

    #[test]
    fn scrape_requires_an_owner_bearer_credential_without_writing() {
        let tmp = tempfile::tempdir().unwrap();
        let store = Store::open(tmp.path().join("steve.sqlite3"), NoPublisher).unwrap();
        store.ensure_workspace().unwrap();
        let tokens: Vec<_> = ["owner", "member"]
            .iter()
            .map(|role| {
                let user = store.users().create(role, role, None, role).unwrap();
                let (_, client, secret) = store
                    .access()
                    .client_create(&user, None, &NewClient::default())
                    .unwrap()
                    .unwrap();
                credential(&client.id, &secret)
            })
            .collect();
        let recorder = recorder();
        let handle = recorder.handle();
        let app = router(store.clone(), handle);
        let before = steve_store::testkit::hub::writer_changes(&store).unwrap();
        metrics::with_local_recorder(&recorder, || {
            runtime().block_on(async {
                for (token, expected) in [
                    (None, 401),
                    (Some("invalid"), 401),
                    (Some(tokens[1].as_str()), 403),
                    (Some(tokens[0].as_str()), 200),
                ] {
                    let mut request = HttpRequest::builder().uri("/metrics");
                    if let Some(token) = token {
                        request = request.header(header::AUTHORIZATION, format!("Bearer {token}"));
                    }
                    let response = app
                        .clone()
                        .oneshot(request.body(Body::empty()).unwrap())
                        .await
                        .unwrap();
                    assert_eq!(response.status().as_u16(), expected);
                    if expected == 200 {
                        assert!(
                            response.headers()[header::CONTENT_TYPE]
                                .to_str()
                                .unwrap()
                                .contains("version=0.0.4")
                        );
                        let text = String::from_utf8(
                            response
                                .into_body()
                                .collect()
                                .await
                                .unwrap()
                                .to_bytes()
                                .to_vec(),
                        )
                        .unwrap();
                        assert!(text.contains("steve_process_disk_written_bytes"), "{text}");
                    }
                }
                let response = app
                    .oneshot(
                        HttpRequest::builder()
                            .uri("/metrics")
                            .header(
                                header::COOKIE,
                                format!("{}={}", crate::access::COOKIE, tokens[0]),
                            )
                            .body(Body::empty())
                            .unwrap(),
                    )
                    .await
                    .unwrap();
                assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
            })
        });
        assert_eq!(
            steve_store::testkit::hub::writer_changes(&store).unwrap(),
            before
        );
    }

    #[test]
    fn http_uses_templates_counts_actual_bytes_and_keeps_sse_out_of_latency() {
        let recorder = recorder();
        let handle = recorder.handle();
        metrics::with_local_recorder(&recorder, || {
            runtime().block_on(async {
                let app = Router::new()
                    .route(
                        "/metrics-items/{id}",
                        get(|| async { (StatusCode::CREATED, "hello") }),
                    )
                    .route(
                        "/metrics-events",
                        get(|| async {
                            (
                                [(header::CONTENT_TYPE, "text/event-stream; charset=utf-8")],
                                "data: hi\n\n",
                            )
                        }),
                    )
                    .layer(axum::middleware::from_fn(http));
                for path in [
                    "/metrics-items/private-id",
                    "/metrics-events",
                    "/missing-private-id",
                ] {
                    let response = app
                        .clone()
                        .oneshot(
                            HttpRequest::builder()
                                .uri(path)
                                .body(Body::empty())
                                .unwrap(),
                        )
                        .await
                        .unwrap();
                    response.into_body().collect().await.unwrap();
                }
            })
        });
        let text = handle.render();
        for (name, value) in [
            ("steve_http_requests_total", 1),
            ("steve_http_response_bytes_total", 5),
        ] {
            assert!(
                text.lines()
                    .any(|line| line.starts_with(&format!("{name}{{"))
                        && line.contains("route=\"/metrics-items/{id}\"")
                        && line.contains("method=\"GET\"")
                        && line.contains("status=\"2xx\"")
                        && line.ends_with(&format!("}} {value}"))),
                "{text}"
            );
        }
        assert!(
            text.contains("steve_http_request_duration_seconds_bucket")
                && text.contains("le=\"0.0025\"")
                && text.contains("le=\"0.000025\""),
            "{text}"
        );
        assert!(
            text.contains("steve_http_sse_connections_total{route=\"/metrics-events\"} 1"),
            "{text}"
        );
        assert!(
            text.lines().any(|line| line.contains("route=\"unmatched\"") && line.contains("status=\"4xx\"")),
            "{text}"
        );
        assert!(!text.contains("private-id"), "{text}");
        assert!(
            !text.lines().any(
                |line| (line.contains("duration") || line.contains("store_calls"))
                    && line.contains("/metrics-events")
            ),
            "{text}"
        );
    }

    #[test]
    fn declared_trailers_and_body_errors_preserve_the_existing_response() {
        let recorder = recorder();
        metrics::with_local_recorder(&recorder, || {
            runtime().block_on(async {
                let app = Router::new()
                    .route(
                        "/metrics-trailers",
                        get(|| async {
                            let mut trailers = HeaderMap::new();
                            trailers.insert("x-check", "kept".parse().unwrap());
                            let body =
                                http_body_util::Full::new(axum::body::Bytes::from_static(b"data"))
                                    .with_trailers(async { Some(Ok(trailers)) });
                            ([(header::TRAILER, "x-check")], Body::new(body))
                        }),
                    )
                    .route(
                        "/metrics-body-error",
                        get(|| async {
                            Body::from_stream(futures::stream::iter([
                                Ok(axum::body::Bytes::from_static(b"part")),
                                Err(std::io::Error::other("fixture body error")),
                            ]))
                        }),
                    )
                    .layer(axum::middleware::from_fn(http));
                let response = app
                    .clone()
                    .oneshot(
                        HttpRequest::builder()
                            .uri("/metrics-trailers")
                            .body(Body::empty())
                            .unwrap(),
                    )
                    .await
                    .unwrap();
                let body = response.into_body().collect().await.unwrap();
                assert_eq!(body.trailers().unwrap()["x-check"], "kept");
                assert_eq!(body.to_bytes(), "data");
                let response = app
                    .oneshot(
                        HttpRequest::builder()
                            .uri("/metrics-body-error")
                            .body(Body::empty())
                            .unwrap(),
                    )
                    .await
                    .unwrap();
                assert!(response.into_body().collect().await.is_err());
            })
        });
        let text = recorder.handle().render();
        assert!(
            text.lines()
                .any(|line| line.starts_with("steve_http_response_bytes_total{")
                    && line.contains("route=\"/metrics-trailers\"")
                    && line.ends_with(" 4")),
            "{text}"
        );
        assert!(
            text.lines()
                .any(|line| line.starts_with("steve_http_response_bytes_total{")
                    && line.contains("route=\"/metrics-body-error\"")
                    && line.ends_with(" 4")),
            "{text}"
        );
        assert!(!text.contains("source=\"response_trailers\""), "{text}");
    }

    #[test]
    fn mcp_routes_without_matched_path_use_bounded_mcp_label_and_revoked_node_series_are_removed() {
        let recorder = recorder();
        let handle = recorder.handle();
        metrics::with_local_recorder(&recorder, || {
            runtime().block_on(async {
                let app = Router::new()
                    .fallback(|| async { (StatusCode::OK, "ok") })
                    .layer(axum::middleware::from_fn(http));
                let res = app
                    .oneshot(
                        HttpRequest::builder()
                            .uri("/mcp/tools/secret-id")
                            .body(Body::empty())
                            .unwrap(),
                    )
                    .await
                    .unwrap();
                res.into_body().collect().await.unwrap();
            });
            metrics::gauge!("steve_node_link_up", "node" => "n-revoked").set(1.0);
            metrics::counter!("steve_node_commands_total", "node" => "n-revoked", "method" => "ping", "result" => "ok").increment(3);
            metrics::histogram!("steve_node_command_seconds", "node" => "n-revoked", "method" => "ping").record(0.05);
            metrics::gauge!("steve_node_link_up", "node" => "n-keep").set(1.0);
        });
        let before = handle.render();
        assert!(before.contains("route=\"/mcp\""), "{before}");
        assert!(!before.contains("secret-id"), "{before}");
        assert!(before.contains("node=\"n-revoked\""), "{before}");
        assert!(
            before
                .contains("steve_node_command_seconds_count{method=\"ping\",node=\"n-revoked\"} 1")
                || before.contains(
                    "steve_node_command_seconds_count{node=\"n-revoked\",method=\"ping\"} 1"
                ),
            "{before}"
        );
        handle.remove_label("node", "n-revoked");
        let after = handle.render();
        assert!(
            !after.contains("n-revoked"),
            "revoked node series must be removed: {after}"
        );
        assert!(
            after.contains("steve_node_link_up{node=\"n-keep\"} 1"),
            "{after}"
        );
    }

    #[test]
    fn request_counts_follow_blocking_and_do_not_leak_between_requests() {
        use steve_store::instrumentation::{RequestCalls, enable_lock_metrics};
        enable_lock_metrics();
        let tmp = tempfile::tempdir().unwrap();
        let store = Store::open(tmp.path().join("steve.sqlite3"), NoPublisher).unwrap();
        let first = RequestCalls::default();
        let second = RequestCalls::default();
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .max_blocking_threads(1)
            .build()
            .unwrap();
        runtime.block_on(async {
            let barrier = tokio::sync::Barrier::new(2);
            let a = first.scope(async {
                store
                    .read(|_| Ok::<_, steve_store::StoreError>(()))
                    .unwrap();
                tokio::task::yield_now().await;
                store
                    .write::<_, steve_store::StoreError>(|_| Ok(()))
                    .unwrap();
                barrier.wait().await;
                let store = store.clone();
                crate::api::blocking(move || {
                    store.users().count().unwrap();
                    store
                        .write(|w| {
                            // A read and a savepoint share the writer's existing lock.
                            store.read(|_| Ok::<_, steve_store::StoreError>(()))?;
                            w.scope(|_| Ok::<_, steve_store::StoreError>(()))
                        })
                        .unwrap();
                    Ok(())
                })
                .await
                .unwrap();
            });
            let b = second.scope(async {
                barrier.wait().await;
                let store = store.clone();
                crate::api::blocking(move || {
                    store
                        .write::<_, steve_store::StoreError>(|_| Ok(()))
                        .unwrap();
                    Ok(())
                })
                .await
                .unwrap();
            });
            tokio::join!(a, b);
            // The one blocking worker must have restored its context after both carried requests.
            let outside = store.clone();
            crate::api::blocking(move || {
                outside.users().count().unwrap();
                Ok(())
            })
            .await
            .unwrap();
            let uncarried = RequestCalls::default();
            let store = store.clone();
            uncarried
                .scope(async {
                    tokio::task::spawn_blocking(move || {
                        store
                            .write::<_, steve_store::StoreError>(|_| Ok(()))
                            .unwrap()
                    })
                    .await
                    .unwrap();
                })
                .await;
            assert_eq!(
                uncarried.count(),
                0,
                "raw spawn_blocking does not inherit request context"
            );
        });
        assert_eq!(first.count(), 4);
        assert_eq!(second.count(), 1);
    }

    #[test]
    fn full_router_records_short_responses_and_repeatable_write_store_counts() {
        use std::sync::Arc;
        steve_store::instrumentation::enable_lock_metrics();
        let tmp = tempfile::tempdir().unwrap();
        let store = Store::open(tmp.path().join("steve.sqlite3"), NoPublisher).unwrap();
        store.ensure_workspace().unwrap();
        let owner = store
            .users()
            .create("fixture", "Owner", None, "owner")
            .unwrap();
        store
            .auth_sessions()
            .create(&owner, "fixture-session", 60_000)
            .unwrap();
        let cfg = crate::config::Config::from_vars(|key| match key {
            "STEVE_HOME" | "HOME" => Some(tmp.path().to_string_lossy().into_owned()),
            "STEVE_PORT" => Some("18710".into()),
            "STEVE_NO_RESUME" => Some("1".into()),
            "STEVE_CLIENT_ORIGINS" => Some("http://client.example".into()),
            key if key.starts_with("STEVE_ADAPTER_") => Some("/usr/bin/false".into()),
            _ => None,
        })
        .unwrap();
        let cfg = Arc::new(cfg);
        let bus = crate::bus::Bus::new(std::sync::Arc::new(steve_runtime::cache::Caches::new(
            Default::default(),
        )));
        let st = crate::api::AppState::new(
            cfg.clone(),
            crate::wire::store::StoreBackend(store.clone()),
            crate::wire::core::CoreDomain::for_test(&cfg, store.clone(), &bus, None),
            bus,
        );
        let recorder = recorder();
        let handle = recorder.handle();
        metrics::with_local_recorder(&recorder, || {
            runtime().block_on(async {
                // The outermost metrics layer wraps the real CORS/auth/locale pipeline.
                let app = crate::app::app(st, Router::new(), None, Router::new(), Router::new());
                let preflight = HttpRequest::builder()
                    .method("OPTIONS")
                    .uri("/api/notifications/read")
                    .header(header::HOST, "127.0.0.1:18710")
                    .header(header::ORIGIN, "http://client.example")
                    .header(header::ACCESS_CONTROL_REQUEST_METHOD, "POST")
                    .body(Body::empty())
                    .unwrap();
                assert_eq!(
                    app.clone().oneshot(preflight).await.unwrap().status(),
                    StatusCode::NO_CONTENT
                );
                let anonymous = HttpRequest::builder()
                    .method("POST")
                    .uri("/api/notifications/read")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(r#"{"at":1}"#))
                    .unwrap();
                assert_eq!(
                    app.clone().oneshot(anonymous).await.unwrap().status(),
                    StatusCode::UNAUTHORIZED
                );
                for _ in 0..2 {
                    let request = HttpRequest::builder()
                        .method("POST")
                        .uri("/api/notifications/read")
                        .header(header::CONTENT_TYPE, "application/json")
                        .header(header::COOKIE, "steve_session=fixture-session")
                        .header("x-steve-ws", steve_store::WS)
                        .body(Body::from(r#"{"at":1}"#))
                        .unwrap();
                    let response = app.clone().oneshot(request).await.unwrap();
                    assert_eq!(response.status(), StatusCode::OK);
                    let bytes = response.into_body().collect().await.unwrap().to_bytes();
                    assert_eq!(
                        serde_json::from_slice::<serde_json::Value>(&bytes).unwrap(),
                        serde_json::json!({"seen":1})
                    );
                }
            })
        });
        assert_eq!(
            store
                .settings()
                .get::<u64>(steve_store::WS, "notifySeen", 0)
                .unwrap(),
            1
        );
        let text = handle.render();
        assert!(
            text.lines()
                .any(|line| line.starts_with("steve_http_requests_total{")
                    && line.contains("method=\"OPTIONS\"")
                    && line.contains("status=\"2xx\"")
                    && line.ends_with(" 1")),
            "{text}"
        );
        assert!(
            text.lines()
                .any(|line| line.starts_with("steve_http_requests_total{")
                    && line.contains("method=\"POST\"")
                    && line.contains("status=\"4xx\"")
                    && line.ends_with(" 1")),
            "{text}"
        );
        assert!(
            text.contains(
                "steve_http_store_calls_per_request_count{route=\"/api/notifications/read\"} 4"
            ),
            "{text}"
        );
        assert!(text.contains("steve_http_store_calls_per_request_bucket{route=\"/api/notifications/read\",le=\"1024\"} 4"), "{text}");
        let sum = text
            .lines()
            .find(|line| line.starts_with("steve_http_store_calls_per_request_sum{"))
            .unwrap()
            .split_whitespace()
            .last()
            .unwrap()
            .parse::<u64>()
            .unwrap();
        assert!(
            sum >= 6,
            "authentication reads and both writes must be carried: {text}"
        );
    }

    #[test]
    fn store_lock_callers_and_nested_transactions_are_observable() {
        let recorder = recorder();
        let handle = recorder.handle();
        steve_store::instrumentation::enable_lock_metrics();
        let tmp = tempfile::tempdir().unwrap();
        metrics::with_local_recorder(&recorder, || {
            let store = Store::open(tmp.path().join("steve.sqlite3"), NoPublisher).unwrap();
            let line = line!() + 1;
            let result = store.write(|_| Ok::<_, steve_store::StoreError>(()));
            result.unwrap();
            let text = handle.render();
            assert!(
                text.contains(&format!("caller=\"{}:{line}\"", file!())),
                "{text}"
            );
            assert!(text.contains("steve_lock_wait_seconds_count"), "{text}");
            assert!(text.contains("steve_lock_hold_seconds_count"), "{text}");
            store
                .write(|w| w.scope(|_| Ok::<_, steve_store::StoreError>(())))
                .unwrap();
            assert!(handle.render().contains("steve_store_transactions_total 3"));
        });
    }

    #[test]
    fn node_meters_are_per_node_and_an_in_process_node_repeats_no_process_figures() {
        let recorder = recorder();
        let handle = recorder.handle();
        let meters = steve_node_api::NodeMeters {
            journal_bytes: 4096,
            event_backlog: 2,
            network: [(
                "eth0".into(),
                steve_node_api::NetworkBytes {
                    received: 42,
                    sent: 81,
                },
            )]
            .into(),
            process: Some(steve_node_api::ProcessMeters {
                disk_written_bytes: Some(1 << 20),
                ..Default::default()
            }),
            ..Default::default()
        };
        metrics::with_local_recorder(&recorder, || {
            node_meters("n1", &meters, true);
            node_meters("local", &meters, false);
        });
        let text = handle.render();
        for line in [
            "steve_node_journal_bytes{node=\"n1\"} 4096",
            "steve_node_event_backlog{node=\"local\"} 2",
            "steve_node_network_received_bytes{node=\"n1\"} 42",
            "steve_node_network_sent_bytes{node=\"local\"} 81",
            "steve_node_process_disk_written_bytes{node=\"n1\"} 1048576",
        ] {
            assert!(text.lines().any(|l| l == line), "{line} in {text}");
        }
        assert!(
            !text.contains("steve_node_process_disk_written_bytes{node=\"local\"}"),
            "{text}"
        );
    }

    #[test]
    fn web_reports_record_their_samples_and_refuse_out_of_bounds_ones() {
        let recorder = recorder();
        let handle = recorder.handle();
        let post =
            |b: &str| crate::api::misc::record_web_perf(&axum::body::Bytes::from(b.to_string()));
        metrics::with_local_recorder(&recorder, || {
            let first = post(
                r#"{"page":"calendar","long_tasks":[120,60000],"interactions":[250],"patch_apply":[3],"transcript_open":[50],"nav_opens":{"inbox":2,"chat":5,"unknown_page":9},"errors":{"error":2,"unhandledrejection":1,"custom":4},"sse_reconnects":{"stale":3,"wake":1,"bogus":9}}"#,
            )
            .expect("first report");
            assert_eq!(first, vec![("chat", 5), ("inbox", 2)]);
            let second = post(r#"{"nav_opens":{"inbox":1,"todos":3}}"#).expect("second report");
            assert_eq!(second, vec![("inbox", 1), ("todos", 3)]);
            let many = format!(r#"{{"long_tasks":[{}]}}"#, vec!["1"; 501].join(","));
            let many_nav = format!(
                r#"{{"nav_opens":{{{}}}}}"#,
                (0..65)
                    .map(|i| format!(r#""k{i}":1"#))
                    .collect::<Vec<_>>()
                    .join(",")
            );
            for bad in [
                r#"{"long_tasks":[-1]}"#,
                r#"{"interactions":[60001]}"#,
                r#"{"long_tasks":"x"}"#,
                r#"{"nav_opens":{"chat":10001}}"#,
                r#"{"errors":{"error":10001}}"#,
                r#"{"sse_reconnects":{"error":10001}}"#,
                &many,
                &many_nav,
            ] {
                assert!(post(bad).is_err(), "{bad}");
            }
        });
        let text = handle.render();
        for line in [
            "steve_web_reports_total 2",
            "steve_web_long_task_seconds_count{page=\"calendar\"} 2",
            "steve_web_interaction_seconds_sum{page=\"calendar\"} 0.25",
            "steve_web_patch_apply_seconds_count{page=\"calendar\"} 1",
            "steve_web_transcript_open_seconds_count{page=\"calendar\"} 1",
            "steve_web_nav_opens_total{item=\"chat\"} 5",
            "steve_web_nav_opens_total{item=\"inbox\"} 3",
            "steve_web_nav_opens_total{item=\"todos\"} 3",
            "steve_web_errors_total{page=\"calendar\",kind=\"error\"} 2",
            "steve_web_errors_total{page=\"calendar\",kind=\"unhandledrejection\"} 1",
            "steve_web_sse_reconnects_total{reason=\"stale\"} 3",
            "steve_web_sse_reconnects_total{reason=\"wake\"} 1",
        ] {
            assert!(text.lines().any(|l| l == line), "{line} in {text}");
        }
        for unknown in ["unknown_page", "custom", "bogus"] {
            assert!(
                !text.contains(unknown),
                "unknown keys are never exported as labels ({unknown}): {text}"
            );
        }
    }

    #[test]
    fn periodic_task_gating_and_token_metrics_are_described_and_recorded() {
        use steve_runtime::jobs::{
            AvoidReason, SkipReason, record_model_calls_avoided, record_periodic_skipped,
            record_periodic_token_counts, record_periodic_triggered,
        };
        let recorder = recorder();
        let handle = recorder.handle();
        metrics::with_local_recorder(&recorder, || {
            catalog::describe();
            record_periodic_triggered("flow.report");
            record_periodic_skipped("flow.report", SkipReason::NoChange);
            record_periodic_skipped("digest.cron", SkipReason::QueueDriven);
            record_periodic_token_counts("flow.report", 1200, 300, 400, 0);
            record_model_calls_avoided("channels.intent", AvoidReason::Batched, 2);
            record_model_calls_avoided("channels.intent", AvoidReason::Cached, 1);
            record_model_calls_avoided("channels.intent", AvoidReason::Deduplicated, 1);
        });
        let text = handle.render();
        for line in [
            "# HELP steve_periodic_task_evaluations_total ",
            "# HELP steve_periodic_task_tokens_total ",
            "# HELP steve_model_calls_avoided_total ",
            "steve_periodic_task_evaluations_total{task=\"flow.report\",outcome=\"triggered\",reason=\"changed\"} 1",
            "steve_periodic_task_evaluations_total{task=\"flow.report\",outcome=\"skipped\",reason=\"no_change\"} 1",
            "steve_periodic_task_evaluations_total{task=\"digest.cron\",outcome=\"skipped\",reason=\"queue_driven\"} 1",
            "steve_periodic_task_tokens_total{task=\"flow.report\",kind=\"input\"} 1200",
            "steve_periodic_task_tokens_total{task=\"flow.report\",kind=\"output\"} 300",
            "steve_periodic_task_tokens_total{task=\"flow.report\",kind=\"cache_read\"} 400",
            "steve_model_calls_avoided_total{purpose=\"channels.intent\",reason=\"batched\"} 2",
            "steve_model_calls_avoided_total{purpose=\"channels.intent\",reason=\"cached\"} 1",
            "steve_model_calls_avoided_total{purpose=\"channels.intent\",reason=\"deduplicated\"} 1",
        ] {
            assert!(text.contains(line), "missing `{line}` in:\n{text}");
        }
        assert!(
            !text.contains("kind=\"cache_write\""),
            "zero token counts must not emit empty series:\n{text}"
        );
    }

    #[test]
    fn node_link_mode_and_protocol_delta_metrics_are_described_and_removable() {
        let recorder = recorder();
        let handle = recorder.handle();
        metrics::with_local_recorder(&recorder, || {
            catalog::describe();
            metrics::gauge!("steve_node_link_mode", "node" => "n_stale", "mode" => "normal")
                .set(0.0);
            metrics::gauge!("steve_node_link_mode", "node" => "n_stale", "mode" => "upgrade_only")
                .set(1.0);
            metrics::gauge!("steve_node_link_protocol_delta", "node" => "n_stale").set(2.0);
        });
        let text = handle.render();
        for line in [
            "# HELP steve_node_link_mode ",
            "# HELP steve_node_link_protocol_delta ",
            "steve_node_link_mode{node=\"n_stale\",mode=\"normal\"} 0",
            "steve_node_link_mode{node=\"n_stale\",mode=\"upgrade_only\"} 1",
            "steve_node_link_protocol_delta{node=\"n_stale\"} 2",
        ] {
            assert!(text.contains(line), "missing `{line}` in:\n{text}");
        }
        handle.remove_label("node", "n_stale");
        let after = handle.render();
        assert!(
            !after.contains("n_stale"),
            "removed node label must disappear from rendered metrics:\n{after}"
        );
    }

    #[test]
    fn update_check_metric_is_described_and_recorded() {
        let recorder = recorder();
        let handle = recorder.handle();
        metrics::with_local_recorder(&recorder, || {
            catalog::describe();
            metrics::counter!("steve_update_check_total", "result" => "available").increment(1);
            metrics::counter!("steve_update_check_total", "result" => "unavailable").increment(2);
            metrics::counter!("steve_update_check_total", "result" => "error").increment(1);
        });
        let text = handle.render();
        for line in [
            "# HELP steve_update_check_total ",
            "steve_update_check_total{result=\"available\"} 1",
            "steve_update_check_total{result=\"unavailable\"} 2",
            "steve_update_check_total{result=\"error\"} 1",
        ] {
            assert!(text.contains(line), "missing `{line}` in:\n{text}");
        }
    }

    #[test]
    fn placement_and_offer_metrics_are_described_and_recorded() {
        let recorder = recorder();
        let handle = recorder.handle();
        metrics::with_local_recorder(&recorder, || {
            catalog::describe();
            metrics::counter!("steve_bot_selection_total", "reason" => "stable_order").increment(1);
            metrics::counter!("steve_offer_preview_total", "result" => "placed").increment(1);
            metrics::counter!(
                "steve_offer_action_total",
                "action" => "take",
                "bot_override" => "explicit"
            )
            .increment(1);
        });
        let text = handle.render();
        for line in [
            "# HELP steve_bot_selection_total ",
            "# HELP steve_offer_preview_total ",
            "# HELP steve_offer_action_total ",
            "steve_bot_selection_total{reason=\"stable_order\"} 1",
            "steve_offer_preview_total{result=\"placed\"} 1",
            "steve_offer_action_total{action=\"take\",bot_override=\"explicit\"} 1",
        ] {
            assert!(text.contains(line), "missing `{line}` in:\n{text}");
        }
    }
}
