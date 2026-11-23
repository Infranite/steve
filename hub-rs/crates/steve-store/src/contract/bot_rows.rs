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

#[derive(Debug, Clone, PartialEq, Default)]
pub struct ReplyBot {
    pub id: String,
    pub name: String,
    pub role: String,
    pub persona: String,
    pub rules: String,
    pub prompt: String,
    pub model: Option<String>,
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
    pub fn reply_profile(&mut self, ws: &str, id: Option<&str>) -> Result<Option<ReplyBot>> {
        let mut query = local(ws);
        query
            .columns([
                ("b", "id"),
                ("b", "name"),
                ("b", "role"),
                ("b", "persona"),
                ("b", "rules"),
                ("b", "prompt"),
                ("b", "model"),
            ])
            .and_where(Expr::col(("b", "enabled")).eq(1_i64))
            .limit(1);
        match id {
            Some(id) => query.and_where(Expr::col(("b", "id")).eq(id)),
            None => query
                .and_where(Expr::col(("b", "main")).eq(1_i64))
                .order_by(("b", "updated"), Order::Asc),
        };
        self.0.one(&query, |row| {
            Ok(ReplyBot {
                id: row.get(0)?,
                name: row.get(1)?,
                role: row.get(2)?,
                persona: row.get(3)?,
                rules: row.get(4)?,
                prompt: row.get::<Option<String>>(5)?.unwrap_or_default(),
                model: row
                    .get::<Option<String>>(6)?
                    .filter(|model| !model.is_empty()),
            })
        })
    }

    pub fn persona_seed(&mut self, ws: &str) -> Result<Option<(String, String)>> {
        self.0.one(
            local(ws)
                .columns([("b", "id"), ("b", "persona")])
                .order_by_expr(Expr::col(("b", "id")).eq("steve"), Order::Desc)
                .order_by_columns([
                    (("b", "main"), Order::Desc),
                    (("w", "created"), Order::Asc),
                    (("b", "id"), Order::Asc),
                ])
                .limit(1),
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
    }

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
        self.first_named(ws, name)
    }

    pub fn first_named(&mut self, ws: &str, name: &str) -> Result<Option<String>> {
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
                .order_by_columns([
                    (("b", "main"), Order::Desc),
                    (("b", "name"), Order::Asc),
                    (("b", "id"), Order::Asc),
                ]),
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

impl<C: Read> BotRows<'_, C> {
    pub fn owned_in(&mut self, ws: &str) -> Result<Vec<BotRef>> {
        let memberships = Query::select()
            .from("bot_workspaces")
            .column("bot_id")
            .and_where(Expr::col("workspace_id").eq(ws))
            .take();
        self.0.all(
            Query::select()
                .from("bots")
                .columns(["id", "name", "role", "main", "enabled"])
                .and_where(Expr::col("deleted").eq(0_i64))
                .and_where(
                    Expr::col("workspace_id")
                        .eq(ws)
                        .or(Expr::col("id").in_subquery(memberships)),
                )
                .order_by("main", Order::Desc)
                .order_by("name", Order::Asc),
            |r| {
                Ok(BotRef {
                    id: r.get(0)?,
                    name: r.get(1)?,
                    role: r.get(2)?,
                    main: r.get(3)?,
                    enabled: r.get(4)?,
                })
            },
        )
    }

    pub fn home(&mut self, bot: &str) -> Result<Option<String>> {
        self.0.one(
            Query::select()
                .from("bots")
                .column("workspace_id")
                .and_where(Expr::col("id").eq(bot)),
            |row| row.get(0),
        )
    }

    pub fn visible_in(&mut self, ws: &str, bot: &str) -> Result<bool> {
        let Some(home) = self.0.one(
            Query::select()
                .from("bots")
                .column("workspace_id")
                .and_where(Expr::col("id").eq(bot))
                .and_where(Expr::col("deleted").eq(0_i64)),
            |row| row.get::<String>(0),
        )?
        else {
            return Ok(false);
        };
        Ok(home == ws || self.serves(ws, bot)?)
    }

    pub fn serves(&mut self, ws: &str, bot: &str) -> Result<bool> {
        Ok(self
            .0
            .one(
                Query::select()
                    .from("bot_workspaces")
                    .column("bot_id")
                    .and_where(Expr::col("workspace_id").eq(ws))
                    .and_where(Expr::col("bot_id").eq(bot)),
                |_| Ok(()),
            )?
            .is_some())
    }
}
