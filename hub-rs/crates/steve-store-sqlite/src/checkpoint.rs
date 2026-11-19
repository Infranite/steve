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
    /// An idle maintenance deadline allows truncation after a large WAL was checkpointed:
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
        let writer = self.writer_conn()?;
        checkpoint_on(&writer, mode).map_err(db_error)
    }
}

// Keep raw SQLite failures for the internal, rate-limited diagnostic log; the
// public checkpoint API continues returning the sanitized transport error.
fn checkpoint_on(writer: &rusqlite::Connection, mode: Mode) -> rusqlite::Result<(i64, i64, i64)> {
    let sql = match mode {
        Mode::Passive => "PRAGMA main.wal_checkpoint(PASSIVE)",
        Mode::Truncate => "PRAGMA main.wal_checkpoint(TRUNCATE)",
    };
    let timeout: u32 = writer.pragma_query_value(None, "busy_timeout", |r| r.get(0))?;
    writer.busy_timeout(Duration::ZERO)?;
    let result = writer.query_row(sql, [], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)));
    writer.busy_timeout(Duration::from_millis(u64::from(timeout)))?;
    result
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
    checkpoint_errors: WarningLimit,
    pressure_warnings: WarningLimit,
    optimize_errors: WarningLimit,
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
            checkpoint_errors: WarningLimit::default(),
            pressure_warnings: WarningLimit::default(),
            optimize_errors: WarningLimit::default(),
        })
    }
    pub(crate) fn run(&mut self, engine: &Engine, now: Duration, idle: bool) -> Option<Duration> {
        self.last_checkpoint.get_or_insert(now);
        self.last_optimize.get_or_insert(now);
        let checkpoint_at = self.run_checkpoint(engine, now, idle);
        let optimize_at = self.run_optimize(engine, now);
        Some(checkpoint_at.min(optimize_at))
    }

    fn run_checkpoint(&mut self, engine: &Engine, now: Duration, idle: bool) -> Duration {
        if let Some(at) = self.checkpoint_retry.filter(|&at| at > now) {
            return at;
        }
        if let Err(error) = self.checkpoint_action(engine, now, idle) {
            self.checkpoint_errors.report(now, "checkpoint", &error);
            self.checkpoint_retry = Some(now.saturating_add(self.policy.retry_interval));
        }
        self.checkpoint_retry.unwrap_or(
            self.last_checkpoint
                .unwrap()
                .saturating_add(self.policy.checkpoint.interval),
        )
    }

    fn checkpoint_action(
        &mut self,
        engine: &Engine,
        now: Duration,
        idle: bool,
    ) -> Result<(), MaintenanceError> {
        let mut wal = engine.path().into_os_string();
        wal.push("-wal");
        let bytes = match std::fs::metadata(std::path::PathBuf::from(wal)) {
            Ok(m) => m.len(),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => 0,
            Err(e) => return Err(e.into()),
        };
        let mut action = self.policy.checkpoint.decide(
            bytes,
            idle,
            now.saturating_sub(self.last_checkpoint.unwrap()),
        );
        if self.checkpoint_retry.is_some() && action == Action::None {
            action = Action::Passive;
        }
        let mode = match action {
            Action::None => return Ok(()),
            Action::Truncate => Mode::Truncate,
            Action::Passive | Action::ForcedPassive { .. } => Mode::Passive,
        };
        if let Action::ForcedPassive { alert: true } = action {
            if self.pressure_warnings.allow(now) {
                tracing::warn!(
                    wal_bytes = bytes,
                    "storage WAL pressure requires a forced checkpoint"
                );
            }
            metrics::counter!("steve_store_checkpoint_alert_total").increment(1);
        }
        let writer = engine.writer_conn()?;
        let (busy, log, done) = checkpoint_on(&writer, mode)?;
        metrics::counter!("steve_store_checkpoint_total", "mode"=>if mode==Mode::Truncate {"truncate"} else {"passive"}).increment(1);
        self.last_checkpoint = Some(now);
        self.checkpoint_errors = WarningLimit::default();
        self.checkpoint_retry = if busy != 0 || done < log {
            metrics::counter!("steve_store_checkpoint_busy_total").increment(1);
            Some(now.saturating_add(self.policy.retry_interval))
        } else {
            None
        };
        Ok(())
    }

    fn run_optimize(&mut self, engine: &Engine, now: Duration) -> Duration {
        let due = self.optimize_retry.unwrap_or(
            self.last_optimize
                .unwrap()
                .saturating_add(self.policy.optimize_interval),
        );
        if now < due {
            return due;
        }
        let result = engine
            .writer_conn()
            .map_err(MaintenanceError::from)
            .and_then(|writer| crate::pragmas::optimize(&writer).map_err(MaintenanceError::from));
        match result {
            Ok(()) => {
                self.last_optimize = Some(now);
                self.optimize_retry = None;
                self.optimize_errors = WarningLimit::default();
                metrics::counter!("steve_store_optimize_total").increment(1);
                now.saturating_add(self.policy.optimize_interval)
            }
            Err(error) => {
                self.optimize_errors.report(now, "optimize", &error);
                let retry = now.saturating_add(self.policy.retry_interval);
                self.optimize_retry = Some(retry);
                retry
            }
        }
    }
}

#[derive(Debug, thiserror::Error)]
enum MaintenanceError {
    #[error(transparent)]
    Store(#[from] Error),
    #[error(transparent)]
    Sqlite(#[from] rusqlite::Error),
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

// Keep retries responsive without emitting a warning every second during a
// persistent failure. Checkpoint and optimize have independent warning budgets.
const ERROR_LOG_INTERVAL: Duration = Duration::from_secs(60);
#[derive(Default)]
struct WarningLimit {
    next: Option<Duration>,
}
impl WarningLimit {
    fn allow(&mut self, now: Duration) -> bool {
        if self.next.is_some_and(|next| now < next) {
            return false;
        }
        self.next = Some(now.saturating_add(ERROR_LOG_INTERVAL));
        true
    }
    fn report(&mut self, now: Duration, operation: &'static str, error: &impl std::fmt::Display) {
        if self.allow(now) {
            tracing::warn!(operation, %error, "storage maintenance failed; retrying at a later writer gap");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn persistent_failures_warn_at_most_once_per_minute_and_recovery_resets_the_limit() {
        let mut failures = WarningLimit::default();
        let allowed: Vec<_> = (0..=120)
            .filter(|&s| failures.allow(Duration::from_secs(s)))
            .collect();
        assert_eq!(allowed, [0, 60, 120]);
        failures = WarningLimit::default(); // Successful maintenance resets its failure streak.
        assert!(failures.allow(Duration::from_secs(121)));
    }
}
