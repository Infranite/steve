//! An injected fixture adapter with an explicit root, including a deterministic failure mode.
use std::{path::PathBuf, sync::Arc};

pub fn port(
    database: PathBuf,
    root: PathBuf,
    fail: bool,
) -> Arc<dyn steve_channels::PreDestructiveBackup> {
    Arc::new(FixtureBackup {
        database,
        root,
        fail,
    })
}
struct FixtureBackup {
    root: PathBuf,
    database: PathBuf,
    fail: bool,
}
impl steve_channels::PreDestructiveBackup for FixtureBackup {
    fn backup(&self, what: &str) -> steve_channels::ChannelResult<PathBuf> {
        if self.fail {
            return Err(steve_modules::ApiError::Internal(
                "fixture backup failed".into(),
            ));
        }
        Ok(steve_store::testkit::channels::destructive_backup(
            &self.database,
            &self.root,
            what,
        )?)
    }
}
