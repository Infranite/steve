//! SQL of `connections` (migration 005, owned by steve-store).

use rusqlite::{Connection, OptionalExtension, Row, params};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use steve_store::Result;

/// `connections.health`. The shown part is `last_ok`, `last_error`, `error_at`; the rest drives the
/// jobs: `fails` in a row, `next_at` (backoff: no check or refresh before it), and the token expiry
/// mirrored from the credential so the refresh job needn't decrypt to know what is due.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct HealthState {
    pub last_ok: i64,
    pub last_error: String,
    pub error_at: i64,
    pub fails: i64,
    pub next_at: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub refresh_expires_at: Option<i64>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ConnRow {
    pub id: String,
    pub workspace_id: String,
    pub account_id: String,
    pub adapter: String,
    pub auth: String,
    pub granted_scopes: Vec<String>,
    pub caps: Vec<String>,
    pub credential_item: Option<String>,
    /// `ok|reauth|error|paused|revoked`.
    pub status: String,
    pub health: HealthState,
    pub config: Value,
    pub last_sync: Option<i64>,
    pub created_by: String,
    pub revision: i64,
    pub created: i64,
    pub updated: i64,
}

fn json_list(s: String) -> Vec<String> {
    serde_json::from_str(&s).unwrap_or_default()
}

impl ConnRow {
    fn from_row(r: &Row) -> rusqlite::Result<Self> {
        Ok(Self {
            id: r.get("id")?,
            workspace_id: r.get("workspace_id")?,
            account_id: r.get("account_id")?,
            adapter: r.get("adapter")?,
            auth: r.get("auth")?,
            granted_scopes: json_list(r.get("granted_scopes")?),
            caps: json_list(r.get("caps")?),
            credential_item: r.get("credential_item")?,
            status: r.get("status")?,
            health: serde_json::from_str(&r.get::<_, String>("health")?).unwrap_or_default(),
            config: serde_json::from_str(&r.get::<_, String>("config")?)
                .unwrap_or(Value::Object(Default::default())),
            last_sync: r.get("last_sync")?,
            created_by: r.get("created_by")?,
            revision: r.get("revision")?,
            created: r.get("created")?,
            updated: r.get("updated")?,
        })
    }
}

fn to_json<T: Serialize>(v: &T) -> String {
    serde_json::to_string(v).unwrap_or_else(|_| "null".into())
}

pub fn get(c: &Connection, id: &str) -> Result<Option<ConnRow>> {
    Ok(c.prepare_cached("SELECT * FROM connections WHERE id = ?")?
        .query_row([id], ConnRow::from_row)
        .optional()?)
}

pub fn of_account(c: &Connection, account: &str) -> Result<Option<ConnRow>> {
    Ok(
        c.prepare_cached("SELECT * FROM connections WHERE account_id = ?")?
            .query_row([account], ConnRow::from_row)
            .optional()?,
    )
}

