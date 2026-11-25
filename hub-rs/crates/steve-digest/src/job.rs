//! Deterministic intake only. The ordinary digest schedule consumes packets through tools.
use std::time::Duration;

use steve_modules::Modules;

/// Cut windows from already stored messages once a minute. No history fetch, model or harness call.
pub fn task(m: Modules) -> steve_runtime::Task {
    let clock = m.clock.clone();
    steve_runtime::every(
        "digest.intake",
        clock,
        Duration::from_secs(60),
        steve_runtime::FirstRun::Immediate,
        move || {
            let m = m.clone();
            async move {
                let result = tokio::task::spawn_blocking(move || {
                    let now = m.clock.now();
                    for ws in m.store.workspaces().all()? {
                        if let Err(e) = crate::window::tick(&m.store, &ws.id, now) {
                            tracing::warn!(workspace = %ws.id, "digest intake: {e}");
                        }
                    }
                    Ok::<_, steve_store::StoreError>(())
                })
                .await;
                match result {
                    Ok(Ok(())) => {}
                    Ok(Err(e)) => tracing::warn!("digest intake: {e}"),
                    Err(e) => tracing::warn!("digest intake task: {e}"),
                }
            }
        },
    )
}

/// Cut windows from already stored messages once a minute. No history fetch, model or harness call.
pub async fn run(m: Modules) {
    task(m).await;
}
