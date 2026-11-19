//! Hub policy for backup locations. Domain modules receive a capability, never infer a home.
use std::path::{Path, PathBuf};
use steve_store_sqlite::backup::{self as storage, BackupPlan, CancelFlag, Reason};

/// All Hub backup purposes share this configured root. Callers retain their existing
/// deadlines, file-archive and retention settings after obtaining the plan.
pub fn plan(home: &Path, reason: Reason) -> BackupPlan {
    BackupPlan::new(home.join("backups"), reason)
}

/// The channel cleanup adapter owns installation paths; it opens an independent snapshot,
/// so a backup never takes the legacy Store writer mutex.
pub struct ChannelBackup {
    home: PathBuf,
    database: PathBuf,
}
impl ChannelBackup {
    pub fn new(home: PathBuf, database: PathBuf) -> Self {
        Self { home, database }
    }
}
impl steve_channels::PreDestructiveBackup for ChannelBackup {
    fn backup(&self, what: &str) -> steve_channels::ChannelResult<PathBuf> {
        let result = (|| -> anyhow::Result<PathBuf> {
            let plan = plan(&self.home, Reason::PreDestructive { what: what.into() });
            let manifest = storage::backup(&self.database, &plan, &|_| {}, &CancelFlag::default())?;
            // Preserve the cleanup response's existing path relative to the backup root.
            Ok(manifest
                .database_path()
                .strip_prefix(&plan.dest_root)?
                .to_owned())
        })();
        result.map_err(|e| steve_modules::ApiError::Internal(format!("备份失败：{e}")))
    }
}
