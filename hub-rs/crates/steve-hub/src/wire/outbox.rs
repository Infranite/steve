//! The outbox dispatcher (steve-store [`outbox`]): Work's and Automation's consumers of
//! `work.item.done`, Mail's of `mail.event.ready` and GitHub's of `github.after`, registered while
//! the Hub is wired (before anything can finish an item), and run as a job once the flow scheduler
//! recovered interrupted runs, woken by each commit that emitted an event and every [`FALLBACK`]
//! in case a wake-up was missed.

use std::sync::Arc;
use std::time::Duration;

use steve_runtime::{
    SystemClock,
    due::{DueError, DueSource, due_loop},
    policy::{Backoff, Jitter},
};
use steve_store::Store;
use steve_store::outbox::{self, Dispatcher};

/// A pass without a wake-up, in case one was missed (a commit through a repository hook).
pub const FALLBACK: Duration = steve_runtime::due::FALLBACK;

const RETRY_BASE: Duration = Duration::from_secs(1);
const RETRY_MAX: Duration = Duration::from_secs(60);

/// The consumers of this Hub, registered: those new to the database start at the beginning of their
/// topic, so nothing this version emitted before is skipped. `modules`: the other modules' consumers
/// (a stored mail's event).
pub fn dispatcher(
    store: &Store,
    core: &steve_core::Core,
    flow: &Arc<steve_flow::FlowCx>,
    modules: Vec<Arc<dyn outbox::Consumer>>,
) -> Result<Arc<Dispatcher>, steve_store::StoreError> {
    let mut consumers = core.outbox_consumers();
    consumers.push(steve_flow::ItemTrigger::new(flow.clone()));
    consumers.extend(modules);
    let d = Arc::new(Dispatcher::new(consumers));
    d.register(store)?;
    Ok(d)
}

/// The text of the one notice a consumer's first dead delivery in a workspace gets (stored: zh-CN).
pub fn dead_notice(g: &outbox::DeadGroup) -> String {
    let why = g
        .last_error
        .as_deref()
        .map(|e| e.chars().take(200).collect::<String>())
        .unwrap_or_default();
    steve_i18n::stored(|| {
        steve_i18n::t!(
            "hub.notice.outbox_dead",
            consumer = g.consumer,
            topic = g.topic,
            why = why
        )
    })
}

/// One notice per consumer and workspace that has its first dead delivery (until it is revived).
async fn notice_dead(store: &Store, groups: &[outbox::DeadGroup]) {
    if groups.is_empty() {
        return;
    }
    let (store, groups) = (store.clone(), groups.to_vec());
    let r = tokio::task::spawn_blocking(move || {
        for g in &groups {
            let Some(ws) = g.workspace_id.as_deref() else {
                continue;
            };
            if let Err(e) =
                steve_store::notify::notify(&store, ws, "system", "fail", &dead_notice(g), None)
            {
                tracing::error!(consumer = %g.consumer, "outbox dead notice: {e}");
            }
        }
    })
    .await;
    if let Err(e) = r {
        tracing::error!("outbox dead notice panicked: {e}");
    }
}

struct OutboxSource {
    dispatcher: Arc<Dispatcher>,
    store: Store,
}

impl DueSource for OutboxSource {
    fn name(&self) -> &'static str {
        "outbox.dispatch"
    }

    async fn next_due(&self) -> Result<Option<i64>, DueError> {
        let (d, s) = (self.dispatcher.clone(), self.store.clone());
        tokio::task::spawn_blocking(move || d.next_due(&s))
            .await
            .map_err(|e| DueError(format!("outbox next_due panicked: {e}")))?
            .map_err(|e| DueError(e.to_string()))
    }

    async fn run_due(&self, now: i64) -> Result<(), DueError> {
        let (d, s) = (self.dispatcher.clone(), self.store.clone());
        let pumped = tokio::task::spawn_blocking(move || d.pump(&s, now))
            .await
            .map_err(|e| DueError(format!("outbox pass panicked: {e}")))?
            .map_err(|e| DueError(e.to_string()))?;
        notice_dead(&self.store, &pumped.newly_dead).await;
        Ok(())
    }
}

/// The `outbox.dispatch` job, driven by [`due_loop`] and woken on every outbox commit.
pub async fn run(d: Arc<Dispatcher>, store: Store, flow: Arc<steve_flow::FlowCx>) {
    flow.recovered().await;
    let wake = store.outbox_wake();
    tracing::info!("outbox dispatcher started");
    let source = Arc::new(OutboxSource {
        dispatcher: d,
        store,
    });
    let backoff = Backoff {
        base: RETRY_BASE,
        max: RETRY_MAX,
        factor: 2,
        jitter: Jitter::Equal,
        attempts: None,
        reset_after: None,
    };
    due_loop(source, Arc::new(SystemClock), wake, backoff).await;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn a_dead_group_leaves_one_failure_notice_in_its_workspace() {
        let dir = tempfile::tempdir().unwrap();
        let store =
            Store::open(dir.path().join("steve.sqlite3"), steve_store::NoPublisher).unwrap();
        steve_store::testkit::hub::outbox_checks::seed_notice_workspace(&store).unwrap();
        let g = outbox::DeadGroup {
            consumer: "github.after".into(),
            topic: "github.after".into(),
            workspace_id: Some("default".into()),
            count: 1,
            last_error: Some("connection reset".into()),
            updated: 1,
        };
        notice_dead(&store, std::slice::from_ref(&g)).await;
        let text = dead_notice(&g);
        assert!(
            text.contains("github.after") && text.contains("connection reset"),
            "{text}"
        );
        let n: i64 =
            steve_store::testkit::hub::hub_fixtures::messages_count_default_notify(&store, &text)
                .unwrap();
        assert_eq!(n, 1);
    }
}
