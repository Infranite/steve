//! A Bot's skills (migration 63, `bot_skills`): Markdown instructions its harness reads as
//! `<name>/SKILL.md`, written by the session's Node where the harness adapter declares it reads
//! skills. Only enabled ones are given to a session.

use rusqlite::{Connection, OptionalExtension, Row, params};
use serde::{Deserialize, Serialize};

use crate::Result;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Skill {
    pub id: String,
    pub workspace_id: String,
    pub bot_id: String,
    pub name: String,
    pub description: String,
    pub content: String,
    pub enabled: bool,
    pub created: i64,
    pub updated: i64,
}

const COLS: &str =
    "id, workspace_id, bot_id, name, description, content, enabled, created, updated";

fn row(r: &Row) -> rusqlite::Result<Skill> {
    Ok(Skill {
        id: r.get(0)?,
        workspace_id: r.get(1)?,
        bot_id: r.get(2)?,
        name: r.get(3)?,
        description: r.get(4)?,
        content: r.get(5)?,
        enabled: r.get::<_, i64>(6)? != 0,
        created: r.get(7)?,
        updated: r.get(8)?,
    })
}

/// A Bot's skills, by name.
pub fn list(c: &Connection, bot_id: &str) -> Result<Vec<Skill>> {
    let mut st = c.prepare_cached(&format!(
        "SELECT {COLS} FROM bot_skills WHERE bot_id = ? ORDER BY name"
    ))?;
    let v = st.query_map([bot_id], row)?.collect::<Result<_, _>>()?;
    Ok(v)
}

pub fn get(c: &Connection, id: &str) -> Result<Option<Skill>> {
    Ok(c.query_row(
        &format!("SELECT {COLS} FROM bot_skills WHERE id = ?"),
        [id],
        row,
    )
    .optional()?)
}

/// Another skill of `bot_id` named `name`, other than `except`.
pub fn name_taken(c: &Connection, bot_id: &str, name: &str, except: Option<&str>) -> Result<bool> {
    let n: i64 = c.query_row(
        "SELECT COUNT(*) FROM bot_skills WHERE bot_id = ?1 AND name = ?2 AND id <> ?3",
        params![bot_id, name, except.unwrap_or("")],
        |r| r.get(0),
    )?;
    Ok(n > 0)
}

pub fn insert(c: &Connection, s: &Skill) -> Result<()> {
    c.execute(
        &format!("INSERT INTO bot_skills ({COLS}) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)"),
        params![
            s.id,
            s.workspace_id,
            s.bot_id,
            s.name,
            s.description,
            s.content,
            s.enabled as i64,
            s.created,
            s.updated,
        ],
    )?;
    Ok(())
}

/// Writes every field but `id`, `workspace_id`, `bot_id` and `created`.
pub fn update(c: &Connection, s: &Skill) -> Result<()> {
    c.execute(
        "UPDATE bot_skills SET name = ?2, description = ?3, content = ?4, enabled = ?5, updated = ?6
          WHERE id = ?1",
        params![
            s.id,
            s.name,
            s.description,
            s.content,
            s.enabled as i64,
            s.updated
        ],
    )?;
    Ok(())
}

pub fn delete(c: &Connection, id: &str) -> Result<bool> {
    Ok(c.execute("DELETE FROM bot_skills WHERE id = ?", [id])? > 0)
}

/// The enabled skills of the Bot a session works as (none for a session without a Bot).
pub fn enabled_for_session(c: &Connection, session: &str) -> Result<Vec<Skill>> {
    let bot: Option<Option<String>> = c
        .query_row("SELECT bot_id FROM sessions WHERE id = ?", [session], |r| {
            r.get(0)
        })
        .optional()?;
    match bot.flatten() {
        Some(b) => Ok(list(c, &b)?.into_iter().filter(|s| s.enabled).collect()),
        None => Ok(vec![]),
    }
}
