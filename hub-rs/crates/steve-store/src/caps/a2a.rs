//! The workspace's remote A2A agents (migration 65, `a2a_agents`): each added by its Agent Card URL,
//! the card kept as last fetched. A Bot assigned one gets a Steve tool to send it a task
//! (`steve_a2a_<name>`). The secret its requests carry is never here: it is the field `auth` of the
//! agent's `credential_item` in the workspace's system vault.

use rusqlite::{Connection, OptionalExtension, Row, params};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::Result;

/// How a request to the agent is authenticated.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(tag = "scheme", rename_all = "lowercase")]
pub enum Auth {
    #[default]
    None,
    /// `Authorization: Bearer <secret>`
    Bearer,
    /// `<header>: <secret>`
    Header { header: String },
}

impl Auth {
    pub fn needs_secret(&self) -> bool {
        !matches!(self, Auth::None)
    }
}

/// The vault field label of the secret.
pub const SECRET_LABEL: &str = "auth";

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Agent {
    pub id: String,
    pub workspace_id: String,
    pub name: String,
    pub card_url: String,
    pub card: Value,
    pub auth: Auth,
    pub credential_item: Option<String>,
    pub fetched: i64,
    pub created: i64,
    pub updated: i64,
}

impl Agent {
    /// The card's `name`, else the key.
    pub fn title(&self) -> String {
        self.card["name"]
            .as_str()
            .filter(|s| !s.trim().is_empty())
            .map(str::to_string)
            .unwrap_or_else(|| self.name.clone())
    }
    pub fn description(&self) -> String {
        self.card["description"].as_str().unwrap_or("").to_string()
    }
    /// Where its JSON-RPC requests go: the card's `url`.
    pub fn endpoint(&self) -> Option<String> {
        self.card["url"]
            .as_str()
            .map(str::to_string)
            .filter(|s| !s.is_empty())
    }
}

const COLS: &str =
    "id, workspace_id, name, card_url, card, auth, credential_item, fetched, created, updated";

fn row(r: &Row) -> rusqlite::Result<Agent> {
    Ok(Agent {
        id: r.get(0)?,
        workspace_id: r.get(1)?,
        name: r.get(2)?,
        card_url: r.get(3)?,
        card: serde_json::from_str(&r.get::<_, String>(4)?).unwrap_or(Value::Null),
        auth: serde_json::from_str(&r.get::<_, String>(5)?).unwrap_or_default(),
        credential_item: r.get(6)?,
        fetched: r.get(7)?,
        created: r.get(8)?,
        updated: r.get(9)?,
    })
}

pub fn list(c: &Connection, ws: &str) -> Result<Vec<Agent>> {
    let mut st = c.prepare_cached(&format!(
        "SELECT {COLS} FROM a2a_agents WHERE workspace_id = ? ORDER BY name"
    ))?;
    let v = st.query_map([ws], row)?.collect::<Result<_, _>>()?;
    Ok(v)
}

pub fn get(c: &Connection, id: &str) -> Result<Option<Agent>> {
    Ok(c.query_row(
        &format!("SELECT {COLS} FROM a2a_agents WHERE id = ?"),
        [id],
        row,
    )
    .optional()?)
}

pub fn name_taken(c: &Connection, ws: &str, name: &str, except: Option<&str>) -> Result<bool> {
    let n: i64 = c.query_row(
        "SELECT COUNT(*) FROM a2a_agents WHERE workspace_id = ?1 AND name = ?2 AND id <> ?3",
        params![ws, name, except.unwrap_or("")],
        |r| r.get(0),
    )?;
    Ok(n > 0)
}

pub fn insert(c: &Connection, a: &Agent) -> Result<()> {
    c.execute(
        &format!(
            "INSERT INTO a2a_agents ({COLS}) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)"
        ),
        params![
            a.id,
            a.workspace_id,
            a.name,
            a.card_url,
            a.card.to_string(),
            serde_json::to_string(&a.auth)?,
            a.credential_item,
            a.fetched,
            a.created,
            a.updated,
        ],
    )?;
    Ok(())
}

/// Writes every field but `id`, `workspace_id` and `created`.
pub fn update(c: &Connection, a: &Agent) -> Result<()> {
    c.execute(
        "UPDATE a2a_agents SET name = ?2, card_url = ?3, card = ?4, auth = ?5, credential_item = ?6,
            fetched = ?7, updated = ?8 WHERE id = ?1",
        params![
            a.id,
            a.name,
            a.card_url,
            a.card.to_string(),
            serde_json::to_string(&a.auth)?,
            a.credential_item,
            a.fetched,
            a.updated,
        ],
    )?;
    Ok(())
}

pub fn delete(c: &Connection, id: &str) -> Result<bool> {
    super::remove_entry(c, "a2a", id)?;
    Ok(c.execute("DELETE FROM a2a_agents WHERE id = ?", [id])? > 0)
}

/// The agents `bot` is assigned, each with whether it is switched on for it, by name.
pub fn of_bot(c: &Connection, bot: &str) -> Result<Vec<(Agent, bool)>> {
    let mut out = vec![];
    for a in super::of_bot(c, bot, "a2a")? {
        if let Some(x) = get(c, &a.entry_id)? {
            out.push((x, a.enabled));
        }
    }
    out.sort_by(|(a, _), (b, _)| a.name.cmp(&b.name));
    Ok(out)
}

/// The agents the Bot a session works as has switched on.
pub fn enabled_for_session(c: &Connection, session: &str) -> Result<Vec<Agent>> {
    match super::bot_of_session(c, session)? {
        Some(b) => Ok(of_bot(c, &b)?
            .into_iter()
            .filter(|(_, on)| *on)
            .map(|(a, _)| a)
            .collect()),
        None => Ok(vec![]),
    }
}
