use super::*;
use std::sync::atomic::{AtomicI64, Ordering};
use std::time::Duration;
use steve_modules::vault::SecretUses;
use steve_runtime::due::{DueError, DueSource, FALLBACK, due_loop};
use steve_runtime::policy::{Backoff, Jitter};
use steve_store::node_write;

install! { install(reg, _cx) [mut jobs, modules, tools] => [jobs, modules, tools] {
    let source = Source {
        modules: modules.clone(),
        tools: tools.clone(),
        channel_scan_at: AtomicI64::new(modules.clock.now()),
    };
    let clock = modules.clock.clone();
    let wake = modules.store.commit_wake(&["runtime_permissions", "secret_uses", "mems", "mem_candidates"]);
    jobs.push(Job::new("decisions.expire", due_loop(
        Arc::new(source),
        clock,
        wake,
        Backoff {
            base: Duration::from_secs(1),
            max: FALLBACK,
            factor: 2,
            jitter: Jitter::None,
            attempts: None,
            reset_after: None,
        },
    )));
}}

struct Source {
    modules: Modules,
    tools: Tools,
    channel_scan_at: AtomicI64,
}

impl DueSource for Source {
    fn name(&self) -> &'static str {
        "decisions.expire"
    }

    async fn next_due(&self) -> Result<Option<i64>, DueError> {
        let channel_scan_at = self.channel_scan_at.load(Ordering::Relaxed);
        self.modules.blocking(move |m| {
            m.store.read(|r| {
                Ok([
                    Some(channel_scan_at),
                    node_write::next_permission_expiry(r)?,
                    steve_knowledge::candidates::next_expiry(r)?,
                    SecretUses::next_expiry(r)?,
                ].into_iter().flatten().min())
            })
        }).await.map_err(|e| DueError(e.to_string()))
    }

    async fn run_due(&self, now: i64) -> Result<(), DueError> {
        let tools = self.tools.clone();
        let scan_channels = self.channel_scan_at.load(Ordering::Relaxed) <= now;
        let (mut error, channels) = tokio::task::spawn_blocking({
            let modules = self.modules.clone();
            move || {
                let results = [
                    crate::wire::node_runtime::NodeRuntime::expire_permissions(&modules.store, now)
                        .map_err(|e| DueError(e.to_string())),
                    steve_knowledge::candidates::expire(&modules, now)
                        .map(|_| ()).map_err(|e| DueError(e.to_string())),
                    tools.uses.expire_due(now).map_err(|e| DueError(e.to_string())),
                ];
                let channels = scan_channels.then(|| tools.outbox.expire_stale(&tools.channels, now));
                (results.into_iter().find_map(Result::err), channels)
            }
        }).await.map_err(|e| DueError(e.to_string()))?;
        match channels {
            Some(Ok(false)) => self.channel_scan_at.store(
                now.saturating_add(FALLBACK.as_millis() as i64), Ordering::Relaxed,
            ),
            Some(Err(e)) => { error.get_or_insert_with(|| DueError(e.to_string())); }
            Some(Ok(true)) | None => {}
        }
        error.map_or(Ok(()), Err)
    }
}
