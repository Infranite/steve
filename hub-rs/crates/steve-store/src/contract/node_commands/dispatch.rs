//! Durable send deadlines, retry observations and command dependencies.

use sea_query::{CaseStatement, Expr, ExprTrait, Func, JoinType, Order, Query};
use steve_node_api::ApiError;

use crate::{Read, Result, W};

const TABLE: &str = "node_commands";

pub fn next_retry(c: &mut impl Read, node: &str, now: i64) -> Result<Option<i64>> {
    Ok(c.one(
        Query::select()
            .expr(Func::min(Expr::col("retry_at")))
            .from(TABLE)
            .and_where(Expr::col("node_id").eq(node))
            .and_where(Expr::col("state").eq("pending"))
            .and_where(Expr::col("retry_at").gt(now)),
        |r| r.get::<Option<i64>>(0),
    )?
    .flatten())
}

pub fn method(c: &mut impl Read, operation: &str, node: &str) -> Result<Option<String>> {
    c.one(
        Query::select()
            .column("method")
            .from(TABLE)
            .and_where(Expr::col("operation_id").eq(operation))
            .and_where(Expr::col("node_id").eq(node)),
        |r| r.get(0),
    )
}

fn receipt() -> Expr {
    Func::coalesce([Expr::col("receipt"), Expr::Constant("{}".into())]).into()
}

pub fn record_retry_error(w: &mut W<'_>, operation: &str, error: &ApiError) -> Result<()> {
    let value = w.dialect().json_set_field(
        receipt(),
        "last_error",
        Expr::val(serde_json::to_string(error)?),
    );
    w.exec(
        Query::update()
            .table(TABLE)
            .value("receipt", value)
            .and_where(Expr::col("operation_id").eq(operation))
            .and_where(Expr::col("state").eq("pending")),
    )?;
    Ok(())
}

pub fn mark_waiting(w: &mut W<'_>, op: &str) -> Result<()> {
    let value = w
        .dialect()
        .json_set_field(receipt(), "waiting", Expr::val("1"));
    w.exec(
        Query::update()
            .table(TABLE)
            .value("receipt", value)
            .and_where(Expr::col("operation_id").eq(op)),
    )?;
    Ok(())
}

pub fn record_exhausted(w: &mut W<'_>, op: &str, error: &ApiError) -> Result<i64> {
    use crate::contract::Q;
    let dialect = w.dialect();
    let before = w
        .one(
            Query::select()
                .expr(dialect.json_field(Expr::col("receipt"), "exhausted"))
                .from(TABLE)
                .and_where(Expr::col("operation_id").eq(op))
                .and_where(Expr::col("state").eq("pending")),
            |r| r.get::<Option<i64>>(0),
        )?
        .flatten();
    let count = before.unwrap_or(0) + 1;
    let value = dialect.json_set_field(receipt(), "success", Expr::val("false"));
    let value = dialect.json_set_field(value, "payload", Expr::val(serde_json::to_string(error)?));
    let value = dialect.json_set_field(value, "exhausted", Expr::val(count.to_string()));
    w.exec(
        Query::update()
            .table(TABLE)
            .value("receipt", value)
            .and_where(Expr::col("operation_id").eq(op))
            .and_where(Expr::col("state").eq("pending")),
    )?;
    Ok(count)
}

pub fn pending_dependents(c: &mut impl Read, operation: &str) -> Result<Vec<String>> {
    c.all(
        Query::select()
            .column("operation_id")
            .from(TABLE)
            .and_where(Expr::col("dependency_operation").eq(operation))
            .and_where(Expr::col("state").eq("pending")),
        |r| r.get(0),
    )
}

pub fn release_failures(c: &mut impl Read, session: &str) -> Result<i64> {
    let dialect = c.db().dialect();
    Ok(c.one(
        Query::select()
            .expr(Func::count(Expr::col("operation_id")))
            .from(TABLE)
            .and_where(Expr::col("method").eq("release_session"))
            .and_where(dialect.json_field(Expr::col("request"), "body").eq(session))
            .and_where(Expr::col("state").is_in(["finished", "rejected"]))
            .and_where(
                dialect
                    .json_field(Expr::col("receipt"), "success")
                    .eq(false),
            ),
        |r| r.get(0),
    )?
    .unwrap_or(0))
}

pub fn postpone(w: &mut W<'_>, op: &str, retry: i64, at: i64) -> Result<()> {
    w.exec(
        Query::update()
            .table(TABLE)
            .value("retry_at", retry)
            .value("updated", at)
            .and_where(Expr::col("operation_id").eq(op))
            .and_where(Expr::col("state").eq("pending")),
    )?;
    Ok(())
}

pub fn retarget_dependency(w: &mut W<'_>, op: &str, dependency: &str, at: i64) -> Result<()> {
    w.exec(
        Query::update()
            .table(TABLE)
            .value("dependency_operation", dependency)
            .value("updated", at)
            .and_where(Expr::col("operation_id").eq(op))
            .and_where(Expr::col("state").eq("pending")),
    )?;
    Ok(())
}

