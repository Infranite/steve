//! The workspace's skills (migration 65, `skills`): Markdown instructions the harness of a Bot
//! assigned one reads as `<name>/SKILL.md`, written by the session's Node where the harness adapter
//! declares it reads skills. Only the ones switched on for the Bot are given ([`super`]).

use rusqlite::{Connection, OptionalExtension, Row, params};
use serde::{Deserialize, Serialize};

use crate::Result;

pub const RETENTION_MS: i64 = 30 * 24 * 60 * 60 * 1000;

#[derive(Debug, Clone, Copy, Serialize)]
pub struct History {
    pub head_version: i64,
    pub published_version: Option<i64>,
    pub publication: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Version {
    pub version: i64,
    pub name: String,
    pub description: String,
    pub content: String,
    pub saved_at: i64,
    pub saved_by: Option<String>,
    pub note: String,
    pub first_published_at: Option<i64>,
}

fn version_row(r: &Row) -> rusqlite::Result<Version> {
    Ok(Version {
        version: r.get(0)?,
        name: r.get(1)?,
        description: r.get(2)?,
        content: r.get(3)?,
        saved_at: r.get(4)?,
        saved_by: r.get(5)?,
        note: r.get(6)?,
        first_published_at: r.get(7)?,
    })
}

const VERSION_COLS: &str = "v.version, v.name, v.description, v.content, v.saved_at, v.saved_by, v.note, v.first_published_at";

pub fn history(c: &Connection, id: &str) -> Result<History> {
    Ok(c.query_row(
        "SELECT head_version, published_version, publication FROM skills WHERE id = ?",
        [id],
        |r| {
            Ok(History {
                head_version: r.get(0)?,
                published_version: r.get(1)?,
                publication: r.get(2)?,
            })
        },
    )?)
}

pub fn version(c: &Connection, id: &str, n: i64) -> Result<Option<Version>> {
    Ok(c.query_row(
        &format!("SELECT {VERSION_COLS} FROM skill_versions v WHERE v.skill_id=? AND v.version=?"),
        params![id, n],
        version_row,
    )
    .optional()?)
}

#[derive(Debug, Serialize)]
pub struct VersionSummary {
    version: i64,
    name: String,
    saved_at: i64,
    saved_by: Option<String>,
    note: String,
    first_published_at: Option<i64>,
}

pub fn version_summaries(c: &Connection, id: &str) -> Result<Vec<VersionSummary>> {
    let mut st = c.prepare_cached(
        "SELECT version, name, saved_at, saved_by, note, first_published_at FROM skill_versions
          WHERE skill_id=? ORDER BY version DESC",
    )?;
    Ok(st
        .query_map([id], |r| {
            Ok(VersionSummary {
                version: r.get(0)?,
                name: r.get(1)?,
                saved_at: r.get(2)?,
                saved_by: r.get(3)?,
                note: r.get(4)?,
                first_published_at: r.get(5)?,
            })
        })?
        .collect::<rusqlite::Result<_>>()?)
}

pub fn published(c: &Connection, id: &str) -> Result<Option<Version>> {
    Ok(c.query_row(
        &format!(
            "SELECT {VERSION_COLS} FROM skills s JOIN skill_versions v
                  ON v.skill_id=s.id AND v.version=s.published_version WHERE s.id=?"
        ),
        [id],
        version_row,
    )
    .optional()?)
}

pub fn save_version(c: &Connection, s: &Skill, n: i64, by: &str, note: &str) -> Result<()> {
    c.execute(
        "INSERT INTO skill_versions
             (skill_id, version, name, description, content, saved_at, saved_by, note)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
        params![
            s.id,
            n,
            s.name,
            s.description,
            s.content,
            s.updated,
            by,
            note
        ],
    )?;
    c.execute(
        "UPDATE skills SET head_version=? WHERE id=?",
        params![n, s.id],
    )?;
    Ok(())
}

pub fn publish(c: &Connection, id: &str, n: i64, at: i64) -> Result<()> {
    c.execute(
        "UPDATE skill_versions SET first_published_at=COALESCE(first_published_at, ?)
               WHERE skill_id=? AND version=?",
        params![at, id, n],
    )?;
    c.execute(
        "UPDATE skills SET published_version=?, publication=publication+1 WHERE id=?",
        params![n, id],
    )?;
    Ok(())
}

/// Keep the current publication, the three latest *other* versions, and versions on the cutoff.
pub fn prune(c: &Connection, now: i64) -> Result<usize> {
    Ok(c.execute(
        "DELETE FROM skill_versions WHERE (skill_id, version) IN (
           SELECT skill_id, version FROM (
             SELECT v.skill_id, v.version, v.saved_at,
                    row_number() OVER (PARTITION BY v.skill_id ORDER BY v.version DESC) AS pos
               FROM skill_versions v JOIN skills s ON s.id=v.skill_id
              WHERE s.published_version IS NOT v.version
           ) WHERE pos > 3 AND saved_at < ?
         )",
        [now.saturating_sub(RETENTION_MS)],
    )?)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PublishedSkill {
    pub id: String,
    pub publication: i64,
    pub version: Version,
}

/// Execution scope is the session's workspace, not every workspace the Bot serves.
pub fn published_for_bot(c: &Connection, ws: &str, bot: &str) -> Result<Vec<PublishedSkill>> {
    let mut st = c.prepare_cached(&format!(
        "SELECT {VERSION_COLS}, s.id, s.publication FROM bot_capabilities a JOIN skills s ON s.id=a.entry_id
          JOIN skill_versions v ON v.skill_id=s.id AND v.version=s.published_version
          JOIN bots b ON b.id=a.bot_id AND b.deleted=0
         WHERE a.kind='skill' AND a.enabled=1 AND s.workspace_id=?1 AND a.bot_id=?2
           AND (b.workspace_id=?1 OR EXISTS (
             SELECT 1 FROM bot_workspaces w WHERE w.workspace_id=?1 AND w.bot_id=?2))
         ORDER BY s.id"
    ))?;
    Ok(st
        .query_map(params![ws, bot], |r| {
            Ok(PublishedSkill {
                id: r.get(8)?,
                publication: r.get(9)?,
                version: version_row(r)?,
            })
        })?
        .collect::<rusqlite::Result<_>>()?)
}

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
        "SELECT COUNT(*) FROM skills s WHERE s.workspace_id = ?1 AND s.id <> ?3
          AND (s.name = ?2 OR EXISTS (
            SELECT 1 FROM skill_versions v WHERE v.skill_id=s.id
               AND v.version=s.published_version AND v.name=?2))",
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

/// The live versions for a new session, with no access outside its workspace.
pub fn enabled_for_session(c: &Connection, session: &str) -> Result<Vec<PublishedSkill>> {
    let binding: Option<(String, Option<String>)> = c
        .query_row(
            "SELECT workspace_id, bot_id FROM sessions WHERE id=?",
            [session],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    match binding {
        Some((ws, Some(bot))) => published_for_bot(c, &ws, &bot),
        _ => Ok(vec![]),
    }
}
