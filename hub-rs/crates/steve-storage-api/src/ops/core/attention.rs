//! Attention operation contracts. Backend triggers are provenance, not contract requirements.
use crate::{
    Domain,
    registry::{EntryKind, RegistryEntry},
};

/// Atomically adjust unread scope count and revision when an unread index row is inserted or deleted. Implementation deferred to wave 4.
pub const ADJUST_UNREAD_COUNT: RegistryEntry = RegistryEntry {
    name: "core.attention.adjust_unread_count",
    domain: Domain::Core,
    kind: EntryKind::Operation,
    parameter_type: "undeclared (wave 4)",
    semantics: "Atomically adjust unread scope count and revision when an unread index row is inserted or deleted.",
    implemented: false,
    triggers: &["attention_feed_count_add", "attention_feed_count_remove"],
};

/// Remove old unread entries then rebuild the new audience/member/read projection atomically. Implementation deferred to wave 4.
pub const SYNC_FEED_ENTRY: RegistryEntry = RegistryEntry {
    name: "core.attention.sync_feed_entry",
    domain: Domain::Core,
    kind: EntryKind::Operation,
    parameter_type: "undeclared (wave 4)",
    semantics: "Remove old unread entries then rebuild the new audience/member/read projection atomically.",
    implemented: false,
    triggers: &[
        "attention_feed_entry_add",
        "attention_feed_entry_before_update",
        "attention_feed_entry_update",
    ],
};

/// Initialize member scope and unread projection from audience and read state. Implementation deferred to wave 4.
pub const SYNC_MEMBER_UNREAD: RegistryEntry = RegistryEntry {
    name: "core.attention.sync_member_unread",
    domain: Domain::Core,
    kind: EntryKind::Operation,
    parameter_type: "undeclared (wave 4)",
    semantics: "Initialize member scope and unread projection from audience and read state.",
    implemented: false,
    triggers: &["attention_feed_member_add"],
};

/// Maintain both old and new unread projections when a read is added, removed or moved. Implementation deferred to wave 4.
pub const SYNC_FEED_READ: RegistryEntry = RegistryEntry {
    name: "core.attention.sync_feed_read",
    domain: Domain::Core,
    kind: EntryKind::Operation,
    parameter_type: "undeclared (wave 4)",
    semantics: "Maintain both old and new unread projections when a read is added, removed or moved.",
    implemented: false,
    triggers: &[
        "attention_feed_read_add",
        "attention_feed_read_remove",
        "attention_feed_read_update",
    ],
};

/// Record only terminal runs with a valid named reply and original user message; deduplicate by message id. Implementation deferred to wave 4.
pub const RECORD_FINAL_REPLY: RegistryEntry = RegistryEntry {
    name: "core.attention.record_final_reply",
    domain: Domain::Core,
    kind: EntryKind::Operation,
    parameter_type: "undeclared (wave 4)",
    semantics: "Record only terminal runs with a valid named reply and original user message; deduplicate by message id.",
    implemented: false,
    triggers: &["attention_final_reply"],
};
