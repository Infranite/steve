//! The outbox dispatcher (steve-store [`outbox`]): Work's and Automation's consumers of
//! `work.item.done`, Mail's of `mail.event.ready` and GitHub's of `github.after`, registered while
//! the Hub is wired (before anything can finish an item), and run as a job once the flow scheduler
//! recovered interrupted runs, woken by each commit that emitted an event and every [`FALLBACK`]
//! in case a wake-up was missed.

use std::sync::Arc;
use std::time::Duration;

use steve_store::Store;
use steve_store::outbox::{self, Dispatcher};

/// A pass without a wake-up, in case one was missed (a commit through a repository hook).
pub const FALLBACK: Duration = Duration::from_secs(60);
/// How often fully delivered events older than [`outbox::KEEP_MS`] are pruned.
pub const PRUNE_EVERY: Duration = Duration::from_secs(3600);

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

fn now_ms() -> i64 {
    steve_store::now()
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
        let db = steve_core::repo::Db::new(store);
        for g in &groups {
            let Some(ws) = g.workspace_id.as_deref() else {
                continue;
            };
            if let Err(e) =
                steve_core::notify::notify(&db, ws, "system", "fail", &dead_notice(g), None)
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

/// The `outbox.dispatch` job. A panicking or failing pass is logged and the loop goes on.
pub async fn run(d: Arc<Dispatcher>, store: Store, flow: Arc<steve_flow::FlowCx>) {
    flow.recovered().await;
    let wake = store.outbox_wake();
    let mut pruned = std::time::Instant::now();
    tracing::info!("outbox dispatcher started");
    loop {
        // drain: a full batch may leave more behind
        loop {
            let (d2, s2) = (d.clone(), store.clone());
            match tokio::task::spawn_blocking(move || d2.pump(&s2, now_ms())).await {
                Ok(Ok(p)) => {
                    notice_dead(&store, &p.newly_dead).await;
                    if p.done + p.retried + p.dead < outbox::BATCH {
                        break;
                    }
                }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn a_dead_group_leaves_one_failure_notice_in_its_workspace() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path().join("hub.sqlite3"), steve_store::NoPublisher).unwrap();
        store
            .with_conn(|c| {
                Ok(c.execute(
                    "INSERT INTO workspaces (id, name, created) VALUES ('default', '个人', 1)",
                    [],
                )?)
            })
            .unwrap();
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
        let n: i64 = store
            .with_conn(|c| {
                Ok(c.query_row(
                    "SELECT count(*) FROM messages WHERE workspace_id = 'default' AND space = 'notify' AND text = ?",
                    [&text],
                    |r| r.get(0),
                )?)
            })
            .unwrap();
        assert_eq!(n, 1);
    }
}
