//! Owner permission policy and its change event. Applied mode belongs to the session runtime, not this policy.

use crate::*;
use serde::{Deserialize, Serialize};

pub const REASONS: &[ReasonSet] = &[];

#[derive(
    Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize,
)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(test, derive(ts_rs::TS))]
pub enum ApprovalMode {
    #[default]
    Ask,
    Auto,
    FullAccess,
}

impl ApprovalMode {
    pub const ALL: [Self; 3] = [Self::Ask, Self::Auto, Self::FullAccess];

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Ask => "ask",
            Self::Auto => "auto",
            Self::FullAccess => "full_access",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "ask" => Some(Self::Ask),
            "auto" => Some(Self::Auto),
            "full_access" => Some(Self::FullAccess),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
pub struct ApprovalPolicy {
    pub mode: ApprovalMode,
    pub revision: i64,
    // No actor until the owner has changed the default.
    pub actor: Option<UserId>,
}

/// Spec-derived permission capabilities for a harness discovered across Nodes.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
pub struct PermissionHarnessSupport {
    pub id: String,
    pub name: String,
    pub ready: Option<bool>,
    pub auto: bool,
    pub auto_mode: Option<String>,
    pub full_access: bool,
    pub full_access_mode: Option<String>,
    pub nested_sandbox: bool,
}

/// Workspace permission policy view (`GET /api/permission-policy`, `PUT /api/permission-policy`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
pub struct PermissionPolicyView {
    pub policy: ApprovalPolicy,
    pub editable: bool,
    pub harnesses: Vec<PermissionHarnessSupport>,
    /// Display names of known harnesses whose spec declares `nested_sandbox = false`.
    pub unconfined_harnesses: Vec<String>,
}

/// Effective status of a session's harness under the workspace permission policy.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(test, derive(ts_rs::TS))]
pub enum SessionPermissionStatus {
    Applied,
    Pending,
    Unsupported,
    Refused,
}

/// Per-session permission state (`GET /api/permission-policy/sessions/{id}`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
pub struct SessionPermissionView {
    pub policy: ApprovalMode,
    pub status: SessionPermissionStatus,
    pub target: Option<String>,
    pub native_id: Option<String>,
    pub applied: Option<String>,
    pub desired: Option<String>,
    pub refused: Option<String>,
    pub unknown: bool,
    pub released: bool,
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
