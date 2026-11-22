use crate::{Read, Result};
use sea_query::{Expr, ExprTrait, Order, Query};

/// The columns of a project this module reads.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectBrief {
    pub name: String,
    pub kind: String,
    pub path: String,
    pub archived: bool,
    pub device_id: String,
}

/// A project in a workspace.
pub fn project(c: &mut impl Read, ws: &str, id: &str) -> Result<Option<ProjectBrief>> {
    c.one(
        Query::select()
            .columns(["name", "kind", "path", "archived", "device_id"])
            .from("projects")
            .and_where(Expr::col("id").eq(id))
            .and_where(Expr::col("workspace_id").eq(ws)),
        |r| {
            Ok(ProjectBrief {
                name: r.get(0)?,
                kind: r.get(1)?,
                path: r.get(2)?,
                archived: r.get::<i64>(3)? != 0,
                device_id: r.get(4)?,
            })
        },
    )
}

/// `(id, name, path)` of the workspace's git projects on the Hub machine, not archived.
pub fn git_projects(c: &mut impl Read, ws: &str) -> Result<Vec<(String, String, String)>> {
    c.all(
        Query::select()
            .columns(["id", "name", "path"])
            .from("projects")
            .and_where(Expr::col("workspace_id").eq(ws))
            .and_where(Expr::col("kind").eq("git"))
            .and_where(Expr::col("archived").eq(0_i64))
            .and_where(Expr::col("device_id").eq("hub"))
            .order_by("name", Order::Asc),
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
    )
}

/// The project's release action, if it has one (accepted ≠ finished there).
pub fn project_release(c: &mut impl Read, ws: &str, id: &str) -> Result<Option<String>> {
    Ok(c.one(
        Query::select()
            .columns(["release"])
            .from("projects")
            .and_where(Expr::col("id").eq(id))
            .and_where(Expr::col("workspace_id").eq(ws)),
        |r| r.get::<Option<String>>(0),
    )?
    .flatten()
    .filter(|r| !r.trim().is_empty()))
}

pub fn named_projects(c: &mut impl Read, ws: &str, name: &str) -> Result<Vec<String>> {
    c.all(
        Query::select()
            .columns(["id"])
            .from("projects")
            .and_where(Expr::col("workspace_id").eq(ws))
            .and_where(Expr::col("archived").eq(0_i64))
            .and_where(Expr::col("id").eq(name).or(Expr::col("name").eq(name)))
            .order_by_expr(Expr::col("id").eq(name), Order::Desc)
            .limit(2),
        |r| r.get(0),
    )
}
