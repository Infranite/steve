use sea_query::{Expr, ExprTrait, JoinType, OnConflict, Order, Query};

use crate::{Read, Result, W};

const EVERY_CHAT: &str = "";

pub fn explicit_request(
    r: &mut impl Read,
    ws: &str,
    connection: &str,
    space: &str,
    source: &str,
) -> Result<bool> {
    let Some(owner) = super::connections::owner(r, ws, connection)? else {
        return Ok(false);
    };
    let dialect = r.db().dialect();
    let mentioned = dialect
        .json_rows(Expr::col(("m", "mentions")), "mention")
        .and_where(
            dialect
                .json_field(Expr::col(("mention", "value")), "account_id")
                .eq(owner.account),
        )
        .take();
    Ok(r.one(
        Query::select()
            .column(("m", "id"))
            .from_as("chan_messages", "m")
            .join_as(
                JoinType::InnerJoin,
                "spaces",
                "s",
                Expr::col(("s", "id")).eq(Expr::col(("m", "space_id"))),
            )
            .and_where(Expr::col(("m", "id")).eq(source))
            .and_where(Expr::col(("m", "workspace_id")).eq(ws))
            .and_where(Expr::col(("m", "space_id")).eq(space))
            .and_where(Expr::col(("m", "direction")).eq("in"))
            .and_where(Expr::col(("m", "history")).is_null())
            .and_where(Expr::col(("m", "purged")).is_null())
            .and_where(
                Expr::col(("s", "kind"))
                    .eq("dm")
                    .or(Expr::exists(mentioned)),
            ),
        |_| Ok(()),
    )?
    .is_some())
}

/// A chat refusal, or a refusal affecting the whole connection when no chat is given.
pub struct WriteRefusal<'a> {
    pub ws: &'a str,
    pub connection: &'a str,
    pub chat: Option<&'a str>,
    pub reason: &'a str,
    pub now: i64,
    pub memory_ms: i64,
}

/// Persists the refusal. Returns true when a new notice is due.
pub fn record(w: &mut W<'_>, refusal: WriteRefusal<'_>) -> Result<bool> {
    use crate::contract::Q;
    let chat = refusal.chat.unwrap_or(EVERY_CHAT);
    let noticed = w
        .one(
            Query::select()
                .column("noticed_at")
                .from("chan_write_blocks")
                .and_where(Expr::col("connection_id").eq(refusal.connection))
                .and_where(Expr::col("space_id").eq(chat)),
            |r| r.get::<Option<i64>>(0),
        )?
        .flatten();
    let notice = noticed.is_none_or(|at| at <= refusal.now - refusal.memory_ms);
    w.exec(
        Query::insert()
            .into_table("chan_write_blocks")
            .columns([
                "connection_id",
                "space_id",
                "workspace_id",
                "reason",
                "blocked_at",
                "until",
                "noticed_at",
            ])
            .values_panic([
                refusal.connection.into(),
                chat.into(),
                refusal.ws.into(),
                refusal.reason.into(),
                refusal.now.into(),
                (refusal.now + refusal.memory_ms).into(),
                (if notice { Some(refusal.now) } else { noticed }).into(),
            ])
            .on_conflict(
                OnConflict::columns(["connection_id", "space_id"])
                    .update_columns(["reason", "blocked_at", "until", "noticed_at"])
                    .to_owned(),
            ),
    )?;
    Ok(notice)
}

/// A live refusal for the connection takes precedence over a refusal for just this chat.
pub fn active(
    r: &mut impl Read,
    connection: &str,
    space: &str,
    now: i64,
) -> Result<Option<String>> {
    r.one(
        Query::select()
            .column("reason")
            .from("chan_write_blocks")
            .and_where(Expr::col("connection_id").eq(connection))
            .and_where(Expr::col("space_id").is_in([EVERY_CHAT, space]))
            .and_where(Expr::col("until").gt(now))
            .order_by_expr(Expr::col("space_id").eq(EVERY_CHAT), Order::Desc)
            .limit(1),
        |r| r.get(0),
    )
}

/// Lifts the connection-wide refusal and optionally every chat refusal, keeping notice times.
pub fn lift_connection(w: &mut W<'_>, connection: &str, every_chat: bool) -> Result<bool> {
    let mut update = Query::update();
    update
        .table("chan_write_blocks")
        .value("until", 0_i64)
        .and_where(Expr::col("connection_id").eq(connection))
        .and_where(Expr::col("until").gt(0_i64));
    if !every_chat {
        update.and_where(Expr::col("space_id").eq(EVERY_CHAT));
    }
    Ok(w.exec(&update)? > 0)
}

/// A successful write clears the refusal on that chat and the connection-wide refusal.
pub fn lift_after_send(w: &mut W<'_>, connection: &str, space: &str) -> Result<()> {
    w.exec(
        Query::update()
            .table("chan_write_blocks")
            .value("until", 0_i64)
            .and_where(Expr::col("connection_id").eq(connection))
            .and_where(Expr::col("space_id").is_in([EVERY_CHAT, space]))
            .and_where(Expr::col("until").gt(0_i64)),
    )?;
    Ok(())
}
