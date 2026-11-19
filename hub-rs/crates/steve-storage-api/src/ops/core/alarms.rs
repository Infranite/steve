//! Alarms operation contracts. Backend triggers are provenance, not contract requirements.
use crate::{
    Domain,
    registry::{EntryKind, RegistryEntry},
};

/// Deduplicate workspace/brief dirty subjects after flow, policy, timezone or member changes. Implementation deferred to wave 4.
pub const MARK_DIRTY: RegistryEntry = RegistryEntry {
    name: "core.alarms.mark_dirty",
    domain: Domain::Core,
    kind: EntryKind::Operation,
    parameter_type: "undeclared (wave 4)",
    semantics: "Deduplicate workspace/brief dirty subjects after flow, policy, timezone or member changes.",
    implemented: false,
    triggers: &[
        "brief_alarms_flow_delete",
        "brief_alarms_flow_insert",
        "brief_alarms_flow_update",
        "brief_alarms_member_delete",
        "brief_alarms_policy_insert",
        "brief_alarms_policy_update",
        "brief_alarms_tz_delete",
        "brief_alarms_tz_insert",
        "brief_alarms_tz_update",
    ],
};