pub fn refresh_envelope(
    w: &mut W<'_>,
    op: &str,
    envelope: &steve_node_api::Command<serde_json::Value>,
    digest: &str,
    at: i64,
) -> Result<()> {
    w.exec(
        Query::update()
            .table(TABLE)
            .value("request", serde_json::to_string(envelope)?)
            .value("digest", digest)
            .value("updated", at)
            .and_where(Expr::col("operation_id").eq(op))
            .and_where(Expr::col("state").eq("pending")),
    )?;
    Ok(())
}

fn request_session(dialect: crate::contract::Dialect) -> Expr {
    CaseStatement::new()
        .case(
            Expr::col(("c", "method")).eq("release_session"),
            dialect.json_field(Expr::col(("c", "request")), "body"),
        )
        .finally(dialect.json_path(Expr::col(("c", "request")), "$.body.session"))
        .into()
}

pub fn session(c: &mut impl Read, op: &str) -> Result<String> {
    let session = request_session(c.db().dialect());
    c.one(
        Query::select()
            .expr(session)
            .from_as(TABLE, "c")
            .and_where(Expr::col(("c", "operation_id")).eq(op)),
        |r| r.get(0),
    )?
    .ok_or_else(|| crate::StoreError::Rejected(format!("missing command {op}")))
}

pub fn any_session(c: &mut impl Read, op: &str) -> Result<Option<String>> {
    let dialect = c.db().dialect();
    let target = Query::select()
        .column(("t", "session_id"))
        .from_as("runtime_attempts", "a")
        .join_as(
            JoinType::InnerJoin,
            "runtime_turns",
            "t",
            Expr::col(("t", "turn_id")).eq(Expr::col(("a", "turn_id"))),
        )
        .and_where(Expr::col(("a", "attempt_id")).eq(Expr::col(("c", "target_attempt"))))
        .take();
    let waiting = Query::select()
        .expr(dialect.json_path(Expr::col(("d", "request")), "$.body.session"))
        .from_as(TABLE, "d")
        .and_where(Expr::col(("d", "dependency_operation")).eq(Expr::col(("c", "operation_id"))))
        .order_by(("d", "created"), Order::Asc)
        .order_by_expr(dialect.insertion_order(Some("d")), Order::Asc)
        .limit(1)
        .take();
    Ok(c.one(
        Query::select()
            .expr(Func::coalesce([
                request_session(dialect),
                Expr::from(target),
                Expr::from(waiting),
            ]))
            .from_as(TABLE, "c")
            .and_where(Expr::col(("c", "operation_id")).eq(op)),
        |r| r.get::<Option<String>>(0),
    )?
    .flatten())
}

pub fn restart_only_abandon(c: &mut impl Read, attempt: &str, restart: &str) -> Result<bool> {
    let reason = c
        .db()
        .dialect()
        .json_path(Expr::col("request"), "$.body.reason");
    let mut commands = Query::select();
    commands
        .column("operation_id")
        .from(TABLE)
        .and_where(Expr::col("target_attempt").eq(attempt))
        .and_where(Expr::col("method").eq("abandon"))
        .limit(1);
    let restart = c
        .one(
            commands.clone().and_where(reason.clone().eq(restart)),
            |_| Ok(()),
        )?
        .is_some()
        && c.one(commands.and_where(reason.ne(restart)), |_| Ok(()))?
            .is_none();
    if !restart {
        return Ok(false);
    }
    let cancelled = c.one(
        Query::select()
            .column("cancel_requested")
            .from("runtime_attempts")
            .and_where(Expr::col("attempt_id").eq(attempt)),
        |r| r.get::<bool>(0),
    )?;
    Ok(cancelled == Some(false))
}

pub fn abandoned_turn(c: &mut impl Read, turn: &str) -> Result<bool> {
    Ok(c.one(
        Query::select()
            .column(("c", "operation_id"))
            .from_as(TABLE, "c")
            .join_as(
                JoinType::InnerJoin,
                "runtime_attempts",
                "a",
                Expr::col(("a", "attempt_id")).eq(Expr::col(("c", "target_attempt"))),
            )
            .and_where(Expr::col(("a", "turn_id")).eq(turn))
            .and_where(Expr::col(("c", "method")).eq("abandon"))
            .limit(1),
        |_| Ok(()),
    )?
    .is_some())
}

#[derive(serde::Serialize)]
pub struct CapacityWait<'a> {
    pub deadline: i64,
    pub error: &'a str,
    pub retries: i64,
}

pub fn schedule_turn_retry(
    w: &mut W<'_>,
    turn: &str,
    at: i64,
    capacity: Option<CapacityWait<'_>>,
) -> Result<()> {
    let target = Query::select()
        .column("active_attempt")
        .from("runtime_turns")
        .and_where(Expr::col("turn_id").eq(turn))
        .take();
    let mut q = Query::update();
    q.table(TABLE)
        .value("retry_at", at)
        .and_where(Expr::col("target_attempt").eq(Expr::from(target)))
        .and_where(Expr::col("method").eq("start_turn"));
    if let Some(capacity) = capacity {
        q.value(
            "receipt",
            w.dialect().json_set_field(
                receipt(),
                "provider_capacity",
                Expr::val(serde_json::to_string(&capacity)?),
            ),
        );
    }
    w.exec(&q)?;
    Ok(())
}
