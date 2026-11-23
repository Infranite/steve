//! Session and permission observations checked after a Node reconnects.

use sea_query::{Expr, ExprTrait, JoinType, Query};

use crate::{Read, Result};

pub fn waiting_sessions(r: &mut impl Read, node: &str) -> Result<Vec<String>> {
    let queue = Query::select()
        .column(("q", "id"))
        .from_as("queued", "q")
        .and_where(Expr::col(("q", "session_id")).eq(Expr::col(("r", "session_id"))))
        .take();
    r.all(
        Query::select()
            .column(("r", "session_id"))
            .from_as("runtime_sessions", "r")
            .and_where(Expr::col(("r", "node_id")).eq(node))
            .and_where(
                Expr::col(("r", "desired_config"))
                    .is_not_null()
                    .or(Expr::exists(queue)),
            ),
        |row| row.get(0),
    )
}

pub fn open_permissions(r: &mut impl Read, node: &str) -> Result<Vec<(String, String)>> {
    r.all(
        Query::select()
            .columns([("d", "id"), ("d", "subject_id")])
            .from_as("decisions", "d")
            .join_as(
                JoinType::InnerJoin,
                "runtime_sessions",
                "s",
                Expr::col(("s", "session_id")).eq(Expr::col(("d", "session_id"))),
            )
            .and_where(Expr::col(("s", "node_id")).eq(node))
            .and_where(Expr::col(("d", "subject_type")).eq("tool_call"))
            .and_where(Expr::col(("d", "status")).eq("open")),
        |row| Ok((row.get(0)?, row.get(1)?)),
    )
}

pub fn seen_view(r: &mut impl Read, node: &str, session: &str, revision: &str) -> Result<bool> {
    let mut views = r
        .db()
        .dialect()
        .json_rows(Expr::col(("s", "node_views")), "v");
    views.and_where(Expr::col(("v", "value")).eq(revision));
    Ok(r.one(
        Query::select()
            .column(("s", "session_id"))
            .from_as("runtime_sessions", "s")
            .and_where(Expr::col(("s", "session_id")).eq(session))
            .and_where(Expr::col(("s", "node_id")).eq(node))
            .and_where(Expr::exists(views)),
        |_| Ok(()),
    )?
    .is_some())
}
