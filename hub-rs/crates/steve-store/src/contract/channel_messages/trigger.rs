//! Facts used to admit and revalidate a personal channel trigger.

use sea_query::{Expr, ExprTrait, JoinType, Query, Values};

use crate::{Read, Result};

pub struct Receipt {
    pub sender_account: Option<String>,
    pub sender_party: Option<String>,
    pub from_self: bool,
}

pub fn admitted_receipt(
    r: &mut impl Read,
    ws: &str,
    space: &str,
    conn: &str,
    message: &str,
) -> Result<Option<Receipt>> {
    r.one(
        Query::select()
            .columns([
                ("r", "sender_account_id"),
                ("r", "sender_party_id"),
                ("r", "from_self"),
            ])
            .from_as("chan_trigger_receipts", "r")
            .join_as(
                JoinType::InnerJoin,
                "chan_messages",
                "m",
                Expr::col(("m", "id")).equals(("r", "message_id")),
            )
            .join_as(
                JoinType::InnerJoin,
                "connections",
                "c",
                Expr::col(("c", "id")).equals(("r", "connection_id")),
            )
            .join_as(
                JoinType::InnerJoin,
                "space_identities",
                "i",
                Expr::col(("i", "space_id"))
                    .equals(("m", "space_id"))
                    .and(Expr::col(("i", "connection_id")).equals(("c", "id"))),
            )
            .and_where(Expr::col(("r", "message_id")).eq(message))
            .and_where(Expr::col(("r", "connection_id")).eq(conn))
            .and_where(Expr::col(("m", "workspace_id")).eq(ws))
            .and_where(Expr::col(("m", "space_id")).eq(space))
            .and_where(Expr::col(("c", "workspace_id")).eq(ws))
            .and_where(Expr::col(("c", "status")).is_in(["ok", "error"]))
            .and_where(Expr::col(("i", "left_at")).is_null())
            .and_where(Expr::col(("m", "direction")).eq("in"))
            .and_where(Expr::col(("m", "history")).is_null())
            .and_where(
                Expr::col(("r", "from_self"))
                    .eq(0_i64)
                    .or(Expr::col(("r", "sender_account_id")).equals(("c", "account_id"))),
            ),
        |row| {
            Ok(Receipt {
                sender_account: row.get(0)?,
                sender_party: row.get(1)?,
                from_self: row.get::<i64>(2)? != 0,
            })
        },
    )
}

pub fn continues(r: &mut impl Read, space: &str, message: &str, conn: &str) -> Result<bool> {
    const ANSWERED: &str = "EXISTS (SELECT 1 FROM messages turn WHERE turn.workspace_id = t.workspace_id
        AND json_extract(turn.meta, '$.chan.message') = t.id AND json_extract(turn.meta, '$.chan.command') = 1
        AND json_extract(turn.meta, '$.chan.connection') = ?3)";
    let mut found = false;
    r.db().query(
        &format!("SELECT EXISTS (
               SELECT 1 FROM chan_messages m, chan_messages t
                WHERE m.id = ?1 AND t.space_id = ?2 AND t.id <> m.id AND t.created <= m.created
                  AND (
                    (m.thread_key IS NOT NULL AND (t.external_id = m.thread_key OR t.thread_key = m.thread_key) AND {ANSWERED})
                    OR (m.reply_to IS NOT NULL AND t.external_id = m.reply_to
                        AND ({ANSWERED} OR (t.direction = 'out' AND t.on_behalf = 'proxy'
                            AND t.sender_account_id=(SELECT account_id FROM connections WHERE id=?3))))
                  ))"),
        &Values(vec![message.into(), space.into(), conn.into()]),
        &mut |row| {
            found = row.get::<i64>(0)? != 0;
            Ok(())
        },
    )?;
    Ok(found)
}

fn command_turn(r: &mut impl Read, ws: &str, message: &str) -> sea_query::SelectStatement {
    let dialect = r.db().dialect();
    let meta = Expr::col("meta");
    Query::select()
        .from("messages")
        .and_where(Expr::col("workspace_id").eq(ws))
        .and_where(
            dialect
                .json_path(meta.clone(), "$.chan.message")
                .eq(message),
        )
        .and_where(dialect.json_path(meta, "$.chan.command").eq(1_i64))
        .take()
}

pub fn has_command_turn(r: &mut impl Read, ws: &str, message: &str) -> Result<bool> {
    let mut query = command_turn(r, ws, message);
    Ok(r.one(query.column("id").limit(1), |_| Ok(()))?.is_some())
}

pub fn personal_command_connection(
    r: &mut impl Read,
    ws: &str,
    message: &str,
) -> Result<Option<String>> {
    let mut query = command_turn(r, ws, message);
    let dialect = r.db().dialect();
    let meta = Expr::col("meta");
    Ok(r.one(
        query
            .expr(dialect.json_path(meta.clone(), "$.chan.connection"))
            .and_where(dialect.json_path(meta, "$.chan.mode").eq("self"))
            .order_by("created", sea_query::Order::Asc)
            .limit(1),
        |row| row.get(0),
    )?
    .flatten())
}
