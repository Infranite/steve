use crate::contract::Q;
use crate::{Read, Result, W};
use sea_query::{Expr, ExprTrait, Order, Query};

/// `(title, status)` of an item in a workspace.
pub fn item_brief(c: &mut impl Read, ws: &str, id: &str) -> Result<Option<(String, String)>> {
    c.one(
        Query::select()
            .columns(["title", "status"])
            .from("items")
            .and_where(Expr::col("id").eq(id))
            .and_where(Expr::col("workspace_id").eq(ws)),
        |r| Ok((r.get(0)?, r.get(1)?)),
    )
}

/// The item whose branch is `branch` in `project` (newest first).
pub fn item_for_branch(
    c: &mut impl Read,
    ws: &str,
    branch: &str,
    project: &str,
) -> Result<Option<String>> {
    c.one(
        Query::select()
            .columns(["id"])
            .from("items")
            .and_where(Expr::col("workspace_id").eq(ws))
            .and_where(Expr::col("branch").eq(branch))
            .and_where(Expr::col("project_id").eq(project))
            .order_by("created", Order::Desc)
            .limit(1),
        |r| r.get(0),
    )
}

/// `status` of an item in a workspace.
pub fn item_status(c: &mut impl Read, ws: &str, id: &str) -> Result<Option<String>> {
    c.one(
        Query::select()
            .columns(["status"])
            .from("items")
            .and_where(Expr::col("id").eq(id))
            .and_where(Expr::col("workspace_id").eq(ws)),
        |r| r.get(0),
    )
}

/// A timeline line `steve sys <text>` (what the TS Hub writes for system notes).
pub fn add_timeline(c: &mut W<'_>, item: &str, text: &str, now: i64) -> Result<()> {
    c.exec(
        Query::insert()
            .into_table("timeline")
            .columns(["item_id", "ts", "actor", "kind", "text", "session_id"])
            .values_panic([
                Expr::val(item),
                Expr::val(now),
                Expr::val("steve"),
                Expr::val("sys"),
                Expr::val(text),
                Expr::val(None::<String>),
            ]),
    )?;
    Ok(())
}

/// An artifact, unless the item already has one of that kind at that location. `true` if added.
pub fn add_artifact_once(
    c: &mut W<'_>,
    item: &str,
    kind: &str,
    title: &str,
    location: &str,
    now: i64,
) -> Result<bool> {
    let has: bool = c
        .one(
            Query::select()
                .expr(Expr::cust("1"))
                .from("artifacts")
                .and_where(Expr::col("item_id").eq(item))
                .and_where(Expr::col("kind").eq(kind))
                .and_where(Expr::col("location").eq(location))
                .limit(1),
            |_| Ok(()),
        )
        .map(|row| row.is_some())?;
    if has {
        return Ok(false);
    }
    c.exec(
        Query::insert()
            .into_table("artifacts")
            .columns(["id", "item_id", "kind", "title", "location", "created"])
            .values_panic([
                Expr::val(crate::uid("art")),
                Expr::val(item),
                Expr::val(kind),
                Expr::val(title),
                Expr::val(location),
                Expr::val(now),
            ]),
    )?;
    Ok(true)
}

pub fn item_workspace(c: &mut impl Read, id: &str) -> Result<Option<String>> {
    c.one(
        Query::select()
            .columns(["workspace_id"])
            .from("items")
            .and_where(Expr::col("id").eq(id)),
        |r| r.get(0),
    )
}

pub fn existing_ids(
    read: &mut impl Read,
    ws: &str,
    ids: &[&str],
) -> Result<std::collections::HashSet<String>> {
    let ids = read.db().dialect().text_array(ids)?;
    Ok(read
        .all(
            Query::select()
                .column("id")
                .from("items")
                .and_where(Expr::col("workspace_id").eq(ws))
                .and_where(Expr::col("id").in_subquery(ids)),
            |r| r.get(0),
        )?
        .into_iter()
        .collect())
}

pub fn set_progress(w: &mut W<'_>, id: &str, status: &str, stage: &str, now: &str) -> Result<()> {
    w.exec(
        Query::update()
            .table("items")
            .value("status", status)
            .value("stage", stage)
            .value("now", now)
            .and_where(Expr::col("id").eq(id)),
    )?;
    Ok(())
}

#[derive(Clone)]
pub struct LinkedItem {
    pub id: String,
    pub title: String,
    pub status: String,
    pub archived: bool,
}

