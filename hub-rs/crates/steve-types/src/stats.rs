//! Group 9 owns events and reasons for workspace statistics.
//! Associated ID labels (WorkspaceId) are all defined centrally in `id.rs`.

use crate::*;
use serde::Serialize;

pub const REASONS: &[ReasonSet] = &[];

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
