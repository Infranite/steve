//! Shared storage capability contract. Each flat entry has the same meaning:
//! accept min_read..=target, then migrate to target; never open future schemas.
use crate::{
    layout::{self, Layout},
    migrate,
    open::{db_error, incompatible},
};
use rusqlite::Connection;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use steve_types::Error;

pub const LEGACY_MIN: i64 = 113;
/// Frozen replay head, derived from the embedded legacy migration catalog.
pub const LEGACY_MAX: i64 = crate::legacy::SCHEMA_VERSION;
/// Old-reader rejection signal, not a migration or physical reader generation.
pub const LEGACY_READER_FENCE: i64 = 1000;
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
/// Exact current identity shared by administration, snapshots and backup manifests.
/// A legacy source cannot be represented as a successful current backup.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CurrentStorage {
    pub layout: Layout,
    pub domains: BTreeMap<String, u32>,
    pub contract_schema: u32,
}
impl CurrentStorage {
    /// Read metadata only. Opening/conversion and physical file verification stay with Engine.
    pub fn inspect(conn: &Connection) -> Result<Self, Error> {
        let state = crate::open::inspect(conn)?;
        StorageCapabilities::compiled().can_open(&state, false)?;
        match state {
            StorageState::Current {
                layout,
                domains,
                contract_schema,
            } => Ok(Self {
                layout,
                domains,
                contract_schema,
            }),
            _ => Err(incompatible(
                "current storage required; start the upgraded Hub to initialize or convert it first",
            )),
        }
    }
    pub fn state(&self) -> StorageState {
        StorageState::Current {
            layout: self.layout.clone(),
            domains: self.domains.clone(),
            contract_schema: self.contract_schema,
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SchemaRange {
    pub min_read: u32,
    pub target: u32,
}
impl SchemaRange {
    fn valid(self) -> bool {
        self.min_read > 0 && self.min_read <= self.target
    }
    fn accepts(self, version: u32, exact: bool) -> bool {
        (self.min_read..=self.target).contains(&version) && (!exact || version == self.target)
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StorageCapabilities {
    pub layout: SchemaRange,
    pub contract: SchemaRange,
    pub core: SchemaRange,
    pub stream: SchemaRange,
    pub comms: SchemaRange,
    /// Presence alone declares conversion support. This temporary entry may be
    /// omitted by a candidate after its legacy converter has been removed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub legacy: Option<SchemaRange>,
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
    /// Domain bounds come from the first/last embedded migrations, legacy from
    /// its supported input floor and compiled replay head, and physical layout
    /// from the compiled layout catalog.
    /// No release version or separately hand-maintained JSON numbers are used.
    pub fn compiled() -> Self {
        let range = |domain| {
            let set = migrate::CATALOG.iter().find(|s| s.domain == domain);
            SchemaRange {
                min_read: set
                    .and_then(|s| s.migrations.first())
                    .map_or(0, |m| m.version),
                target: set
                    .and_then(|s| s.migrations.last())
                    .map_or(0, |m| m.version),
            }
        };
        let legacy_head = LEGACY_MAX as u32;
        Self {
            layout: SchemaRange {
                min_read: layout::LAYOUT_GENERATION,
                target: layout::LAYOUT_GENERATION,
            },
            contract: range("contract"),
            core: range("core"),
            stream: range("stream"),
            comms: range("comms"),
            legacy: Some(SchemaRange {
                min_read: LEGACY_MIN as u32,
                target: legacy_head,
            }),
        }
    }
    /// Parse the shared release/build-info declaration. Legacy support is either
    /// absent or a range object; null is not a declaration.
    pub fn from_manifest(value: &serde_json::Value) -> Result<Self, Error> {
        let storage = value
            .get("storage")
            .ok_or_else(|| incompatible("target release does not declare storage capabilities"))?;
        if storage
            .get("legacy")
            .is_some_and(serde_json::Value::is_null)
        {
            return Err(incompatible("null legacy capability"));
        }
        let capabilities: Self = serde_json::from_value(storage.clone())
            .map_err(|_| incompatible("invalid target storage capabilities"))?;
        capabilities.validate()?;
        Ok(capabilities)
    }
    fn validate(&self) -> Result<(), Error> {
        if [
            self.layout,
            self.contract,
            self.core,
            self.stream,
            self.comms,
        ]
        .into_iter()
        .chain(self.legacy)
        .any(|r| !r.valid())
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
                let Some(legacy) = self.legacy else {
                    return Err(incompatible(
                        "candidate does not include a legacy converter",
                    ));
                };
                if u32::try_from(*schema_version).is_ok_and(|v| legacy.accepts(v, false)) {
                    Ok(())
                } else {
                    Err(legacy_refusal(*schema_version, legacy.target.into()))
                }
            }
            StorageState::Current {
                layout,
                domains,
                contract_schema,
            } => {
                layout.validate_shape()?;
                require_domains(domains)?;
                // One predicate for every declared storage item, including
                // physical min_reader. A current store never consults legacy.
                let versions = [
                    (layout.generation, self.layout),
                    (*contract_schema, self.contract),
                    (domains["core"], self.core),
                    (domains["stream"], self.stream),
                    (domains["comms"], self.comms),
                ];
                if versions
                    .into_iter()
                    .any(|(v, range)| !range.accepts(v, exact))
                    || !self.layout.accepts(layout.min_reader, false)
                {
                    return Err(incompatible(
                        "storage schema differs from this program; start or upgrade the Hub first",
                    ));
                }
                Ok(())
            }
        }
    }
    /// Storage object only. Legacy top-level schema wrappers retain their three
    /// fields in the release/build-info caller; new gates never consult them.
    /// The name stays stable for callers wiring this payload into build-info.
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
