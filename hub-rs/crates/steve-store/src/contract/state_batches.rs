//! Batched projections for the workspace state view.

use std::collections::HashMap;

use sea_query::{Expr, ExprTrait, Order, Query, Values};
use serde::Serialize;
use serde_json::Value;

use super::backend::Row;
use crate::{Queued, Read, Result};

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

pub fn item_extras<C>(
    c: &mut impl Read,
    ws: &str,
    map_closeout: impl Fn(&Value, bool, bool) -> Option<C>,
) -> Result<HashMap<String, ItemExtras<C>>> {
    let mut m: HashMap<String, ItemExtras<C>> = HashMap::new();
    // same orders as the per-item queries of repo.ts (index order for the PK lookups)
    const ITEMS: &str = "SELECT id FROM items WHERE workspace_id = ?1 AND kind = 'task'";
    each(
        c,
        &format!(
            "SELECT item_id, text FROM item_goals WHERE item_id IN ({ITEMS}) ORDER BY item_id, version"
        ),
        ws,
        |r| {
            let x = m.entry(r.get(0)?).or_default();
            x.goal = r.get(1)?;
            x.goals += 1;
            Ok(())
        },
    )?;
    each(
        c,
        &format!(
            "SELECT it.item_id, it.tag_id FROM item_tags it LEFT JOIN tags t ON t.id = it.tag_id WHERE it.item_id IN ({ITEMS}) ORDER BY it.item_id, coalesce(t.cat = '类型', 0) DESC, it.rowid"
        ),
        ws,
        |r| {
            m.entry(r.get(0)?).or_default().tags.push(r.get(1)?);
            Ok(())
        },
    )?;
    each(
        c,
        &format!(
            "SELECT item_id, dep_id FROM item_deps WHERE item_id IN ({ITEMS}) ORDER BY item_id, created, dep_id"
        ),
        ws,
        |r| {
            m.entry(r.get(0)?).or_default().deps.push(r.get(1)?);
            Ok(())
        },
    )?;
    each(
        c,
        &format!(
            "SELECT item_id, id, status FROM sessions WHERE item_id IN ({ITEMS}) ORDER BY item_id, no, rowid"
        ),
        ws,
        |r| {
            m.entry(r.get(0)?)
                .or_default()
                .sessions
                .push((r.get(1)?, r.get(2)?));
            Ok(())
        },
    )?;
    each(
        c,
        &format!(
            "SELECT item_id, id FROM decisions WHERE status = 'open' AND item_id IN ({ITEMS}) ORDER BY item_id, created, rowid"
        ),
        ws,
        |r| {
            m.entry(r.get(0)?).or_default().open.push(r.get(1)?);
            Ok(())
        },
    )?;
    let mut counts: HashMap<String, usize> = HashMap::new();
    each(
        c,
        &format!(
            "SELECT q.session_id, count(*) FROM queued q JOIN sessions s ON s.id = q.session_id WHERE s.item_id IN ({ITEMS}) GROUP BY q.session_id"
        ),
        ws,
        |r| {
            counts.insert(r.get(0)?, r.get::<i64>(1)? as usize);
            Ok(())
        },
    )?;
    each(
        c,
        &format!(
            "SELECT w.item_id, w.paused, w.cycle FROM workflows w WHERE w.item_id IN ({ITEMS})"
        ),
        ws,
        |r| {
            let x = m.entry(r.get(0)?).or_default();
            x.workflow_paused = Some(r.get(1)?);
            x.workflow_cycle = Some(r.get(2)?);
            Ok(())
        },
    )?;
    each(
        c,
        &format!(
            "SELECT u.workflow_id, u.item_id FROM workflow_units u JOIN workflows w ON w.item_id = u.workflow_id JOIN items i ON i.id = w.item_id AND i.cycle = w.cycle AND i.phase != 'closed' WHERE u.status = 'failed' AND u.item_id IS NOT NULL AND u.workflow_id IN ({ITEMS}) ORDER BY u.workflow_id, u.position"
        ),
        ws,
        |r| {
            m.entry(r.get(0)?)
                .or_default()
                .workflow_failed
                .push(r.get(1)?);
            Ok(())
        },
    )?;
    each(
        c,
        &format!(
            "SELECT u.item_id, u.attempt FROM workflow_units u WHERE u.item_id IS NOT NULL AND u.item_id IN ({ITEMS})"
        ),
        ws,
        |r| {
            m.entry(r.get(0)?).or_default().workflow_attempt = Some(r.get(1)?);
            Ok(())
        },
    )?;
    each(
        c,
        &format!(
            "SELECT a.item_id, max(a.attempt) FROM workflow_attempts a WHERE a.item_id IN ({ITEMS}) AND NOT EXISTS (SELECT 1 FROM workflow_units u WHERE u.item_id = a.item_id) GROUP BY a.item_id"
        ),
        ws,
        |r| {
            let x = m.entry(r.get(0)?).or_default();
            x.workflow_superseded = true;
            if x.workflow_attempt.is_none() {
                x.workflow_attempt = r.get(1)?;
            }
            Ok(())
        },
    )?;
    pr_closeouts(
        c,
        "i.workspace_id = ?1",
        ws,
        map_closeout,
        |id, closeout| {
            m.entry(id).or_default().closeout = Some(closeout);
        },
    )?;
    for x in m.values_mut() {
        if let Some((sid, _)) = x.sessions.iter().rev().find(|(_, st)| st != "ended") {
            x.queued = counts.get(sid).copied().unwrap_or(0);
        }
    }
    Ok(m)
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

pub fn queued_by_session(r: &mut impl Read, ws: &str) -> Result<HashMap<String, Vec<Queued>>> {
    let order = r.db().dialect().insertion_order(None);
    let sessions = Query::select()
        .column("id")
        .from("sessions")
        .and_where(Expr::col("workspace_id").eq(ws))
        .order_by("updated", Order::Desc)
        .limit(300)
        .take();
    let mut grouped = HashMap::<String, Vec<Queued>>::new();
    r.scan(
        Query::select()
            .columns(["id", "session_id", "text", "via", "created", "files"])
            .from("queued")
            .and_where(Expr::col("session_id").in_subquery(sessions))
            .order_by("created", Order::Asc)
            .order_by_expr(order, Order::Asc),
        |row| {
            let files: Option<String> = row.get(5)?;
            let row = Queued {
                id: row.get(0)?,
                session_id: row.get(1)?,
                text: row.get(2)?,
                via: row.get(3)?,
                created: row.get(4)?,
                files: files
                    .and_then(|value| serde_json::from_str(&value).ok())
                    .unwrap_or_default(),
            };
            grouped.entry(row.session_id.clone()).or_default().push(row);
            Ok(())
        },
    )?;
    Ok(grouped)
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
    pub memories: HashMap<String, i64>,
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

pub fn bot_extras(r: &mut impl Read, ws: &str) -> Result<BotExtras> {
    use sea_query::Func;
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
    r.scan(
        Query::select()
            .column("bot_id")
            .expr(Func::count(Expr::col("bot_id")))
            .from("bot_memories")
            .and_where(Expr::col("workspace_id").eq(ws))
            .group_by_col("bot_id"),
        |row| {
            extras.memories.insert(row.get(0)?, row.get(1)?);
            Ok(())
        },
    )?;
    Ok(extras)
}
