//! The built-in SQLite implementation of the domain storage contract (`steve-storage-api`):
//! the target schema, backup, retention and the digest intake.

pub mod backup;
pub mod digest;
pub mod retention;
pub mod schema;
