//! Recorded Bot selection. Execution identity is reported independently by the session.
use serde::{Deserialize, Serialize};

use crate::{BotId, ItemId, Millis, NodeId};

/// Decision `subject_type` for workspace-scoped "no eligible executor Bot" cards.
pub const NO_EXECUTOR: &str = "no_executor";

macro_rules! placement_reasons {
    ($($variant:ident => $code:literal),+ $(,)?) => {
        #[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
        #[cfg_attr(test, derive(ts_rs::TS))]
        pub enum PlacementRejection {
            $(#[serde(rename = $code)] $variant),+
        }

        impl PlacementRejection {
            pub const fn as_str(self) -> &'static str {
                match self { $(Self::$variant => $code),+ }
            }
        }
    };
}

placement_reasons! {
    NoCandidates => "no_candidates",
    OutsideTeam => "outside_team",
    Bot => "bot",
    Specialty => "specialty",
    NoBindings => "no_bindings",
    Unauthorized => "unauthorized",
    Project => "project",
    Affinity => "affinity",
    Tag => "tag",
    Offline => "offline",
    Harness => "harness",
    SignIn => "signin",
    SessionProbe => "session_probe",
    Model => "model",
    Effort => "effort",
    Permission => "permission",
    Pending => "pending",
    Local => "local",
    BindFailed => "bind_failed",
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(test, derive(ts_rs::TS))]
pub enum PlacementReason {
    Explicit,
    Specialty,
    Description,
    ProjectPreference,
    StableOrder,
    CoordinatorDeferred,
    ExistingAssignment,
    HandoffQuotaExhausted,
    HandoffProviderError,
    HandoffOwnerRequest,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(test, derive(ts_rs::TS))]
pub enum CoordinatorDeferSource {
    Card,
    Setting,
}

impl CoordinatorDeferSource {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Card => "card",
            Self::Setting => "setting",
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(test, derive(ts_rs::TS))]
pub enum NoExecutorPolicy {
    #[default]
    Ask,
    Steve,
}

impl NoExecutorPolicy {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Ask => "ask",
            Self::Steve => "steve",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "ask" => Some(Self::Ask),
            "steve" => Some(Self::Steve),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
pub struct NoExecutorItem {
    pub id: ItemId,
    pub title: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
pub struct PlacementBlocker {
    pub bot: Option<BotId>,
    pub reason: PlacementRejection,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
pub struct PlacementBlocked {
    pub reason: PlacementRejection,
    pub rejected: Vec<PlacementBlocker>,
}

impl std::fmt::Display for PlacementBlocked {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.reason.as_str())
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
pub struct PlacementExplanation {
    pub bot: BotId,
    pub reason: PlacementReason,
    pub matched: Vec<String>,
    pub rejected: Vec<PlacementBlocker>,
    pub fallback: Option<PlacementRejection>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub deferred_by: Option<CoordinatorDeferSource>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BotPlacement {
    pub rule_version: u32,
    pub bot_revision: i64,
    pub at: Millis,
    pub node: NodeId,
    pub required_node: Option<NodeId>,
    pub node_priority: u32,
    pub harness: String,
    pub model: Option<String>,
    #[serde(default)]
    pub reasoning_effort: crate::ReasoningEffort,
    pub explanation: PlacementExplanation,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
pub struct BotProposal {
    pub id: String,
    pub revision: u32,
    pub name: String,
    pub role: String,
    pub specialties: Vec<String>,
    pub harness: String,
    pub model: Option<String>,
    pub node: NodeId,
    pub team: Option<ProposalTeam>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
pub struct ProposalTeam {
    pub id: String,
    pub name: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConfirmBotProposal {
    pub expected_revision: u32,
    pub name: String,
    pub role: String,
    pub specialties: Vec<String>,
    pub harness: String,
    pub model: Option<String>,
}
