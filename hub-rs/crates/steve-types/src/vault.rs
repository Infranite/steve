//! Group 8 owns events and reasons for vault items.
//! Associated ID labels (VaultItemId) are all defined centrally in `id.rs`.

use crate::event::event_enum;
use crate::*;

event_enum! {
VaultEvent {
    #[serde(rename = "vault")]
    Vault {  id: VaultItemId, ws: WorkspaceId  },
}
}
impl Scoped for VaultEvent {
    fn scope(&self) -> Scope {
        match self {
            Self::Vault { ws, .. } => Scope::Ws(ws.clone()),
        }
    }
}
