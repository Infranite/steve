//! Group 9 owns events and reasons for workspace statistics.
//! Associated ID labels (WorkspaceId) are all defined centrally in `id.rs`.

use crate::*;
use serde::{Deserialize, Serialize};

pub const REASONS: &[ReasonSet] = &[];

/// Time window for durable menu navigation usage (`GET /api/stats/nav?range=...`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
pub enum NavUsageRange {
    #[serde(rename = "7d")]
    Days7,
    #[serde(rename = "30d")]
    Days30,
    #[serde(rename = "all")]
    All,
}

/// Navigation open count for one bounded menu destination (`GET /api/stats/nav`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
pub struct NavUsageItem {
    pub item: String,
    pub opens: u64,
}

/// Durable menu navigation usage aggregated by workspace local day (`GET /api/stats/nav`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
pub struct NavUsage {
    pub range: NavUsageRange,
    pub from: Option<String>,
    pub to: String,
    pub total: u64,
    pub items: Vec<NavUsageItem>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, strum::EnumCount)]
#[serde(tag = "type")]
#[cfg_attr(test, derive(ts_rs::TS))]
pub enum StatsEvent {
    #[serde(rename = "stats")]
    Stats {
        ws: WorkspaceId,
        #[serde(skip_serializing_if = "Option::is_none")]
        day: Option<String>,
    },
}

impl Scoped for StatsEvent {
    fn scope(&self) -> Scope {
        match self {
            Self::Stats { ws, .. } => Scope::Ws(ws.clone()),
        }
    }
}

impl StatsEvent {
    /// The public SSE tag for this domain event.
    pub fn wire_type(&self) -> &'static str {
        match self {
            Self::Stats { .. } => "stats",
        }
    }
}

impl StatsEvent {
    pub fn entity_id(&self) -> Option<&str> {
        match self {
            Self::Stats { .. } => None,
        }
    }
}
