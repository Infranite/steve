use sea_query::{Expr, ExprTrait, Func, Iden, OnConflict, Order, Query, SelectStatement};

use crate::contract::Row;
use crate::{Read, Result, W};

const EXTERNAL: &str = "ext";

#[derive(Iden, Clone, Copy)]
enum Column {
    ConnectionId,
    WorkspaceId,
    Container,
    Kind,
    ChatId,
    Cursor,
    Boundary,
    LastNew,
    Polled,
    Error,
    ErrorAt,
    Created,
    Updated,
}

/// Timestamp and boundary IDs for a Feishu chat or thread.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PollPosition {
    pub container: String,
    pub kind: String,
    pub chat_id: String,
    pub cursor: Option<i64>,
    pub boundary: Vec<String>,
    pub last_new: Option<i64>,
    pub polled: Option<i64>,
    pub error: Option<String>,
    pub error_at: Option<i64>,
}

fn polls(connection: &str) -> SelectStatement {
    Query::select()
        .columns(["container", "kind", "chat_id"])
        .column(Column::Cursor)
        .columns(["boundary", "last_new", "polled", "error", "error_at"])
        .from("chan_cursors")
        .and_where(Expr::col("connection_id").eq(connection))
        .take()
}

fn poll_row(row: &Row<'_>) -> Result<PollPosition> {
    Ok(PollPosition {
        container: row.get(0)?,
        kind: row.get(1)?,
        chat_id: row.get(2)?,
        cursor: row.get(3)?,
        boundary: serde_json::from_str(&row.get::<String>(4)?).unwrap_or_default(),
        last_new: row.get(5)?,
        polled: row.get(6)?,
        error: row.get(7)?,
        error_at: row.get(8)?,
    })
}

pub fn poll(r: &mut impl Read, connection: &str, container: &str) -> Result<Option<PollPosition>> {
    r.one(
        polls(connection).and_where(Expr::col("container").eq(container)),
        poll_row,
    )
}

pub fn active_threads(
    r: &mut impl Read,
    connection: &str,
    since: i64,
    limit: u64,
) -> Result<Vec<PollPosition>> {
    r.all(
        polls(connection)
            .and_where(Expr::col("kind").eq("thread"))
            .and_where(Expr::col("last_new").gte(since))
            .order_by_expr(
                Expr::from(Func::coalesce([Expr::col("polled"), Expr::val(0_i64)])),
                Order::Asc,
            )
            .order_by("container", Order::Asc)
            .limit(limit),
        poll_row,
    )
}

pub fn save_poll(
    w: &mut W<'_>,
    ws: &str,
    connection: &str,
    cursor: &PollPosition,
    now: i64,
) -> Result<()> {
    use Column::*;
    w.exec(
        Query::insert()
            .into_table("chan_cursors")
            .columns([
                ConnectionId,
                WorkspaceId,
                Container,
                Kind,
                ChatId,
                Cursor,
                Boundary,
                LastNew,
                Polled,
                Error,
                ErrorAt,
                Created,
                Updated,
            ])
            .values_panic([
                connection.into(),
                ws.into(),
                cursor.container.as_str().into(),
                cursor.kind.as_str().into(),
                cursor.chat_id.as_str().into(),
                cursor.cursor.into(),
                serde_json::to_string(&cursor.boundary)?.into(),
                cursor.last_new.into(),
                cursor.polled.into(),
                cursor.error.as_deref().into(),
                cursor.error_at.into(),
                now.into(),
                now.into(),
            ])
            .on_conflict(
                OnConflict::columns(["connection_id", "container"])
                    .update_columns([Cursor, Boundary, LastNew, Polled, Error, ErrorAt, Updated])
                    .to_owned(),
            ),
    )?;
    Ok(())
}

pub fn external_token(
    r: &mut impl Read,
    connection: &str,
    container: &str,
) -> Result<Option<String>> {
    Ok(r.one(
        Query::select()
            .column("token")
            .from("chan_cursors")
            .and_where(Expr::col("connection_id").eq(connection))
            .and_where(Expr::col("container").eq(container))
            .and_where(Expr::col("kind").eq(EXTERNAL)),
        |row| row.get::<Option<String>>(0),
    )?
    .flatten())
}

/// Returns false when the container already belongs to the self poller.
pub fn save_external_token(
    w: &mut W<'_>,
    ws: &str,
    connection: &str,
    container: &str,
    token: &str,
    now: i64,
) -> Result<bool> {
    Ok(w.exec(
        Query::insert()
            .into_table("chan_cursors")
            .columns([
                "connection_id",
                "workspace_id",
                "container",
                "kind",
                "chat_id",
                "token",
                "created",
                "updated",
            ])
            .values_panic([
                connection.into(),
                ws.into(),
                container.into(),
                EXTERNAL.into(),
                container.into(),
                token.into(),
                now.into(),
                now.into(),
            ])
            .on_conflict(
                OnConflict::columns(["connection_id", "container"])
                    .update_columns(["token", "updated"])
                    .action_and_where(Expr::col(("chan_cursors", "kind")).eq(EXTERNAL))
                    .to_owned(),
            ),
    )? == 1)
}

pub(crate) fn remove_connection(w: &mut W<'_>, connection: &str) -> Result<()> {
    w.exec(
        Query::delete()
            .from_table("chan_cursors")
            .and_where(Expr::col("connection_id").eq(connection)),
    )?;
    Ok(())
}

#[cfg(feature = "testkit")]
pub fn count(r: &mut impl Read, connection: &str) -> Result<i64> {
    Ok(r.one(
        Query::select()
            .expr(Expr::cust("count(*)"))
            .from("chan_cursors")
            .and_where(Expr::col("connection_id").eq(connection)),
        |row| row.get(0),
    )?
    .unwrap_or_default())
}
