//! The workspace's skills (migration 65, `skills`): Markdown instructions the harness of a Bot
//! assigned one reads as `<name>/SKILL.md`, written by the session's Node where the harness adapter
//! declares it reads skills. Only the ones switched on for the Bot are given ([`super`]).

use rusqlite::{Connection, OptionalExtension, Row, params};
use serde::{Deserialize, Serialize};

use crate::Result;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Skill {
    pub id: String,
    pub workspace_id: String,
    pub name: String,
    pub description: String,
    pub content: String,
    pub created: i64,
    pub updated: i64,
}

const COLS: &str = "id, workspace_id, name, description, content, created, updated";

fn row(r: &Row) -> rusqlite::Result<Skill> {
    Ok(Skill {
        id: r.get(0)?,
        workspace_id: r.get(1)?,
        name: r.get(2)?,
        description: r.get(3)?,
        content: r.get(4)?,
        created: r.get(5)?,
        updated: r.get(6)?,
    })
}

/// A workspace's skills, by name.
pub fn list(c: &Connection, ws: &str) -> Result<Vec<Skill>> {
    let mut st = c.prepare_cached(&format!(
        "SELECT {COLS} FROM skills WHERE workspace_id = ? ORDER BY name"
    ))?;
    let v = st.query_map([ws], row)?.collect::<Result<_, _>>()?;
    Ok(v)
}

pub fn get(c: &Connection, id: &str) -> Result<Option<Skill>> {
    Ok(c.query_row(
        &format!("SELECT {COLS} FROM skills WHERE id = ?"),
        [id],
        row,
    )
    .optional()?)
}

/// Another skill of `ws` named `name`, other than `except`.
pub fn name_taken(c: &Connection, ws: &str, name: &str, except: Option<&str>) -> Result<bool> {
    let n: i64 = c.query_row(
        "SELECT COUNT(*) FROM skills WHERE workspace_id = ?1 AND name = ?2 AND id <> ?3",
        params![ws, name, except.unwrap_or("")],
        |r| r.get(0),
    )?;
    Ok(n > 0)
}

pub fn insert(c: &Connection, s: &Skill) -> Result<()> {
    c.execute(
        &format!("INSERT INTO skills ({COLS}) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)"),
        params![
            s.id,
            s.workspace_id,
            s.name,
            s.description,
            s.content,
            s.created,
            s.updated,
        ],
    )?;
    Ok(())
}

/// Writes every field but `id`, `workspace_id` and `created`.
pub fn update(c: &Connection, s: &Skill) -> Result<()> {
    c.execute(
        "UPDATE skills SET name = ?2, description = ?3, content = ?4, updated = ?5 WHERE id = ?1",
        params![s.id, s.name, s.description, s.content, s.updated],
    )?;
    Ok(())
}

/// The skill and who used it go.
pub fn delete(c: &Connection, id: &str) -> Result<bool> {
    super::remove_entry(c, "skill", id)?;
    Ok(c.execute("DELETE FROM skills WHERE id = ?", [id])? > 0)
}

/// The skills `bot` is assigned, each with whether it is switched on for it, by name.
pub fn of_bot(c: &Connection, bot: &str) -> Result<Vec<(Skill, bool)>> {
    let mut out = vec![];
    for a in super::of_bot(c, bot, "skill")? {
        if let Some(s) = get(c, &a.entry_id)? {
            out.push((s, a.enabled));
        }
    }
    out.sort_by(|(a, _), (b, _)| a.name.cmp(&b.name));
    Ok(out)
}

/// The skills the Bot a session works as has switched on (none for a session without a Bot).
pub fn enabled_for_session(c: &Connection, session: &str) -> Result<Vec<Skill>> {
    match super::bot_of_session(c, session)? {
        Some(b) => Ok(of_bot(c, &b)?
            .into_iter()
            .filter(|(_, on)| *on)
            .map(|(s, _)| s)
            .collect()),
        None => Ok(vec![]),
    }
}
