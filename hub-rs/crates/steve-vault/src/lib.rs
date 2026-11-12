//! Accounts, connections and the secret vault
//!
//! The vault service itself (keys, items, grants,
//! injection) lives in `steve_modules::vault`; this crate has the store-free piece other crates
//! call directly:
//!
//! - [`harness`]: what an agent harness Steve launches must not read or inherit.

pub mod harness;
