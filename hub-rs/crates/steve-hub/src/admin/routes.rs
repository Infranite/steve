//! Management-only routes: never merged into the public Router.
use crate::access::{self, CliOutput};
use axum::{
    Extension, Json, Router,
    extract::{Path, Query, Request, State},
    http::StatusCode,
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{delete, get},
};
use serde::{Deserialize, Serialize};
use std::{path::PathBuf, time::Duration};
use steve_store::Store;

async fn admin_lane(request: Request, next: Next) -> Response {
    steve_store::lane::scope(steve_store::lane::Lane::Admin, next.run(request)).await
}

async fn blocking<T: Send + 'static>(
    work: impl FnOnce() -> T + Send + 'static,
) -> Result<T, tokio::task::JoinError> {
    tokio::task::spawn_blocking(steve_store::instrumentation::carry(work)).await
}

#[derive(Deserialize, Serialize)]
pub(super) struct Create {
    /// Original argv preserves legacy flag ordering, duplicate flags and values beginning
    /// with '--'; no shell or process is involved in executing these arguments.
    pub args: Vec<String>,
    pub bootstrap_user: Option<String>,
}

#[derive(Default, Deserialize)]
struct Format {
    #[serde(default)]
    json: bool,
}
async fn deadline(State(limit): State<Duration>, request: Request, next: Next) -> Response {
    match tokio::time::timeout(limit, next.run(request)).await {
        Ok(response) => response,
        Err(_) => (StatusCode::GATEWAY_TIMEOUT, Json(CliOutput::result(Err(anyhow::anyhow!(
            "management request deadline exceeded; a write may already have completed; inspect before retrying"))))).into_response(),
    }
}
pub(super) fn router(store: Store, home: PathBuf, limit: Duration) -> Router {
    // The executor records queue-only wait at acquisition with file/lane labels.
    metrics::describe_histogram!(
        "steve_store_wait_seconds",
        metrics::Unit::Seconds,
        "Time queued for storage writer authorization, partitioned by file and lane"
    );
    Router::new()
        .route("/admin/v1/access/tokens", get(tokens).post(create))
        .route("/admin/v1/access/tokens/{id}", delete(revoke_token))
        .route("/admin/v1/access/clients", get(clients))
        .route("/admin/v1/access/clients/{id}", delete(revoke_client))
        .route("/admin/v1/storage/status", get(status))
        .route("/admin/v1/backup", get(backups).post(make_backup))
        .with_state(store)
        .layer(Extension(home))
        .layer(middleware::from_fn(admin_lane))
        .layer(middleware::from_fn_with_state(limit, deadline))
}
async fn command(store: Store, args: Vec<String>) -> Response {
    command_with_user(store, args, None).await
}
async fn command_with_user(
    store: Store,
    args: Vec<String>,
    bootstrap_user: Option<String>,
) -> Response {
    match blocking(move || match bootstrap_user {
        Some(user) => access::command_with_default_user(&store, &args, &user),
        None => access::command_on(&store, &args),
    })
    .await
    {
        Ok(result) => {
            let output = CliOutput::result(result);
            let status = if output.code == 0 {
                StatusCode::OK
            } else {
                StatusCode::BAD_REQUEST
            };
            (status, Json(output)).into_response()
        }
        Err(_) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(CliOutput::result(Err(anyhow::anyhow!(
                "management worker failed"
            )))),
        )
            .into_response(),
    }
}
async fn create(State(store): State<Store>, Json(input): Json<Create>) -> Response {
    let words: Vec<_> = input
        .args
        .iter()
        .map(String::as_str)
        .take_while(|s| !s.starts_with("--"))
        .collect();
    if words != ["token", "create"] {
        return (
            StatusCode::BAD_REQUEST,
            Json(CliOutput::result(Err(access::usage()))),
        )
            .into_response();
    }
    command_with_user(store, input.args, input.bootstrap_user).await
}

async fn tokens(State(store): State<Store>, Query(format): Query<Format>) -> Response {
    let mut args = vec!["token".into(), "list".into()];
    if format.json {
        args.push("--json".into());
    }
    command(store, args).await
}
async fn clients(State(store): State<Store>, Query(format): Query<Format>) -> Response {
    let mut args = vec!["client".into(), "list".into()];
    if format.json {
        args.push("--json".into());
    }
    command(store, args).await
}
async fn revoke_token(State(store): State<Store>, Path(id): Path<String>) -> Response {
    command(store, vec!["token".into(), "revoke".into(), id]).await
}
async fn revoke_client(State(store): State<Store>, Path(id): Path<String>) -> Response {
    command(store, vec!["client".into(), "revoke".into(), id]).await
}
async fn status(State(store): State<Store>) -> Response {
    let result = blocking(move || -> anyhow::Result<_> {
        let schema: i64 = store.reader(|c| Ok::<_, steve_store::StoreError>(c.query_row("SELECT v FROM schema_version LIMIT 1", [], |r| r.get(0))?))?;
        Ok(serde_json::json!({"path": store.path(), "schema_version": schema, "file_size": std::fs::metadata(store.path())?.len()}))
    }).await;
    match result {
        Ok(Ok(status)) => Json(status).into_response(),
        _ => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(CliOutput::result(Err(anyhow::anyhow!(
                "cannot read storage status"
            )))),
        )
            .into_response(),
    }
}

