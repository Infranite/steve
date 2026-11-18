//! Group 8 owns events and reasons for flows, flow runs, signals and calendars.
//! Associated ID labels (FlowId, FlowRunId, SignalId, CalendarId) are all defined centrally in `id.rs`.

use crate::*;
use serde::Serialize;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, strum::EnumCount)]
#[serde(tag = "type")]
#[cfg_attr(test, derive(ts_rs::TS))]
pub enum AutomationEvent {
    #[serde(rename = "signal")]
    Signal { id: SignalId, ws: WorkspaceId },
    #[serde(rename = "flow")]
    Flow { id: FlowId, ws: WorkspaceId },
    #[serde(rename = "run")]
    Run {
        id: FlowRunId,
        flow_id: FlowId,
        ws: WorkspaceId,
    },
    #[serde(rename = "calendar")]
    Calendar { id: CalendarId, ws: WorkspaceId },
}

impl Scoped for AutomationEvent {
    fn scope(&self) -> Scope {
        match self {
            Self::Signal { ws, .. } => Scope::Ws(ws.clone()),
            Self::Flow { ws, .. } => Scope::Ws(ws.clone()),
            Self::Run { ws, .. } => Scope::Ws(ws.clone()),
            Self::Calendar { ws, .. } => Scope::Ws(ws.clone()),
        }
    }
}
