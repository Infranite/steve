//! Deliveries operation contracts. Backend triggers are provenance, not contract requirements.
use crate::{
    Domain,
    registry::{EntryKind, RegistryEntry},
};

/// Clear delivery intent content when channel message text is cleared; retain routing, intent key and fingerprint. Implementation deferred to wave 4.
pub const PURGE_MESSAGE_INTENT: RegistryEntry = RegistryEntry {
    name: "comms.deliveries.purge_message_intent",
    domain: Domain::Comms,
    kind: EntryKind::Operation,
    parameter_type: "undeclared (wave 4)",
    semantics: "Clear delivery intent content when channel message text is cleared; retain routing, intent key and fingerprint.",
    implemented: false,
    triggers: &["notification_message_purged"],
};
