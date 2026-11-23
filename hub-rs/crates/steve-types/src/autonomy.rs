//! Owner policy and its change event. Applied mode belongs to the session runtime, not this policy.

use crate::*;
use serde::{Deserialize, Serialize};

pub const REASONS: &[ReasonSet] = &[];

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(test, derive(ts_rs::TS))]
pub enum ApprovalMode {
    #[default]
    AskOwner,
    HarnessAuto,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
pub struct ApprovalPolicy {
    pub mode: ApprovalMode,
    pub revision: i64,
    // No actor until the owner has changed the default.
    pub actor: Option<UserId>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, strum::EnumCount)]
#[serde(tag = "type")]
#[cfg_attr(test, derive(ts_rs::TS))]
pub enum AutonomyEvent {
    #[serde(rename = "approval_policy")]
    ApprovalPolicy {
        ws: WorkspaceId,
        policy: ApprovalPolicy,
    },
}

impl Scoped for AutonomyEvent {
    fn scope(&self) -> Scope {
        match self {
            Self::ApprovalPolicy { ws, .. } => Scope::Ws(ws.clone()),
        }
    }
}

impl AutonomyEvent {
    pub fn wire_type(&self) -> &'static str {
        "approval_policy"
    }

    pub fn entity_id(&self) -> Option<&str> {
        None
    }
}
