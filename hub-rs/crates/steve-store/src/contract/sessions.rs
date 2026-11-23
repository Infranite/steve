use crate::{Read, Result};
use sea_query::{Expr, ExprTrait, LikeExpr, Query};

pub fn cwd(c: &mut impl Read, ws: &str, session: &str) -> Result<Option<String>> {
    Ok(c.one(
        Query::select()
            .column("cwd")
            .from("sessions")
            .and_where(Expr::col("workspace_id").eq(ws))
            .and_where(Expr::col("id").eq(session)),
        |r| r.get(0),
    )?
    .flatten())
}

pub fn effective_model(c: &mut impl Read, ws: &str, session: &str) -> Result<Option<String>> {
    let row = c.one(
        Query::select()
            .columns(["model", "bot_id"])
            .from("sessions")
            .and_where(Expr::col("workspace_id").eq(ws))
            .and_where(Expr::col("id").eq(session)),
        |r| Ok((r.get::<Option<String>>(0)?, r.get::<Option<String>>(1)?)),
    )?;
    let Some((model, bot)) = row else {
        return Ok(None);
    };
    if let Some(model) = model.filter(|m| !m.is_empty()) {
        return Ok(Some(model));
    }
    match bot {
        Some(bot) => Ok(super::bots::model(c, &bot)?.filter(|m| !m.is_empty())),
        None => Ok(None),
    }
}

pub fn with_queued_input(c: &mut impl Read) -> Result<Vec<String>> {
    c.all(
        Query::select()
            .distinct()
            .column(("q", "session_id"))
            .from_as("queued", "q")
            .join_as(
                sea_query::JoinType::InnerJoin,
                "sessions",
                "s",
                Expr::col(("s", "id")).eq(Expr::col(("q", "session_id"))),
            )
            .and_where(Expr::col(("s", "status")).ne("ended")),
        |row| row.get(0),
    )
}

pub fn has_dispatch_prefix(c: &mut impl Read, session: &str, prefix: &str) -> Result<bool> {
    let literal = prefix
        .replace('\\', "\\\\")
        .replace('%', "\\%")
        .replace('_', "\\_");
    c.one(
        Query::select()
            .column("session_id")
            .from("session_dispatch")
            .and_where(Expr::col("session_id").eq(session))
            .and_where(Expr::col("text").like(LikeExpr::new(format!("{literal}%")).escape('\\')))
            .limit(1),
        |_| Ok(()),
    )
    .map(|row| row.is_some())
}

pub fn item_cost(c: &mut impl Read, item: &str) -> Result<Option<f64>> {
    Ok(c.one(
        Query::select()
            .expr(sea_query::Func::sum(Expr::col("cost")))
            .from("sessions")
            .and_where(Expr::col("item_id").eq(item)),
        |r| r.get::<Option<f64>>(0),
    )?
    .flatten())
}

pub fn has_recent_queued_input(c: &mut impl Read, item: &str, since: i64) -> Result<bool> {
    c.one(
        Query::select()
            .column(("q", "id"))
            .from_as("queued", "q")
            .join_as(
                sea_query::JoinType::InnerJoin,
                "sessions",
                "s",
                Expr::col(("s", "id")).eq(Expr::col(("q", "session_id"))),
            )
            .and_where(Expr::col(("s", "item_id")).eq(item))
            .and_where(Expr::col(("q", "via")).eq("me"))
            .and_where(Expr::col(("q", "created")).gte(since))
            .limit(1),
        |_| Ok(()),
    )
    .map(|row| row.is_some())
}

pub fn latest_system_text(c: &mut impl Read, session: &str) -> Result<Option<String>> {
    let dialect = c.db().dialect();
    Ok(c.one(
        Query::select()
            .expr(dialect.json_field(Expr::col("data"), "text"))
            .from("events")
            .and_where(Expr::col("session_id").eq(session))
            .and_where(Expr::col("kind").eq("system"))
            .order_by("seq", sea_query::Order::Desc)
            .limit(1),
        |row| row.get::<Option<String>>(0),
    )?
    .flatten())
}
