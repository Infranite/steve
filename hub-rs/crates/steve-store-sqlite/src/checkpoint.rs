//! Checkpoint policy and per-Engine maintenance state; only the executor drives it.
use crate::{engine::Engine, open::db_error};
use std::time::Duration;
use steve_types::Error;

pub const PASSIVE_BYTES: u64 = 64 * 1024 * 1024;
pub const FORCE_BYTES: u64 = 512 * 1024 * 1024;
pub const INTERVAL: Duration = Duration::from_secs(30);
/// Policy result. ForcedPassive must be scheduled at the next writer gap and emit alert.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    None,
    Passive,
    Truncate,
    ForcedPassive { alert: bool },
}
/// Tunable byte/time thresholds; elapsed time is supplied by the executor's clock.
#[derive(Clone, Copy, Debug)]
pub struct CheckpointPolicy {
    pub passive_bytes: u64,
    pub force_bytes: u64,
    pub interval: Duration,
}
impl Default for CheckpointPolicy {
    fn default() -> Self {
        Self {
            passive_bytes: PASSIVE_BYTES,
            force_bytes: FORCE_BYTES,
            interval: INTERVAL,
        }
    }
}
impl CheckpointPolicy {
    /// An idle writer gap allows truncation even after a large WAL was checkpointed:
    /// PASSIVE alone does not shrink the allocated file. Under load, pressure forces
    /// PASSIVE and an alert. Busy truncation is retried by the executor on a later slice.
    pub fn decide(&self, wal_bytes: u64, idle: bool, since_last: Duration) -> Action {
        if idle && wal_bytes > 0 {
            Action::Truncate
        } else if wal_bytes >= self.force_bytes {
            Action::ForcedPassive { alert: true }
        } else if wal_bytes >= self.passive_bytes || since_last >= self.interval {
            Action::Passive
        } else {
            Action::None
        }
    }
}
/// Only supported checkpoint modes; SQL mode text cannot come from an untrusted string.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    Passive,
    Truncate,
}
impl Engine {
    /// Return SQLite's (busy, log, checkpointed) tuple. Busy is progress, not a fatal error;
    /// retry later. This writer access must be scheduled by the executor, like all writes.
    /// TRUNCATE never waits behind a pinned reader: temporarily replace the ordinary
    /// busy timeout, restoring it even when SQLite returns an error.
    pub fn checkpoint(&self, mode: Mode) -> Result<(i64, i64, i64), Error> {
        let sql = match mode {
            Mode::Passive => "PRAGMA main.wal_checkpoint(PASSIVE)",
            Mode::Truncate => "PRAGMA main.wal_checkpoint(TRUNCATE)",
        };
        let writer = self.writer_conn()?;
        let timeout: u32 = writer
            .pragma_query_value(None, "busy_timeout", |r| r.get(0))
            .map_err(db_error)?;
        writer.busy_timeout(Duration::ZERO).map_err(db_error)?;
        let result = writer.query_row(sql, [], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)));
        writer
            .busy_timeout(Duration::from_millis(u64::from(timeout)))
            .map_err(db_error)?;
        result.map_err(db_error)
    }
}

