//! Account storage types and the shared entry points used by integrations.

use steve_store::{Accounts, Read, Result, W};
pub use steve_store::{MatchRule, PrefixConfig};

pub use steve_store::System;

// ---------------------------------------------------------------- accounts

pub use steve_store::account_rows::{ACCOUNT, Account, AccountPatch, AccountQuery, NewAccount};

pub fn get(c: &mut impl Read, id: &str) -> Result<Option<Account>> {
    Accounts(c).get(id)
}

pub fn insert(tx: &mut W<'_>, ws: &str, a: &NewAccount) -> Result<Account> {
    Accounts(tx).insert(ws, a)
}

pub fn update(tx: &mut W<'_>, id: &str, p: &AccountPatch, revision: Option<i64>) -> Result<()> {
    Accounts(tx).update(id, p, revision)
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
    tx: &mut W<'_>,
    ws: &str,
    family: &str,
    handle: &str,
    account: &str,
    connection: Option<&str>,
) -> Result<bool> {
    Accounts(tx).add_handle(ws, family, handle, account, connection)
}

pub use steve_store::{ConnectionBrief, vault_items::ItemRef};
