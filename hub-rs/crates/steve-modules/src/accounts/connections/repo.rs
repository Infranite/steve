//! SQL of `connections` (migration 005, owned by steve-store).

use serde_json::Value;
use steve_store::{Connections, Read, Result};

pub use steve_store::{ConnRow, HealthState};

pub fn get(c: &mut impl Read, id: &str) -> Result<Option<ConnRow>> {
    Connections(c).get(id)
}

pub fn list(c: &mut impl Read, ws: &str) -> Result<Vec<ConnRow>> {
    Connections(c).list(ws)
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
    w: &mut steve_store::W<'_>,
    id: &str,
    missing: &[(&str, &str)],
    granted: &[&str],
    now: i64,
) -> Result<bool> {
    let mut connections = Connections(w);
    let Some(mut row) = connections.get(id)? else {
        return Ok(false);
    };
    if !note_permissions_in(&mut row.config, missing, granted, now) {
        return Ok(false);
    }
    connections.replace_config(id, &row.config)?;
    Ok(true)
}

pub fn change_signature(r: &mut impl Read, id: &str) -> Result<Option<String>> {
    Connections(r).change_signature(id)
}
