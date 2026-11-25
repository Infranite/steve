//! Deterministic intake only. The ordinary digest schedule consumes packets through tools.
use std::sync::{
    Arc,
    atomic::{AtomicI64, Ordering},
};
use std::time::Duration;

use steve_modules::Modules;
use steve_runtime::{
    due::{DueError, DueSource, due_loop},
    policy::{Backoff, Jitter},
};
use tokio::sync::Notify;

/// Deterministic window-cutting cadence over already-stored messages (no model or harness calls).
pub const INTAKE_INTERVAL: Duration = Duration::from_secs(60);
const RETRY_BASE: Duration = Duration::from_secs(5);
const RETRY_MAX: Duration = Duration::from_secs(60);

struct DigestIntakeSource {
    modules: Modules,
    next_at: AtomicI64,
}

impl DueSource for DigestIntakeSource {
    fn name(&self) -> &'static str {
        "digest.intake"
    }

    async fn next_due(&self) -> Result<Option<i64>, DueError> {
        Ok(Some(self.next_at.load(Ordering::Relaxed)))
    }

    async fn run_due(&self, now: i64) -> Result<(), DueError> {
        let m = self.modules.clone();
        let result = tokio::task::spawn_blocking(move || {
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
        self.next_at.store(
            now.saturating_add(INTAKE_INTERVAL.as_millis() as i64),
            Ordering::Relaxed,
        );
        Ok(())
    }
}

/// Cut windows from already stored messages once a minute via [`due_loop`]. No history fetch,
/// model or harness call.
pub fn task(m: Modules) -> steve_runtime::Task {
    let clock = m.clock.clone();
    let first_at = clock.now();
    let source = Arc::new(DigestIntakeSource {
        modules: m,
        next_at: AtomicI64::new(first_at),
    });
    let backoff = Backoff {
        base: RETRY_BASE,
        max: RETRY_MAX,
        factor: 2,
        jitter: Jitter::Equal,
        attempts: None,
        reset_after: None,
    };
    steve_runtime::spawn(
        "digest.intake",
        due_loop(source, clock, Arc::new(Notify::new()), backoff),
    )
}

/// Cut windows from already stored messages once a minute. No history fetch, model or harness call.
pub async fn run(m: Modules) {
    task(m).await;
}
