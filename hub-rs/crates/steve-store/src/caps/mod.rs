//! The capabilities library (migration 65): MCP servers ([`mcp`]), skills ([`skills`]) and remote A2A
//! agents ([`a2a`]) are defined once per workspace and assigned per Bot (`bot_capabilities`, this
//! module). A session gets the entries its Bot is assigned and has switched on.

pub mod a2a;
pub mod mcp;
pub mod skills;
pub mod tools;

use sea_query::{Expr, ExprTrait, JoinType, OnConflict, Order, Query, SelectStatement};
use serde::Serialize;

use crate::contract::Row;
use crate::{Read, Result, W};

/// The kinds of entry a Bot can be assigned.
pub const KINDS: &[&str] = &["mcp", "skill", "a2a"];

/// One assignment: Bot `bot_id` uses entry `entry_id` of `kind`, switched on or off for it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Assignment {
    pub bot_id: String,
    pub kind: String,
    pub entry_id: String,
    pub enabled: bool,
    pub created: i64,
}

pub struct Assignments<'a, C>(pub &'a mut C);

fn row(r: &Row<'_>) -> Result<Assignment> {
    Ok(Assignment {
        bot_id: r.get(0)?,
        kind: r.get(1)?,
        entry_id: r.get(2)?,
        enabled: r.get(3)?,
        created: r.get(4)?,
    })
}

fn assignments(kind: &str) -> SelectStatement {
    Query::select()
        .from_as("bot_capabilities", "a")
        .columns(["bot_id", "kind", "entry_id", "enabled", "created"].map(|col| ("a", col)))
        .and_where(Expr::col(("a", "kind")).eq(kind))
        .take()
}

fn with_live_bots(kind: &str) -> SelectStatement {
    assignments(kind)
        .join_as(
            JoinType::InnerJoin,
            "bots",
            "b",
            Expr::col(("b", "id"))
                .equals(("a", "bot_id"))
                .and(Expr::col(("b", "deleted")).eq(0_i64)),
        )
        .order_by(("a", "created"), Order::Asc)
        .order_by(("a", "bot_id"), Order::Asc)
        .take()
}

impl<C: Read> Assignments<'_, C> {
    pub fn of_bot(&mut self, bot: &str, kind: &str) -> Result<Vec<Assignment>> {
        self.0.all(
            assignments(kind)
                .and_where(Expr::col(("a", "bot_id")).eq(bot))
                .order_by(("a", "created"), Order::Asc)
                .order_by(("a", "entry_id"), Order::Asc),
            row,
        )
    }

    pub fn of_entry(&mut self, kind: &str, entry: &str) -> Result<Vec<Assignment>> {
        self.0.all(
            with_live_bots(kind).and_where(Expr::col(("a", "entry_id")).eq(entry)),
            row,
        )
    }

    pub fn all(&mut self, kind: &str) -> Result<Vec<Assignment>> {
        self.0.all(&with_live_bots(kind), row)
    }

    pub fn get(&mut self, bot: &str, kind: &str, entry: &str) -> Result<Option<Assignment>> {
        self.0.one(
            assignments(kind)
                .and_where(Expr::col(("a", "bot_id")).eq(bot))
                .and_where(Expr::col(("a", "entry_id")).eq(entry)),
            row,
        )
    }

    pub fn bot_of_session(&mut self, session: &str) -> Result<Option<String>> {
        Ok(self
            .0
            .one(
                Query::select()
                    .from("sessions")
                    .column("bot_id")
                    .and_where(Expr::col("id").eq(session)),
                |r| r.get::<Option<String>>(0),
            )?
            .flatten())
    }

    pub fn enabled_ids(&mut self, bot: &str, kind: &str) -> Result<Vec<String>> {
        Ok(self
            .of_bot(bot, kind)?
            .into_iter()
            .filter(|a| a.enabled)
            .map(|a| a.entry_id)
            .collect())
    }

    pub fn is_enabled(&mut self, bot: &str, kind: &str, entry: &str) -> Result<bool> {
        Ok(self.get(bot, kind, entry)?.is_some_and(|a| a.enabled))
    }
}

fn assigned<C: Read, T>(
    r: &mut C,
    bot: &str,
    kind: &str,
    load: impl Fn(&mut C, &str) -> Result<Option<T>>,
    order: impl Fn(&T) -> String,
) -> Result<Vec<(T, bool)>> {
    let mut out = Vec::new();
    for a in Assignments(r).of_bot(bot, kind)? {
        if let Some(item) = load(r, &a.entry_id)? {
            out.push((item, a.enabled));
        }
    }
    out.sort_by_cached_key(|(item, _)| order(item));
    Ok(out)
}

impl Assignments<'_, W<'_>> {
    pub fn set(
        &mut self,
        bot: &str,
        kind: &str,
        entry: &str,
        enabled: bool,
        now: i64,
    ) -> Result<()> {
        self.0.exec(
            Query::insert()
                .into_table("bot_capabilities")
                .columns(["bot_id", "kind", "entry_id", "enabled", "created"])
                .values_panic([
                    bot.into(),
                    kind.into(),
                    entry.into(),
                    i64::from(enabled).into(),
                    now.into(),
                ])
                .on_conflict(
                    OnConflict::columns(["bot_id", "kind", "entry_id"])
                        .update_column("enabled")
                        .to_owned(),
                ),
        )?;
        Ok(())
    }

    pub fn remove(&mut self, bot: &str, kind: &str, entry: &str) -> Result<bool> {
        tools::ToolRows(self.0).set_pin(bot, kind, entry, false, 0)?;
        Ok(self.0.exec(
            Query::delete()
                .from_table("bot_capabilities")
                .and_where(Expr::col("bot_id").eq(bot))
                .and_where(Expr::col("kind").eq(kind))
                .and_where(Expr::col("entry_id").eq(entry)),
        )? != 0)
    }

    fn remove_entry(&mut self, kind: &str, entry: &str) -> Result<()> {
        tools::ToolRows(self.0).drop_target(kind, entry)?;
        self.0.exec(
            Query::delete()
                .from_table("bot_capabilities")
                .and_where(Expr::col("kind").eq(kind))
                .and_where(Expr::col("entry_id").eq(entry)),
        )?;
        Ok(())
    }
}
