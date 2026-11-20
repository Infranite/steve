//! Registry of implemented named operations and queries. Generic CRUD is not counted.
//! `rev` is reserved: entity fields must not use that name. A revision is the commit
//! sequence within a physical file, not a per-row counter. Callers compare only equality;
//! they must not assume an update gives the previous row revision plus one.
use crate::{Domain, Error};

/// A name is unique within a domain and kind; operation/query names should remain stable.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EntryKind {
    Operation,
    Query,
}
/// Static contract entry. A named operation or query enters the contract only when
/// its implementation and semantic conformance test land together in the same task.
/// Planned trigger replacements live only in the SQLite schema/triggers.tsv catalogue.
/// Backends advertise the entries they support as op:/query: features in Hello.
#[derive(Clone, Copy, Debug)]
pub struct RegistryEntry {
    pub name: &'static str,
    pub domain: Domain,
    pub kind: EntryKind,
    pub parameter_type: &'static str,
    pub semantics: &'static str,
}
/// Named operations and named queries together are limited to 120 entries per domain.
pub const DOMAIN_LIMIT: usize = 120;
/// Implemented contracts; domain context modules own entries and parameter types.
/// Digest has a reference implementation and semantic test; production SQLite wiring
/// is completed separately by its backend.
pub static ENTRIES: &[RegistryEntry] = &[crate::ops::comms::digest::SYNC_PENDING_MEMBERSHIP];
/// Unknown names fail closed. Hello negotiates support in the chosen backend.
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
