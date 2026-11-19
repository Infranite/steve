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
    fn backup(&self, what: &str) -> steve_modules::ApiResult<PathBuf> {
        if self.fail {
            return Err(steve_modules::ApiError::Internal(
                "fixture backup failed".into(),
            ));
        }
        let plan = steve_store_sqlite::backup::BackupPlan::new(
            self.root.clone(),
            steve_store_sqlite::backup::Reason::PreDestructive { what: what.into() },
        );
        let saved = steve_store_sqlite::backup::backup(
            &self.database,
            &plan,
            &|_| {},
            &steve_store_sqlite::backup::CancelFlag::default(),
        )
        .map_err(|e| steve_modules::ApiError::Internal(e.to_string()))?;
        Ok(saved
            .database_path()
            .strip_prefix(&self.root)
            .unwrap()
            .to_owned())
    }
}
