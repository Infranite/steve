//! Deterministic intake only. Phase D owns consuming packets and starting a digest Bot.
use std::time::Duration;

use steve_modules::Modules;

/// Cut windows from already stored messages once a minute. No history fetch, model or harness call.
pub fn spawn_job(m: Modules) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(60));
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
    })
}
