//! Deterministic intake only. The ordinary digest schedule consumes packets through tools.
use std::time::Duration;

use steve_modules::Modules;

/// Deterministic window-cutting cadence over already-stored messages (no model or harness calls).
pub const INTAKE_INTERVAL: Duration = Duration::from_secs(60);

/// Cut windows from already stored messages once a minute. No history fetch, model or harness call.
pub async fn run(m: Modules) {
    let mut interval = tokio::time::interval(INTAKE_INTERVAL);
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        interval.tick().await;
        let m = m.clone();
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
}
