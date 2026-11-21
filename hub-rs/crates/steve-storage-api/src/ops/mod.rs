//! Domain-owned operations, organized by bounded context.
//! A named operation enters this contract only when its implementation and semantic
//! conformance test land together in the same task. Planned replacements belong only
//! in the SQLite backend's schema/triggers.tsv catalogue.
pub mod comms;
