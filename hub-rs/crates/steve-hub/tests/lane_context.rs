//! Exercise real HTTP/job futures across blocking hops into the writer scheduler.
use axum::{Router, body::Body, http::Request, routing::get};
use std::sync::Arc;
use steve_hub::{
    api::AppState,
    backend::{BResult, Backend, NoDomain, Repo},
    bus::Bus,
    config::Config,
};
use steve_store::{
    instrumentation::carry,
    lane::{self, Lane},
};
use steve_store_sqlite::exec::{BatchRunner, Executor, SystemClock};
use tower::ServiceExt;

struct Runner;
impl BatchRunner<()> for Runner {
    type Job = ();
    type Output = ();
    fn run(&self, _: &(), jobs: Vec<()>) -> Vec<Result<(), steve_store_sqlite::exec::CommitError>> {
        jobs.into_iter().map(Ok).collect()
    }
}
fn executor() -> Executor<(), Runner> {
    Executor::new(
        "lane-test",
        (),
        Arc::new(SystemClock::default()),
        Runner,
        || false,
    )
    .unwrap()
}
async fn at_executor(exec: Executor<(), Runner>) -> Lane {
    tokio::task::spawn_blocking(carry(move || exec.grant_current().unwrap().lane()))
        .await
        .unwrap()
}

struct NoStorage;
impl Backend for NoStorage {
    fn reader(&self) -> BResult<Box<dyn Repo + '_>> {
        panic!("unexpected DB read")
    }
    fn writer(&self) -> BResult<Box<dyn Repo + '_>> {
        panic!("unexpected DB write")
    }
}

#[tokio::test]
async fn http_scope_covers_the_real_app_including_mcp_and_auth_routes() {
    let home = tempfile::tempdir().unwrap();
    let cfg = Arc::new(
        Config::from_vars(&|key: &str| match key {
            "STEVE_HOME" => Some(home.path().to_string_lossy().into_owned()),
            "STEVE_PORT" => Some("18723".into()),
            "STEVE_NO_RESUME" => Some("1".into()),
            _ => None,
        })
        .unwrap(),
    );
    let st = AppState::new(
        cfg,
        Arc::new(NoStorage),
        Arc::new(NoDomain),
        Bus::new(Arc::new(steve_runtime::cache::Caches::new(
            steve_runtime::cache::CacheConfig::default(),
        ))),
    );
    let probe = || {
        Router::new().route(
            "/probe",
            get(|| async {
                assert_eq!(lane::current(), Lane::Interactive);
                assert_eq!(at_executor(executor()).await, Lane::Interactive);
                "ok"
            }),
        )
    };
    let mcp = Router::new().route(
        "/mcp/probe",
        get(|| async {
            assert_eq!(at_executor(executor()).await, Lane::Interactive);
            "ok"
        }),
    );
    let app = steve_hub::app::app(st, probe(), Some(mcp), Router::new(), probe());
    for path in ["/probe", "/auth/probe", "/mcp/probe"] {
        let request = Request::builder()
            .uri(path)
            .extension(axum::extract::ConnectInfo(
                "127.0.0.1:12345".parse::<std::net::SocketAddr>().unwrap(),
            ))
            .body(Body::empty())
            .unwrap();
        let response = lane::background(app.clone().oneshot(request))
            .await
            .unwrap();
        assert_eq!(response.status(), 200, "{path}");
    }
}

#[tokio::test]
async fn jobs_carry_background_to_executor_and_child_tasks_remain_unclassified() {
    let (sent, received) = tokio::sync::oneshot::channel();
    let job = steve_hub::wire::modules::Job::new("lane.job", async move {
        tokio::task::yield_now().await;
        let child = tokio::spawn(async { lane::capture() }).await.unwrap();
        assert_eq!(child, None, "Tokio children need their own wrapper");
        sent.send(at_executor(executor()).await).unwrap();
    });
    let task = job.spawn();
    assert_eq!(received.await.unwrap(), Lane::Background);
    task.await;
}

#[tokio::test]
async fn every_factory_scopes_each_pass_before_runtime_spawns_it() {
    let clock = Arc::new(steve_runtime::FakeClock::new(0));
    let timer = clock.clone();
    let (sent, mut received) = tokio::sync::mpsc::unbounded_channel();
    let job = steve_hub::wire::modules::Job::task("lane.periodic", move || {
        steve_runtime::every(
            "lane.periodic",
            timer,
            std::time::Duration::from_secs(1),
            steve_runtime::FirstRun::Immediate,
            move || {
                let sent = sent.clone();
                lane::background(async move {
                    tokio::task::yield_now().await;
                    sent.send(at_executor(executor()).await).unwrap();
                })
            },
        )
    });
    let tasks = steve_hub::jobs::Tasks::default();
    job.start(&tasks);
    assert_eq!(received.recv().await, Some(Lane::Background));
    clock.advance(1000);
    assert_eq!(received.recv().await, Some(Lane::Background));
    tasks.stop().await;
}

#[tokio::test]
async fn carry_works_without_lock_metrics_and_restores_after_unwind() {
    // This test process never enables lock metrics.
    for expected in [
        Lane::Admin,
        Lane::Ingest,
        Lane::Background,
        Lane::Interactive,
    ] {
        assert_eq!(
            lane::scope(expected, at_executor(executor())).await,
            expected
        );
    }
    let (admin, ingest) = tokio::join!(
        lane::scope(Lane::Admin, at_executor(executor())),
        lane::scope(Lane::Ingest, at_executor(executor())),
    );
    assert_eq!((admin, ingest), (Lane::Admin, Lane::Ingest));
    let unset = carry(lane::capture);
    lane::scope(Lane::Admin, async {
        assert_eq!(
            unset(),
            None,
            "carried absence overrides an unrelated task scope"
        );
        assert_eq!(lane::current(), Lane::Admin);
    })
    .await;
    std::thread::spawn(|| {
        assert_eq!(lane::capture(), None);
        let failed = std::panic::catch_unwind(|| lane::with_lane(Lane::Admin, || panic!("probe")));
        assert!(failed.is_err());
        assert_eq!(lane::capture(), None);
        assert_eq!(lane::current(), Lane::Interactive);
    })
    .join()
    .unwrap();
}
