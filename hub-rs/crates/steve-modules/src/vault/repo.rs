//! SQL of slice V1: `vault_keys`, and the `dek_wrapped`/`kek_version` columns of `vault_items` that
//! rotation rewrites. Parameterized queries only.

use rusqlite::{Connection, OptionalExtension, params};
use steve_store::Result;

/// One row of `vault_keys`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeyRow {
    pub version: i64,
    pub keychain_account: String,
    pub fingerprint: String,
    /// `active` | `retiring` | `retired`
    pub status: String,
    pub created: i64,
    pub retired: Option<i64>,
}

pub const ACTIVE: &str = "active";
pub const RETIRING: &str = "retiring";
pub const RETIRED: &str = "retired";

/// All key versions of a workspace, oldest first.
pub fn keys(c: &Connection, ws: &str) -> Result<Vec<KeyRow>> {
    let mut st = c.prepare_cached(
        "SELECT version, keychain_account, fingerprint, status, created, retired FROM vault_keys
         WHERE workspace_id = ? ORDER BY version",
    )?;
    let rows = st.query_map([ws], |r| {
        Ok(KeyRow {
            version: r.get(0)?,
            keychain_account: r.get(1)?,
            fingerprint: r.get(2)?,
            status: r.get(3)?,
            created: r.get(4)?,
            retired: r.get(5)?,
        })
    })?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

pub fn insert_key(
    c: &Connection,
    ws: &str,
    version: i64,
    account: &str,
    fingerprint: &str,
    status: &str,
    now: i64,
) -> Result<()> {
    c.prepare_cached(
        "INSERT INTO vault_keys (workspace_id, version, keychain_account, fingerprint, status, created)
         VALUES (?, ?, ?, ?, ?, ?)",
    )?
    .execute(params![ws, version, account, fingerprint, status, now])?;
    Ok(())
}

pub fn set_key_status(
    c: &Connection,
    ws: &str,
    version: i64,
    status: &str,
    retired: Option<i64>,
) -> Result<()> {
    c.prepare_cached(
        "UPDATE vault_keys SET status = ?, retired = ? WHERE workspace_id = ? AND version = ?",
    )?
    .execute(params![status, retired, ws, version])?;
    Ok(())
}

/// A wrapped data key as stored on an item.
#[derive(Debug, Clone)]
pub struct WrappedRow {
    pub item_id: String,
    pub dek_wrapped: Vec<u8>,
    pub kek_version: i64,
}

/// Up to `limit` items of `ws` whose DEK is still wrapped by a version other than `target`, skipping
/// `skip` (items that failed to re-wrap, so one bad row can't stall the loop).
pub fn items_to_rewrap(
    c: &Connection,
    ws: &str,
    target: i64,
    skip: &[String],
    limit: usize,
) -> Result<Vec<WrappedRow>> {
    let mut st = c.prepare_cached(
        "SELECT id, dek_wrapped, kek_version FROM vault_items
         WHERE workspace_id = ? AND dek_wrapped IS NOT NULL AND kek_version IS NOT NULL
           AND kek_version != ? AND id NOT IN (SELECT value FROM json_each(?))
         ORDER BY id LIMIT ?",
    )?;
    let skip = serde_json::to_string(skip).unwrap_or_else(|_| "[]".into());
    let rows = st.query_map(params![ws, target, skip, limit as i64], |r| {
        Ok(WrappedRow {
            item_id: r.get(0)?,
            dek_wrapped: r.get(1)?,
            kek_version: r.get(2)?,
        })
    })?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

pub fn set_item_dek(c: &Connection, item_id: &str, wrapped: &[u8], version: i64) -> Result<()> {
    c.prepare_cached("UPDATE vault_items SET dek_wrapped = ?, kek_version = ? WHERE id = ?")?
        .execute(params![wrapped, version, item_id])?;
    Ok(())
}

/// How many items of `ws` still have a DEK under `version`.
pub fn count_on_version(c: &Connection, ws: &str, version: i64) -> Result<i64> {
    Ok(c.prepare_cached(
        "SELECT COUNT(*) FROM vault_items WHERE workspace_id = ? AND dek_wrapped IS NOT NULL AND kek_version = ?",
    )?
    .query_row(params![ws, version], |r| r.get(0))?)
}

/// Workspaces with a version in `retiring` (an interrupted rotation to resume at startup).
pub fn rotating_workspaces(c: &Connection) -> Result<Vec<String>> {
    let mut st = c.prepare_cached(
        "SELECT DISTINCT workspace_id FROM vault_keys WHERE status = 'retiring' ORDER BY workspace_id",
    )?;
    let rows = st.query_map([], |r| r.get(0))?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

/// The active version of `ws`, if initialized.
pub fn active_version(c: &Connection, ws: &str) -> Result<Option<i64>> {
    Ok(c.prepare_cached(
        "SELECT version FROM vault_keys WHERE workspace_id = ? AND status = 'active' ORDER BY version DESC LIMIT 1",
    )?
    .query_row([ws], |r| r.get(0))
    .optional()?)
}
