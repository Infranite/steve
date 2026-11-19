//! The built-in SQLite implementation of the domain storage contract (`steve-storage-api`).
//!
//! One engine per physical file; in wave 2 all three domains (core, stream, comms) live in one
//! file, `data/steve.sqlite3`. Each module below is owned by one task card; see the group-2 design.

pub mod backup;
pub mod checkpoint;
pub mod digest;
pub mod domains;
pub mod engine;
pub mod exec;
pub mod layout;
pub mod legacy;
pub mod migrate;
pub mod open;
pub mod pragmas;
pub mod publish;
pub mod retention;
pub mod rev;
pub mod scan;
pub mod schema;
pub mod snapshot;
pub mod transfer;
pub mod uow;
