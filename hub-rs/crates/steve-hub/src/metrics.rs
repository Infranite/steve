//! Optional, memory-only metrics. No exporter thread or database writes.
use std::collections::HashMap;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

use axum::body::Body;
use axum::extract::{MatchedPath, Request, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use axum::{Router, routing::get};
use futures::TryStreamExt;
use metrics_exporter_prometheus::{PrometheusBuilder, PrometheusHandle, PrometheusRecorder};
use steve_store::Store;

use crate::config::ConfigError;

static ENABLED: AtomicBool = AtomicBool::new(false);

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
    PrometheusBuilder::new()
        .set_buckets(&[
            0.00001, 0.000025, 0.00005, 0.0001, 0.00025, 0.0005, 0.001, 0.0025, 0.005, 0.01, 0.025,
            0.05, 0.1, 0.25, 0.5, 1.0, 2.5, 5.0, 10.0, 30.0, 100.0,
        ])
        .expect("static nonempty buckets")
        .build_recorder()
}

/// Start independently of the business listener. Optional metrics failures never stop the Hub.
pub async fn start(st: crate::api::AppState) {
    let Some(listen) = &st.cfg.metrics else {
        return;
    };
    let Some(modules) = &st.modules else {
        tracing::warn!("metrics unavailable: no access store");
        return;
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
            return;
        }
    };
    let recorder = recorder();
    let handle = recorder.handle();
    if let Err(e) = metrics::set_global_recorder(recorder) {
        tracing::warn!("metrics recorder unavailable: {e}");
        return;
    }
    steve_store::instrumentation::enable_lock_metrics();
    ENABLED.store(true, Ordering::Relaxed);
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
    // Disk accounting is sampled only on demand, on the scrape worker.
    if let Some(bytes) = disk_write_bytes() {
        metrics::gauge!("steve_process_disk_written_bytes").set(bytes as f64);
    } else {
        metrics::counter!("steve_metrics_collection_errors_total", "source" => "disk_io")
            .increment(1);
    }
    (
        [(
            header::CONTENT_TYPE,
            "text/plain; version=0.0.4; charset=utf-8",
        )],
        st.handle.render(),
    )
        .into_response()
}

#[cfg(target_os = "macos")]
fn disk_write_bytes() -> Option<u64> {
    let mut usage = std::mem::MaybeUninit::<libc::rusage_info_v2>::zeroed();
    // SAFETY: RUSAGE_INFO_V2 writes exactly this struct; no data is used on failure.
    let status = unsafe {
        libc::proc_pid_rusage(
            std::process::id() as i32,
            libc::RUSAGE_INFO_V2,
            usage.as_mut_ptr().cast(),
        )
    };
    (status == 0).then(|| unsafe { usage.assume_init() }.ri_diskio_byteswritten)
}

#[cfg(target_os = "linux")]
fn disk_write_bytes() -> Option<u64> {
    let io = std::fs::read_to_string("/proc/self/io").ok()?;
    io.lines()
        .find_map(|line| line.strip_prefix("write_bytes:")?.trim().parse().ok())
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn disk_write_bytes() -> Option<u64> {
    None
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

/// To be wired after the route refactor. Handler latency excludes SSE and body transmission.
/// Hub responses carry data frames; the data-stream adapter forwards errors without buffering.
pub async fn http(request: Request, next: Next) -> Response {
    if !ENABLED.load(Ordering::Relaxed) {
        return next.run(request).await;
    }
    http_enabled(request, next).await
}

async fn http_enabled(request: Request, next: Next) -> Response {
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
    let response = next.run(request).await;
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
        let store = Store::open(tmp.path().join("metrics.sqlite3"), NoPublisher).unwrap();
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
        let before = store.with_conn(|c| Ok(c.total_changes())).unwrap();
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
        assert_eq!(store.with_conn(|c| Ok(c.total_changes())).unwrap(), before);
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
                    .layer(axum::middleware::from_fn(http_enabled));
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
            !text
                .lines()
                .any(|line| line.contains("duration") && line.contains("/metrics-events")),
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
                    .layer(axum::middleware::from_fn(http_enabled));
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
    fn default_http_switch_does_not_register_any_metrics() {
        let recorder = recorder();
        let handle = recorder.handle();
        metrics::with_local_recorder(&recorder, || {
            runtime().block_on(async {
                let app = Router::new()
                    .route("/metrics-disabled", get(|| async { "unchanged" }))
                    .layer(axum::middleware::from_fn(http));
                let response = app
                    .oneshot(
                        HttpRequest::builder()
                            .uri("/metrics-disabled")
                            .body(Body::empty())
                            .unwrap(),
                    )
                    .await
                    .unwrap();
                assert_eq!(
                    response.into_body().collect().await.unwrap().to_bytes(),
                    "unchanged"
                );
            })
        });
        assert_eq!(handle.render(), "");
    }

    #[test]
    fn store_lock_callers_and_nested_transactions_are_observable() {
        let recorder = recorder();
        let handle = recorder.handle();
        steve_store::instrumentation::enable_lock_metrics();
        let tmp = tempfile::tempdir().unwrap();
        metrics::with_local_recorder(&recorder, || {
            let store = Store::open(tmp.path().join("locks.sqlite3"), NoPublisher).unwrap();
            let line = line!() + 1;
            store.with_conn(|_| Ok(())).unwrap();
            let text = handle.render();
            assert!(
                text.contains(&format!("caller=\"{}:{line}\"", file!())),
                "{text}"
            );
            assert!(text.contains("steve_lock_wait_seconds_count"), "{text}");
            assert!(text.contains("steve_lock_hold_seconds_count"), "{text}");
            store
                .atomic(|| store.transaction(|_| Ok::<_, steve_store::StoreError>(())))
                .unwrap();
            assert!(handle.render().contains("steve_store_transactions_total 2"));
        });
    }
}
