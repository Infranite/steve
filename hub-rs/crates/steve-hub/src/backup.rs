//! Hub policy for backup locations. Domain modules receive a capability, never infer a home.
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::Mutex,
};
use steve_store_sqlite::backup::{self as storage, BackupPlan, CancelFlag, Reason};

/// All Hub backup purposes share this configured root. Callers retain their existing
/// deadlines after obtaining the plan; purpose rules choose archives and retention.
pub fn root(home: &Path) -> PathBuf {
    home.join("backups")
}

pub fn plan(home: &Path, reason: Reason) -> BackupPlan {
    let scope = reason.rule().files;
    let mut plan = BackupPlan::new(root(home), reason);
    plan.with_files = crate::admin::client::backup::files(home, scope);
    plan
}

/// The channel cleanup adapter owns installation paths; it opens an independent snapshot,
/// so a backup never takes the Store's writer mutex.
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

/// Installation settings live alongside the existing operator settings in the default workspace.
#[derive(Clone, Copy, Debug, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct DailySettings {
    pub enabled: bool,
    pub keep_days: u64,
}
impl Default for DailySettings {
    fn default() -> Self {
        Self {
            enabled: true,
            keep_days: storage::DEFAULT_DAILY_KEEP_DAYS,
        }
    }
}
impl DailySettings {
    pub fn validate(&self) -> anyhow::Result<()> {
        anyhow::ensure!(
            (1..=storage::MAX_DAILY_KEEP_DAYS).contains(&self.keep_days),
            "keep_days must be between 1 and {}",
            storage::MAX_DAILY_KEEP_DAYS
        );
        Ok(())
    }
}
const DAILY_SETTINGS: &str = "backup.daily";
pub fn daily_settings(store: &steve_store::Store) -> steve_store::Result<DailySettings> {
    store
        .settings()
        .get(steve_store::WS, DAILY_SETTINGS, DailySettings::default())
}
pub fn save_daily_settings(
    store: &steve_store::Store,
    settings: DailySettings,
    author: &str,
) -> anyhow::Result<()> {
    settings.validate()?;
    let mut values = serde_json::Map::new();
    values.insert(DAILY_SETTINGS.into(), serde_json::to_value(settings)?);
    store.settings().set(steve_store::WS, &values, author)?;
    Ok(())
}
// Last sweep is process-local status, not a user setting or a second persisted policy.
static RETENTION_FAILURES: Mutex<BTreeMap<PathBuf, usize>> = Mutex::new(BTreeMap::new());
pub(crate) fn retention_failures(home: &Path) -> usize {
    RETENTION_FAILURES
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get(home)
        .copied()
        .unwrap_or(0)
}
/// Every sweep resolves the current setting, including when daily generation is disabled.
pub fn retain(
    store: &steve_store::Store,
    home: &Path,
    now: u64,
) -> anyhow::Result<storage::Pruned> {
    let result = (|| {
        let settings = daily_settings(store)?;
        Ok(storage::retain_at(&root(home), now, settings.keep_days)?)
    })();
    let failures = result.as_ref().map_or(1, |r: &storage::Pruned| r.failures);
    RETENTION_FAILURES
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .insert(home.to_owned(), failures);
    if failures > 0 {
        tracing::warn!(
            failures,
            "backup retention completed with failures; see backup status"
        );
    }
    result
}
