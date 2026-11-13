//! The workspace's MCP servers (migration 65, `mcp_servers`): what the harness of a Bot assigned one
//! gets in the ACP `mcpServers` of every session, next to Steve's own tools ([`super`]).
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
    pub name: String,
    pub transport: String,
    pub command: Option<String>,
    pub args: Vec<String>,
    pub env: Vec<Var>,
    pub url: Option<String>,
    pub headers: Vec<Var>,
    pub credential_item: Option<String>,
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

const COLS: &str = "id, workspace_id, name, transport, command, args, env, url, headers, credential_item, created, updated";

fn json_list<T: for<'de> Deserialize<'de>>(s: String) -> Vec<T> {
    serde_json::from_str(&s).unwrap_or_default()
}

fn row(r: &Row) -> rusqlite::Result<Server> {
    Ok(Server {
        id: r.get(0)?,
        workspace_id: r.get(1)?,
        name: r.get(2)?,
        transport: r.get(3)?,
        command: r.get(4)?,
        args: json_list(r.get(5)?),
        env: json_list(r.get(6)?),
        url: r.get(7)?,
        headers: json_list(r.get(8)?),
        credential_item: r.get(9)?,
        created: r.get(10)?,
        updated: r.get(11)?,
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

/// A workspace's servers, by name.
pub fn list(c: &Connection, ws: &str) -> Result<Vec<Server>> {
    let mut st = c.prepare_cached(&format!(
        "SELECT {COLS} FROM mcp_servers WHERE workspace_id = ? ORDER BY lower(name)"
    ))?;
    let v = st.query_map([ws], row)?.collect::<Result<_, _>>()?;
    Ok(v)
}

pub fn get(c: &Connection, id: &str) -> Result<Option<Server>> {
    Ok(c.query_row(
        &format!("SELECT {COLS} FROM mcp_servers WHERE id = ?"),
        [id],
        row,
    )
    .optional()?)
}

/// Another server of `ws` named `name` (any case), other than `except`.
pub fn name_taken(c: &Connection, ws: &str, name: &str, except: Option<&str>) -> Result<bool> {
    let n: i64 = c.query_row(
        "SELECT COUNT(*) FROM mcp_servers WHERE workspace_id = ?1 AND lower(name) = lower(?2) AND id <> ?3",
        params![ws, name, except.unwrap_or("")],
        |r| r.get(0),
    )?;
    Ok(n > 0)
}

pub fn insert(c: &Connection, s: &Server) -> Result<()> {
    c.execute(
        &format!(
            "INSERT INTO mcp_servers ({COLS}) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)"
        ),
        params![
            s.id,
            s.workspace_id,
            s.name,
            s.transport,
            s.command,
            serde_json::to_string(&s.args)?,
            stored(&s.env)?,
            s.url,
            stored(&s.headers)?,
            s.credential_item,
            s.created,
            s.updated,
        ],
    )?;
    Ok(())
}

/// Writes every field but `id`, `workspace_id` and `created`.
pub fn update(c: &Connection, s: &Server) -> Result<()> {
    c.execute(
        "UPDATE mcp_servers SET name = ?2, transport = ?3, command = ?4, args = ?5, env = ?6, url = ?7,
            headers = ?8, credential_item = ?9, updated = ?10 WHERE id = ?1",
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
            s.updated,
        ],
    )?;
    Ok(())
}

/// The server and who used it go.
pub fn delete(c: &Connection, id: &str) -> Result<bool> {
    super::remove_entry(c, "mcp", id)?;
    Ok(c.execute("DELETE FROM mcp_servers WHERE id = ?", [id])? > 0)
}

/// The servers `bot` is assigned, each with whether it is switched on for it, by name.
pub fn of_bot(c: &Connection, bot: &str) -> Result<Vec<(Server, bool)>> {
    let mut out = vec![];
    for a in super::of_bot(c, bot, "mcp")? {
        if let Some(s) = get(c, &a.entry_id)? {
            out.push((s, a.enabled));
        }
    }
    out.sort_by_key(|(s, _)| s.name.to_lowercase());
    Ok(out)
}

/// The servers the Bot a session works as has switched on (none for a session without a Bot).
pub fn enabled_for_session(c: &Connection, session: &str) -> Result<Vec<Server>> {
    match super::bot_of_session(c, session)? {
        Some(b) => Ok(of_bot(c, &b)?
            .into_iter()
            .filter(|(_, on)| *on)
            .map(|(s, _)| s)
            .collect()),
        None => Ok(vec![]),
    }
}
