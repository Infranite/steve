//! Notifications operation contracts. Backend triggers are provenance, not contract requirements.
use crate::{
    Domain,
    registry::{EntryKind, RegistryEntry},
};

/// Clear personal notification copies and mark purged when a delivery is removed or its intent content cleared. Implementation deferred to wave 4.
pub const PURGE_DELIVERY_COPY: RegistryEntry = RegistryEntry {
    name: "comms.notifications.purge_delivery_copy",
    domain: Domain::Comms,
    kind: EntryKind::Operation,
    parameter_type: "undeclared (wave 4)",
    semantics: "Clear personal notification copies and mark purged when a delivery is removed or its intent content cleared.",
    implemented: false,
    triggers: &[
        "notification_delivery_deleted",
        "notification_delivery_purged",
    ],
};

/// Atomically clear starred/forwarded message, delivery and notification body copies when their source is purged or deleted. Implementation deferred to wave 4.
pub const PURGE_SOURCE_COPIES: RegistryEntry = RegistryEntry {
    name: "comms.notifications.purge_source_copies",
    domain: Domain::Comms,
    kind: EntryKind::Operation,
    parameter_type: "undeclared (wave 4)",
    semantics: "Atomically clear starred/forwarded message, delivery and notification body copies when their source is purged or deleted.",
    implemented: false,
    triggers: &["starred_source_deleted", "starred_source_purged"],
};

/// Publish a stable notification feed fact in comms; core consumes it idempotently to upsert feed/unread state. Implementation deferred to wave 4.
pub const PUBLISH_FEED_FACT: RegistryEntry = RegistryEntry {
    name: "comms.notifications.publish_feed_fact",
    domain: Domain::Comms,
    kind: EntryKind::Operation,
    parameter_type: "undeclared (wave 4)",
    semantics: "Publish a stable notification feed fact in comms; core consumes it idempotently to upsert feed/unread state.",
    implemented: false,
    triggers: &["notification_feed_insert"],
};