#[derive(Default, Deserialize, Serialize)]
pub(super) struct BackupRequest {
    pub reason: Option<String>,
}
async fn make_backup(
    State(store): State<Store>,
    Extension(home): Extension<PathBuf>,
    input: Option<Json<BackupRequest>>,
) -> Response {
    let input = input.map(|Json(input)| input).unwrap_or_default();
    let args = vec![
        "backup".into(),
        "--reason".into(),
        input.reason.unwrap_or_else(|| "manual".into()),
    ];
    let result = blocking(move || {
        let db = store.path();
        super::client::backup::command(&home, db, &args)
    })
    .await;
    let output =
        CliOutput::result(result.unwrap_or_else(|_| Err(anyhow::anyhow!("backup worker failed"))));
    (
        if output.code == 0 {
            StatusCode::OK
        } else {
            StatusCode::BAD_REQUEST
        },
        Json(output),
    )
        .into_response()
}
async fn backups(Extension(home): Extension<PathBuf>) -> Response {
    let result =
        blocking(move || steve_store_sqlite::backup::usage(&crate::backup::root(&home))).await;
    match result {
        Ok(Ok(usage)) => Json(usage).into_response(),
        _ => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{body::Body, http::Request};
    use tower::ServiceExt;

    #[tokio::test]
    async fn admin_reads_and_writes_keep_the_lane_on_blocking_threads() {
        use steve_store::lane::{self, Lane};
        let dir = tempfile::tempdir().unwrap();
        let store =
            Store::open(dir.path().join("steve.sqlite3"), steve_store::NoPublisher).unwrap();
        let probe = Router::new()
            .route("/read", get(|State(store): State<Store>| async move {
                blocking(move || {
                    assert_eq!(lane::current(), Lane::Admin);
                    store.reader(|c| Ok::<_, steve_store::StoreError>(c.query_row("SELECT 1", [], |r| r.get::<_, i64>(0))?)).unwrap()
                }).await.unwrap().to_string()
            }))
            .route("/write", get(|State(store): State<Store>| async move {
                blocking(move || {
                    assert_eq!(lane::current(), Lane::Admin);
                    store.with_conn(|c| {
                        c.execute_batch("CREATE TABLE admin_probe(v); INSERT INTO admin_probe VALUES(1);")?;
                        Ok::<_, steve_store::StoreError>(())
                    }).unwrap();
                }).await.unwrap();
                "written"
            }))
            .with_state(store.clone())
            .layer(middleware::from_fn(admin_lane));
        for path in ["/read", "/write"] {
            let response = lane::background(
                probe
                    .clone()
                    .oneshot(Request::builder().uri(path).body(Body::empty()).unwrap()),
            )
            .await
            .unwrap();
            assert_eq!(response.status(), StatusCode::OK);
        }
        assert_eq!(
            store
                .reader(|c| Ok::<_, steve_store::StoreError>(c.query_row(
                    "SELECT count(*) FROM admin_probe",
                    [],
                    |r| r.get::<_, i64>(0)
                )?))
                .unwrap(),
            1
        );
    }

    #[tokio::test]
    async fn production_management_router_overrides_its_background_job_lane() {
        use steve_store::lane::{self, Lane};
        let dir = tempfile::tempdir().unwrap();
        let store =
            Store::open(dir.path().join("steve.sqlite3"), steve_store::NoPublisher).unwrap();
        store.ensure_workspace().unwrap();
        let observed = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let captured = observed.clone();
        store
            .with_conn(|c| {
                c.authorizer(Some(move |_: rusqlite::hooks::AuthContext<'_>| {
                    captured.lock().unwrap().push(lane::capture());
                    rusqlite::hooks::Authorization::Allow
                }))?;
                Ok::<_, steve_store::StoreError>(())
            })
            .unwrap();
        observed.lock().unwrap().clear();
        let app = router(store, dir.path().to_owned(), Duration::from_secs(30));
        let request = Request::builder()
            .method("POST")
            .uri("/admin/v1/access/tokens")
            .header("content-type", "application/json")
            .body(Body::from(
                r#"{"args":["token","create","--user","owner","--json"]}"#,
            ))
            .unwrap();
        assert_eq!(
            lane::background(app.oneshot(request))
                .await
                .unwrap()
                .status(),
            StatusCode::OK
        );
        let seen = observed.lock().unwrap();
        assert!(
            !seen.is_empty(),
            "observe the actual command's SQLite operations"
        );
        assert!(
            seen.iter().all(|lane| *lane == Some(Lane::Admin)),
            "{seen:?}"
        );
    }

    type Readings = (
        std::sync::Mutex<Vec<(metrics::Key, f64)>>,
        std::sync::Condvar,
    );
    #[derive(Clone, Default)]
    struct Samples(std::sync::Arc<Readings>);
    impl Samples {
        fn push(&self, key: metrics::Key, value: f64) {
            self.0.0.lock().unwrap().push((key, value));
            self.0.1.notify_all();
        }
        fn tagged(key: &metrics::Key, lane: &str) -> bool {
            key.labels()
                .any(|tag| tag.key() == "lane" && tag.value() == lane)
        }
        fn queued(&self, lane: &str, depth: f64) -> bool {
            let events = self.0.0.lock().unwrap();
            let (_events, timeout) = self
                .0
                .1
                .wait_timeout_while(events, Duration::from_secs(10), |events| {
                    !events.iter().any(|(key, value)| {
                        key.name() == "steve_store_queue_depth"
                            && Self::tagged(key, lane)
                            && *value >= depth
                    })
                })
                .unwrap();
            !timeout.timed_out()
        }
    }
    struct Sample(Samples, metrics::Key);
    impl metrics::GaugeFn for Sample {
        fn set(&self, value: f64) {
            self.0.push(self.1.clone(), value);
        }
        fn increment(&self, _: f64) {}
        fn decrement(&self, _: f64) {}
    }
    impl metrics::HistogramFn for Sample {
        fn record(&self, value: f64) {
            self.0.push(self.1.clone(), value);
        }
    }
    impl metrics::Recorder for Samples {
        fn describe_counter(
            &self,
            _: metrics::KeyName,
            _: Option<metrics::Unit>,
            _: metrics::SharedString,
        ) {
        }
        fn describe_gauge(
            &self,
            _: metrics::KeyName,
            _: Option<metrics::Unit>,
            _: metrics::SharedString,
        ) {
        }
        fn describe_histogram(
            &self,
            _: metrics::KeyName,
            _: Option<metrics::Unit>,
            _: metrics::SharedString,
        ) {
        }
        fn register_counter(
            &self,
            _: &metrics::Key,
            _: &metrics::Metadata<'_>,
        ) -> metrics::Counter {
            metrics::Counter::noop()
        }
        fn register_gauge(&self, key: &metrics::Key, _: &metrics::Metadata<'_>) -> metrics::Gauge {
            metrics::Gauge::from_arc(std::sync::Arc::new(Sample(self.clone(), key.clone())))
        }
        fn register_histogram(
            &self,
            key: &metrics::Key,
            _: &metrics::Metadata<'_>,
        ) -> metrics::Histogram {
            metrics::Histogram::from_arc(std::sync::Arc::new(Sample(self.clone(), key.clone())))
        }
    }

    #[derive(Default)]
    struct Time(std::sync::atomic::AtomicU64);
    impl Time {
        fn advance(&self, millis: u64) {
            self.0
                .fetch_add(millis, std::sync::atomic::Ordering::SeqCst);
        }
    }
    impl steve_store_sqlite::exec::Clock for Time {
        fn now(&self) -> Duration {
            Duration::from_millis(self.0.load(std::sync::atomic::Ordering::SeqCst))
        }
    }
    struct Runner;
    impl steve_store_sqlite::exec::BatchRunner<rusqlite::Connection> for Runner {
        type Job = ();
        type Output = ();
        fn run(
            &self,
            _: &rusqlite::Connection,
            jobs: Vec<()>,
        ) -> Vec<Result<(), steve_store_sqlite::exec::CommitError>> {
            jobs.into_iter().map(Ok).collect()
        }
    }

    /// Deterministic scheduler/HTTP approximation, not the QA #426 wall-time benchmark.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn admin_burst_records_queue_wait_under_three_saturated_lanes() {
        use std::sync::{Arc, mpsc};
        use steve_store::lane::{self, Lane};
        use steve_store_sqlite::exec::Executor;
        let dir = tempfile::tempdir().unwrap();
        let conn = rusqlite::Connection::open(dir.path().join("steve.sqlite3")).unwrap();
        conn.execute_batch("CREATE TABLE work(lane TEXT);").unwrap();
        let time = Arc::new(Time::default());
        let exec = Executor::new("admin-approx", conn, time.clone(), Runner, || false).unwrap();
        let samples = Samples::default();
        let (release, resume) = mpsc::channel();
        let (ready, held) = mpsc::channel();
        let writer = exec.clone();
        let holder = std::thread::spawn(move || {
            let _grant = writer.grant(Lane::Interactive).unwrap();
            ready.send(()).unwrap();
            resume.recv().unwrap();
        });
        held.recv_timeout(Duration::from_secs(10)).unwrap();
        let mut producers = Vec::new();
        for (tag, name) in [
            (Lane::Interactive, "interactive"),
            (Lane::Ingest, "ingest"),
            (Lane::Background, "background"),
        ] {
            let (writer, time, samples) = (exec.clone(), time.clone(), samples.clone());
            producers.push(std::thread::spawn(move || {
                metrics::with_local_recorder(&samples, || {
                    lane::with_lane(tag, || {
                        for _ in 0..16 {
                            let grant = writer.grant_current().unwrap();
                            grant
                                .execute("INSERT INTO work VALUES(?1)", [name])
                                .unwrap();
                            time.advance(1);
                        }
                    })
                })
            }));
        }
        for tag in ["interactive", "ingest", "background"] {
            assert!(samples.queued(tag, 1.0));
        }
        let (waiting, elapsed) = (samples.clone(), time.clone());
        let observed = samples.clone();
        let coordinator = std::thread::spawn(move || {
            let queued = waiting.queued("admin", 20.0);
            elapsed.advance(500);
            release.send(()).unwrap();
            queued
        });
        let app = Router::new()
            .route(
                "/probe",
                get(move || {
                    let (writer, time, samples) = (exec.clone(), time.clone(), samples.clone());
                    async move {
                        let seen = blocking(move || {
                            metrics::with_local_recorder(&samples, || {
                                assert_eq!(lane::current(), Lane::Admin);
                                let grant = writer.grant_current().unwrap();
                                assert_eq!(grant.lane(), Lane::Admin);
                                grant
                                    .execute("INSERT INTO work VALUES('admin')", [])
                                    .unwrap();
                                let rows: i64 = grant
                                    .query_row("SELECT count(*) FROM work", [], |r| r.get(0))
                                    .unwrap();
                                time.advance(1);
                                rows
                            })
                        })
                        .await
                        .unwrap();
                        Json(seen)
                    }
                }),
            )
            .layer(middleware::from_fn(admin_lane));
        let requests = (0..20)
            .map(|_| {
                let app = app.clone();
                tokio::spawn(async move {
                    app.oneshot(
                        Request::builder()
                            .uri("/probe")
                            .body(Body::empty())
                            .unwrap(),
                    )
                    .await
                    .unwrap()
                })
            })
            .collect::<Vec<_>>();
        for request in requests {
            let response = request.await.unwrap();
            assert_eq!(response.status(), StatusCode::OK);
            let body = axum::body::to_bytes(response.into_body(), 128)
                .await
                .unwrap();
            assert!(
                serde_json::from_slice::<i64>(&body).unwrap() < 68,
                "admin ran before all three backlogs drained"
            );
        }
        assert!(
            coordinator.join().unwrap(),
            "all requests really entered the admin queue"
        );
        holder.join().unwrap();
        for producer in producers {
            producer.join().unwrap();
        }
        let readings = observed.0.0.lock().unwrap();
        let mut waits: Vec<_> = readings
            .iter()
            .filter(|(key, _)| {
                key.name() == "steve_store_wait_seconds" && Samples::tagged(key, "admin")
            })
            .map(|(key, value)| {
                assert!(
                    key.labels()
                        .any(|tag| tag.key() == "file" && tag.value() == "admin-approx")
                );
                *value
            })
            .collect();
        waits.sort_by(f64::total_cmp);
        assert_eq!(
            waits.len(),
            20,
            "one queue-wait observation per admin grant"
        );
        assert!(waits[0] >= 0.5 && waits[19] < 0.6, "{waits:?}");
        println!(
            "local approximation: 20 admin requests, logical queue wait min={:.3}s p99={:.3}s; QA wall-time p99 remains pending 6a+6b integration",
            waits[0], waits[19]
        );
    }
    #[tokio::test(start_paused = true)]
    async fn deadline_reports_why_without_waiting_for_wall_time() {
        let router = Router::new()
            .route(
                "/slow",
                get(|| async {
                    std::future::pending::<()>().await;
                    "done"
                }),
            )
            .layer(middleware::from_fn_with_state(
                Duration::from_secs(30),
                deadline,
            ));
        let response = router
            .oneshot(Request::builder().uri("/slow").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::GATEWAY_TIMEOUT);
        let body = axum::body::to_bytes(response.into_body(), 4096)
            .await
            .unwrap();
        let reply: CliOutput = serde_json::from_slice(&body).unwrap();
        assert_eq!(reply.code, 2);
        assert!(reply.stderr.contains("deadline exceeded"));
    }
}