pub const OPTIMIZE_INTERVAL: Duration = Duration::from_secs(60 * 60);
pub const RETRY_INTERVAL: Duration = Duration::from_secs(1);
/// Runtime maintenance cadence; test clocks can advance without wall-clock sleeps.
#[derive(Clone, Copy, Debug)]
pub struct MaintenancePolicy {
    pub checkpoint: CheckpointPolicy,
    pub optimize_interval: Duration,
    pub retry_interval: Duration,
}
impl Default for MaintenancePolicy {
    fn default() -> Self {
        Self {
            checkpoint: CheckpointPolicy::default(),
            optimize_interval: OPTIMIZE_INTERVAL,
            retry_interval: RETRY_INTERVAL,
        }
    }
}
pub(crate) struct Maintenance {
    policy: MaintenancePolicy,
    last_checkpoint: Option<Duration>,
    last_optimize: Option<Duration>,
    checkpoint_retry: Option<Duration>,
    optimize_retry: Option<Duration>,
}
impl Default for Maintenance {
    fn default() -> Self {
        Self::new(MaintenancePolicy::default()).expect("positive default maintenance intervals")
    }
}
impl Maintenance {
    pub(crate) fn new(policy: MaintenancePolicy) -> Result<Self, Error> {
        if policy.checkpoint.interval.is_zero()
            || policy.optimize_interval.is_zero()
            || policy.retry_interval.is_zero()
            || policy.checkpoint.passive_bytes == 0
            || policy.checkpoint.force_bytes < policy.checkpoint.passive_bytes
        {
            return Err(steve_storage_api::error(
                steve_types::Code::Invalid,
                "invalid maintenance policy",
            ));
        }
        Ok(Self {
            policy,
            last_checkpoint: None,
            last_optimize: None,
            checkpoint_retry: None,
            optimize_retry: None,
        })
    }
    pub(crate) fn run(&mut self, engine: &Engine, now: Duration, idle: bool) -> Option<Duration> {
        let last_checkpoint = *self.last_checkpoint.get_or_insert(now);
        let last_optimize = *self.last_optimize.get_or_insert(now);
        if self.checkpoint_retry.is_none_or(|at| at <= now) {
            let mut wal = engine.path().into_os_string();
            wal.push("-wal");
            let bytes = match std::fs::metadata(std::path::PathBuf::from(wal)) {
                Ok(m) => Some(m.len()),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => Some(0),
                Err(_) => None,
            };
            if let Some(bytes) = bytes {
                let mut action =
                    self.policy
                        .checkpoint
                        .decide(bytes, idle, now.saturating_sub(last_checkpoint));
                if self.checkpoint_retry.is_some() && action == Action::None {
                    action = Action::Passive;
                }
                let mode = match action {
                    Action::None => None,
                    Action::Truncate => Some(Mode::Truncate),
                    Action::Passive | Action::ForcedPassive { .. } => Some(Mode::Passive),
                };
                if let Some(mode) = mode {
                    if let Action::ForcedPassive { alert: true } = action {
                        tracing::warn!(
                            wal_bytes = bytes,
                            "storage WAL pressure requires a forced checkpoint"
                        );
                        metrics::counter!("steve_store_checkpoint_alert_total").increment(1);
                    }
                    match engine.checkpoint(mode) {
                        Ok((busy, log, done)) => {
                            metrics::counter!("steve_store_checkpoint_total", "mode"=>if mode==Mode::Truncate {"truncate"} else {"passive"}).increment(1);
                            self.last_checkpoint = Some(now);
                            if busy != 0 || done < log {
                                self.checkpoint_retry =
                                    Some(now.saturating_add(self.policy.retry_interval));
                                metrics::counter!("steve_store_checkpoint_busy_total").increment(1);
                            } else {
                                self.checkpoint_retry = None;
                            }
                        }
                        Err(_) => {
                            tracing::warn!(
                                "storage checkpoint failed; retrying at a later writer gap"
                            );
                            self.checkpoint_retry =
                                Some(now.saturating_add(self.policy.retry_interval));
                        }
                    }
                }
            } else {
                self.checkpoint_retry = Some(now.saturating_add(self.policy.retry_interval));
            }
        }
        let optimize_due = self
            .optimize_retry
            .unwrap_or(last_optimize.saturating_add(self.policy.optimize_interval));
        if now >= optimize_due {
            let result = engine
                .writer_conn()
                .and_then(|c| crate::pragmas::optimize(&c).map_err(crate::open::db_error));
            match result {
                Ok(()) => {
                    self.last_optimize = Some(now);
                    self.optimize_retry = None;
                    metrics::counter!("steve_store_optimize_total").increment(1);
                }
                Err(_) => {
                    tracing::warn!("storage optimize failed; retrying at a later writer gap");
                    self.optimize_retry = Some(now.saturating_add(self.policy.retry_interval));
                }
            }
        }
        Some(
            self.checkpoint_retry
                .unwrap_or(
                    self.last_checkpoint
                        .unwrap()
                        .saturating_add(self.policy.checkpoint.interval),
                )
                .min(
                    self.optimize_retry.unwrap_or(
                        self.last_optimize
                            .unwrap()
                            .saturating_add(self.policy.optimize_interval),
                    ),
                ),
        )
    }
}
