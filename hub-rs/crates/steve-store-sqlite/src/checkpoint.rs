//! Pure checkpoint policy; the executor schedules and retries, never this module.
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
#[derive(Clone, Copy, Debug)]
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
