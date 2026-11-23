//! Batched reads for Hub configuration and operational summaries.

use std::collections::HashMap;

use sea_query::{Expr, ExprTrait, Func, Order, Query, Values};
use serde::Serialize;
use serde_json::Value;

use super::backend::Row;
use crate::{Read, Result};

#[derive(Debug, Clone, PartialEq)]
pub struct ItemExtras<C> {
    pub goal: String,
    pub goals: usize,
    pub tags: Vec<String>,
    pub deps: Vec<String>,
    /// `(id, status)` of the item's sessions by `no`.
    pub sessions: Vec<(String, String)>,
    pub open: Vec<String>,
    /// Queued rows of the last non-ended session.
    pub queued: usize,
    pub closeout: Option<C>,
    /// Graph identity and pause flag, plus failed children only when the graph matches the item cycle.
    pub workflow_paused: Option<bool>,
    pub workflow_cycle: Option<i64>,
    pub workflow_failed: Vec<String>,
    pub workflow_attempt: Option<i64>,
    pub workflow_superseded: bool,
}

impl<C> Default for ItemExtras<C> {
    fn default() -> Self {
        Self {
            goal: String::new(),
            goals: 0,
            tags: Vec::new(),
            deps: Vec::new(),
            sessions: Vec::new(),
            open: Vec::new(),
            queued: 0,
            closeout: None,
            workflow_paused: None,
            workflow_cycle: None,
            workflow_failed: Vec::new(),
            workflow_attempt: None,
            workflow_superseded: false,
        }
    }
}

/// The workspace reads use the store's snapshot and its cached statements.
fn each(
    r: &mut impl Read,
    sql: &str,
    parameter: &str,
    mut visit: impl FnMut(&Row<'_>) -> Result<()>,
) -> Result<()> {
    r.db()
        .query(sql, &Values(vec![parameter.into()]), &mut visit)
}
/// The same scoped read for the item detail and the whole workspace's list. Older revisions
/// (including a merge of a void result) are history, not a live closeout.
fn pr_closeouts<C>(
    c: &mut impl Read,
    scope: &str,
    param: &str,
    map_closeout: impl Fn(&Value, bool, bool) -> Option<C>,
    mut found: impl FnMut(String, C),
) -> Result<()> {
    each(c, &format!(
        "SELECT i.id, i.phase = 'closed', o.evidence,
                EXISTS (SELECT 1 FROM decisions d WHERE d.item_id = i.id
                        AND d.subject_type = 'pr_awaiting_merge' AND d.subject_id = o.id AND d.status = 'open')
         FROM items i JOIN item_operations o ON o.id = (
            SELECT p.id FROM item_operations p WHERE p.item_id = i.id AND p.cycle = i.cycle
                AND p.revision = i.revision AND p.kind = 'release'
                AND json_extract(p.evidence, '$.pr_slug') IS NOT NULL
                AND p.status IN ('pending', 'succeeded')
            ORDER BY p.created DESC, p.id DESC LIMIT 1
         )
         WHERE {scope} AND i.kind = 'task'
           AND (i.phase = 'closing' OR (i.phase = 'closed' AND i.status = 'done'))"
    ), param, |r| {
        let evidence = serde_json::from_str(&r.get::<String>(2)?).unwrap_or(Value::Null);
        if let Some(p) = map_closeout(&evidence, r.get(1)?, r.get(3)?) {
            found(r.get(0)?, p);
        }
        Ok(())
    })
}

pub fn item_closeout<C>(
    c: &mut impl Read,
    id: &str,
    map_closeout: impl Fn(&Value, bool, bool) -> Option<C>,
) -> Result<Option<C>> {
    let mut closeout = None;
    pr_closeouts(c, "i.id = ?1", id, map_closeout, |_, p| closeout = Some(p))?;
    Ok(closeout)
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