impl LinkedItem {
    pub fn is_open(&self) -> bool {
        !self.archived && !matches!(self.status.as_str(), "done" | "drop")
    }
}

pub fn linked_items(c: &mut impl Read, ws: &str, ids: &[String]) -> Result<Vec<LinkedItem>> {
    let selected = c.db().dialect().text_array(ids)?;
    let rows = c.all(
        Query::select()
            .columns(["id", "title", "status", "archived"])
            .from("items")
            .and_where(Expr::col("workspace_id").eq(ws))
            .and_where(Expr::col("id").in_subquery(selected)),
        |r| {
            Ok(LinkedItem {
                id: r.get(0)?,
                title: r.get(1)?,
                status: r.get(2)?,
                archived: r.get::<i64>(3)? != 0,
            })
        },
    )?;
    let mut by_id = rows
        .into_iter()
        .map(|row| (row.id.clone(), row))
        .collect::<std::collections::HashMap<_, _>>();
    Ok(ids.iter().filter_map(|id| by_id.remove(id)).collect())
}

pub fn open_items(c: &mut impl Read, ids: &[String]) -> Result<Vec<(String, String, String)>> {
    let selected = c.db().dialect().text_array(ids)?;
    let rows = c.all(
        Query::select()
            .columns(["id", "title", "status"])
            .from("items")
            .and_where(Expr::col("id").in_subquery(selected))
            .and_where(Expr::col("archived").eq(0_i64))
            .and_where(Expr::col("status").is_not_in(["done", "drop"])),
        |r| {
            Ok((
                r.get::<String>(0)?,
                r.get::<String>(1)?,
                r.get::<String>(2)?,
            ))
        },
    )?;
    let by_id: std::collections::HashMap<_, _> =
        rows.iter().map(|row| (row.0.as_str(), row)).collect();
    Ok(ids
        .iter()
        .filter_map(|id| by_id.get(id.as_str()).map(|r| (*r).clone()))
        .collect())
}

/// Item references associated with a message's conversation.
pub fn in_conversation(
    c: &mut impl crate::Read,
    ws: &str,
    space: &str,
) -> crate::Result<Vec<String>> {
    use sea_query::{Expr, ExprTrait, JoinType, Query};
    c.all(
        Query::select()
            .column(("ti", "item_id"))
            .from_as("thread_items", "ti")
            .join_as(
                JoinType::InnerJoin,
                "messages",
                "m",
                Expr::col(("m", "id")).eq(Expr::col(("ti", "origin_msg"))),
            )
            .and_where(Expr::col(("m", "space")).eq(space))
            .and_where(Expr::col(("m", "workspace_id")).eq(ws))
            .and_where(Expr::col(("ti", "workspace_id")).eq(ws)),
        |r| r.get(0),
    )
}

/// Admits dependency management for a current active parent with unresolved prerequisites.
pub fn dependency_management(r: &mut impl Read, item: Option<&str>) -> Result<bool> {
    let mut allowed = false;
    r.db().query(
        "SELECT EXISTS(
                    SELECT 1 FROM items i WHERE i.id=?1 AND i.phase='active'
                    AND i.status IN ('run','block') AND i.archived=0
                    AND COALESCE(json_extract(i.source,'$[1]'),'')<>'workflow'
                    AND NOT EXISTS(SELECT 1 FROM workflow_units u WHERE u.item_id=i.id)
                    AND NOT EXISTS(SELECT 1 FROM workflow_attempts a WHERE a.item_id=i.id)
                    AND NOT EXISTS(SELECT 1 FROM workflows w WHERE w.item_id=i.id AND w.paused=1)
                    AND (
                        EXISTS(SELECT 1 FROM item_dependency_waits dw
                            WHERE dw.item_id=i.id AND dw.cycle=i.cycle AND dw.state='waiting')
                        OR EXISTS(SELECT 1 FROM item_deps d LEFT JOIN items dep ON dep.id=d.dep_id
                            WHERE d.item_id=i.id AND (dep.id IS NULL OR dep.status<>'done'
                                OR dep.phase<>'closed' OR dep.outcome IS NOT 'completed'))
                    )
                )",
        &sea_query::Values(vec![item.into()]),
        &mut |row| {
            allowed = row.get(0)?;
            Ok(())
        },
    )?;
    Ok(allowed)
}
