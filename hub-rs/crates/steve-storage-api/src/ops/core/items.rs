//! Items operation contracts. Backend triggers are provenance, not contract requirements.
use crate::{
    Domain,
    registry::{EntryKind, RegistryEntry},
};

/// Reject the entire unit before completion when dependencies or waiting gates remain unsatisfied. Implementation deferred to wave 4.
pub const VALIDATE_COMPLETION_DEPENDENCIES: RegistryEntry = RegistryEntry {
    name: "core.items.validate_completion_dependencies",
    domain: Domain::Core,
    kind: EntryKind::Operation,
    parameter_type: "undeclared (wave 4)",
    semantics: "Reject the entire unit before completion when dependencies or waiting gates remain unsatisfied.",
    implemented: false,
    triggers: &["item_dependencies_completion_gate"],
};
