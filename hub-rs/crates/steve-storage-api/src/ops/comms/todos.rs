//! Todos operation contracts. Backend triggers are provenance, not contract requirements.
use crate::{
    Domain,
    registry::{EntryKind, RegistryEntry},
};

/// Commit the todo change and alarm-dirty fact together; retain old-subject recomputation on deletion. Implementation deferred to wave 4.
pub const CHANGE_WITH_ALARM_FACT: RegistryEntry = RegistryEntry {
    name: "comms.todos.change_with_alarm_fact",
    domain: Domain::Comms,
    kind: EntryKind::Operation,
    parameter_type: "undeclared (wave 4)",
    semantics: "Commit the todo change and alarm-dirty fact together; retain old-subject recomputation on deletion.",
    implemented: false,
    triggers: &[
        "todo_alarms_delete",
        "todo_alarms_insert",
        "todo_alarms_update",
    ],
};
