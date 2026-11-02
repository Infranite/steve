//! Accounts and identification (slice A1 of steve-rs-plan/modules/accounts-vault.md): systems, accounts
//! (some party's identity in some system), their handles, the `email_prefix` match rule, and
//! [`identify::identify`], which turns a handle seen on a channel, in mail or in a directory into an
//! account and a contact.
//!
//! Accounts never hold secrets: passwords, tokens and full card numbers live in the vault (V1/V2); an
//! account row of a bank card keeps only its last four digits.
//!
//! - [`repo`] — the SQL (tables of migration 5, owned by steve-store);
//! - [`service`] — the rules, shared by REST, the change operations and identification;
//! - [`identify`] — §2.3;
//! - [`ops`] — `account.add` / `account.patch` change operations;
//! - [`api`] — the REST handlers; [`routes`] is what the integrator mounts under `/api`.
//!
//! Integration (integrator):
//! 1. `.merge(steve_modules::accounts::routes())` on the `/api` router, behind the `Caller` layer;
//! 2. `steve_modules::accounts::register(&mut registry)` (change operations);
//! 3. `steve_modules::accounts::plug(&mut ports)` before building the contacts router ([`Ports`]:
//!    `PartyHooks` + `HandleSearch`), so contact lists count accounts, `q` matches handles, and
//!    deleting/merging/splitting a contact takes its accounts along in the same transaction.

pub mod api;
pub mod connections;
pub mod enrich;
pub mod identify;
pub mod ops;
pub mod repo;
pub mod service;

use std::sync::Arc;

use rusqlite::Connection;
use serde_json::{Value, json};
use steve_store::Tx;

use crate::contacts::{HandleSearch, PartyHooks, Ports};
use crate::{ApiError, ApiResult, Registry};

pub use api::routes;
pub use enrich::{Enriched, Enrichment, TenantHint, enrich, looks_like_handle, tenant_org};
pub use identify::{ExtraHandle, Hints, Identified, identify};

/// Registers the module's change operations.
pub fn register(reg: &mut Registry) {
    reg.op("account.add", Box::new(ops::AccountAdd))
        .op("account.patch", Box::new(ops::AccountPatchOp));
}

/// What the accounts module plugs into contacts, alone.
pub fn ports() -> Ports {
    let mut p = Ports::default();
    plug(&mut p);
    p
}

/// Adds the accounts module to the contacts ports other modules fill too.
pub fn plug(p: &mut Ports) {
    p.hooks.push(Arc::new(AccountHooks));
    p.handles = Some(Arc::new(AccountHandles));
}

/// [`PartyHooks`] of the accounts module (§2.1). Its merge result is kept under `moved.hooks.vault`:
/// `{"accounts": [<moved account ids>]}`.
pub struct AccountHooks;

impl PartyHooks for AccountHooks {
    fn name(&self) -> &str {
        "vault"
    }

    /// Deletes the party's accounts and their handles; linked vault items stay, unlinked. An account
    /// with a connection refuses (“我” and the own organization cannot be deleted anyway).
    fn on_delete(&self, tx: &Tx, ws: &str, party_id: &str) -> ApiResult<()> {
        let accounts = repo::of_party(tx, ws, party_id)?;
        for a in &accounts {
            if repo::connection_of(tx, &a.id)?.is_some() {
                return Err(ApiError::conflict("先断开 Steve 的接入"));
            }
        }
        for a in &accounts {
            repo::delete(tx, &a.id)?;
            service::event(tx, ws, &a.id);
        }
        Ok(())
    }

    /// Moves `from`'s accounts to `into`.
    fn on_merge(&self, tx: &Tx, ws: &str, from: &str, into: &str) -> ApiResult<Value> {
        let ids: Vec<String> = repo::of_party(tx, ws, from)?
            .into_iter()
            .map(|a| a.id)
            .collect();
        let moved = repo::move_party(tx, ws, &ids, from, into)?;
        for id in &moved {
            service::event(tx, ws, id);
        }
        Ok(json!({ "accounts": moved }))
    }

    /// Moves the accounts recorded by [`AccountHooks::on_merge`] back to `from`, if they are still on
    /// `into` (one moved by hand since stays where you put it).
    fn on_split(&self, tx: &Tx, ws: &str, from: &str, into: &str, moved: &Value) -> ApiResult<()> {
        let ids: Vec<String> = moved
            .get("accounts")
            .and_then(Value::as_array)
            .map(|xs| {
                xs.iter()
                    .filter_map(Value::as_str)
                    .map(String::from)
                    .collect()
            })
            .unwrap_or_default();
        for id in repo::move_party(tx, ws, &ids, into, from)? {
            service::event(tx, ws, &id);
        }
        Ok(())
    }
}

/// [`HandleSearch`] over `account_handles` and the display handles.
pub struct AccountHandles;

impl HandleSearch for AccountHandles {
    fn parties(&self, c: &Connection, ws: &str, q: &str) -> ApiResult<Vec<String>> {
        Ok(repo::parties_by_handle(c, ws, q)?)
    }

    fn count(&self, c: &Connection, ws: &str, party_id: &str) -> ApiResult<i64> {
        Ok(repo::count_of(c, ws, party_id)?)
    }
}
