//! Group 8 owns events and reasons for parties and accounts.
//! Associated ID labels (PartyId, AccountId, UserId) are all defined centrally in `id.rs`.

use crate::event::event_enum;
use crate::*;

event_enum! {
PeopleEvent {
    #[serde(rename = "party")]
    Party {  id: PartyId, ws: WorkspaceId  },
    #[serde(rename = "account")]
    Account {  id: AccountId, ws: WorkspaceId  },
}
}
impl Scoped for PeopleEvent {
    fn scope(&self) -> Scope {
        match self {
            Self::Party { ws, .. } => Scope::Ws(ws.clone()),
            Self::Account { ws, .. } => Scope::Ws(ws.clone()),
        }
    }
}
