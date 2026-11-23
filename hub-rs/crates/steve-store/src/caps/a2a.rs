//! The workspace's remote A2A agents (migration 65, `a2a_agents`): each added by its Agent Card URL,
//! the card kept as last fetched. A Bot assigned one gets a Steve tool to send it a task
//! (`steve_a2a_<name>`). The secret its requests carry is never here: it is the field `auth` of the
//! agent's `credential_item` in the workspace's system vault.

use crate::contract::Row;
use sea_query::{Expr, ExprTrait, Order, Query, SelectStatement};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::Assignments;
use crate::{Read, Result, W};

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

pub struct Agents<'a, C>(pub &'a mut C);

const COLS: [&str; 10] = [
    "id",
    "workspace_id",
    "name",
    "card_url",
    "card",
    "auth",
    "credential_item",
    "fetched",
    "created",
    "updated",
];

fn row(r: &Row<'_>) -> Result<Agent> {
    Ok(Agent {
        id: r.get(0)?,
        workspace_id: r.get(1)?,
        name: r.get(2)?,
        card_url: r.get(3)?,
        card: serde_json::from_str(&r.get::<String>(4)?).unwrap_or(Value::Null),
        auth: serde_json::from_str(&r.get::<String>(5)?).unwrap_or_default(),
        credential_item: r.get(6)?,
        fetched: r.get(7)?,
        created: r.get(8)?,
        updated: r.get(9)?,
    })
}

fn rows() -> SelectStatement {
    Query::select().from("a2a_agents").columns(COLS).take()
}

impl<C: Read> Agents<'_, C> {
    pub fn list(&mut self, ws: &str) -> Result<Vec<Agent>> {
        self.0.all(
            rows()
                .and_where(Expr::col("workspace_id").eq(ws))
                .order_by("name", Order::Asc),
            row,
        )
    }

    pub fn get(&mut self, id: &str) -> Result<Option<Agent>> {
        self.0.one(rows().and_where(Expr::col("id").eq(id)), row)
    }

    pub fn name_taken(&mut self, ws: &str, name: &str, except: Option<&str>) -> Result<bool> {
        let by_name = Expr::col("name").eq(name);
        Ok(self
            .0
            .one(
                Query::select()
                    .from("a2a_agents")
                    .column("id")
                    .and_where(Expr::col("workspace_id").eq(ws))
                    .and_where(by_name)
                    .and_where(Expr::col("id").ne(except.unwrap_or_default()))
                    .limit(1),
                |_| Ok(()),
            )?
            .is_some())
    }

    pub fn of_bot(&mut self, bot: &str) -> Result<Vec<(Agent, bool)>> {
        super::assigned(
            self.0,
            bot,
            "a2a",
            |r, id| Agents(r).get(id),
            |row| row.name.clone(),
        )
    }

    pub fn enabled_for_session(&mut self, session: &str) -> Result<Vec<Agent>> {
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

impl Agents<'_, W<'_>> {
    pub fn insert(&mut self, a: &Agent) -> Result<()> {
        self.0.exec(
            Query::insert()
                .into_table("a2a_agents")
                .columns(COLS)
                .values_panic([
                    a.id.as_str().into(),
                    a.workspace_id.as_str().into(),
                    a.name.as_str().into(),
                    a.card_url.as_str().into(),
                    a.card.to_string().into(),
                    serde_json::to_string(&a.auth)?.into(),
                    a.credential_item.as_deref().into(),
                    a.fetched.into(),
                    a.created.into(),
                    a.updated.into(),
                ]),
        )?;
        Ok(())
    }

    pub fn update(&mut self, a: &Agent) -> Result<()> {
        self.0.exec(
            Query::update()
                .table("a2a_agents")
                .and_where(Expr::col("id").eq(a.id.as_str()))
                .value("name", a.name.as_str())
                .value("card_url", a.card_url.as_str())
                .value("card", a.card.to_string())
                .value("auth", serde_json::to_string(&a.auth)?)
                .value("credential_item", a.credential_item.as_deref())
                .value("fetched", a.fetched)
                .value("updated", a.updated),
        )?;
        Ok(())
    }

    pub fn delete(&mut self, id: &str) -> Result<bool> {
        Assignments(self.0).remove_entry("a2a", id)?;
        Ok(self.0.exec(
            Query::delete()
                .from_table("a2a_agents")
                .and_where(Expr::col("id").eq(id)),
        )? != 0)
    }
}
