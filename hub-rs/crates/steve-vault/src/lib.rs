//! Accounts, connections and the secret vault
//!
//! The vault service itself (keys, items, grants,
//! injection) lives in `steve_modules::vault`; this crate has the two store-free pieces other crates
//! call directly:
//!
//! - [`guard`]: `guard::scan(text)` finds pasted passwords, tokens, keys and card numbers;
//! - [`redact`]: `redact::scrub(session_id, &mut Value)` removes injected secret values (and their
//!   base64, URL-encoded and hex forms) from what Steve records about a session.

pub mod guard;
pub mod redact;
