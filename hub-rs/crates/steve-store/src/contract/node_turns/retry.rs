//! Durable observations used when retrying Node attempts.

use sea_query::{Expr, ExprTrait, Func, JoinType, Query};

use crate::{Read, Result, StoreError, W};

pub fn first_failure(r: &mut impl Read, turn: &str, reason: &str) -> Result<Option<i64>> {
    let d = r.db().dialect();
    Ok(r.one(
        Query::select()
            .expr(Func::min(Expr::col("updated")))
            .from("runtime_attempts")
            .and_where(Expr::col("turn_id").eq(turn))
            .and_where(
                d.json_field(Expr::col("completion"), "stop_reason")
                    .eq(reason),
            ),
        |row| row.get::<Option<i64>>(0),
    )?
    .flatten())
}

pub struct CapacityWait {
    pub operation: String,
    pub attempt: String,
    pub session: String,
    pub error: String,
    pub deadline: i64,
    pub retries: i64,
}

pub fn unsent_capacity_waits(r: &mut impl Read, node: &str) -> Result<Vec<CapacityWait>> {
    let d = r.db().dialect();
    let field = |path| d.json_path(Expr::col(("c", "receipt")), path);
    let deadline = field("$.provider_capacity.deadline");
    r.all(
        Query::select()
            .columns([
                ("c", "operation_id"),
                ("a", "attempt_id"),
                ("t", "session_id"),
            ])
            .expr(field("$.provider_capacity.error"))
            .expr(deadline.clone())
            .expr(field("$.provider_capacity.retries"))
            .from_as("node_commands", "c")
            .join_as(
                JoinType::InnerJoin,
                "runtime_attempts",
                "a",
                Expr::col(("a", "attempt_id")).eq(Expr::col(("c", "target_attempt"))),
            )
            .join_as(
                JoinType::InnerJoin,
                "runtime_turns",
                "t",
                Expr::col(("t", "turn_id")).eq(Expr::col(("a", "turn_id"))),
            )
            .and_where(Expr::col(("c", "node_id")).eq(node))
            .and_where(Expr::col(("c", "method")).eq("start_turn"))
            .and_where(Expr::col(("c", "state")).eq("pending"))
            .and_where(Expr::col(("c", "maybe_sent")).eq(false))
            .and_where(Expr::col(("a", "state")).eq("planned"))
            .and_where(deadline.is_not_null()),
        |row| {
            Ok(CapacityWait {
                operation: row.get(0)?,
                attempt: row.get(1)?,
                session: row.get(2)?,
                error: row.get(3)?,
                deadline: row.get(4)?,
                retries: row.get(5)?,
            })
        },
    )
}

/// Read at most the last 1024 characters of the last assistant message. Stop walking the
/// descending index when the message changes or the buffer is full.
pub fn assistant_tail(w: &mut W<'_>, node: &str, session: &str, attempt: &str) -> Result<String> {
    let mut st = w.conn.prepare_cached(
        "SELECT json_extract(data,'$.message'), json_extract(data,'$.text') FROM events
         WHERE session_id=?3 AND source_node=?1 AND source_attempt=?2
           AND kind='text' AND json_extract(data,'$.role')='assistant'
         ORDER BY seq DESC",
    )?;
    let mut rows = st.query([node, attempt, session])?;
    let mut tail = Vec::new();
    let mut last_message = None;
    while let Some(row) = rows.next()? {
        let message: String = row.get(0)?;
        if last_message.as_ref().is_some_and(|m| *m != message) {
            break;
        }
        last_message = Some(message);
        let text: String = row.get(1)?;
        tail.extend(text.chars().rev().take(1024 - tail.len()));
        if tail.len() == 1024 {
            break;
        }
    }
    Ok(tail.into_iter().rev().collect())
}

pub fn allowed(r: &mut impl Read, session: &str) -> Result<bool> {
    let open = Query::select()
        .column(("d", "id"))
        .from_as("decisions", "d")
        .and_where(Expr::col(("d", "item_id")).eq(Expr::col(("i", "id"))))
        .and_where(Expr::col(("d", "status")).eq("open"))
        .take();
    let allowed = Expr::col(("s", "status"))
        .ne("ended")
        .and(
            Expr::col(("i", "id"))
                .is_null()
                .or(Expr::col(("i", "phase"))
                    .eq("active")
                    .and(Expr::col(("i", "archived")).eq(false))),
        )
        .and(Expr::exists(open).not());
    r.one(
        Query::select()
            .expr(allowed)
            .from_as("sessions", "s")
            .join_as(
                JoinType::LeftJoin,
                "items",
                "i",
                Expr::col(("i", "id")).eq(Expr::col(("s", "item_id"))),
            )
            .and_where(Expr::col(("s", "id")).eq(session)),
        |row| row.get(0),
    )?
    .ok_or_else(|| StoreError::Rejected(format!("missing session {session}")))
}
