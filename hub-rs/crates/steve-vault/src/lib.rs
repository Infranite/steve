//! Accounts, connections and the secret vault
//!
//! Spec: steve-rs-plan/modules/accounts-vault.md. The vault service itself (keys, items, grants,
//! injection) lives in `steve_modules::vault`; this crate has the two store-free pieces other crates
//! call directly:
//!
//! - [`guard`]: `guard::scan(text)` finds pasted passwords, tokens, keys and card numbers (§4.6);
//! - [`redact`]: `redact::scrub(session_id, &mut Value)` removes injected secret values (and their
//!   base64, URL-encoded and hex forms) from what Steve records about a session (§4.4).

pub mod guard;
pub mod redact;
