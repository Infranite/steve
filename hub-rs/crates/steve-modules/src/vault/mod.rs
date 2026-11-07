//! Vault slice V1: keys and encryption.
//!
//! - one 256-bit master key (KEK) per workspace in an OS-native key store ([`keystore`]: the macOS
//!   keychain, a Linux Secret Service keyring, or a protected key directory); the database holds only
//!   its version and fingerprint (`vault_keys`);
//! - one data key (DEK) per item, wrapped by the KEK; secret fields encrypted by the DEK
//!   ([`crypto`], XChaCha20-Poly1305, AAD binding workspace/item/field);
//! - a one-time recovery code (the KEK in base32) from `POST /vault/init` and `/vault/rotate`;
//! - rotation re-wraps DEKs in the background, then retires and deletes the old KEK ([`Vault`]).
//!
//! Slice V2 (items, fields, masked reads, the reveal gate, `with_credential`) is [`items`].
//!
//! Secrets live only in [`secret`] types (zeroized, opaque `Debug`). Tables: migration 005 (written by
//! slice M0; the draft is `docs/v1-schema-draft.sql`).
//!
//! Integration: `let ks = keystore::from_env(&steve_home, |k| std::env::var(k).ok())?;` (fails at
//! startup for `STEVE_VAULT_KEYSTORE=file` on the real `~/.steve`, or when the system has no store), `let vault = Vault::new(store, ks);`,
//! `vault.resume_rotations()` once the runtime is up, and merge [`routes`]`(vault)` under `/api`.
//!
//! Slice V4 (bot use of secrets): [`grants`], [`r#use`] (requests, the `secret_use`
//! decision handler, the job that runs approved commands), [`inject`] (Steve runs the command),
//! [`mcp`] (`steve_secret_list`, `steve_secret_use`), [`use_api`] (grants and the use log), and the
//! store-free [`redact`] and [`guard`] re-exported from steve-vault. Integration:
//! `let uses = SecretUses::new(vault.clone(), steve_home, Arc::new(<steve-core send>));`,
//! `registry.decision(r#use::SUBJECT, uses.decision_handler())`, `uses.start()` once the runtime is
//! up, merge [`use_api::routes`]`(vault)`, route [`mcp::tools`] to [`mcp::call`].

mod api;
pub mod crypto;
mod error;
pub mod flow_use;
pub mod grants;
pub mod inject;
pub mod items;
pub mod keystore;
pub mod mcp;
pub mod repo;
pub mod secret;
mod service;
#[cfg(test)]
mod tests;
pub mod r#use;
pub mod use_api;
pub mod use_repo;
pub mod webauthn;

pub use api::routes;
pub use error::{VaultError, VaultResult};
pub use service::{KeyStoreInfo, Keychain, NewItemKey, Rotation, Status, Vault, ensure_active};
pub use steve_vault::{guard, redact};
pub use r#use::{
    NodePlace, NodeUse, SecretUses, SessionSink, UseCx, UseNodes, UseOutcome, UseRequest,
    UseValues, pasted_secret,
};
