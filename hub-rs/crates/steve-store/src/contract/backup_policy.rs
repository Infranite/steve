//! The owner-configured daily policy. Snapshot data and observations are not stored in SQLite.
use sea_query::{OnConflict, Query};
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::{Read, Result, StoreError, W, WS};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DailySettings {
    pub enabled: bool,
    pub keep_days: u64,
}

impl Default for DailySettings {
    fn default() -> Self {
        Self {
            enabled: true,
            keep_days: 7,
        }
    }
}

impl DailySettings {
    pub const MAX_KEEP_DAYS: u64 = 365;

    pub fn validate(self) -> Result<()> {
        if (1..=Self::MAX_KEEP_DAYS).contains(&self.keep_days) {
            Ok(())
        } else {
            Err(StoreError::Rejected(format!(
                "keep_days must be between 1 and {}",
                Self::MAX_KEEP_DAYS
            )))
        }
    }
}

pub fn daily(read: &mut impl Read) -> Result<DailySettings> {
    Ok(read
        .one(
            Query::select()
                .columns(["enabled", "keep_days"])
                .from("backup_policy"),
            |row| {
                Ok(DailySettings {
                    enabled: row.get::<i64>(0)? != 0,
                    keep_days: row.get::<i64>(1)? as u64,
                })
            },
        )?
        .unwrap_or_default())
}

pub fn set_daily(write: &mut W<'_>, settings: DailySettings, author: &str) -> Result<()> {
    settings.validate()?;
    write.exec(
        Query::insert()
            .into_table("backup_policy")
            .columns(["singleton", "enabled", "keep_days"])
            .values_panic([
                1_i64.into(),
                i64::from(settings.enabled).into(),
                (settings.keep_days as i64).into(),
            ])
            .on_conflict(
                OnConflict::column("singleton")
                    .update_columns(["enabled", "keep_days"])
                    .to_owned(),
            ),
    )?;
    write.exec(&super::audit::revision_statement(
        WS,
        "settings",
        WS,
        0,
        &json!({"backup.daily": settings}),
        author,
    )?)?;
    write.emit(steve_types::ConfigEvent::Settings {
        ws: WS.into(),
        id: WS.into(),
    });
    Ok(())
}
