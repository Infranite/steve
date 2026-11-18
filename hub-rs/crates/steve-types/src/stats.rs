//! Group 9 owns events and reasons for workspace statistics.
//! Associated ID labels (WorkspaceId) are all defined centrally in `id.rs`.

use crate::event::event_enum;
use crate::*;

event_enum! {
StatsEvent {
    #[serde(rename = "stats")]
    Stats {
        ws: WorkspaceId,
        #[serde(skip_serializing_if = "Option::is_none")]
        day: Option<String>,
     },
}
}
impl Scoped for StatsEvent {
    fn scope(&self) -> Scope {
        match self {
            Self::Stats { ws, .. } => Scope::Ws(ws.clone()),
        }
    }
}
