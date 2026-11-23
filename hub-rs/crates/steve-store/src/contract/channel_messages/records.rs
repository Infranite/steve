//! Stored conversation turns that explain how a channel message was handled.

use crate::contract::Q;
use sea_query::{Expr, ExprTrait, Func, Order, Query};

use crate::contract::Row;
use crate::{Read, Result, W};
use serde_json::Value;

pub struct Turn {
    pub id: String,
    pub author: String,
    pub text: String,
    pub item: Option<String>,
    pub chan: Option<String>,
    pub out: Option<String>,
    pub origin: Option<String>,
    pub error: bool,
}

fn decode(row: &Row<'_>) -> Result<Turn> {
    Ok(Turn {
        id: row.get(0)?,
        author: row.get(1)?,
        text: row.get(2)?,
        item: row.get(3)?,
        chan: row.get(4)?,
        out: row.get(5)?,
        origin: row.get(6)?,
        error: row.get::<Option<i64>>(7)?.unwrap_or_default() != 0,
    })
}

fn turns(r: &mut impl Read, ws: &str, conv: &str) -> sea_query::SelectStatement {
    let dialect = r.db().dialect();
    let meta = Expr::col("meta");
    Query::select()
        .columns(["id", "author", "text", "item_id"])
        .expr(dialect.json_path(meta.clone(), "$.chan.message"))
        .expr(dialect.json_path(meta.clone(), "$.chan_out.message"))
        .expr(dialect.json_path(meta.clone(), "$.origin"))
        .expr(dialect.json_path(meta, "$.error"))
        .from("messages")
        .and_where(Expr::col("workspace_id").eq(ws))
        .and_where(Expr::col("space").eq(conv))
        .order_by("created", Order::Asc)
        .order_by_expr(dialect.insertion_order(None), Order::Asc)
        .take()
}

pub fn channel_turns(r: &mut impl Read, ws: &str, conv: &str) -> Result<Vec<Turn>> {
    let query = turns(r, ws, conv);
    r.all(&query, decode)
}

pub fn web_turns(r: &mut impl Read, ws: &str, main: &str, space: &str) -> Result<Vec<Turn>> {
    let dialect = r.db().dialect();
    let channel = dialect
        .json_path(Expr::col("meta"), "$.chan.space")
        .eq(space);
    let origin = dialect.json_path(Expr::col("meta"), "$.origin");
    let source = Query::select()
        .column("id")
        .from("messages")
        .and_where(Expr::col("workspace_id").eq(ws))
        .and_where(Expr::col("space").eq(main))
        .and_where(Expr::col("author").eq("me"))
        .and_where(channel.clone())
        .take();
    let mut query = turns(r, ws, main);
    r.all(
        query.and_where(
            Expr::col("author")
                .eq("me")
                .and(channel)
                .or(Expr::col("author")
                    .eq("steve")
                    .and(origin.in_subquery(source))),
        ),
        decode,
    )
}

pub enum Content<'a> {
    Generated(&'a Value),
    Outcome(&'a Value),
    FirstOutcome(&'a Value),
}

pub fn set_channel_content(
    w: &mut W<'_>,
    ws: &str,
    id: &str,
    content: Content<'_>,
) -> Result<bool> {
    let (path, value, claim) = match content {
        Content::Generated(value) => ("$.chan_gen", value, false),
        Content::Outcome(value) => ("$.chan_out", value, false),
        Content::FirstOutcome(value) => ("$.chan_out", value, true),
    };
    let meta = Expr::col("meta");
    let meta: Expr = Expr::case(w.dialect().json_valid(meta.clone()), meta)
        .finally(Expr::cust("'{}'"))
        .into();
    let mut update = Query::update();
    update
        .table("messages")
        .value(
            "meta",
            Func::cust("json_set").args([
                meta.clone(),
                Expr::Constant(path.into()),
                Func::cust("json").arg(value.to_string()).into(),
            ]),
        )
        .and_where(Expr::col("workspace_id").eq(ws))
        .and_where(Expr::col("id").eq(id));
    if claim {
        update.and_where(w.dialect().json_type(meta, path).is_null());
    }
    let Some(space) = w.one(update.returning_col("space"), |row| row.get::<String>(0))? else {
        return Ok(false);
    };
    if path == "$.chan_out" {
        w.emit(steve_types::DomainEvent::from(
            steve_types::WorkEvent::Message {
                space: space.into(),
                ws: ws.into(),
                id: id.into(),
            },
        ));
    }
    Ok(true)
}

pub fn first_turn_for_message(
    r: &mut impl Read,
    ws: &str,
    message: &str,
) -> Result<Option<(String, String)>> {
    let dialect = r.db().dialect();
    r.one(
        Query::select()
            .columns(["id", "space"])
            .from("messages")
            .and_where(Expr::col("workspace_id").eq(ws))
            .and_where(Expr::col("author").eq("me"))
            .and_where(
                dialect
                    .json_path(Expr::col("meta"), "$.chan.message")
                    .eq(message),
            )
            .order_by("created", Order::Asc)
            .order_by_expr(dialect.insertion_order(None), Order::Asc)
            .limit(1),
        |row| Ok((row.get(0)?, row.get(1)?)),
    )
}

pub fn route_matches(
    r: &mut impl Read,
    ws: &str,
    origin: &str,
    reply: &str,
    kind: &str,
) -> Result<bool> {
    Ok(r.one(
        Query::select()
            .column("message_id")
            .from("route_log")
            .and_where(Expr::col("workspace_id").eq(ws))
            .and_where(Expr::col("message_id").eq(origin))
            .and_where(Expr::col("reply_id").eq(reply))
            .and_where(Expr::col("kind").eq(kind))
            .limit(1),
        |_| Ok(()),
    )?
    .is_some())
}

pub fn has_turn(r: &mut impl Read, ws: &str, space: &str, message: &str) -> Result<bool> {
    let channel = r
        .db()
        .dialect()
        .json_path(Expr::col("meta"), "$.chan.message");
    Ok(r.one(
        Query::select()
            .column("id")
            .from("messages")
            .and_where(Expr::col("workspace_id").eq(ws))
            .and_where(Expr::col("space").eq(space))
            .and_where(Expr::col("author").eq("me"))
            .and_where(channel.eq(message))
            .limit(1),
        |_| Ok(()),
    )?
    .is_some())
}
