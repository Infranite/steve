//! Transcript operation contracts. Backend triggers are provenance, not contract requirements.
use crate::{
    Domain,
    registry::{EntryKind, RegistryEntry},
};

/// Collect a session/hash payload after its last event reference is removed in the same unit. Implementation deferred to wave 4.
pub const COLLECT_UNREFERENCED_PAYLOADS: RegistryEntry = RegistryEntry {
    name: "stream.transcript.collect_unreferenced_payloads",
    domain: Domain::Stream,
    kind: EntryKind::Operation,
    parameter_type: "undeclared (wave 4)",
    semantics: "Collect a session/hash payload after its last event reference is removed in the same unit.",
    implemented: false,
    triggers: &["event_payload_refs_gc"],
};
