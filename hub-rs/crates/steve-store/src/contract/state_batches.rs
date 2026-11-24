//! Batched reads for Hub configuration and operational summaries.

use std::collections::HashMap;

use sea_query::{Expr, ExprTrait, Func, Order, Query};
use serde::Serialize;

use crate::{Read, Result};

#[derive(Debug, Clone, PartialEq, Default)]
pub struct ItemExtras {
    pub goal: String,
    pub goals: usize,
    pub tags: Vec<String>,
    pub deps: Vec<String>,
    /// `(id, status)` of the item's sessions by `no`.
    pub sessions: Vec<(String, String)>,
    pub open: Vec<String>,
    /// Queued rows of the last non-ended session.
    pub queued: usize,
    /// Graph identity and pause flag, plus failed children only when the graph matches the item cycle.
    pub workflow_paused: Option<bool>,
    pub workflow_cycle: Option<i64>,
    pub workflow_failed: Vec<String>,
    pub workflow_attempt: Option<i64>,
    pub workflow_superseded: bool,
}

#[derive(Serialize)]
pub struct McpSummary {
    pub id: String,
    pub name: String,
    pub transport: String,
    pub enabled: bool,
}

#[derive(Default)]
pub struct BotExtras {
    pub workspaces: HashMap<String, Vec<String>>,
    pub mcp: HashMap<String, Vec<McpSummary>>,
}

fn scan_mcp(
    r: &mut impl Read,
    bot: Option<&str>,
    mut add: impl FnMut(String, McpSummary),
) -> Result<()> {
    use sea_query::{Func, JoinType};
    let mut query = Query::select();
    query
        .columns([
            ("a", "bot_id"),
            ("s", "id"),
            ("s", "name"),
            ("s", "transport"),
            ("a", "enabled"),
        ])
        .from_as("bot_capabilities", "a")
        .join_as(
            JoinType::InnerJoin,
            "mcp_servers",
            "s",
            Expr::col(("s", "id")).eq(Expr::col(("a", "entry_id"))),
        )
        .and_where(Expr::col(("a", "kind")).eq("mcp"));
    if let Some(bot) = bot {
        query.and_where(Expr::col(("a", "bot_id")).eq(bot));
    } else {
        query.order_by(("a", "bot_id"), Order::Asc);
    }
    query.order_by_expr(Func::lower(Expr::col(("s", "name"))).into(), Order::Asc);
    r.scan(&query, |row| {
        add(
            row.get(0)?,
            McpSummary {
                id: row.get(1)?,
                name: row.get(2)?,
                transport: row.get(3)?,
                enabled: row.get(4)?,
            },
        );
        Ok(())
    })
}

pub fn bot_mcp(r: &mut impl Read, bot: &str) -> Result<Vec<McpSummary>> {
    let mut summaries = Vec::new();
    scan_mcp(r, Some(bot), |_, server| summaries.push(server))?;
    Ok(summaries)
}

pub fn bot_extras(r: &mut impl Read) -> Result<BotExtras> {
    let mut extras = BotExtras::default();
    r.scan(
        Query::select()
            .columns(["bot_id", "workspace_id"])
            .from("bot_workspaces")
            .order_by("bot_id", Order::Asc)
            .order_by("workspace_id", Order::Asc),
        |row| {
            extras
                .workspaces
                .entry(row.get(0)?)
                .or_default()
                .push(row.get(1)?);
            Ok(())
        },
    )?;
    scan_mcp(r, None, |bot, server| {
        extras.mcp.entry(bot).or_default().push(server)
    })?;
    Ok(extras)
}

/// Counts available and registered devices in the same snapshot as the summary.
pub fn device_counts(r: &mut impl Read) -> Result<(i64, i64)> {
    Ok(r.one(
        Query::select()
            .expr(Func::count(Expr::col("id")).filter(
                Expr::from(Func::lower(Expr::col("status"))).is_not_in(["offline", "离线"]),
            ))
            .expr(Func::count(Expr::col("id")))
            .from("devices"),
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?
    .unwrap_or_default())
}
