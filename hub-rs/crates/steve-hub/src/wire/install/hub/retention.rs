use super::*;
use steve_store::retention::{Backoff, Clock, Jitter, SystemClock, due_loop};

install! { install(reg, cx) [db, mut jobs] => [db, jobs] {
    let HubEnv { cfg, .. } = *cx;
    if !cfg.no_resume {
        jobs.push(Box::pin(run(db.clone())));
    }
}}

async fn run(db: steve_store::Store) {
    let clock = Arc::new(SystemClock);
    let source = Arc::new(steve_store::retention::Source::new(
        clock.now(),
        move |kind, now, after| {
            let db = db.clone();
            async move {
                tokio::task::spawn_blocking(move || {
                    db.retention_slice(
                        kind,
                        now,
                        steve_store::retention::Budget {
                            rows: 500,
                            target_ms: 20,
                        },
                        after,
                    )
                })
                .await
                .map_err(|e| steve_store::StoreError::Conflict(e.to_string()))?
            }
        },
    ));
    due_loop(
        source,
        clock,
        Arc::new(tokio::sync::Notify::new()),
        Backoff {
            base: std::time::Duration::from_secs(1),
            max: std::time::Duration::from_secs(60),
            factor: 2,
            jitter: Jitter::None,
            attempts: None,
            reset_after: None,
        },
    )
    .await;
}
