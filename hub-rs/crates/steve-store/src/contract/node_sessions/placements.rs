//! Location facts used to route filesystem and workspace operations.

use sea_query::{Expr, ExprTrait, Func, JoinType, Order, Query, Values};
use steve_node_api::WorkspaceRef;

use crate::{Read, Result};

pub struct Directory {
    pub device: String,
    pub path: String,
    pub archived: bool,
}

pub fn project(r: &mut impl Read, id: &str) -> Result<Option<Directory>> {
    r.one(
        Query::select()
            .columns(["device_id", "path", "archived"])
            .from("projects")
            .and_where(Expr::col("id").eq(id)),
        |row| {
            Ok(Directory {
                device: row.get(0)?,
                path: row.get(1)?,
                archived: row.get(2)?,
            })
        },
    )
}

fn archived_project() -> Expr {
    Func::coalesce([Expr::col(("p", "archived")), Expr::Constant(false.into())]).into()
}

pub fn session(r: &mut impl Read, id: &str) -> Result<Option<Directory>> {
    r.one(
        Query::select()
            .columns([("s", "device_id"), ("s", "cwd")])
            .expr(archived_project())
            .from_as("sessions", "s")
            .join_as(
                JoinType::LeftJoin,
                "items",
                "i",
                Expr::col(("i", "id")).eq(Expr::col(("s", "item_id"))),
            )
            .join_as(
                JoinType::LeftJoin,
                "projects",
                "p",
                Expr::col(("p", "id")).eq(Expr::col(("i", "project_id"))),
            )
            .and_where(Expr::col(("s", "id")).eq(id)),
        |row| {
            Ok(Directory {
                device: row.get(0)?,
                path: row.get(1)?,
                archived: row.get(2)?,
            })
        },
    )
}

pub struct Item {
    pub cwd: Option<String>,
    pub project: Option<String>,
    pub archived: bool,
}

pub fn item(r: &mut impl Read, id: &str) -> Result<Option<Item>> {
    r.one(
        Query::select()
            .columns([("i", "cwd"), ("i", "project_id")])
            .expr(archived_project())
            .from_as("items", "i")
            .join_as(
                JoinType::LeftJoin,
                "projects",
                "p",
                Expr::col(("p", "id")).eq(Expr::col(("i", "project_id"))),
            )
            .and_where(Expr::col(("i", "id")).eq(id)),
        |row| {
            Ok(Item {
                cwd: row.get(0)?,
                project: row.get(1)?,
                archived: row.get(2)?,
            })
        },
    )
}

pub fn bound(r: &mut impl Read, session: &str) -> Result<Option<WorkspaceRef>> {
    bound_workspace(
        r,
        Query::select()
            .columns([("r", "node_id"), ("r", "workspace_ref")])
            .from_as("runtime_sessions", "r")
            .and_where(Expr::col(("r", "session_id")).eq(session)),
    )
}

pub fn latest_bound(r: &mut impl Read, item: &str) -> Result<Option<WorkspaceRef>> {
    let order = r.db().dialect().insertion_order(Some("s"));
    bound_workspace(
        r,
        Query::select()
            .columns([("r", "node_id"), ("r", "workspace_ref")])
            .from_as("runtime_sessions", "r")
            .join_as(
                JoinType::InnerJoin,
                "sessions",
                "s",
                Expr::col(("s", "id")).eq(Expr::col(("r", "session_id"))),
            )
            .and_where(Expr::col(("s", "item_id")).eq(item))
            .order_by(("s", "created"), Order::Desc)
            .order_by_expr(order, Order::Desc)
            .limit(1),
    )
}

fn bound_workspace(
    r: &mut impl Read,
    q: &sea_query::SelectStatement,
) -> Result<Option<WorkspaceRef>> {
    let row: Option<(String, String)> = r.one(q, |row| Ok((row.get(0)?, row.get(1)?)))?;
    Ok(row.and_then(|(node, raw)| {
        let workspace: WorkspaceRef = serde_json::from_str(&raw).ok()?;
        (workspace.node.as_str() == node).then_some(workspace)
    }))
}

pub struct KnowledgeSource {
    pub kind: String,
    pub path: Option<String>,
}

pub fn knowledge_source(r: &mut impl Read, id: &str) -> Result<Option<KnowledgeSource>> {
    let d = r.db().dialect();
    r.one(
        Query::select()
            .column("kind")
            .expr(d.json_field(Expr::col("config"), "path"))
            .from("kb_sources")
            .and_where(Expr::col("id").eq(id)),
        |row| {
            Ok(KnowledgeSource {
                kind: row.get(0)?,
                path: row.get(1)?,
            })
        },
    )
}

pub fn earlier_session_elsewhere(
    r: &mut impl Read,
    item: &str,
    session: &str,
    node: &str,
) -> Result<bool> {
    Ok(r.one(
        Query::select()
            .column(("s", "id"))
            .from_as("sessions", "s")
            .join_as(
                JoinType::LeftJoin,
                "runtime_sessions",
                "r",
                Expr::col(("s", "id")).eq(Expr::col(("r", "session_id"))),
            )
            .and_where(Expr::col(("s", "item_id")).eq(item))
            .and_where(Expr::col(("s", "id")).ne(session))
            .and_where(
                Expr::col(("r", "node_id"))
                    .is_null()
                    .or(Expr::col(("r", "node_id")).ne(node)),
            )
            .limit(1),
        |_| Ok(()),
    )?
    .is_some())
}

pub fn roots(r: &mut impl Read) -> Result<(Vec<String>, Vec<String>)> {
    let mut rw = Vec::new();
    r.db().query(
        "SELECT path FROM projects WHERE archived=0 AND device_id='hub'
         UNION SELECT s.cwd FROM sessions s LEFT JOIN items i ON i.id=s.item_id LEFT JOIN projects p ON p.id=i.project_id
          WHERE s.status<>'ended' AND s.readonly=0 AND s.source='Steve' AND s.device_id='hub' AND (p.id IS NULL OR p.archived=0)
         UNION SELECT i.cwd FROM items i LEFT JOIN projects p ON p.id=i.project_id
          WHERE i.cwd IS NOT NULL AND (p.id IS NULL OR p.archived=0)",
        &Values(vec![]),
        &mut |row| { rw.push(row.get(0)?); Ok(()) },
    )?;
    let d = r.db().dialect();
    let ro = r.all(
        Query::select()
            .expr(d.json_field(Expr::col("config"), "path"))
            .from("kb_sources")
            .and_where(Expr::col("kind").eq("folder"))
            .and_where(d.json_type(Expr::col("config"), "$.path").eq("text")),
        |row| row.get(0),
    )?;
    Ok((rw, ro))
}

pub enum Subject<'a> {
    Project(&'a str),
    Item(&'a str),
    Session(&'a str),
    KnowledgeSource(&'a str),
}

pub fn owner(r: &mut impl Read, subject: Subject<'_>) -> Result<Option<String>> {
    let (table, id) = match subject {
        Subject::Project(id) => ("projects", id),
        Subject::Item(id) => ("items", id),
        Subject::Session(id) => ("sessions", id),
        Subject::KnowledgeSource(id) => ("kb_sources", id),
    };
    r.one(
        Query::select()
            .column("workspace_id")
            .from(table)
            .and_where(Expr::col("id").eq(id)),
        |row| row.get(0),
    )
}
