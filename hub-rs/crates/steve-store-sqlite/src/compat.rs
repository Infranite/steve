//! Storage compatibility semantics shared by opens and upgrade callers. JSON encoding
//! is a draft build-info payload; release manifest field names await protocol review.
use crate::{
    layout::{self, Layout},
    migrate,
    open::{db_error, incompatible},
};
use rusqlite::Connection;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use steve_types::Error;

/// Only the immediately preceding published legacy schema is a conversion input.
pub const LEGACY_INPUT: i64 = 113;
/// Defensive marker for old binaries/gates, in their own numbering namespace.
pub const LEGACY_READER_FENCE: i64 = LEGACY_INPUT + 1;
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum StorageState {
    Empty,
    Legacy {
        schema_version: i64,
    },
    Current {
        layout: Layout,
        domains: BTreeMap<String, u32>,
        contract_schema: u32,
    },
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SchemaRange {
    pub min_read: u32,
    pub max_read: u32,
    pub target: u32,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StorageCapabilities {
    pub writable_layout: u32,
    pub physical_reader: u32,
    pub domains: BTreeMap<String, SchemaRange>,
    pub contract_schema: SchemaRange,
    pub legacy_input: i64,
}
impl StorageCapabilities {
    /// Targets come from the embedded catalog, never a release version or manifest.
    pub fn compiled() -> Self {
        let range = |set: &migrate::MigrationSet| {
            let target = set.migrations.last().map_or(0, |m| m.version);
            SchemaRange {
                min_read: 0,
                max_read: target,
                target,
            }
        };
        Self {
            writable_layout: layout::LAYOUT_GENERATION,
            physical_reader: layout::LAYOUT_GENERATION,
            domains: migrate::CATALOG
                .iter()
                .filter(|s| s.domain != "contract")
                .map(|s| (s.domain.into(), range(s)))
                .collect(),
            contract_schema: range(&migrate::CATALOG[0]),
            legacy_input: LEGACY_INPUT,
        }
    }
    /// exact is administrative mode: no initialization, conversion or migration.
    pub fn can_open(&self, state: &StorageState, exact: bool) -> Result<(), Error> {
        let valid_range = |r: &SchemaRange| r.min_read <= r.target && r.target <= r.max_read;
        if self.writable_layout == 0
            || self.physical_reader < self.writable_layout
            || self.legacy_input != LEGACY_INPUT
            || self.domains.len() != 3
            || layout::DOMAINS
                .iter()
                .any(|d| !self.domains.contains_key(layout::domain_name(*d)))
            || self.domains.values().any(|r| !valid_range(r))
            || !valid_range(&self.contract_schema)
        {
            return Err(incompatible("invalid candidate storage capabilities"));
        }
        match state {
            StorageState::Empty if !exact => Ok(()),
            StorageState::Legacy { schema_version }
                if !exact && *schema_version == self.legacy_input =>
            {
                Ok(())
            }
            StorageState::Empty | StorageState::Legacy { .. } if exact => Err(incompatible(
                "storage requires initialization or conversion; start or upgrade the Hub first",
            )),
            StorageState::Legacy { .. } => Err(incompatible(
                "legacy conversion accepts only schema 113; first upgrade to the previous published release (先升级到上一个发布版)",
            )),
            StorageState::Current {
                layout,
                domains,
                contract_schema,
            } => {
                let accepts = |v: u32, r: &SchemaRange| {
                    v >= r.min_read && v <= r.max_read && (!exact || v == r.target)
                };
                layout.validate_shape()?;
                if layout.generation != self.writable_layout
                    || layout.min_reader > self.physical_reader
                    || domains.len() != self.domains.len()
                    || self
                        .domains
                        .iter()
                        .any(|(d, r)| domains.get(d).is_none_or(|v| !accepts(*v, r)))
                    || !accepts(*contract_schema, &self.contract_schema)
                {
                    return Err(incompatible(
                        "storage schema differs from this program; start or upgrade the Hub first",
                    ));
                }
                Ok(())
            }
            StorageState::Empty => unreachable!(),
        }
    }
    /// Draft only: consumers must not freeze manifest keys until protocol approval.
    pub fn encode_build_info_draft(&self) -> Result<serde_json::Value, Error> {
        serde_json::to_value(self).map_err(|_| incompatible("cannot encode storage capabilities"))
    }
}
/// Install the old-reader fence in the caller's metadata transaction. Keep any
/// schema_version until the real previous-release verification settles its removal.
pub(crate) fn stamp_reader_fence(conn: &Connection) -> Result<(), Error> {
    conn.execute_batch("CREATE TABLE IF NOT EXISTS schema_compat(min_reader INTEGER NOT NULL); DELETE FROM schema_compat;").map_err(db_error)?;
    conn.execute(
        "INSERT INTO schema_compat(min_reader) VALUES(?1)",
        [LEGACY_READER_FENCE],
    )
    .map_err(db_error)?;
    Ok(())
}
