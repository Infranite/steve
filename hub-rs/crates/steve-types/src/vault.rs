//! Group 8 owns events and reasons for vault items.
//! Associated ID labels (VaultItemId) are all defined centrally in `id.rs`.

use crate::*;
use serde::Serialize;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, strum::EnumCount)]
#[serde(tag = "type")]
#[cfg_attr(test, derive(ts_rs::TS))]
pub enum VaultEvent {
    #[serde(rename = "vault")]
    Vault { id: VaultItemId, ws: WorkspaceId },
}

impl Scoped for VaultEvent {
    fn scope(&self) -> Scope {
        match self {
            Self::Vault { ws, .. } => Scope::Ws(ws.clone()),
        }
    }
}

impl VaultEvent {
    /// The public SSE tag for this domain event.
    pub fn wire_type(&self) -> &'static str {
        match self {
            Self::Vault { .. } => "vault",
        }
    }
}

impl VaultEvent {
    pub fn entity_id(&self) -> Option<&str> {
        match self {
            Self::Vault { id, .. } => Some(id.as_str()),
        }
    }
}
