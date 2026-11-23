//! Account storage types and the shared entry points used by integrations.

use steve_store::{Accounts, Read, Result, Tx};
pub use steve_store::{MatchRule, PrefixConfig};

pub use steve_store::System;

/// Installs a system from an extension during the caller's boot transaction.
pub fn upsert_declared(tx: &Tx, id: &str, name: &str, ext: &str) -> Result<()> {
    tx.write(|w| steve_store::Systems(w).upsert_declared(id, name, ext))
}

// ---------------------------------------------------------------- accounts

pub use steve_store::account_rows::{ACCOUNT, Account, AccountPatch, AccountQuery, NewAccount};

pub fn get(c: &mut impl Read, id: &str) -> Result<Option<Account>> {
    Accounts(c).get(id)
}

pub fn insert(tx: &Tx, ws: &str, a: &NewAccount) -> Result<Account> {
    tx.write(|w| Accounts(w).insert(ws, a))
}

pub fn update(tx: &Tx, id: &str, p: &AccountPatch, revision: Option<i64>) -> Result<()> {
    tx.write(|w| Accounts(w).update(id, p, revision))
}

/// Accounts of one party, oldest first.
pub fn by_handle(
    c: &mut impl Read,
    ws: &str,
    family: &str,
    handle: &str,
) -> Result<Option<String>> {
    Accounts(c).by_handle(ws, family, handle)
}

pub fn add_handle(
    tx: &Tx,
    ws: &str,
    family: &str,
    handle: &str,
    account: &str,
    connection: Option<&str>,
) -> Result<bool> {
    tx.write(|w| Accounts(w).add_handle(ws, family, handle, account, connection))
}

pub use steve_store::{ConnectionBrief, vault_items::ItemRef};
