//! Group 8 owns events and reasons for knowledge sources and documents.
//! Associated ID labels (KbSourceId, KbDocId) are all defined centrally in `id.rs`.

use crate::*;
use serde::Serialize;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, strum::EnumCount)]
#[serde(tag = "type")]
#[cfg_attr(test, derive(ts_rs::TS))]
pub enum KnowledgeEvent {
    #[serde(rename = "kb")]
    Kb { id: String, ws: WorkspaceId },
}

impl Scoped for KnowledgeEvent {
    fn scope(&self) -> Scope {
        match self {
            Self::Kb { ws, .. } => Scope::Ws(ws.clone()),
        }
    }
}
