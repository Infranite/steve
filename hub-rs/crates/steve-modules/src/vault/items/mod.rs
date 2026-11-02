//! Vault slice V2 (steve-rs-plan/modules/accounts-vault.md §4.1, §4.2, §5): items and fields.
//!
//! - [`templates`]: the built-in templates (seeded under workspace `*`);
//! - three vaults per workspace, created on first use: `个人` (one per user), `共享`, `系统` (connection
//!   credentials, never listed);
//! - items by template, fields encrypted with the item's data key; responses carry secret fields
//!   with `value: null` (card numbers: last four digits in `hint`); the list leaves fields out;
//! - search (`q`) over title, site, field labels, non-secret values and the linked account only;
//! - delete destroys the data key and every value and revokes the item's grants;
//! - reveal through a [`RevealGate`] (V3: WebAuthn; until then [`NoPasskeys`] answers
//!   `400 先在这台设备上登记 Touch ID 或通行密钥`);
//! - [`Credential`] / [`Vault::with_credential`] and the system-vault calls for connections (A2).
//!
//! Every write and every secret access is audited (`vault_item.*`, `vault_field.*`, `vault.reveal`,
//! `vault.reveal_refused`, `vault.use`) and publishes `{type:"vault", id: <item>, ws}` (ids only).
//!
//! Integration: `.merge(vault::items::routes(vault.clone(), Arc::new(NoPasskeys)))` under `/api` after
//! `require_user`, next to `vault::routes(vault)`; V3 replaces the gate.

mod api;
mod credential;
pub mod repo;
mod reveal;
mod service;
pub mod templates;

pub use api::routes;
pub use credential::Credential;
pub use repo::ItemFilter;
pub use reveal::{NEED_PASSKEY, NoPasskeys, REVEAL_TTL_S, RevealGate, RevealTarget, Verified};
pub(crate) use service::own_item;
pub use service::{
    AccountRef, FieldOut, GrantOut, ItemOut, ItemPatch, NewField, NewItem, PreparedItem, VaultOut,
    VaultRef,
};
#[cfg(test)]
mod tests;
