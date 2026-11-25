//! One snapshot per local day at or after 03:30, with a retry after unsuccessful attempts.
use crate::backup::{self, Purpose};
use anyhow::Result;
use jiff::{Timestamp, tz::TimeZone};
use std::{
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicU32, Ordering},
    },
    time::Duration,
};

const FIRST: Duration = Duration::from_secs(300);
const TICK: Duration = Duration::from_secs(600);
const RETRY: Duration = Duration::from_secs(3600);
const RETRY_TICKS: u32 = (RETRY.as_secs() / TICK.as_secs()) as u32;

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

/// The background loop as a supervised periodic [`steve_runtime::Task`]: looks every ten minutes
/// whether the day's snapshot is due.
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
    let wait = Arc::new(AtomicU32::new(0));
    steve_runtime::every_after(
        "backup.daily",
        Arc::new(steve_runtime::SystemClock),
        TICK,
        steve_runtime::FirstRun::After(FIRST),
        move || {
            let (home, db, store, wait) = (home.clone(), db.clone(), store.clone(), wait.clone());
            async move {
                let remaining = wait.load(Ordering::Relaxed);
                if remaining > 0 {
                    wait.store(remaining - 1, Ordering::Relaxed);
                    return;
                }
                let result = tokio::task::spawn_blocking(move || attempt(&home, &db, &store)).await;
                if !matches!(result, Ok(Ok(()))) {
                    tracing::warn!(result = ?result, "daily backup will retry");
                    wait.store(RETRY_TICKS, Ordering::Relaxed);
                }
            }
        },
    )
}

/// The background loop (a Hub job): looks every ten minutes whether the day's snapshot is due.
pub async fn run(home: PathBuf, db: PathBuf, store: steve_store::Store, on: bool) {
    task(home, db, store, on).await;
}
