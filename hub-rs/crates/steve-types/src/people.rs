//! Group 8 owns events and reasons for parties and accounts.
//! Associated ID labels (PartyId, AccountId, UserId) are all defined centrally in `id.rs`.

use crate::*;
use serde::{Deserialize, Serialize};

pub const REASONS: &[ReasonSet] = &[];

/// A person's picture reference: uploaded Hub file (`file`) or external provider URL (`url`).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
pub struct AvatarRef {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub file: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub seed: Option<String>,
}

impl AvatarRef {
    pub fn is_empty(&self) -> bool {
        self.file.as_deref().is_none_or(str::is_empty)
            && self.url.as_deref().is_none_or(str::is_empty)
            && self.seed.as_deref().is_none_or(str::is_empty)
    }
}

/// Unified workspace member identity: a login account linked to a person (contact party).
/// Display name and avatar belong to the person; `login` is strictly for account/access settings.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
pub struct MemberIdentity {
    pub user_id: UserId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub party_id: Option<PartyId>,
    /// Display name from the linked person (`parties.name`), falling back to login if unlinked.
    pub name: String,
    /// Login handle (`users.name`), shown only in account/access settings or for disambiguation.
    pub login: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub email: Option<String>,
    /// Workspace role (`owner` | `member`).
    pub role: String,
    #[serde(default, skip_serializing_if = "AvatarRef::is_empty")]
    #[cfg_attr(test, ts(as = "Option<AvatarRef>", optional))]
    pub avatar: AvatarRef,
    /// Optional role/organization subtitle for disambiguating duplicate names.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub subtitle: Option<String>,
}

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
