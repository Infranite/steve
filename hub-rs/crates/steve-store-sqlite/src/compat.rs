//! Shared compatibility semantics. The storage JSON follows the 6v proposal;
//! it remains a draft until release-protocol review, not a finalized manifest.
use crate::{
    layout::{self, Layout},
    migrate,
    open::{db_error, incompatible},
};
use rusqlite::Connection;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use steve_types::Error;

pub const LEGACY_INPUT: i64 = 113;
/// Old-reader rejection signal, not a migration or physical reader generation.
pub const LEGACY_READER_FENCE: i64 = 1000;
pub const STORAGE_FORMAT_VERSION: u32 = 1;
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
impl SchemaRange {
    fn valid(self) -> bool {
        self.min_read <= self.target && self.target <= self.max_read
    }
    fn accepts(self, version: u32, exact: bool) -> bool {
        (self.min_read..=self.max_read).contains(&version) && (!exact || version == self.target)
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LegacyCapabilities {
    pub min_read: i64,
    pub max_read: i64,
    pub convert: bool,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StorageCapabilities {
    pub format_version: u32,
    pub layout: SchemaRange,
    pub contract_schema: u32,
    pub domains: BTreeMap<String, SchemaRange>,
    pub legacy: LegacyCapabilities,
}
/// Structural domain completeness is shared by observation and candidate validation.
pub(crate) fn require_domains<T>(domains: &BTreeMap<String, T>) -> Result<(), Error> {
    if domains.len() != layout::DOMAINS.len()
        || layout::DOMAINS
            .iter()
            .any(|d| !domains.contains_key(layout::domain_name(*d)))
    {
        return Err(incompatible("incomplete or unknown domain schema"));
    }
    Ok(())
}
pub(crate) fn legacy_refusal(version: i64, maximum: i64) -> Error {
    if version > maximum {
        incompatible("legacy storage requires a newer program (需要更新的程序)")
    } else {
        incompatible(
            "legacy conversion input is unsupported; first upgrade to the previous published release (先升级到上一个发布版)",
        )
    }
}
impl StorageCapabilities {
    /// All schema targets come from the embedded catalog, selected by domain name.
    pub fn compiled() -> Self {
        let head = |set: &migrate::MigrationSet| set.migrations.last().map_or(0, |m| m.version);
        Self {
            format_version: STORAGE_FORMAT_VERSION,
            layout: SchemaRange {
                min_read: layout::LAYOUT_GENERATION,
                max_read: layout::LAYOUT_GENERATION,
                target: layout::LAYOUT_GENERATION,
            },
            contract_schema: migrate::CATALOG
                .iter()
                .find(|s| s.domain == "contract")
                .map_or(0, head),
            domains: migrate::CATALOG
                .iter()
                .filter(|s| s.domain != "contract")
                .map(|s| {
                    let target = head(s);
                    (
                        s.domain.into(),
                        SchemaRange {
                            min_read: 0,
                            max_read: target,
                            target,
                        },
                    )
                })
                .collect(),
            legacy: LegacyCapabilities {
                min_read: LEGACY_INPUT,
                max_read: LEGACY_INPUT,
                convert: true,
            },
        }
    }
    fn validate(&self) -> Result<(), Error> {
        require_domains(&self.domains)?;
        if self.format_version != STORAGE_FORMAT_VERSION
            || !self.layout.valid()
            || self.layout.min_read == 0
            || self.contract_schema == 0
            || self.domains.values().any(|r| !r.valid())
            || self.legacy.min_read < 0
            || self.legacy.min_read > self.legacy.max_read
        {
            return Err(incompatible("invalid candidate storage capabilities"));
        }
        Ok(())
    }
    /// Evaluate this candidate's declaration, not the inspecting binary's legacy
    /// input or layout target. exact is admin mode (no initialization/conversion).
    pub fn can_open(&self, state: &StorageState, exact: bool) -> Result<(), Error> {
        self.validate()?;
        match state {
            StorageState::Empty => {
                if exact {
                    Err(needs_hub())
                } else {
                    Ok(())
                }
            }
            StorageState::Legacy { schema_version } => {
                if exact {
                    return Err(needs_hub());
                }
                if self.legacy.convert
                    && (self.legacy.min_read..=self.legacy.max_read).contains(schema_version)
                {
                    Ok(())
                } else {
                    Err(legacy_refusal(*schema_version, self.legacy.max_read))
                }
            }
            StorageState::Current {
                layout,
                domains,
                contract_schema,
            } => {
                layout.validate_shape()?;
                require_domains(domains)?;
                if !self.layout.accepts(layout.generation, exact)
                    || layout.min_reader > self.layout.max_read
                    || *contract_schema != self.contract_schema
                    || self
                        .domains
                        .iter()
                        .any(|(d, r)| !r.accepts(domains[d], exact))
                {
                    return Err(incompatible(
                        "storage schema differs from this program; start or upgrade the Hub first",
                    ));
                }
                Ok(())
            }
        }
    }
    /// Draft storage object only. Legacy top-level schema wrappers belong to the
    /// release/build-info caller, and must not be used by new compatibility gates.
    pub fn encode_build_info_draft(&self) -> Result<serde_json::Value, Error> {
        self.validate()?;
        serde_json::to_value(self).map_err(|_| incompatible("cannot encode storage capabilities"))
    }
}
fn needs_hub() -> Error {
    incompatible("storage requires initialization or conversion; start or upgrade the Hub first")
}
/// Inside the metadata transaction, make fresh and converted current shapes equal.
pub(crate) fn stamp_reader_fence(conn: &Connection) -> Result<(), Error> {
    conn.execute_batch("DROP TABLE IF EXISTS schema_version; DROP TABLE IF EXISTS schema_compat; CREATE TABLE schema_compat(min_reader INTEGER NOT NULL);").map_err(db_error)?;
    conn.execute(
        "INSERT INTO schema_compat(min_reader) VALUES(?1)",
        [LEGACY_READER_FENCE],
    )
    .map_err(db_error)?;
    Ok(())
}
