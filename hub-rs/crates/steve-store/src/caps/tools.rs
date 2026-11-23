//! Tool search (migration 69): what a Bot keeps loaded in every session (`bot_tool_pins`) and how often
//! each tool came up in its searches and was called (`tool_usage`). Steve's core tools are always
//! loaded; the rest are found with `steve_tools_search` and run with `steve_tool_call`.

use sea_query::{Expr, ExprTrait, Func, OnConflict, Order, Query};
use serde::Serialize;

use crate::{Read, Result, W};

/// The kinds of pin: a tool by name, every tool of a library MCP server, a remote agent's tool.
pub const PIN_KINDS: &[&str] = &["tool", "mcp", "a2a"];

/// Days a usage row is kept.
pub const KEEP_DAYS: i64 = 90;

/// One pin of a Bot.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Pin {
    pub kind: String,
    pub target: String,
}

/// The day (since the Unix epoch, UTC) of a millisecond timestamp.
pub fn day_of(ms: i64) -> i64 {
    ms.div_euclid(86_400_000)
}

/// One tool's use by a Bot over some days.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Usage {
    pub tool: String,
    pub searched: i64,
    pub called: i64,
}

pub struct ToolRows<'a, C>(pub &'a mut C);

impl<C: Read> ToolRows<'_, C> {
    pub fn pins(&mut self, bot: &str) -> Result<Vec<Pin>> {
        self.0.all(
            Query::select()
                .from("bot_tool_pins")
                .columns(["kind", "target"])
                .and_where(Expr::col("bot_id").eq(bot))
                .order_by("created", Order::Asc)
                .order_by("kind", Order::Asc)
                .order_by("target", Order::Asc),
            |row| {
                Ok(Pin {
                    kind: row.get(0)?,
                    target: row.get(1)?,
                })
            },
        )
    }

    pub fn usage(&mut self, bot: &str, today: i64, days: i64) -> Result<Vec<Usage>> {
        let searched = Func::sum(Expr::col("searched"));
        let called = Func::sum(Expr::col("called"));
        self.0.all(
            Query::select()
                .from("tool_usage")
                .column("tool")
                .expr(searched.clone())
                .expr(called.clone())
                .and_where(Expr::col("bot_id").eq(bot))
                .and_where(Expr::col("day").gt(today - days))
                .group_by_col("tool")
                .order_by_expr(called.into(), Order::Desc)
                .order_by_expr(searched.into(), Order::Desc)
                .order_by("tool", Order::Asc),
            |row| {
                Ok(Usage {
                    tool: row.get(0)?,
                    searched: row.get(1)?,
                    called: row.get(2)?,
                })
            },
        )
    }
}

impl ToolRows<'_, W<'_>> {
    pub fn set_pin(
        &mut self,
        bot: &str,
        kind: &str,
        target: &str,
        on: bool,
        now: i64,
    ) -> Result<bool> {
        let n = if on {
            self.0.exec(
                Query::insert()
                    .into_table("bot_tool_pins")
                    .columns(["bot_id", "kind", "target", "created"])
                    .values_panic([bot.into(), kind.into(), target.into(), now.into()])
                    .on_conflict(
                        OnConflict::columns(["bot_id", "kind", "target"])
                            .do_nothing()
                            .to_owned(),
                    ),
            )?
        } else {
            self.0.exec(
                Query::delete()
                    .from_table("bot_tool_pins")
                    .and_where(Expr::col("bot_id").eq(bot))
                    .and_where(Expr::col("kind").eq(kind))
                    .and_where(Expr::col("target").eq(target)),
            )?
        };
        Ok(n != 0)
    }

    pub(super) fn drop_target(&mut self, kind: &str, target: &str) -> Result<()> {
        self.0.exec(
            Query::delete()
                .from_table("bot_tool_pins")
                .and_where(Expr::col("kind").eq(kind))
                .and_where(Expr::col("target").eq(target)),
        )?;
        Ok(())
    }

    pub fn record(
        &mut self,
        bot: &str,
        day: i64,
        tools: &[&str],
        searched: i64,
        called: i64,
    ) -> Result<()> {
        fn combined(column: &'static str) -> Expr {
            Expr::col(("tool_usage", column)).add(Expr::col(("excluded", column)))
        }
        for tool in tools {
            self.0.exec(
                Query::insert()
                    .into_table("tool_usage")
                    .columns(["bot_id", "tool", "day", "searched", "called"])
                    .values_panic([
                        bot.into(),
                        (*tool).into(),
                        day.into(),
                        searched.into(),
                        called.into(),
                    ])
                    .on_conflict(
                        OnConflict::columns(["bot_id", "tool", "day"])
                            .value("searched", combined("searched"))
                            .value("called", combined("called"))
                            .to_owned(),
                    ),
            )?;
        }
        Ok(())
    }
}
