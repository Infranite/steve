//! The capabilities library (migration 65): MCP servers ([`mcp`]), skills ([`skills`]) and remote A2A
//! agents ([`a2a`]) are defined once per workspace and assigned per Bot (`bot_capabilities`, this
//! module). A session gets the entries its Bot is assigned and has switched on.

pub mod a2a;
pub mod mcp;
pub mod skills;
pub mod tools;

use rusqlite::{Connection, OptionalExtension, params};
use serde::Serialize;

use crate::Result;

/// The kinds of entry a Bot can be assigned.
pub const KINDS: &[&str] = &["mcp", "skill", "a2a"];

/// One assignment: Bot `bot_id` uses entry `entry_id` of `kind`, switched on or off for it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Assignment {
    pub bot_id: String,
    pub kind: String,
    pub entry_id: String,
    pub enabled: bool,
    pub created: i64,
}

fn row(r: &rusqlite::Row) -> rusqlite::Result<Assignment> {
    Ok(Assignment {
        bot_id: r.get(0)?,
        kind: r.get(1)?,
        entry_id: r.get(2)?,
        enabled: r.get::<_, i64>(3)? != 0,
        created: r.get(4)?,
    })
}

/// A Bot's assignments of `kind`.
pub fn of_bot(c: &Connection, bot: &str, kind: &str) -> Result<Vec<Assignment>> {
    let mut st = c.prepare_cached(
        "SELECT bot_id, kind, entry_id, enabled, created FROM bot_capabilities
          WHERE bot_id = ? AND kind = ? ORDER BY created, entry_id",
    )?;
    let v = st.query_map([bot, kind], row)?.collect::<Result<_, _>>()?;
    Ok(v)
}

/// Who uses entry `entry_id` of `kind` (Bots not deleted).
pub fn of_entry(c: &Connection, kind: &str, entry_id: &str) -> Result<Vec<Assignment>> {
    let mut st = c.prepare_cached(
        "SELECT a.bot_id, a.kind, a.entry_id, a.enabled, a.created FROM bot_capabilities a
           JOIN bots b ON b.id = a.bot_id AND b.deleted = 0
          WHERE a.kind = ? AND a.entry_id = ? ORDER BY a.created, a.bot_id",
    )?;
    let v = st
        .query_map([kind, entry_id], row)?
        .collect::<Result<_, _>>()?;
    Ok(v)
}

/// Every assignment of `kind` of Bots not deleted, for the library's lists.
pub fn all(c: &Connection, kind: &str) -> Result<Vec<Assignment>> {
    let mut st = c.prepare_cached(
        "SELECT a.bot_id, a.kind, a.entry_id, a.enabled, a.created FROM bot_capabilities a
           JOIN bots b ON b.id = a.bot_id AND b.deleted = 0
          WHERE a.kind = ? ORDER BY a.created, a.bot_id",
    )?;
    let v = st.query_map([kind], row)?.collect::<Result<_, _>>()?;
    Ok(v)
}

pub fn get(c: &Connection, bot: &str, kind: &str, entry_id: &str) -> Result<Option<Assignment>> {
    Ok(c.query_row(
        "SELECT bot_id, kind, entry_id, enabled, created FROM bot_capabilities
          WHERE bot_id = ? AND kind = ? AND entry_id = ?",
        [bot, kind, entry_id],
        row,
    )
    .optional()?)
}

/// Assigns or switches: the row as given, made when missing.
pub fn set(
    c: &Connection,
    bot: &str,
    kind: &str,
    entry_id: &str,
    enabled: bool,
    now: i64,
) -> Result<()> {
    c.execute(
        "INSERT INTO bot_capabilities (bot_id, kind, entry_id, enabled, created) VALUES (?1, ?2, ?3, ?4, ?5)
           ON CONFLICT (bot_id, kind, entry_id) DO UPDATE SET enabled = excluded.enabled",
        params![bot, kind, entry_id, enabled as i64, now],
    )?;
    Ok(())
}

/// Takes an entry off a Bot, and its pin with it.
pub fn remove(c: &Connection, bot: &str, kind: &str, entry_id: &str) -> Result<bool> {
    tools::set_pin(c, bot, kind, entry_id, false, 0)?;
    Ok(c.execute(
        "DELETE FROM bot_capabilities WHERE bot_id = ? AND kind = ? AND entry_id = ?",
        [bot, kind, entry_id],
    )? > 0)
}

/// An entry is gone: nobody uses (or pins) it any more.
pub fn remove_entry(c: &Connection, kind: &str, entry_id: &str) -> Result<usize> {
    tools::drop_target(c, kind, entry_id)?;
    Ok(c.execute(
        "DELETE FROM bot_capabilities WHERE kind = ? AND entry_id = ?",
        [kind, entry_id],
    )?)
}

/// The Bot a session works as.
pub fn bot_of_session(c: &Connection, session: &str) -> Result<Option<String>> {
    Ok(
        c.query_row("SELECT bot_id FROM sessions WHERE id = ?", [session], |r| {
            r.get::<_, Option<String>>(0)
        })
        .optional()?
        .flatten(),
    )
}

/// The entries of `kind` `bot` has switched on, by id.
pub fn enabled_ids(c: &Connection, bot: &str, kind: &str) -> Result<Vec<String>> {
    Ok(of_bot(c, bot, kind)?
        .into_iter()
        .filter(|a| a.enabled)
        .map(|a| a.entry_id)
        .collect())
}

/// Whether `bot` has entry `entry_id` of `kind` assigned and switched on.
pub fn is_enabled(c: &Connection, bot: &str, kind: &str, entry_id: &str) -> Result<bool> {
    Ok(get(c, bot, kind, entry_id)?.is_some_and(|a| a.enabled))
}
