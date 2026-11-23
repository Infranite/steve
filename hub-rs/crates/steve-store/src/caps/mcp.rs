//! The workspace's MCP servers (migration 65, `mcp_servers`), assigned to Bots and proxied by the
//! Hub's shared MCP client. Sessions reach their tools through Steve's endpoint ([`super`]).
//!
//! A row never holds a secret value. An env variable or header marked secret is stored by name only
//! (`{"name","secret":true}`); its value is a field of the server's `credential_item` in the
//! workspace's system vault, labelled [`secret_label`], and is taken by the Hub when its client
//! opens the shared connection. [`Servers::insert`] and [`Servers::update`] drop any value given with a secret.

use crate::contract::Row;
use sea_query::{Expr, ExprTrait, Func, Order, Query, SelectStatement};
use serde::{Deserialize, Serialize};

use super::Assignments;
use crate::{Read, Result, W};

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

pub struct Servers<'a, C>(pub &'a mut C);

const COLS: [&str; 12] = [
    "id",
    "workspace_id",
    "name",
    "transport",
    "command",
    "args",
    "env",
    "url",
    "headers",
    "credential_item",
    "created",
    "updated",
];

fn json_list<T: for<'de> Deserialize<'de>>(s: String) -> Vec<T> {
    serde_json::from_str(&s).unwrap_or_default()
}

fn row(r: &Row<'_>) -> Result<Server> {
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

fn rows() -> SelectStatement {
    Query::select().from("mcp_servers").columns(COLS).take()
}

impl<C: Read> Servers<'_, C> {
    pub fn list(&mut self, ws: &str) -> Result<Vec<Server>> {
        self.0.all(
            rows()
                .and_where(Expr::col("workspace_id").eq(ws))
                .order_by_expr(Func::lower(Expr::col("name")).into(), Order::Asc),
            row,
        )
    }

    pub fn get(&mut self, id: &str) -> Result<Option<Server>> {
        self.0.one(rows().and_where(Expr::col("id").eq(id)), row)
    }

    pub fn name_taken(&mut self, ws: &str, name: &str, except: Option<&str>) -> Result<bool> {
        let by_name = Func::lower(Expr::col("name")).eq(Func::lower(Expr::value(name)));
        Ok(self
            .0
            .one(
                Query::select()
                    .from("mcp_servers")
                    .column("id")
                    .and_where(Expr::col("workspace_id").eq(ws))
                    .and_where(by_name)
                    .and_where(Expr::col("id").ne(except.unwrap_or_default()))
                    .limit(1),
                |_| Ok(()),
            )?
            .is_some())
    }

    pub fn of_bot(&mut self, bot: &str) -> Result<Vec<(Server, bool)>> {
        super::assigned(
            self.0,
            bot,
            "mcp",
            |r, id| Servers(r).get(id),
            |row| row.name.to_lowercase(),
        )
    }

    pub fn enabled_for_session(&mut self, session: &str) -> Result<Vec<Server>> {
        let Some(bot) = Assignments(self.0).bot_of_session(session)? else {
            return Ok(Vec::new());
        };
        Ok(self
            .of_bot(&bot)?
            .into_iter()
            .filter_map(|(row, on)| on.then_some(row))
            .collect())
    }
}

impl Servers<'_, W<'_>> {
    pub fn insert(&mut self, s: &Server) -> Result<()> {
        self.0.exec(
            Query::insert()
                .into_table("mcp_servers")
                .columns(COLS)
                .values_panic([
                    s.id.as_str().into(),
                    s.workspace_id.as_str().into(),
                    s.name.as_str().into(),
                    s.transport.as_str().into(),
                    s.command.as_deref().into(),
                    serde_json::to_string(&s.args)?.into(),
                    stored(&s.env)?.into(),
                    s.url.as_deref().into(),
                    stored(&s.headers)?.into(),
                    s.credential_item.as_deref().into(),
                    s.created.into(),
                    s.updated.into(),
                ]),
        )?;
        Ok(())
    }

    pub fn update(&mut self, s: &Server) -> Result<()> {
        self.0.exec(
            Query::update()
                .table("mcp_servers")
                .and_where(Expr::col("id").eq(s.id.as_str()))
                .value("name", s.name.as_str())
                .value("transport", s.transport.as_str())
                .value("command", s.command.as_deref())
                .value("args", serde_json::to_string(&s.args)?)
                .value("env", stored(&s.env)?)
                .value("url", s.url.as_deref())
                .value("headers", stored(&s.headers)?)
                .value("credential_item", s.credential_item.as_deref())
                .value("updated", s.updated),
        )?;
        Ok(())
    }

    pub fn delete(&mut self, id: &str) -> Result<bool> {
        Assignments(self.0).remove_entry("mcp", id)?;
        Ok(self.0.exec(
            Query::delete()
                .from_table("mcp_servers")
                .and_where(Expr::col("id").eq(id)),
        )? != 0)
    }
}
