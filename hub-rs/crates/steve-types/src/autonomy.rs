//! Group 8 owns events and reasons for yolo and yolo actions.
//! Associated ID labels (YoloActionId) are all defined centrally in `id.rs`.

use crate::event::event_enum;
use crate::*;

event_enum! {
AutonomyEvent {
    #[serde(rename = "yolo")]
    Yolo {  ws: WorkspaceId  },
    #[serde(rename = "yolo_action")]
    YoloAction {
        id: YoloActionId,
        decision_id: Option<DecisionId>,
        ws: WorkspaceId,
     },
}
}
impl Scoped for AutonomyEvent {
    fn scope(&self) -> Scope {
        match self {
            Self::Yolo { ws, .. } => Scope::Ws(ws.clone()),
            Self::YoloAction { ws, .. } => Scope::Ws(ws.clone()),
        }
    }
}
