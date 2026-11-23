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

pub(crate) fn progress_of_open_items(r: &mut impl Read) -> Result<Vec<(String, String, String)>> {
    r.all(
        Query::select()
            .columns(["workspace_id", "id", "now"])
            .from("items")
            .and_where(Expr::col("status").is_not_in(["done", "drop"])),
        |row| {
            Ok((
                row.get(0)?,
                row.get(1)?,
                row.get::<Option<String>>(2)?.unwrap_or_default(),
            ))
        },
    )
}

pub struct RaisedItem {
    pub id: String,
    pub title: String,
    pub status: String,
    pub current: String,
}

pub fn raised_open(
    r: &mut impl Read,
    ws: &str,
    party: &str,
    limit: u64,
) -> Result<Vec<RaisedItem>> {
    r.all(
        Query::select()
            .columns([("i", "id"), ("i", "title"), ("i", "status"), ("i", "now")])
            .from_as("items", "i")
            .join_as(
                sea_query::JoinType::InnerJoin,
                "links",
                "l",
                Expr::col(("l", "subject_id"))
                    .equals(("i", "id"))
                    .and(Expr::col(("l", "workspace_id")).equals(("i", "workspace_id"))),
            )
            .and_where(Expr::col(("i", "workspace_id")).eq(ws))
            .and_where(Expr::col(("l", "subject_type")).eq("item"))
            .and_where(Expr::col(("l", "predicate")).eq("raised_by"))
            .and_where(Expr::col(("l", "object_type")).eq("party"))
            .and_where(Expr::col(("l", "object_id")).eq(party))
            .and_where(Expr::col(("i", "status")).is_not_in(["done", "drop"]))
            .and_where(Expr::col(("i", "archived")).eq(0_i64))
            .order_by(("i", "updated"), sea_query::Order::Desc)
            .limit(limit),
        |row| {
            Ok(RaisedItem {
                id: row.get(0)?,
                title: row.get(1)?,
                status: row.get(2)?,
                current: row.get(3)?,
            })
        },
    )
}
