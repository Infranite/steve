//! `steve-hub`: one HTTP server — /mcp for agent tools, /auth and /api for the web app, static files for the rest.

use std::net::SocketAddr;
use std::sync::Arc;

use anyhow::Context;
use steve_hub::api::AppState;
use steve_hub::bus::Bus;
use steve_hub::config::Config;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    if std::env::args().any(|arg| arg == "--build-info") {
        println!(
            "{}",
            serde_json::json!({"version":option_env!("STEVE_BUILD_VERSION"),"commit":option_env!("STEVE_BUILD_COMMIT"),"target":env!("STEVE_BUILD_TARGET"),"schema":steve_store::db::SCHEMA_VERSION})
        );
        return Ok(());
    }
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_env("STEVE_LOG")
                .unwrap_or_else(|_| "info".into()),
        )
        .with_target(false)
        .init();

    steve_hub::wire::tune_sqlite();
    let cfg = Arc::new(Config::from_env()?);
    if std::env::args().any(|arg| arg == "--supervise") {
        return steve_hub::upgrades::supervise((*cfg).clone()).await;
    }
    let _instance_lock = steve_hub::upgrades::instance_lock(&cfg)?;
    let supervised_domain: Arc<std::sync::Mutex<Option<Arc<dyn steve_hub::backend::Domain>>>> =
        Arc::new(std::sync::Mutex::new(None));
    if std::env::var("STEVE_MANAGED").as_deref() == Ok("1") {
        let expected: i32 = std::env::var("STEVE_SUPERVISOR_PID")
            .context("managed child needs supervisor PID")?
            .parse()?;
        anyhow::ensure!(
            expected > 1 && unsafe { libc::getppid() } == expected,
            "managed child lost its supervisor before boot"
        );
        let domain = supervised_domain.clone();
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(std::time::Duration::from_millis(500)).await;
                if unsafe { libc::getppid() } != expected {
                    let domain = domain.lock().unwrap_or_else(|e| e.into_inner()).clone();
                    if let Some(domain) = domain {
                        let _ = tokio::time::timeout(
                            std::time::Duration::from_secs(15),
                            domain.shutdown(),
                        )
                        .await;
                    }
                    steve_hub::upgrades::exit_orphaned_managed_hub();
                }
            }
        });
    }
    steve_hub::upgrades::startup_gate(&cfg).await?;
    let bus = Bus::new();
    let w = steve_hub::wire::wire(&cfg, &bus)?;
    *supervised_domain.lock().unwrap_or_else(|e| e.into_inner()) = Some(w.domain.clone());
    let st = AppState::new(cfg.clone(), w.backend, w.domain, bus)
        .with_modules(w.modules)
        .with_nodes(w.nodes);
    steve_hub::app::boot(&st).await?;

    for job in w.jobs {
        tokio::spawn(job);
    }
    let app = steve_hub::app::app(st.clone(), w.auth, w.mcp, w.module_routes, w.root_routes);
    let listener = tokio::net::TcpListener::bind((cfg.listen.host.as_str(), cfg.listen.port))
        .await
        .with_context(|| format!("listen on {}:{}", cfg.listen.host, cfg.listen.port))?;
    // recovery trouble is reported (`/healthz`.recovery), never a reason not to serve
    if let Err(e) = st.domain.ready().await {
        tracing::error!("recovery after start: {e}");
        steve_hub::recovery::failed(&e.to_string());
    }
    println!(
        "steve hub {} on http://{}:{}",
        cfg.version, cfg.listen.host, cfg.listen.port
    );

    // SIGINT/SIGTERM: stop the adapters and exit at once; in-flight turns are not drained (as in TS)
    let domain = st.domain.clone();
    tokio::spawn(async move {
        let mut term =
            match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
                Ok(s) => s,
                Err(e) => {
                    tracing::error!("signal handler: {e}");
                    return;
                }
            };
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {}
            _ = term.recv() => {}
        }
        domain.shutdown().await;
        std::process::exit(0);
    });

    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .await?;
    Ok(())
}
