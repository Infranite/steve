//! Metrics, collected in memory from the Hub's start (S1). Exporting is optional: with
//! `STEVE_METRICS=prometheus` the Hub serves `/metrics` on `STEVE_METRICS_LISTEN`, the management
//! socket always serves the same text, an export plugin may take a snapshot a minute ([`Export`]),
//! and `/api/system/health` reads the same recorder. Every
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
    (histogram $name:literal) => {
        $crate::metrics::once!(@ metrics::Histogram, metrics::histogram!($name))
    };
    (@ $t:ty, $make:expr) => {{
        static HANDLE: std::sync::OnceLock<$t> = std::sync::OnceLock::new();
        HANDLE.get_or_init(|| $make)
    }};
}
pub(crate) use once;

use std::collections::{BTreeMap, HashMap, VecDeque};
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use axum::body::Body;
use axum::extract::{MatchedPath, Request, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use axum::{Router, routing::get};
use futures::TryStreamExt;
use metrics_exporter_prometheus::{
    Matcher, PrometheusBuilder, PrometheusHandle, PrometheusRecorder,
};
use steve_store::Store;
use steve_types::devices::{NodeVitalsDay, NodeVitalsLatest, NodeVitalsPoint, VitalsTokens};

use crate::config::ConfigError;

/// Health compares now with a snapshot this old at most (one is kept a minute).
const WINDOW_SNAPSHOTS: usize = 15;
const UPKEEP_EVERY: Duration = Duration::from_secs(5);
pub(crate) const CONTENT_TYPE: &str = "text/plain; version=0.0.4; charset=utf-8";

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

fn recorder() -> PrometheusRecorder {
    // Explicit buckets give distributions without the exporter's time-window maintenance task.
    let counts = [
        0.0, 1.0, 2.0, 4.0, 8.0, 16.0, 32.0, 64.0, 128.0, 256.0, 512.0, 1024.0,
    ];
    let long = [
        0.5, 1.0, 2.5, 5.0, 10.0, 30.0, 60.0, 120.0, 300.0, 600.0, 1800.0, 3600.0, 7200.0, 21600.0,
        86400.0, 604800.0,
    ];
    let mut b = PrometheusBuilder::new()
        .set_buckets(&[
            0.00001, 0.000025, 0.00005, 0.0001, 0.00025, 0.0005, 0.001, 0.0025, 0.005, 0.01, 0.025,
            0.05, 0.1, 0.25, 0.5, 1.0, 2.5, 5.0, 10.0, 30.0, 100.0,
        ])
        .expect("static nonempty buckets");
    for name in catalog::COUNTS {
        b = b
            .set_buckets_for_metric(Matcher::Full((*name).into()), &counts)
            .expect("static count buckets");
    }
    for name in catalog::LONG {
        b = b
            .set_buckets_for_metric(Matcher::Full((*name).into()), &long)
            .expect("static long buckets");
    }
    b.build_recorder()
}

struct Installed {
    handle: PrometheusHandle,
    started: Instant,
    history: Mutex<History>,
    backup_paths: Option<(PathBuf, PathBuf)>,
}

#[derive(Default)]
struct History {
    health: VecDeque<health::Snapshot>,
    summary: Option<health::Health>,
    nodes: nodes::History,
}

static INSTALLED: OnceLock<Installed> = OnceLock::new();

/// An export beyond the built-in pull listener (OTLP and others come as plugins from a plugin
/// store): the Hub keeps the one recorder and hands the exporter a snapshot, the exposition text,
/// once a minute. `push` runs on the sampling task and must hand off without blocking.
pub trait Export: Send + Sync {
    fn push(&self, exposition: &str);
}

static EXPORT: OnceLock<Box<dyn Export>> = OnceLock::new();

/// Connects the one external exporter; `false` when one is connected already.
pub fn connect_export(export: Box<dyn Export>) -> bool {
    EXPORT.set(export).is_ok()
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
    let text = {
        let mut history = i.history.lock().unwrap_or_else(|e| e.into_inner());
        history.nodes.record(observations, crate::nodes::now());
        let text = i.render();
        let point = health::Snapshot::parse(&text, i.started.elapsed().as_secs_f64());
        if history.health.len() == WINDOW_SNAPSHOTS {
            history.health.pop_front();
        }
        history.summary = Some(health::assess(&point, history.health.front()));
        history.health.push_back(point);
        text
    };
    if let Some(export) = EXPORT.get() {
        export.push(&text);
    }
}

pub(crate) fn latest_node(id: &str, since: i64) -> Option<NodeVitalsLatest> {
    let history = INSTALLED
        .get()?
        .history
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    history.nodes.latest(id, since).cloned()
}

pub(crate) fn node_days(
    ids: impl IntoIterator<Item = String>,
    now: i64,
    mut tokens: BTreeMap<String, (i64, i64)>,
) -> BTreeMap<String, NodeVitalsDay> {
    let Some(i) = INSTALLED.get() else {
        return BTreeMap::new();
    };
    let history = i.history.lock().unwrap_or_else(|e| e.into_inner());
    ids.into_iter()
        .filter_map(|id| {
            let (per_minute, today) = tokens.remove(&id).unwrap_or_default();
            let day = history
                .nodes
                .day(&id, now, VitalsTokens { per_minute, today })?;
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
        health::Snapshot::parse(&self.render(), self.started.elapsed().as_secs_f64())
    }
}

/// The exposition text now (this process sampled first); `None` before [`install`].
pub fn render() -> Option<String> {
    INSTALLED.get().map(Installed::render)
}

/// The health of the last window (since the start until a window has passed).
pub(crate) fn health() -> Option<health::Health> {
    let i = INSTALLED.get()?;
    let history = i.history.lock().unwrap_or_else(|e| e.into_inner());
    history
        .summary
        .clone()
        .or_else(|| Some(health::assess(&i.snapshot(), None)))
}

/// The pages a web report may name (a bounded label); anything else is `other`.
pub(crate) const WEB_PAGES: [&str; 7] = [
    "chat", "items", "item", "sessions", "session", "inbox", "other",
];

/// What a web client measured on `page` since its last report (`POST /api/system/web-perf`): its
/// long tasks, the latency of each interaction and of merging each patch frame, in seconds.
pub(crate) fn web_report(
    page: &str,
    long_tasks: &[f64],
    interactions: &[f64],
    patches: &[f64],
    snapshots: &[f64],
    transcripts: &[f64],
    dom_nodes: Option<u64>,
) {
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
}

/// This process's use of the machine, sampled when somebody reads the metrics.
fn sample_process() {
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
    handle: PrometheusHandle,
}

fn router(store: Store, handle: PrometheusHandle) -> Router {
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

#[derive(Clone)]
struct HttpMetrics {
    requests: metrics::Counter,
    duration: metrics::Histogram,
    bytes: metrics::Counter,
}

fn http_metrics(route: &str, method: &'static str, status: &'static str) -> HttpMetrics {
    // Only cold route registration allocates labels. MatchedPath never contains an entity ID.
    thread_local! {
        static HANDLES: std::cell::RefCell<HashMap<String, HashMap<(&'static str, &'static str), HttpMetrics>>> =
            std::cell::RefCell::new(HashMap::new());
    }
    HANDLES.with(|cache| {
        let mut cache = cache.borrow_mut();
        if !cache.contains_key(route) { cache.insert(route.to_owned(), HashMap::new()); }
        cache.get_mut(route).expect("inserted").entry((method, status)).or_insert_with(|| HttpMetrics {
            requests: metrics::counter!("steve_http_requests_total", "route" => route.to_owned(), "method" => method, "status" => status),
            duration: metrics::histogram!("steve_http_request_duration_seconds", "route" => route.to_owned(), "method" => method, "status" => status),
            bytes: metrics::counter!("steve_http_response_bytes_total", "route" => route.to_owned(), "method" => method, "status" => status),
        }).clone()
    })
}

/// The header with the newest bus revision a request committed that can change a view.
pub const REV_HEADER: &str = "x-steve-rev";

/// Outermost request observation, including CORS/maintenance/auth short responses. Latency
/// ends at response headers; body bytes reflect the inner compression layer's actual chunks.
/// Hub responses carry data frames; the data-stream adapter forwards errors without buffering.
pub async fn http(request: Request, next: Next) -> Response {
    let route = request.extensions().get::<MatchedPath>().cloned();
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
    let route = route.as_ref().map_or("unmatched", MatchedPath::as_str);
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
        sse_counter(route).increment(1);
        return response;
    }
    let status = match response.status().as_u16() / 100 {
        1 => "1xx",
        2 => "2xx",
        3 => "3xx",
        4 => "4xx",
        _ => "5xx",
    };
    store_calls_histogram(route).record(calls.count() as f64);
    let handles = http_metrics(route, method, status);
    handles.requests.increment(1);
    handles.duration.record(start.elapsed().as_secs_f64());
    // BodyDataStream discards trailers. No current Hub route emits them; preserve any future
    // declared trailer response unchanged rather than changing its wire semantics.
    if response.headers().contains_key(header::TRAILER) {
        metrics::counter!("steve_metrics_collection_errors_total", "source" => "response_trailers")
            .increment(1);
        return response;
    }
    let (parts, body) = response.into_parts();
    let stream = body
        .into_data_stream()
        .inspect_ok(move |chunk| handles.bytes.increment(chunk.len() as u64));
    Response::from_parts(parts, Body::from_stream(stream))
}

/// Blocking API work keeps the request's storage counter alongside its existing language context.
pub(crate) fn carry<R>(f: impl FnOnce() -> R) -> impl FnOnce() -> R {
    steve_store::instrumentation::carry(f)
}

fn store_calls_histogram(route: &str) -> metrics::Histogram {
    thread_local! {
        static HANDLES: std::cell::RefCell<HashMap<String, metrics::Histogram>> =
            std::cell::RefCell::new(HashMap::new());
    }
    HANDLES.with(|cache| {
        let mut cache = cache.borrow_mut();
        if let Some(handle) = cache.get(route) {
            return handle.clone();
        }
        let handle =
            metrics::histogram!("steve_http_store_calls_per_request", "route" => route.to_owned());
        cache.insert(route.to_owned(), handle.clone());
        handle
    })
}

fn sse_counter(route: &str) -> metrics::Counter {
    thread_local! {
        static HANDLES: std::cell::RefCell<HashMap<String, metrics::Counter>> =
            std::cell::RefCell::new(HashMap::new());
    }
    HANDLES.with(|cache| {
        let mut cache = cache.borrow_mut();
        if let Some(handle) = cache.get(route) {
            return handle.clone();
        }
        let handle =
            metrics::counter!("steve_http_sse_connections_total", "route" => route.to_owned());
        cache.insert(route.to_owned(), handle.clone());
        handle
    })
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
        assert!(
            recorder
                .handle()
                .render()
                .contains("source=\"response_trailers\"")
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
        let st = crate::api::AppState::new(
            Arc::new(cfg),
            Arc::new(crate::wire::store::StoreBackend(store.clone())),
            Arc::new(crate::backend::NoDomain),
            crate::bus::Bus::new(std::sync::Arc::new(steve_runtime::cache::Caches::new(
                Default::default(),
            ))),
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
        let post = |b: String| crate::api::misc::system_web_perf(axum::body::Bytes::from(b));
        metrics::with_local_recorder(&recorder, || {
            runtime().block_on(async {
                assert!(
                    post(
                        r#"{"page":"inbox","long_tasks":[120,60000],"interactions":[250],"patch_apply":[3],"transcript_open":[50]}"#
                            .into()
                    )
                        .await
                        .is_ok()
                );
                let many = format!(r#"{{"long_tasks":[{}]}}"#, vec!["1"; 501].join(","));
                for bad in [
                    r#"{"long_tasks":[-1]}"#,
                    r#"{"interactions":[60001]}"#,
                    r#"{"long_tasks":"x"}"#,
                    &many,
                ] {
                    assert!(post(bad.to_string()).await.is_err(), "{bad}");
                }
            })
        });
        let text = handle.render();
        for line in [
            "steve_web_reports_total 1",
            "steve_web_long_task_seconds_count{page=\"inbox\"} 2",
            "steve_web_interaction_seconds_sum{page=\"inbox\"} 0.25",
            "steve_web_patch_apply_seconds_count{page=\"inbox\"} 1",
            "steve_web_transcript_open_seconds_count{page=\"inbox\"} 1",
        ] {
            assert!(text.lines().any(|l| l == line), "{line} in {text}");
        }
    }
}
