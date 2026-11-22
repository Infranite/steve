use crate::{Read, Result};
use sea_query::{Expr, ExprTrait, Query};

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
