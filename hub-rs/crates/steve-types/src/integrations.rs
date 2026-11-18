//! Group 5 owns events and reasons for GitHub and mail notifications; follows the plugin contract when integrations move out.
//! Associated ID labels (MailThreadId, NotificationId) are all defined centrally in `id.rs`.

use crate::event::event_enum;
use crate::*;

event_enum! {
IntegrationsEvent {
    #[serde(rename = "gh")]
    Gh {  id: String, ws: WorkspaceId  },
    #[serde(rename = "mail")]
    Mail {  id: MailThreadId, ws: WorkspaceId  },
}
}
impl Scoped for IntegrationsEvent {
    fn scope(&self) -> Scope {
        match self {
            Self::Gh { ws, .. } => Scope::Ws(ws.clone()),
            Self::Mail { ws, .. } => Scope::Ws(ws.clone()),
        }
    }
}
