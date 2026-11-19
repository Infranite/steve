//! Bounded retention policy data. Scheduling and filesystem effects belong to the caller.
use crate::{entity::Cell, query::Order, uow::AnyOp};
use serde::{Deserialize, Serialize};

/// Slice inspection ceiling and soft target time; overruns are reported, not hidden.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Budget {
    pub rows: u32,
    pub target_ms: u32,
}
impl Default for Budget {
    fn default() -> Self {
        Self {
            rows: 5_000,
            target_ms: 20,
        }
    }
}
/// Candidate selection, evaluated before protection and actions. Durations are milliseconds.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Select {
    MaxAge {
        field: String,
        keep_ms: u64,
    },
    Terminal {
        state: String,
        done: Vec<Cell>,
        since: String,
        keep_ms: u64,
    },
    MaxRows {
        group: Vec<String>,
        keep: u64,
        order: Vec<(String, Order)>,
    },
    Bytes {
        group: Vec<String>,
        max: u64,
    },
}
/// Retention interval source; settings are resolved per workspace by the backend/caller.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum KeepFrom {
    Fixed {
        millis: u64,
    },
    Setting {
        key: String,
        per: SettingScope,
        default_ms: u64,
    },
}
/// Scope of an operator retention setting.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SettingScope {
    Workspace,
}
/// Selected-row action. Purge retains identity/routing while clearing the listed fields.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Action {
    Delete,
    Purge { fields: Vec<String> },
    Op(AnyOp),
}
/// Stable domain operation name used for protection and pre-action hooks.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct OpName(pub String);
/// Owned policy. before runs outside the write lock; only successful/already-absent
/// external resources proceed. Failed rows remain for the next run. protect excludes live rows.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RetentionPolicy {
    pub entity: String,
    pub select: Select,
    pub keep_from: KeepFrom,
    pub before: Option<OpName>,
    pub action: Action,
    pub protect: Option<OpName>,
    pub budget: Budget,
}
/// Observable slice progress. more requires the scheduler to arrange another bounded call.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RetentionReport {
    pub entity: String,
    pub affected: u64,
    pub slices: u32,
    pub max_slice_ms: u32,
    pub overruns: u32,
    pub more: bool,
}
