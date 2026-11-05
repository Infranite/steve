//! SQL of slice V4: grants and `secret_uses` rows, and the few
//! reads of steve-core's tables it needs (session working directory, item project, bot name).
//! Parameterized queries only; no row ever holds a secret value.

use rusqlite::{Connection, OptionalExtension, Row, params};
use serde_json::Value;
use steve_store::Result;

// ------------------------------------------------------------------------------------------ grants

/// One `grants` row (any capability).
#[derive(Debug, Clone, PartialEq)]
pub struct GrantRecord {
    pub id: String,
    pub workspace_id: String,
    pub subject_id: String,
    pub resource_id: String,
    pub scope: Value,
    pub until: i64,
    pub revoked: Option<i64>,
}

fn json_obj(s: &str) -> Value {
    serde_json::from_str(s).unwrap_or(Value::Object(Default::default()))
}

/// Inserts a `secret.use` grant of a bot on a vault item.
#[allow(clippy::too_many_arguments)]
pub fn insert_grant(
    c: &Connection,
    id: &str,
    ws: &str,
    bot: &str,
    item_id: &str,
    scope: &Value,
    until: i64,
    created_by: &str,
    decision_id: Option<&str>,
    now: i64,
) -> Result<()> {
    c.prepare_cached(
        "INSERT INTO grants (id, workspace_id, subject_type, subject_id, capability, resource_type, resource_id,
           scope, until, created_by, decision_id, revoked, created)
         VALUES (?, ?, 'bot', ?, 'secret.use', 'vault_item', ?, ?, ?, ?, ?, NULL, ?)",
    )?
    .execute(params![
        id,
        ws,
        bot,
        item_id,
        scope.to_string(),
        until,
        created_by,
        decision_id,
        now
    ])?;
    Ok(())
}

/// A `secret.use` grant on a vault item by id.
pub fn grant(c: &Connection, id: &str) -> Result<Option<GrantRecord>> {
    Ok(c.prepare_cached(
        "SELECT id, workspace_id, subject_id, resource_id, scope, until, revoked FROM grants
         WHERE id = ? AND capability = 'secret.use' AND resource_type = 'vault_item'",
    )?
    .query_row([id], |r| {
        let scope: String = r.get(4)?;
        Ok(GrantRecord {
            id: r.get(0)?,
            workspace_id: r.get(1)?,
            subject_id: r.get(2)?,
            resource_id: r.get(3)?,
            scope: json_obj(&scope),
            until: r.get(5)?,
            revoked: r.get(6)?,
        })
    })
    .optional()?)
}

pub fn revoke_grant(c: &Connection, id: &str, now: i64) -> Result<bool> {
    Ok(
        c.prepare_cached("UPDATE grants SET revoked = ? WHERE id = ? AND revoked IS NULL")?
            .execute(params![now, id])?
            > 0,
    )
}

/// Open grants of `bot` on `item_id` whose scope is exactly `scope`; the one that lasts longest first.
pub fn covering_grants(
    c: &Connection,
    ws: &str,
    item_id: &str,
    bot: &str,
    scope: &Value,
    now: i64,
) -> Result<Vec<String>> {
    let mut st = c.prepare_cached(
        "SELECT id, scope FROM grants
         WHERE workspace_id = ? AND capability = 'secret.use' AND resource_type = 'vault_item' AND resource_id = ?
           AND subject_type = 'bot' AND subject_id = ? AND revoked IS NULL AND until > ?
         ORDER BY until DESC, id",
    )?;
    let rows = st.query_map(params![ws, item_id, bot, now], |r| {
        Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
    })?;
    let mut out = Vec::new();
    for row in rows {
        let (id, s) = row?;
        if json_obj(&s) == *scope {
            out.push(id);
        }
    }
    Ok(out)
}

// ------------------------------------------------------------------------------------- secret_uses

