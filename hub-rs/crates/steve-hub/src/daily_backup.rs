//! One snapshot per local day at or after 03:30, with a retry after unsuccessful attempts.
use crate::backup::{self, Purpose};
use anyhow::Result;
use jiff::{Timestamp, tz::TimeZone};
use std::{
    path::{Path, PathBuf},
    time::Duration,
};

const FIRST: Duration = Duration::from_secs(300);
const TICK: Duration = Duration::from_secs(600);
const RETRY: Duration = Duration::from_secs(3600);

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

pub async fn run(home: PathBuf, db: PathBuf, store: steve_store::Store, on: bool) {
    if !on {
        return;
    }
    tokio::time::sleep(FIRST).await;
    loop {
        let (home, db, store) = (home.clone(), db.clone(), store.clone());
        let result = tokio::task::spawn_blocking(move || attempt(&home, &db, &store)).await;
        let delay = match result {
            Ok(Ok(())) => TICK,
            other => {
                tracing::warn!(result = ?other, "daily backup will retry");
                RETRY
            }
        };
        tokio::time::sleep(delay).await;
    }
}