pub fn list(c: &Connection, ws: &str) -> Result<Vec<ConnRow>> {
    let mut st =
        c.prepare_cached("SELECT * FROM connections WHERE workspace_id = ? ORDER BY created, id")?;
    let rows = st.query_map([ws], ConnRow::from_row)?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

/// Every connection the jobs look at (`ok` and `error`, all workspaces).
pub fn live(c: &Connection) -> Result<Vec<ConnRow>> {
    let mut st = c.prepare_cached(
        "SELECT * FROM connections WHERE status IN ('ok', 'error') ORDER BY created, id",
    )?;
    let rows = st.query_map([], ConnRow::from_row)?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

/// Every connection that is not waiting for a new authorization or gone (`ok`, `error`, `paused`).
pub fn authorized(c: &Connection) -> Result<Vec<ConnRow>> {
    let mut st = c.prepare_cached(
        "SELECT * FROM connections WHERE status IN ('ok', 'error', 'paused') ORDER BY created, id",
    )?;
    let rows = st.query_map([], ConnRow::from_row)?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

pub fn insert(c: &Connection, r: &ConnRow) -> Result<()> {
    c.prepare_cached(
        "INSERT INTO connections (id, workspace_id, account_id, adapter, auth, granted_scopes, caps, credential_item, status, health, config, last_sync, created_by, revision, created, updated)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, 1, ?, ?)",
    )?
    .execute(params![
        r.id,
        r.workspace_id,
        r.account_id,
        r.adapter,
        r.auth,
        to_json(&r.granted_scopes),
        to_json(&r.caps),
        r.credential_item,
        r.status,
        to_json(&r.health),
        to_json(&r.config),
        r.last_sync,
        r.created_by,
        r.created,
        r.updated
    ])?;
    Ok(())
}

/// Writes every mutable column and bumps the revision.
pub fn update(c: &Connection, r: &ConnRow, now: i64) -> Result<()> {
    c.prepare_cached(
        "UPDATE connections SET granted_scopes = ?, caps = ?, credential_item = ?, status = ?, health = ?, config = ?, last_sync = ?, revision = revision + 1, updated = ? WHERE id = ?",
    )?
    .execute(params![
        to_json(&r.granted_scopes),
        to_json(&r.caps),
        r.credential_item,
        r.status,
        to_json(&r.health),
        to_json(&r.config),
        r.last_sync,
        now,
        r.id
    ])?;
    Ok(())
}

/// The connection was turned on again (connected again, resumed, a capability added) at `now`:
/// what it receives from before then is history (`connections.enabled_at`, migration 44).
pub fn set_enabled_at(c: &Connection, id: &str, now: i64) -> Result<()> {
    c.prepare_cached("UPDATE connections SET enabled_at = ? WHERE id = ?")?
        .execute(params![now, id])?;
    Ok(())
}

/// `config` only, for what an adapter learned while working (a permission a call was refused
/// for): no revision bump, the row didn't change by anyone's hand — and nothing that watches
/// [`change_signature`] runs again because of it.
pub fn set_config(c: &Connection, id: &str, config: &Value, now: i64) -> Result<()> {
    c.prepare_cached("UPDATE connections SET config = ?, updated = ? WHERE id = ?")?
        .execute(params![to_json(config), now, id])?;
    Ok(())
}

/// Health and status only (jobs): no revision bump, the row didn't change by anyone's hand.
pub fn set_health(c: &Connection, id: &str, status: &str, h: &HealthState, now: i64) -> Result<()> {
    c.prepare_cached("UPDATE connections SET status = ?, health = ?, updated = ? WHERE id = ?")?
        .execute(params![status, to_json(h), now, id])?;
    Ok(())
}

pub fn set_last_sync(c: &Connection, id: &str, at: i64) -> Result<()> {
    c.prepare_cached("UPDATE connections SET last_sync = ? WHERE id = ?")?
        .execute(params![at, id])?;
    Ok(())
}

/// Deletes the row and the handles valid only under it.
pub fn delete(c: &Connection, id: &str) -> Result<()> {
    c.prepare_cached("DELETE FROM account_handles WHERE connection_id = ?")?
        .execute([id])?;
    c.prepare_cached("DELETE FROM connections WHERE id = ?")?
        .execute([id])?;
    Ok(())
}

/// Links the credential item to its account (`vault_items.account_id`).
pub fn link_item(c: &Connection, item: &str, account: &str) -> Result<()> {
    c.prepare_cached("UPDATE vault_items SET account_id = ? WHERE id = ?")?
        .execute(params![account, item])?;
    Ok(())
}

/// What a connection learned at run time about its provider-side permissions — the one writer of
/// `config.permissions` (the rows `verify` stores and the 接入 page reads, `[{scope, purpose, level,
/// status}]`) and `config.missing_permissions` after `verify`: each `(scope, purpose)` of `missing`
/// is marked `missing` with `refused_at = now` (a `required` row of its own when `verify` didn't
/// list it), each scope of `granted` not marked `granted` (missing, unknown) becomes `granted`. Returns whether
/// anything changed. Nothing else in `config` moves.
///
/// Callers: the Feishu client when a call is refused for a scope (through
/// `Connections::update_config`), name resolution and the directory sync ([`note_permissions`]).
pub fn note_permissions_in(
    config: &mut Value,
    missing: &[(&str, &str)],
    granted: &[&str],
    now: i64,
) -> bool {
    if !config.is_object() {
        *config = Value::Object(Default::default());
    }
    let mut rows: Vec<Value> = config
        .get("permissions")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let mut changed = false;
    for (scope, purpose) in missing {
        match rows
            .iter_mut()
            .find(|r| r.get("scope").and_then(Value::as_str) == Some(scope))
        {
            Some(r) if r.get("status").and_then(Value::as_str) != Some("missing") => {
                r["status"] = Value::from("missing");
                r["refused_at"] = Value::from(now);
                changed = true;
            }
            Some(_) => {}
            None => {
                rows.push(serde_json::json!({
                    "scope": scope, "purpose": purpose, "level": "required", "status": "missing",
                    "refused_at": now,
                }));
                changed = true;
            }
        }
    }
    for scope in granted {
        if let Some(r) = rows.iter_mut().find(|r| {
            r.get("scope").and_then(Value::as_str) == Some(scope)
                && r.get("status").and_then(Value::as_str) != Some("granted")
        }) {
            r["status"] = Value::from("granted");
            changed = true;
        }
    }
    if !changed {
        return false;
    }
    let missing_list: Vec<Value> = rows
        .iter()
        .filter(|r| r.get("status").and_then(Value::as_str) == Some("missing"))
        .filter_map(|r| r.get("scope").cloned())
        .collect();
    config["permissions"] = Value::Array(rows);
    config["missing_permissions"] = Value::Array(missing_list);
    true
}

/// [`note_permissions_in`] on connection `id`'s row (inside the caller's transaction; no revision,
/// no change event). Returns whether anything changed.
pub fn note_permissions(
    c: &Connection,
    id: &str,
    missing: &[(&str, &str)],
    granted: &[&str],
    now: i64,
) -> Result<bool> {
    let Some(row) = get(c, id)? else {
        return Ok(false);
    };
    let mut config = row.config.clone();
    if !note_permissions_in(&mut config, missing, granted, now) {
        return Ok(false);
    }
    c.execute(
        "UPDATE connections SET config = ? WHERE id = ?",
        params![config.to_string(), id],
    )?;
    Ok(true)
}

/// Sets one top-level key of `config` (state an adapter keeps about itself, never a secret).
pub fn set_config_key(c: &Connection, id: &str, key: &str, value: &Value) -> Result<()> {
    c.execute(
        "UPDATE connections SET config = json_set(CASE WHEN json_valid(config) THEN config ELSE '{}' END, '$.' || ?, json(?)) WHERE id = ?",
        params![key, value.to_string(), id],
    )?;
    Ok(())
}

/// What tells that a connection changed in a way worth acting on again (verified, re-authorized, a
/// permission granted, a new credential, back to `ok`) — not its health counters, which change on
/// every failed call. `None` when it is gone.
pub fn change_signature(c: &Connection, id: &str) -> Result<Option<String>> {
    Ok(c.prepare_cached(
        "SELECT status || '|' || granted_scopes || '|' || caps || '|' || coalesce(credential_item, '') || '|'
                || coalesce(json_extract(config, '$.permissions_checked_at'), '') || '|' || revision
           FROM connections WHERE id = ?",
    )?
    .query_row([id], |r| r.get(0))
    .optional()?)
}
