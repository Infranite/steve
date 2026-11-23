//! Agent session and event projections written with the ingest cursor.

use sea_query::{Expr, ExprTrait, Func, Order, Query, SelectStatement};
use steve_node_api::{Role, SessionView};

use crate::{Read, Result, W};

pub fn effective(w: &mut W<'_>, session: &str, view: &SessionView, at: i64) -> Result<()> {
    let resume = view.resume.as_ref().map(ToString::to_string);
    w.exec(
        Query::update()
            .table("sessions")
            .value(
                "model",
                Func::coalesce([
                    Expr::val(view.effective.model.as_deref()),
                    Expr::col("model"),
                ]),
            )
            .value(
                "reasoning_effort",
                Func::coalesce([
                    Expr::val(view.effective.effort.as_deref()),
                    Expr::col("reasoning_effort"),
                ]),
            )
            .value(
                "acp_session_id",
                Func::coalesce([Expr::val(resume), Expr::col("acp_session_id")]),
            )
            .value("updated", at)
            .and_where(Expr::col("id").eq(session)),
    )?;
    super::changed_event(w, session)
}

pub fn context(
    w: &mut W<'_>,
    session: &str,
    used: Option<i64>,
    size: Option<i64>,
    delta: Option<f64>,
    at: i64,
) -> Result<()> {
    let mut q = Query::update();
    q.table("sessions")
        .value("used", Func::coalesce([Expr::val(used), Expr::col("used")]))
        .value("size", Func::coalesce([Expr::val(size), Expr::col("size")]))
        .value("updated", at)
        .and_where(Expr::col("id").eq(session));
    if let Some(delta) = delta {
        q.value(
            "cost",
            Func::coalesce([Expr::col("cost"), Expr::Constant(0_i64.into())]).add(delta),
        );
    }
    w.exec(&q)?;
    super::changed_event(w, session)
}

pub fn first_user_since_turn(c: &mut impl Read, session: &str) -> Result<Option<i64>> {
    let boundary = Query::select()
        .expr(Func::coalesce([
            Func::max(Expr::col("seq")).into(),
            Expr::Constant(0_i64.into()),
        ]))
        .from("events")
        .and_where(Expr::col("session_id").eq(session))
        .and_where(Expr::col("kind").eq("turn_end"))
        .take();
    Ok(c.one(
        Query::select()
            .expr(Func::min(Expr::col("seq")))
            .from("events")
            .and_where(Expr::col("session_id").eq(session))
            .and_where(Expr::col("kind").eq("user"))
            .and_where(Expr::col("seq").gt(Expr::from(boundary))),
        |r| r.get::<Option<i64>>(0),
    )?
    .flatten())
}

pub fn has_system_code(c: &mut impl Read, session: &str, code: &str) -> Result<bool> {
    let field = c.db().dialect().json_field(Expr::col("data"), "code");
    Ok(c.one(
        Query::select()
            .column("seq")
            .from("events")
            .and_where(Expr::col("session_id").eq(session))
            .and_where(Expr::col("kind").eq("system"))
            .and_where(field.eq(code))
            .limit(1),
        |_| Ok(()),
    )?
    .is_some())
}

fn message_start(
    dialect: crate::contract::Dialect,
    session: &str,
    message: &str,
) -> SelectStatement {
    // These literals match the partial expression index exactly.
    Query::select()
        .expr(dialect.json_field(Expr::col("data"), "role"))
        .from("events")
        .and_where(Expr::col("session_id").eq(session))
        .and_where(Expr::col("kind").eq(Expr::Constant("message".into())))
        .and_where(Expr::col("format").eq(Expr::Constant("steve.v1".into())))
        .and_where(dialect.json_field(Expr::col("data"), "message").eq(message))
        .order_by("seq", Order::Desc)
        .limit(1)
        .take()
}

fn role(value: Option<String>) -> Option<Role> {
    match value.as_deref()? {
        "assistant" => Some(Role::Assistant),
        "thought" => Some(Role::Thought),
        "user" => Some(Role::User),
        _ => None,
    }
}

pub fn message_role(c: &mut impl Read, session: &str, message: &str) -> Result<Option<Role>> {
    let q = message_start(c.db().dialect(), session, message);
    Ok(role(c.one(&q, |r| r.get::<Option<String>>(0))?.flatten()))
}

#[cfg(feature = "testkit")]
pub fn message_role_steps(
    w: &mut W<'_>,
    session: &str,
    message: &str,
) -> Result<(Option<Role>, i32)> {
    let dialect = w.dialect();
    let (sql, values) = dialect.render(&message_start(dialect, session, message))?;
    let (text, steps) = crate::contract::sqlite::test_message_role_steps(w.conn, &sql, &values)?;
    Ok((role(text), steps))
}
