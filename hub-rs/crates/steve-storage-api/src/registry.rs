//! Named operation/query catalogue. Generic CRUD is not counted as a named operation.
use crate::{Domain, Error};

/// A name is unique within a domain and kind; operation/query names should remain stable.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EntryKind {
    Operation,
    Query,
}
/// Static declaration, including implementation status and historical trigger provenance.
/// Only implemented entries may be advertised as op:/query: features by a backend.
#[derive(Clone, Copy, Debug)]
pub struct RegistryEntry {
    pub name: &'static str,
    pub domain: Domain,
    pub kind: EntryKind,
    pub parameter_type: &'static str,
    pub semantics: &'static str,
    pub implemented: bool,
    pub triggers: &'static [&'static str],
}
/// Named operations and named queries together are limited to 120 entries per domain.
pub const DOMAIN_LIMIT: usize = 120;
/// The initial catalogue; domain context modules own declarations and parameter types.
pub static ENTRIES: &[RegistryEntry] = &[
    crate::ops::core::attention::ADJUST_UNREAD_COUNT,
    crate::ops::core::attention::SYNC_FEED_ENTRY,
    crate::ops::core::attention::SYNC_MEMBER_UNREAD,
    crate::ops::core::attention::SYNC_FEED_READ,
    crate::ops::core::attention::RECORD_FINAL_REPLY,
    crate::ops::core::alarms::MARK_DIRTY,
    crate::ops::stream::transcript::COLLECT_UNREFERENCED_PAYLOADS,
    crate::ops::core::gateway::SYNC_RESULT_READY,
    crate::ops::core::items::VALIDATE_COMPLETION_DEPENDENCIES,
    crate::ops::comms::notifications::PURGE_DELIVERY_COPY,
    crate::ops::comms::notifications::PURGE_SOURCE_COPIES,
    crate::ops::comms::notifications::PUBLISH_FEED_FACT,
    crate::ops::comms::deliveries::PURGE_MESSAGE_INTENT,
    crate::ops::comms::todos::CHANGE_WITH_ALARM_FACT,
    crate::ops::comms::digest::SYNC_PENDING_MEMBERSHIP,
];
/// Unknown names fail closed. Declaration does not imply the chosen backend implements it.
pub fn lookup(
    domain: Domain,
    kind: EntryKind,
    name: &str,
) -> Result<&'static RegistryEntry, Error> {
    ENTRIES
        .iter()
        .find(|e| e.domain == domain && e.kind == kind && e.name == name)
        .ok_or_else(|| {
            crate::error(
                steve_types::Code::Unsupported,
                "unknown storage operation or query",
            )
        })
}
