//! Gateway operation contracts. Backend triggers are provenance, not contract requirements.
use crate::{
    Domain,
    registry::{EntryKind, RegistryEntry},
};

/// Insert a unique ready ticket when dirty; remove when clean; preserve existing ticket FIFO order. Implementation deferred to wave 4.
pub const SYNC_RESULT_READY: RegistryEntry = RegistryEntry {
    name: "core.gateway.sync_result_ready",
    domain: Domain::Core,
    kind: EntryKind::Operation,
    parameter_type: "undeclared (wave 4)",
    semantics: "Insert a unique ready ticket when dirty; remove when clean; preserve existing ticket FIFO order.",
    implemented: false,
    triggers: &[
        "gateway_result_clean",
        "gateway_result_dirty",
        "gateway_result_insert",
    ],
};
