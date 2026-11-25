use crate::contract::Q;
use crate::{Read, Result, W};
use sea_query::{Expr, ExprTrait, Order, Query};

/// `(title, phase)` of an item in a workspace.
pub fn item_brief(c: &mut impl Read, ws: &str, id: &str) -> Result<Option<(String, String)>> {
    c.one(
        Query::select()
            .columns(["title", "phase"])
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

pub fn open_cycle(c: &mut impl Read, ws: &str, item: &str) -> Result<Option<i64>> {
    c.one(
        Query::select()
            .column("cycle")
            .from("items")
            .and_where(Expr::col("id").eq(item))
            .and_where(Expr::col("workspace_id").eq(ws))
            .and_where(Expr::col("phase").ne("closed")),
        |row| row.get(0),
    )
}

/// A timeline line `steve sys <text>` (what the TS Hub writes for system notes).
pub fn add_timeline(c: &mut W<'_>, item: &str, text: &str, now: i64) -> Result<()> {
    store_timeline(c, item, "steve", "sys", text, None, None, now)
}

/// A progress report supplied by the item executor.
pub fn add_progress(w: &mut W<'_>, item: &str, actor: &str, text: &str, at: i64) -> Result<()> {
    store_timeline(w, item, actor, "progress", text, None, None, at)
}

pub(crate) fn store_timeline(
    w: &mut W<'_>,
    item: &str,
    actor: &str,
    kind: &str,
    text: &str,
    session: Option<&str>,
    files: Option<&str>,
    now: i64,
) -> Result<()> {
    w.exec(
        Query::insert()
            .into_table("timeline")
            .columns([
                "item_id",
                "ts",
                "actor",
                "kind",
                "text",
                "session_id",
                "files",
            ])
            .values_panic([
                item.into(),
                now.into(),
                actor.into(),
                kind.into(),
                text.into(),
                session.into(),
                files.into(),
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

pub fn requested_by(r: &mut impl Read, item: &str) -> Result<Option<String>> {
    Ok(r.one(
        Query::select()
            .column("requested_by")
            .from("items")
            .and_where(Expr::col("id").eq(item)),
        |row| row.get::<Option<String>>(0),
    )?
    .flatten())
}

pub fn running_tasks(c: &mut impl Read, ws: &str) -> Result<Vec<String>> {
    c.all(
        Query::select()
            .column("id")
            .from("items")
            .and_where(Expr::col("workspace_id").eq(ws))
            .and_where(Expr::col("kind").eq("task"))
            .and_where(Expr::col("archived").eq(0_i64))
            .and_where(Expr::col("phase").eq("active"))
            .order_by("updated", Order::Desc),
        |row| row.get(0),
    )
}

pub fn unarchive(w: &mut W<'_>, id: &str) -> Result<()> {
    w.exec(
        Query::update()
            .table("items")
            .value("archived", 0_i64)
            .and_where(Expr::col("id").eq(id)),
    )?;
    Ok(())
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

#[derive(Clone)]
pub struct LinkedItem {
    pub id: String,
    pub title: String,
    pub phase: String,
    pub archived: bool,
}

impl LinkedItem {
    pub fn is_open(&self) -> bool {
        !self.archived && self.phase != "closed"
    }
}

pub fn linked_items(c: &mut impl Read, ws: &str, ids: &[String]) -> Result<Vec<LinkedItem>> {
    let selected = c.db().dialect().text_array(ids)?;
    let rows = c.all(
        Query::select()
            .columns(["id", "title", "phase", "archived"])
            .from("items")
            .and_where(Expr::col("workspace_id").eq(ws))
            .and_where(Expr::col("id").in_subquery(selected)),
        |r| {
            Ok(LinkedItem {
                id: r.get(0)?,
                title: r.get(1)?,
                phase: r.get(2)?,
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
            .columns(["id", "title", "phase"])
            .from("items")
            .and_where(Expr::col("id").in_subquery(selected))
            .and_where(Expr::col("archived").eq(0_i64))
            .and_where(Expr::col("phase").ne("closed")),
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

pub fn has_recent_user_note(c: &mut impl Read, item: &str, since: i64) -> Result<bool> {
    c.one(
        Query::select()
            .column("item_id")
            .from("timeline")
            .and_where(Expr::col("item_id").eq(item))
            .and_where(Expr::col("kind").eq("me"))
            .and_where(Expr::col("ts").gte(since))
            .limit(1),
        |_| Ok(()),
    )
    .map(|row| row.is_some())
}

/// Append a system note and publish the item change with the rest of the write.
pub fn append_system_note(w: &mut W<'_>, item: &str, text: &str) -> Result<()> {
    add_timeline(w, item, text, crate::now())?;
    if let Some(workspace) = item_workspace(w, item)? {
        w.emit(steve_types::DomainEvent::from(
            steve_types::WorkEvent::Item {
                ws: workspace.into(),
                id: item.into(),
            },
        ));
    }
    Ok(())
}

/// Record the final plan with a completed item.
pub fn set_closed_plan(w: &mut W<'_>, item: &str, plan: Vec<crate::PlanStep>) -> Result<()> {
    w.exec(
        Query::update()
            .table("items")
            .value("plan", crate::repo::to_json(&plan)?)
            .value("updated", crate::now())
            .and_where(Expr::col("id").eq(item)),
    )?;
    if let Some(ws) = item_workspace(w, item)? {
        w.emit(steve_types::WorkEvent::Item {
            ws: ws.into(),
            id: item.into(),
        });
    }
    Ok(())
}

pub(crate) fn progress_of_open_items(r: &mut impl Read) -> Result<Vec<(String, String, String)>> {
    r.all(
        Query::select()
            .columns(["workspace_id", "id"])
            .expr(Expr::cust("(SELECT text FROM timeline WHERE item_id=items.id AND kind='progress' ORDER BY id DESC LIMIT 1)"))
            .from("items")
            .and_where(Expr::col("phase").ne("closed")),
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
    pub phase: String,
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
            .columns([("i", "id"), ("i", "title"), ("i", "phase")])
            .from_as("items", "i")
            .expr(Expr::cust("(SELECT text FROM timeline WHERE item_id=i.id AND kind='progress' ORDER BY id DESC LIMIT 1)"))
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
            .and_where(Expr::col(("i", "phase")).ne("closed"))
            .and_where(Expr::col(("i", "archived")).eq(0_i64))
            .order_by(("i", "updated"), sea_query::Order::Desc)
            .limit(limit),
        |row| {
            Ok(RaisedItem {
                id: row.get(0)?,
                title: row.get(1)?,
                phase: row.get(2)?,
                current: row.get::<Option<String>>(3)?.unwrap_or_default(),
            })
        },
    )
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
            .column(("ti", "target_item_id"))
            .from_as("thread_dispatch_refs", "ti")
            .join_as(
                JoinType::InnerJoin,
                "messages",
                "m",
                Expr::col(("m", "id")).eq(Expr::col(("ti", "origin_message_id"))),
            )
            .and_where(Expr::col(("m", "space")).eq(space))
            .and_where(Expr::col(("m", "workspace_id")).eq(ws))
            .and_where(Expr::col(("ti", "workspace_id")).eq(ws))
            .and_where(Expr::col(("ti", "effect_kind")).eq("create_item"))
            .and_where(Expr::col(("ti", "target_item_id")).is_not_null()),
        |r| r.get(0),
    )
}

/// Admits dependency management for a current active item with unresolved prerequisites.
pub fn dependency_management(r: &mut impl Read, item: Option<&str>) -> Result<bool> {
    let mut allowed = false;
    r.db().query(
        "SELECT EXISTS(
                    SELECT 1 FROM items i WHERE i.id=?1 AND i.phase='active'
                    AND i.archived=0
                    AND (
                        EXISTS(SELECT 1 FROM item_dependency_waits dw
                            WHERE dw.item_id=i.id AND dw.cycle=i.cycle AND dw.state='waiting')
                        OR EXISTS(SELECT 1 FROM item_deps d LEFT JOIN items dep ON dep.id=d.dep_id
                            WHERE d.item_id=i.id AND (dep.id IS NULL OR dep.phase<>'closed' OR dep.outcome IS NOT 'completed'))
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

/// Non-terminal items created by the retired dynamic work-graph engine (`source[1] = "workflow"`).
/// Used once during Hub startup before recovery to cancel leftover active child items cleanly.
pub fn legacy_workflow_children_open(r: &mut impl Read) -> Result<Vec<String>> {
    let dialect = r.db().dialect();
    r.all(
        Query::select()
            .column("id")
            .from("items")
            .and_where(
                dialect
                    .json_path(Expr::col("source"), "$[1]")
                    .eq("workflow"),
            )
            .and_where(Expr::col("phase").ne("closed"))
            .order_by("id", Order::Asc),
        |row| row.get(0),
    )
}
