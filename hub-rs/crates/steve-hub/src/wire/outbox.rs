//! The outbox dispatcher (steve-store [`outbox`]): Work's and Automation's consumers of
//! `work.item.done`, registered while the Hub is wired (before anything can finish an item), and run
//! as a job once the flow scheduler recovered interrupted runs, woken by each commit that emitted an
//! event and every [`FALLBACK`] in case a wake-up was missed.

use std::sync::Arc;
use std::time::Duration;

use steve_store::Store;
use steve_store::outbox::{self, Dispatcher};

/// A pass without a wake-up, in case one was missed (a commit through a repository hook).
pub const FALLBACK: Duration = Duration::from_secs(60);
/// How often fully delivered events older than [`outbox::KEEP_MS`] are pruned.
pub const PRUNE_EVERY: Duration = Duration::from_secs(3600);

/// The consumers of this Hub, registered: those new to the database start at the beginning of their
/// topic, so nothing this version emitted before is skipped.
pub fn dispatcher(
    store: &Store,
    core: &steve_core::Core,
    flow: &Arc<steve_flow::FlowCx>,
) -> Result<Arc<Dispatcher>, steve_store::StoreError> {
    let mut consumers = core.outbox_consumers();
    consumers.push(steve_flow::ItemTrigger::new(flow.clone()));
    let d = Arc::new(Dispatcher::new(consumers));
    d.register(store)?;
    Ok(d)
}

fn now_ms() -> i64 {
    steve_store::now()
}

/// The `outbox.dispatch` job. A panicking or failing pass is logged and the loop goes on.
pub async fn run(d: Arc<Dispatcher>, store: Store, flow: Arc<steve_flow::FlowCx>) {
    flow.recovered().await;
    let wake = store.outbox_wake();
    let mut pruned = std::time::Instant::now();
    tracing::info!(owner = d.owner(), "outbox dispatcher started");
    loop {
        // drain: a full batch may leave more behind
        loop {
            let (d2, s2) = (d.clone(), store.clone());
            match tokio::task::spawn_blocking(move || d2.pump(&s2, now_ms())).await {
                Ok(Ok(p)) if p.done + p.retried + p.dead >= outbox::BATCH => continue,
                Ok(Ok(_)) => break,
                Ok(Err(e)) => {
                    tracing::error!("outbox pass: {e}");
                    break;
                }
                Err(e) => {
                    tracing::error!("outbox pass panicked: {e}");
                    break;
                }
            }
        }
        if pruned.elapsed() >= PRUNE_EVERY {
            pruned = std::time::Instant::now();
            let s2 = store.clone();
            match tokio::task::spawn_blocking(move || outbox::prune(&s2, now_ms(), outbox::KEEP_MS))
                .await
            {
                Ok(Ok(n)) if n > 0 => tracing::info!("outbox: pruned {n} delivered events"),
                Ok(Ok(_)) => {}
                Ok(Err(e)) => tracing::error!("outbox prune: {e}"),
                Err(e) => tracing::error!("outbox prune panicked: {e}"),
            }
        }
        tokio::select! {
            _ = wake.notified() => {}
            _ = tokio::time::sleep(FALLBACK) => {}
        }
    }
}