/// One `secret_uses` row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UseRecord {
    pub id: String,
    pub workspace_id: String,
    pub item_id: String,
    pub actor_type: String,
    pub actor_id: String,
    pub session_id: Option<String>,
    pub work_item_id: Option<String>,
    pub purpose: String,
    pub mode: String,
    pub command: Option<String>,
    pub fields: Vec<String>,
    pub decision_id: Option<String>,
    pub grant_id: Option<String>,
    pub device_id: String,
    pub status: String,
    pub exit_code: Option<i64>,
    pub created: i64,
    pub finished: Option<i64>,
}

const USE_COLS: &str = "u.id, u.workspace_id, u.item_id, u.actor_type, u.actor_id, u.session_id, u.work_item_id,
    u.purpose, u.mode, u.command, u.fields, u.decision_id, u.grant_id, u.device_id, u.status, u.exit_code,
    u.created, u.finished";

fn use_row(r: &Row) -> rusqlite::Result<UseRecord> {
    let fields: String = r.get(10)?;
    Ok(UseRecord {
        id: r.get(0)?,
        workspace_id: r.get(1)?,
        item_id: r.get(2)?,
        actor_type: r.get(3)?,
        actor_id: r.get(4)?,
        session_id: r.get(5)?,
        work_item_id: r.get(6)?,
        purpose: r.get(7)?,
        mode: r.get(8)?,
        command: r.get(9)?,
        fields: serde_json::from_str(&fields).unwrap_or_default(),
        decision_id: r.get(11)?,
        grant_id: r.get(12)?,
        device_id: r.get(13)?,
        status: r.get(14)?,
        exit_code: r.get(15)?,
        created: r.get(16)?,
        finished: r.get(17)?,
    })
}

pub fn insert_use(c: &Connection, u: &UseRecord) -> Result<()> {
    c.prepare_cached(
        "INSERT INTO secret_uses (id, workspace_id, item_id, actor_type, actor_id, session_id, work_item_id, run_id,
           purpose, mode, command, fields, decision_id, grant_id, device_id, status, exit_code, created, finished)
         VALUES (?, ?, ?, ?, ?, ?, ?, NULL, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
    )?
    .execute(params![
        u.id,
        u.workspace_id,
        u.item_id,
        u.actor_type,
        u.actor_id,
        u.session_id,
        u.work_item_id,
        u.purpose,
        u.mode,
        u.command,
        serde_json::to_string(&u.fields).unwrap_or_else(|_| "[]".into()),
        u.decision_id,
        u.grant_id,
        u.device_id,
        u.status,
        u.exit_code,
        u.created,
        u.finished
    ])?;
    Ok(())
}

pub fn get_use(c: &Connection, id: &str) -> Result<Option<UseRecord>> {
    Ok(c.prepare_cached(&format!(
        "SELECT {USE_COLS} FROM secret_uses u WHERE u.id = ?"
    ))?
    .query_row([id], use_row)
    .optional()?)
}

pub fn set_decision(c: &Connection, id: &str, decision_id: &str) -> Result<()> {
    c.prepare_cached("UPDATE secret_uses SET decision_id = ? WHERE id = ?")?
        .execute(params![decision_id, id])?;
    Ok(())
}

/// Moves a use on: `from` → `to`. `grant_id` is set when given; `finished` and `exit_code` too.
/// Returns false when the row was not in `from` (someone else moved it).
pub fn transition(
    c: &Connection,
    id: &str,
    from: &str,
    to: &str,
    grant_id: Option<&str>,
    exit_code: Option<i64>,
    finished: Option<i64>,
) -> Result<bool> {
    Ok(c.prepare_cached(
        "UPDATE secret_uses SET status = ?, grant_id = COALESCE(?, grant_id), exit_code = COALESCE(?, exit_code),
           finished = COALESCE(?, finished) WHERE id = ? AND status = ?",
    )?
    .execute(params![to, grant_id, exit_code, finished, id, from])?
        > 0)
}

