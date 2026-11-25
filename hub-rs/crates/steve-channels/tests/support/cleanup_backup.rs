//! A fixture receipt and a deterministic failure, at the cleanup port.
use std::{path::PathBuf, sync::Arc};
use steve_types::backup::SnapshotId;

pub fn port(
    _database: PathBuf,
    _root: PathBuf,
    fail: bool,
) -> Arc<dyn steve_channels::PreDestructiveBackup> {
    Arc::new(FixtureBackup { fail })
}

struct FixtureBackup {
    fail: bool,
}

impl steve_channels::PreDestructiveBackup for FixtureBackup {
    fn backup(&self) -> steve_channels::ChannelResult<SnapshotId> {
        if self.fail {
            return Err(steve_modules::ApiError::Internal(
                "fixture backup failed".into(),
            ));
        }
        Ok("a".repeat(64).try_into().unwrap())
    }
}
