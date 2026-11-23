//! The durable digest packet queue.
//!
//! A packet is `queued` when a window is cut, `leased` to one digest run (`batch_id`, until `lease_until`),
//! then `done`, or back to `queued` when the run failed (its finished chunks stay finished, so a retry only
//! redoes the rest), and `failed` after [`Batching::max_attempts`]. A lease that runs out returns the packet to
//! the queue. A batch is due every [`Batching::every_ms`] while packets wait, or as soon as
//! [`Batching::packets`] are waiting; higher priority goes first.
//!
//! The batching knobs are the workspace setting `digest` (JSON): `every_min`, `packets`, `chunk_tokens`,
//! `overlap`, `context`, `backlog_days`, `lease_min`, `max_attempts`, `enabled`.

use steve_store::{Store, StoreError};

pub use steve_store::digest::{Batching, NewPacket, Packet};

type Result<T> = std::result::Result<T, StoreError>;

/// Starts the ordinary Bot schedule; the Hub supplies policy and execution, tests supply a consumer.
/// Packet retries and dead letters remain in the durable queue, never in the runner.
pub trait BotRunner: Send + Sync + 'static {
    /// None when unavailable/busy; otherwise the earliest time another start is allowed.
    fn ready(&self, ws: &str) -> Result<Option<i64>>;
    /// Expected schedule rejections must persist a failed/skipped run and return Ok, advancing
    /// ready's deadline. Err is reserved for source failures (storage/read/persistence failure).
    fn start(&self, ws: &str, now: i64) -> Result<()>;
}

/// One due batch contains at most this many workspaces (each starts at most one Bot).
pub const BOT_BATCH: usize = 32;

/// Tables read by BotSource and the Hub's schedule readiness/reference checks.
/// Production and real-store wake tests must subscribe to this same set.
pub const BOT_WAKE_TABLES: &[&str] = &[
    "digest_packets",
    "settings",
    "flows",
    "flow_triggers",
    "workspace_members",
    "bots",
    "bot_workspaces",
    "projects",
    "tags",
    "flow_runs",
    "items",
];

/// Queue/lease deadlines over workspaces with unfinished packets; never scans all workspaces.
pub struct BotSource<R> {
    store: steve_store::Store,
    clock: std::sync::Arc<dyn steve_runtime::Clock>,
    runner: std::sync::Arc<R>,
}
impl<R: BotRunner> BotSource<R> {
    pub fn new(m: &steve_modules::Modules, runner: R) -> Self {
        Self {
            store: m.store.clone(),
            clock: m.clock.clone(),
            runner: std::sync::Arc::new(runner),
        }
    }
}

fn queue_deadlines(store: &Store, now: i64) -> Result<Vec<(String, Option<i64>, Option<i64>)>> {
    store.read(|r| {
        let mut packets = r.digest_packets();
        packets
            .waiting_workspaces()?
            .into_iter()
            .map(|(ws, lease)| {
                let b = packets.batching(&ws)?;
                let due = if b.enabled {
                    packets.due_at(&ws, now, &b)?.map(|(at, _)| at)
                } else {
                    None
                };
                Ok((ws, lease, due))
            })
            .collect()
    })
}

fn bot_work(store: &Store, runner: &impl BotRunner, now: i64) -> Result<Vec<(i64, String)>> {
    let mut work = Vec::new();
    for (ws, mut at, queued) in queue_deadlines(store, now)? {
        if let Some(queued) = queued {
            match runner.ready(&ws) {
                Ok(Some(ready)) => {
                    let queued = queued.max(ready);
                    at = Some(at.map_or(queued, |lease| lease.min(queued)));
                }
                Ok(None) => {}
                Err(e) => tracing::warn!(workspace = %ws, "digest Bot binding: {e}"),
            }
        }
        if let Some(at) = at {
            work.push((at, ws));
        }
    }
    work.sort();
    Ok(work)
}

fn due_error(e: impl std::fmt::Display) -> steve_runtime::due::DueError {
    steve_runtime::due::DueError(e.to_string())
}

impl<R: BotRunner> steve_runtime::due::DueSource for BotSource<R> {
    fn name(&self) -> &'static str {
        "digest.bot"
    }

    async fn next_due(&self) -> std::result::Result<Option<i64>, steve_runtime::due::DueError> {
        let (store, runner, now) = (self.store.clone(), self.runner.clone(), self.clock.now());
        tokio::task::spawn_blocking(move || {
            Ok::<_, StoreError>(
                bot_work(&store, runner.as_ref(), now)?
                    .first()
                    .map(|(at, _)| *at),
            )
        })
        .await
        .map_err(due_error)?
        .map_err(due_error)
    }

    async fn run_due(&self, now: i64) -> std::result::Result<(), steve_runtime::due::DueError> {
        let (store, runner) = (self.store.clone(), self.runner.clone());
        tokio::task::spawn_blocking(move || -> Result<()> {
            let work = bot_work(&store, runner.as_ref(), now)?;
            let mut error = None;
            // Filter before LIMIT: future workspaces cannot hide a due queue or expired lease.
            for (_, ws) in work
                .into_iter()
                .filter(|(at, _)| *at <= now)
                .take(BOT_BATCH)
            {
                let step = (|| -> Result<()> {
                    store.write(|w| w.digest_packets().release_expired(&ws, now))?;
                    let due = store.read::<_, steve_store::StoreError>(|r| {
                        let mut packets = r.digest_packets();
                        let b = packets.batching(&ws)?;
                        Ok(b.enabled && packets.due(&ws, now, &b)?.is_some())
                    })?;
                    if due && runner.ready(&ws)?.is_some_and(|at| at <= now) {
                        runner.start(&ws, now)?;
                    }
                    Ok(())
                })();
                if let Err(e) = step {
                    tracing::warn!(workspace = %ws, "digest Bot queue: {e}");
                    // Expected start rejections have already been recorded by the runner.
                    // Only source failures back off, after the other workspaces run.
                    error.get_or_insert(e);
                }
            }
            error.map_or(Ok(()), Err)
        })
        .await
        .map_err(due_error)?
        .map_err(due_error)
    }
}
