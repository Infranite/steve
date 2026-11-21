//! Group 8 owns events and reasons for parties and accounts.
//! Associated ID labels (PartyId, AccountId, UserId) are all defined centrally in `id.rs`.

use crate::*;
use serde::Serialize;

pub const REASONS: &[ReasonSet] = &[];

#[derive(Clone, Debug, PartialEq, Eq, Serialize, strum::EnumCount)]
#[serde(tag = "type")]
#[cfg_attr(test, derive(ts_rs::TS))]
pub enum PeopleEvent {
    #[serde(rename = "party")]
    Party { id: PartyId, ws: WorkspaceId },
    #[serde(rename = "account")]
    Account { id: AccountId, ws: WorkspaceId },
}

impl Scoped for PeopleEvent {
    fn scope(&self) -> Scope {
        match self {
            Self::Party { ws, .. } => Scope::Ws(ws.clone()),
            Self::Account { ws, .. } => Scope::Ws(ws.clone()),
        }
    }
}

impl PeopleEvent {
    /// The public SSE tag for this domain event.
    pub fn wire_type(&self) -> &'static str {
        match self {
            Self::Party { .. } => "party",
            Self::Account { .. } => "account",
        }
    }
}

impl PeopleEvent {
    pub fn entity_id(&self) -> Option<&str> {
        match self {
            Self::Party { id, .. } => Some(id.as_str()),
            Self::Account { id, .. } => Some(id.as_str()),
        }
    }
}
