//! Group 7 owns events and reasons for spaces, channel messages, personas, channel configuration, connections and digests.
//! Associated ID labels (SpaceId, ChanMessageId, PersonaId, ConnectionId) are all defined centrally in `id.rs`.

use crate::event::event_enum;
use crate::*;

event_enum! {
ChannelsEvent {
    #[serde(rename = "connection")]
    Connection {  id: ConnectionId, ws: WorkspaceId  },
    #[serde(rename = "space")]
    Space {  id: SpaceId, ws: WorkspaceId  },
    #[serde(rename = "chan_message")]
    ChanMessage {
        id: ChanMessageId,
        space_id: SpaceId,
        ws: WorkspaceId,
     },
    #[serde(rename = "persona")]
    Persona {  id: PersonaId, ws: WorkspaceId  },
    #[serde(rename = "digest")]
    Digest {  id: String, ws: WorkspaceId  },
    #[serde(rename = "channel")]
    Channel {  id: ConnectionId, ws: WorkspaceId  },
}
}
impl Scoped for ChannelsEvent {
    fn scope(&self) -> Scope {
        match self {
            Self::Connection { ws, .. } => Scope::Ws(ws.clone()),
            Self::Space { ws, .. } => Scope::Ws(ws.clone()),
            Self::ChanMessage { ws, .. } => Scope::Ws(ws.clone()),
            Self::Persona { ws, .. } => Scope::Ws(ws.clone()),
            Self::Digest { ws, .. } => Scope::Ws(ws.clone()),
            Self::Channel { ws, .. } => Scope::Ws(ws.clone()),
        }
    }
}
