//! Bot Team configuration and actual placement facts.

use sea_query::{Expr, ExprTrait, JoinType, OnConflict, Query};

use crate::{Read, Result, W, bot_dispatch::Member};

pub fn selectable(r: &mut impl Read, ws: &str, bot: &str) -> Result<bool> {
    Ok(r.one(
        Query::select()
            .column(("b", "id"))
            .from_as("bots", "b")
            .join_as(
                JoinType::LeftJoin,
                "bot_workspaces",
                "w",
                Expr::col(("w", "bot_id"))
                    .eq(Expr::col(("b", "id")))
                    .and(Expr::col(("w", "workspace_id")).eq(ws)),
            )
            .and_where(Expr::col(("b", "id")).eq(bot))
            .and_where(Expr::col(("b", "deleted")).eq(false))
            .and_where(Expr::col(("b", "main")).eq(false))
            .and_where(Expr::col(("w", "bot_id")).is_not_null()),
        |_| Ok(()),
    )?
    .is_some())
}

pub fn set_bindings(w: &mut W<'_>, ws: &str, bot: &str, nodes: &[String]) -> Result<()> {
    w.exec(
        Query::insert()
            .into_table("bot_node_bindings")
            .columns(["workspace_id", "bot_id", "nodes"])
            .values_panic([ws.into(), bot.into(), serde_json::to_string(nodes)?.into()])
            .on_conflict(
                OnConflict::columns(["workspace_id", "bot_id"])
                    .update_column("nodes")
                    .to_owned(),
            ),
    )?;
    Ok(())
}

pub fn is_main_bot(r: &mut impl Read, ws: &str, bot: &str) -> Result<bool> {
    Ok(r.one(
        Query::select()
            .column(("b", "id"))
            .from_as("bots", "b")
            .join_as(
                JoinType::LeftJoin,
                "bot_workspaces",
                "w",
                Expr::col(("w", "bot_id"))
                    .eq(Expr::col(("b", "id")))
                    .and(Expr::col(("w", "workspace_id")).eq(ws)),
            )
            .and_where(Expr::col(("b", "id")).eq(bot))
            .and_where(Expr::col(("b", "deleted")).eq(false))
            .and_where(Expr::col(("b", "main")).eq(true))
            .and_where(
                Expr::col(("b", "workspace_id"))
                    .eq(ws)
                    .or(Expr::col(("w", "bot_id")).is_not_null()),
            ),
        |_| Ok(()),
    )?
    .is_some())
}

pub fn save(
    w: &mut W<'_>,
    ws: &str,
    id: Option<&str>,
    name: &str,
    members: &[Member],
) -> Result<String> {
    for member in members {
        if is_main_bot(w, ws, &member.bot)? {
            return Err(crate::StoreError::Rejected(
                "Main bot is the workspace coordinator and cannot be a Team member".into(),
            ));
        }
    }
    let members = serde_json::to_string(members)?;
    if let Some(id) = id {
        let n = w.exec(
            Query::update()
                .table("bot_teams")
                .value("name", name)
                .value("members", members)
                .and_where(Expr::col("workspace_id").eq(ws))
                .and_where(Expr::col("id").eq(id)),
        )?;
        if n == 0 {
            return Err(crate::StoreError::NotFound("Bot Team"));
        }
        return Ok(id.to_string());
    }
    let id = crate::uid("team");
    w.exec(
        Query::insert()
            .into_table("bot_teams")
            .columns(["id", "workspace_id", "name", "members"])
            .values_panic([id.as_str().into(), ws.into(), name.into(), members.into()]),
    )?;
    Ok(id)
}

pub fn set_default(w: &mut W<'_>, ws: &str, team: Option<&str>) -> Result<()> {
    if let Some(team) = team {
        w.exec(
            Query::insert()
                .into_table("bot_team_defaults")
                .columns(["workspace_id", "team_id"])
                .values_panic([ws.into(), team.into()])
                .on_conflict(
                    OnConflict::column("workspace_id")
                        .update_column("team_id")
                        .to_owned(),
                ),
        )?;
    } else {
        w.exec(
            Query::delete()
                .from_table("bot_team_defaults")
                .and_where(Expr::col("workspace_id").eq(ws)),
        )?;
    }
    Ok(())
}

pub fn project_attached(r: &mut impl Read, item: &str, ws: &str) -> Result<bool> {
    r.one(
        Query::select()
            .expr(Expr::col("project_id").is_not_null())
            .from("items")
            .and_where(Expr::col("id").eq(item))
            .and_where(Expr::col("workspace_id").eq(ws)),
        |row| row.get(0),
    )?
    .ok_or(crate::StoreError::NotFound("Item"))
}

pub fn wrong_node(
    r: &mut impl Read,
    item: &str,
    session: Option<&str>,
    node: &str,
) -> Result<bool> {
    let mut q = Query::select();
    q.column(("s", "id"))
        .from_as("sessions", "s")
        .join_as(
            JoinType::InnerJoin,
            "runtime_sessions",
            "r",
            Expr::col(("r", "session_id")).eq(Expr::col(("s", "id"))),
        )
        .and_where(Expr::col(("s", "item_id")).eq(item))
        .and_where(Expr::col(("s", "readonly")).eq(false))
        .and_where(Expr::col(("s", "source")).eq("Steve"))
        .limit(1);
    if let Some(session) = session {
        q.and_where(Expr::col(("s", "id")).ne(session));
    }
    q.and_where(Expr::col(("r", "node_id")).ne(node));
    Ok(r.one(&q, |_| Ok(()))?.is_some())
}

/// The execution Node an item's non-readonly Steve sessions are already bound to, if any.
pub fn bound_node(r: &mut impl Read, item: &str) -> Result<Option<String>> {
    let mut q = Query::select();
    q.column(("r", "node_id"))
        .from_as("sessions", "s")
        .join_as(
            JoinType::InnerJoin,
            "runtime_sessions",
            "r",
            Expr::col(("r", "session_id")).eq(Expr::col(("s", "id"))),
        )
        .and_where(Expr::col(("s", "item_id")).eq(item))
        .and_where(Expr::col(("s", "readonly")).eq(false))
        .and_where(Expr::col(("s", "source")).eq("Steve"))
        .limit(1);
    r.one(&q, |row| row.get(0))
}

pub fn harness_nodes(r: &mut impl Read, bot: &str) -> Result<std::collections::BTreeSet<String>> {
    let mut nodes = std::collections::BTreeSet::new();
    let parameter = sea_query::Values(vec![bot.into()]);
    r.db().query(
        "SELECT r.node_id FROM runtime_sessions r JOIN sessions s ON s.id=r.session_id
           WHERE s.bot_id=?1 AND s.status<>'ended'
         UNION
         SELECT n.value FROM bot_node_bindings b JOIN bot_workspaces w
           ON w.workspace_id=b.workspace_id AND w.bot_id=b.bot_id, json_each(b.nodes) n
           WHERE b.bot_id=?1",
        &parameter,
        &mut |row| {
            nodes.insert(row.get(0)?);
            Ok(())
        },
    )?;
    Ok(nodes)
}