/// Uses in a status, oldest first, created before `before` (`i64::MAX` = all).
pub fn uses_in(c: &Connection, status: &str, before: i64) -> Result<Vec<UseRecord>> {
    let mut st = c.prepare_cached(&format!(
        "SELECT {USE_COLS} FROM secret_uses u WHERE u.status = ? AND u.created < ? ORDER BY u.created, u.id"
    ))?;
    let rows = st.query_map(params![status, before], use_row)?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

/// The one-year retention.
pub fn delete_uses_before(c: &Connection, before: i64) -> Result<usize> {
    Ok(
        c.prepare_cached("DELETE FROM secret_uses WHERE created < ?")?
            .execute([before])?,
    )
}

/// A row of `GET /vault/uses`: the use, the item title, the actor's display name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UseListRow {
    pub u: UseRecord,
    pub item_title: String,
    pub actor_name: Option<String>,
}

/// Uses of items in vaults `user` sees (shared, their personal), newest first.
pub fn list_uses(
    c: &Connection,
    ws: &str,
    user: &str,
    item: Option<&str>,
    bot: Option<&str>,
    limit: i64,
) -> Result<Vec<UseListRow>> {
    let mut st = c.prepare_cached(&format!(
        "SELECT {USE_COLS}, i.title,
           CASE u.actor_type WHEN 'bot' THEN (SELECT b.name FROM bots b WHERE b.id = u.actor_id)
                             WHEN 'me' THEN (SELECT x.name FROM users x WHERE x.id = u.actor_id) END
         FROM secret_uses u JOIN vault_items i ON i.id = u.item_id JOIN vaults v ON v.id = i.vault_id
         WHERE u.workspace_id = ?1 AND i.workspace_id = ?1
           AND (v.scope = 'shared' OR (v.scope = 'personal' AND v.owner_user = ?2))
           AND (?3 IS NULL OR u.item_id = ?3)
           AND (?4 IS NULL OR (u.actor_type = 'bot' AND u.actor_id = ?4))
         ORDER BY u.created DESC, u.id DESC LIMIT ?5"
    ))?;
    let rows = st.query_map(params![ws, user, item, bot, limit], |r| {
        Ok(UseListRow {
            u: use_row(r)?,
            item_title: r.get(18)?,
            actor_name: r.get(19)?,
        })
    })?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

// ---------------------------------------------------------------------------- steve-core's tables

/// `(cwd, item_id, bot_id)` of a session.
pub type SessionRow = (String, Option<String>, Option<String>);

/// The [`SessionRow`] of a session of `ws`.
pub fn session(c: &Connection, ws: &str, id: &str) -> Result<Option<SessionRow>> {
    Ok(c.prepare_cached(
        "SELECT cwd, item_id, bot_id FROM sessions WHERE id = ? AND workspace_id = ?",
    )?
    .query_row([id, ws], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
    .optional()?)
}

/// The project of an item of `ws` (`Some(None)`: the item has none).
pub fn item_project(c: &Connection, ws: &str, item: &str) -> Result<Option<Option<String>>> {
    Ok(
        c.prepare_cached("SELECT project_id FROM items WHERE id = ? AND workspace_id = ?")?
            .query_row([item, ws], |r| r.get(0))
            .optional()?,
    )
}

/// `ownBot`: the bot is a member of `ws`.
pub fn bot_in(c: &Connection, ws: &str, bot: &str) -> Result<bool> {
    Ok(
        c.prepare_cached("SELECT 1 FROM bot_workspaces WHERE bot_id = ? AND workspace_id = ?")?
            .query_row([bot, ws], |_| Ok(()))
            .optional()?
            .is_some(),
    )
}

pub fn bot_name(c: &Connection, bot: &str) -> Result<Option<String>> {
    Ok(c.prepare_cached("SELECT name FROM bots WHERE id = ?")?
        .query_row([bot], |r| r.get(0))
        .optional()?)
}
