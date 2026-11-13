//! A Bot's own MCP servers (migration 62, `bot_mcp_servers`): what its harness gets in the ACP
//! `mcpServers` of every session, next to Steve's own tools.
//!
//! A row never holds a secret value. An env variable or header marked secret is stored by name only
//! (`{"name","secret":true}`); its value is a field of the server's `credential_item` in the
//! workspace's system vault, labelled [`secret_label`], and is taken by the session's Node when the
//! harness opens. [`insert`] and [`update`] drop any value given with a secret.

use rusqlite::{Connection, OptionalExtension, Row, params};
use serde::{Deserialize, Serialize};

use crate::Result;

/// The transports of ACP's `McpServer`.
pub const TRANSPORTS: &[&str] = &["stdio", "http", "sse"];

/// One env variable (stdio) or HTTP header (http, sse).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Var {
    pub name: String,
    /// The plain value; `None` for a secret.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value: Option<String>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub secret: bool,
}

/// Where a variable goes: `env` for stdio, `header` for http and sse.
pub fn var_kind(transport: &str) -> &'static str {
    if transport == "stdio" {
        "env"
    } else {
        "header"
    }
}

/// The vault field label of a secret variable (`env:API_KEY`, `header:Authorization`).
pub fn secret_label(transport: &str, name: &str) -> String {
    format!("{}:{name}", var_kind(transport))
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Server {
    pub id: String,
    pub workspace_id: String,
    pub bot_id: String,
    pub name: String,
    pub transport: String,
    pub command: Option<String>,
    pub args: Vec<String>,
    pub env: Vec<Var>,
    pub url: Option<String>,
    pub headers: Vec<Var>,
    pub credential_item: Option<String>,
    pub enabled: bool,
    pub created: i64,
    pub updated: i64,
}

impl Server {
    /// Its variables: env for stdio, headers otherwise.
    pub fn vars(&self) -> &[Var] {
        if self.transport == "stdio" {
            &self.env
        } else {
            &self.headers
        }
    }

    /// The vault labels of its secret variables, in order.
    pub fn secret_labels(&self) -> Vec<String> {
        self.vars()
            .iter()
            .filter(|v| v.secret)
            .map(|v| secret_label(&self.transport, &v.name))
            .collect()
    }
}

const COLS: &str = "id, workspace_id, bot_id, name, transport, command, args, env, url, headers, credential_item, enabled, created, updated";

fn json_list<T: for<'de> Deserialize<'de>>(s: String) -> Vec<T> {
    serde_json::from_str(&s).unwrap_or_default()
}

fn row(r: &Row) -> rusqlite::Result<Server> {
    Ok(Server {
        id: r.get(0)?,
        workspace_id: r.get(1)?,
        bot_id: r.get(2)?,
        name: r.get(3)?,
        transport: r.get(4)?,
        command: r.get(5)?,
        args: json_list(r.get(6)?),
        env: json_list(r.get(7)?),
        url: r.get(8)?,
        headers: json_list(r.get(9)?),
        credential_item: r.get(10)?,
        enabled: r.get::<_, i64>(11)? != 0,
        created: r.get(12)?,
        updated: r.get(13)?,
    })
}

/// A secret's value never reaches the row.
fn stored(vars: &[Var]) -> Result<String> {
    let clean: Vec<Var> = vars
        .iter()
        .map(|v| Var {
            name: v.name.clone(),
            value: if v.secret { None } else { v.value.clone() },
            secret: v.secret,
        })
        .collect();
    Ok(serde_json::to_string(&clean)?)
}

/// A Bot's servers, by name.
pub fn list(c: &Connection, bot_id: &str) -> Result<Vec<Server>> {
    let mut st = c.prepare_cached(&format!(
        "SELECT {COLS} FROM bot_mcp_servers WHERE bot_id = ? ORDER BY name"
    ))?;
    let v = st.query_map([bot_id], row)?.collect::<Result<_, _>>()?;
    Ok(v)
}

pub fn get(c: &Connection, id: &str) -> Result<Option<Server>> {
    Ok(c.query_row(
        &format!("SELECT {COLS} FROM bot_mcp_servers WHERE id = ?"),
        [id],
        row,
    )
    .optional()?)
}

/// Another server of `bot_id` named `name` (any case), other than `except`.
pub fn name_taken(c: &Connection, bot_id: &str, name: &str, except: Option<&str>) -> Result<bool> {
    let n: i64 = c.query_row(
        "SELECT COUNT(*) FROM bot_mcp_servers WHERE bot_id = ?1 AND lower(name) = lower(?2) AND id <> ?3",
        params![bot_id, name, except.unwrap_or("")],
        |r| r.get(0),
    )?;
    Ok(n > 0)
}

pub fn insert(c: &Connection, s: &Server) -> Result<()> {
    c.execute(
        &format!(
            "INSERT INTO bot_mcp_servers ({COLS}) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)"
        ),
        params![
            s.id,
            s.workspace_id,
            s.bot_id,
            s.name,
            s.transport,
            s.command,
            serde_json::to_string(&s.args)?,
            stored(&s.env)?,
            s.url,
            stored(&s.headers)?,
            s.credential_item,
            s.enabled as i64,
            s.created,
            s.updated,
        ],
    )?;
    Ok(())
}

/// Writes every field but `id`, `workspace_id`, `bot_id` and `created`.
pub fn update(c: &Connection, s: &Server) -> Result<()> {
    c.execute(
        "UPDATE bot_mcp_servers SET name = ?2, transport = ?3, command = ?4, args = ?5, env = ?6, url = ?7,
            headers = ?8, credential_item = ?9, enabled = ?10, updated = ?11 WHERE id = ?1",
        params![
            s.id,
            s.name,
            s.transport,
            s.command,
            serde_json::to_string(&s.args)?,
            stored(&s.env)?,
            s.url,
            stored(&s.headers)?,
            s.credential_item,
            s.enabled as i64,
            s.updated,
        ],
    )?;
    Ok(())
}

pub fn set_enabled(c: &Connection, id: &str, enabled: bool, now: i64) -> Result<bool> {
    Ok(c.execute(
        "UPDATE bot_mcp_servers SET enabled = ?2, updated = ?3 WHERE id = ?1",
        params![id, enabled as i64, now],
    )? > 0)
}

pub fn delete(c: &Connection, id: &str) -> Result<bool> {
    Ok(c.execute("DELETE FROM bot_mcp_servers WHERE id = ?", [id])? > 0)
}

/// The enabled servers of the Bot a session works as (none for a session without a Bot).
pub fn enabled_for_session(c: &Connection, session: &str) -> Result<Vec<Server>> {
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
