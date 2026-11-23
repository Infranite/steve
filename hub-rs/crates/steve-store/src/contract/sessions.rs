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
