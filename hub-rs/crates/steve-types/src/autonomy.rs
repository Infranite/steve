//! Group 8 owns events and reasons for yolo and yolo actions.
//! Associated ID labels (YoloActionId) are all defined centrally in `id.rs`.

use crate::*;
use serde::Serialize;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, strum::EnumCount)]
#[serde(tag = "type")]
#[cfg_attr(test, derive(ts_rs::TS))]
pub enum AutonomyEvent {
    #[serde(rename = "yolo")]
    Yolo { ws: WorkspaceId },
    #[serde(rename = "yolo_action")]
    YoloAction {
        id: YoloActionId,
        decision_id: Option<DecisionId>,
        ws: WorkspaceId,
    },
}

impl Scoped for AutonomyEvent {
    fn scope(&self) -> Scope {
        match self {
            Self::Yolo { ws, .. } => Scope::Ws(ws.clone()),
            Self::YoloAction { ws, .. } => Scope::Ws(ws.clone()),
        }
    }
}

impl AutonomyEvent {
    /// The public SSE tag for this domain event.
    pub fn wire_type(&self) -> &'static str {
        match self {
            Self::Yolo { .. } => "yolo",
            Self::YoloAction { .. } => "yolo_action",
        }
    }
}
