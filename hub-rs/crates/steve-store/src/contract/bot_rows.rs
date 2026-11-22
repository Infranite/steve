use sea_query::{Expr, ExprTrait, JoinType, Order, Query, SelectStatement};

use crate::{Read, Result};

pub struct BotRows<'a, C>(pub &'a mut C);

pub struct BotRef {
    pub id: String,
    pub name: String,
    pub role: String,
    pub main: bool,
    pub enabled: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct BotPrompt {
    pub id: String,
    pub name: String,
    pub role: String,
    pub rules: String,
}

fn decode_prompt(row: &super::Row<'_>) -> Result<BotPrompt> {
    Ok(BotPrompt {
        id: row.get(0)?,
        name: row.get(1)?,
        role: row.get(2)?,
        rules: row.get(3)?,
    })
}

fn prompt_query(ws: &str) -> SelectStatement {
    local(ws)
        .columns([("b", "id"), ("b", "name"), ("b", "role"), ("b", "rules")])
        .take()
}

fn local(ws: &str) -> SelectStatement {
    Query::select()
        .from_as("bots", "b")
        .join_as(
            JoinType::InnerJoin,
            "bot_workspaces",
            "w",
            Expr::col(("w", "bot_id")).equals(("b", "id")),
        )
        .and_where(Expr::col(("w", "workspace_id")).eq(ws))
        .and_where(Expr::col(("b", "deleted")).eq(0_i64))
        .take()
}

impl<C: Read> BotRows<'_, C> {
    pub fn enabled_prompt(&mut self, ws: &str, id: &str) -> Result<Option<BotPrompt>> {
        self.0.one(
            prompt_query(ws)
                .and_where(Expr::col(("b", "id")).eq(id))
                .and_where(Expr::col(("b", "enabled")).eq(1_i64)),
            decode_prompt,
        )
    }

    pub fn main_prompt(&mut self, ws: &str) -> Result<Option<BotPrompt>> {
        self.0.one(
            prompt_query(ws)
                .and_where(Expr::col(("b", "main")).eq(1_i64))
                .order_by(("b", "updated"), Order::Asc)
                .limit(1),
            decode_prompt,
        )
    }

    pub fn resolve_name(&mut self, ws: &str, name: &str) -> Result<Option<String>> {
        if self.local(ws, name)?.is_some() {
            return Ok(Some(name.to_owned()));
        }
        self.0.one(
            local(ws)
                .column(("b", "id"))
                .and_where(Expr::col(("b", "name")).eq(name))
                .limit(1),
            |r| r.get(0),
        )
    }

    pub fn revision(&mut self, id: &str) -> Result<Option<i64>> {
        self.0.one(
            Query::select()
                .column("revision")
                .from("bots")
                .and_where(Expr::col("id").eq(id))
                .and_where(Expr::col("deleted").eq(0_i64)),
            |row| row.get(0),
        )
    }

    pub fn name(&mut self, id: &str) -> Result<Option<String>> {
        self.0.one(
            Query::select()
                .column("name")
                .from("bots")
                .and_where(Expr::col("id").eq(id)),
            |row| row.get(0),
        )
    }

    pub fn local(&mut self, ws: &str, id: &str) -> Result<Option<(String, bool)>> {
        self.0.one(
            local(ws)
                .columns([("b", "name"), ("b", "enabled")])
                .and_where(Expr::col(("b", "id")).eq(id)),
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
    }

    pub fn main(&mut self) -> Result<Option<String>> {
        self.0.one(
            Query::select()
                .column("id")
                .from("bots")
                .and_where(Expr::col("main").eq(1_i64))
                .and_where(Expr::col("deleted").eq(0_i64))
                .order_by("updated", Order::Asc)
                .limit(1),
            |row| row.get(0),
        )
    }

    pub fn workspace(&mut self, ws: &str) -> Result<Vec<BotRef>> {
        self.0.all(
            local(ws)
                .columns([
                    ("b", "id"),
                    ("b", "name"),
                    ("b", "role"),
                    ("b", "main"),
                    ("b", "enabled"),
                ])
                .order_by_columns([(("b", "main"), Order::Desc), (("b", "name"), Order::Asc)]),
            |row| {
                Ok(BotRef {
                    id: row.get(0)?,
                    name: row.get(1)?,
                    role: row.get(2)?,
                    main: row.get(3)?,
                    enabled: row.get(4)?,
                })
            },
        )
    }
}
