//! One snapshot per local day at or after 03:30, with a retry after unsuccessful attempts.
use crate::backup::{self, Purpose};
use anyhow::Result;
use jiff::{Timestamp, tz::TimeZone};
use std::{
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicI64, Ordering},
    },
    time::Duration,
};
use steve_runtime::{
    Clock, SystemClock,
    due::{DueError, DueSource, due_loop},
    policy::{Backoff, Jitter},
};
use tokio::sync::Notify;

const FIRST: Duration = Duration::from_secs(300);
const TICK: Duration = Duration::from_secs(600);
const RETRY: Duration = Duration::from_secs(3600);
const LOOP_RETRY_BASE: Duration = Duration::from_secs(60);

fn attempt(home: &Path, db: &Path, store: &steve_store::Store) -> Result<()> {
    if !backup::daily_settings(store)?.enabled {
        return Ok(());
    }
    let zone = TimeZone::system();
    let now = Timestamp::now().to_zoned(zone.clone());
    if i32::from(now.hour()) * 60 + i32::from(now.minute()) < 3 * 60 + 30 {
        return Ok(());
    }
    let inventory = backup::inventory(home);
    if let Some(error) = inventory.error {
        anyhow::bail!("backup inventory: {error}");
    }
    if let Some(last) = inventory.last
        && Timestamp::from_millisecond(last.at as i64)?
            .to_zoned(zone)
            .date()
            == now.date()
    {
        return Ok(());
    }
    let receipt = backup::create(home, db, Purpose::Daily)?;
    tracing::info!(snapshot = %receipt.snapshot_id, "daily backup completed");
    Ok(())
}

struct DailyBackupSource {
    home: PathBuf,
    db: PathBuf,
    store: steve_store::Store,
    next_at: AtomicI64,
}

impl DueSource for DailyBackupSource {
    fn name(&self) -> &'static str {
        "backup.daily"
    }

    async fn next_due(&self) -> Result<Option<i64>, DueError> {
        Ok(Some(self.next_at.load(Ordering::Relaxed)))
    }

    async fn run_due(&self, now: i64) -> Result<(), DueError> {
        let (home, db, store) = (self.home.clone(), self.db.clone(), self.store.clone());
        let result = tokio::task::spawn_blocking(move || attempt(&home, &db, &store)).await;
        let wait_ms = match result {
            Ok(Ok(())) => TICK.as_millis() as i64,
            other => {
                tracing::warn!(result = ?other, "daily backup will retry");
                RETRY.as_millis() as i64
            }
        };
        self.next_at
            .store(now.saturating_add(wait_ms), Ordering::Relaxed);
        Ok(())
    }
}

/// The background loop as a supervised [`steve_runtime::Task`] driven by [`due_loop`]: checks
/// whether the day's snapshot is due and backs off for an hour on failure.
pub fn task(
    home: PathBuf,
    db: PathBuf,
    store: steve_store::Store,
    on: bool,
) -> steve_runtime::Task {
    if !on {
        return steve_runtime::spawn("backup.daily", async {
            tracing::info!("daily backup turned off (STEVE_BACKUP=off)");
        });
    }
    let clock = Arc::new(SystemClock);
    let first_at = clock.now().saturating_add(FIRST.as_millis() as i64);
    let source = Arc::new(DailyBackupSource {
        home,
        db,
        store,
        next_at: AtomicI64::new(first_at),
    });
    let backoff = Backoff {
        base: LOOP_RETRY_BASE,
        max: RETRY,
        factor: 2,
        jitter: Jitter::Equal,
        attempts: None,
        reset_after: None,
    };
    steve_runtime::spawn(
        "backup.daily",
        due_loop(source, clock, Arc::new(Notify::new()), backoff),
    )
}

/// The background loop (a Hub job): looks every ten minutes whether the day's snapshot is due.
pub async fn run(home: PathBuf, db: PathBuf, store: steve_store::Store, on: bool) {
    task(home, db, store, on).await;
}
