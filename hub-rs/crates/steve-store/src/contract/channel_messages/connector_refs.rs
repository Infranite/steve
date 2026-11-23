//! Scoped references issued after a connector operation.

use sea_query::{Expr, ExprTrait, JoinType, OnConflict, Query, SelectStatement};
use serde_json::Value;

use crate::{Read, Result, W};

pub struct Binding {
    pub external: String,
    pub conversation: String,
    pub kind: String,
    pub expires: i64,
}

pub fn get(
    r: &mut impl Read,
    ws: &str,
    conn: &str,
    account: &str,
    id: &str,
) -> Result<Option<Binding>> {
    r.one(
        Query::select()
            .columns(["external_id", "conversation_id", "kind", "expires"])
            .from("connector_refs")
            .and_where(Expr::col("id").eq(id))
            .and_where(Expr::col("workspace_id").eq(ws))
            .and_where(Expr::col("connection_id").eq(conn))
            .and_where(Expr::col("account_id").eq(account)),
        |row| {
            let external: String = row.get(0)?;
            let kind: String = row.get(2)?;
            Ok(Binding {
                conversation: if kind == "conversation" {
                    external.clone()
                } else {
                    row.get(1)?
                },
                external,
                kind,
                expires: row.get(3)?,
            })
        },
    )
}

pub struct Message {
    pub space: String,
    pub external: Option<String>,
    pub thread: Option<String>,
    pub source: String,
}

pub fn message(r: &mut impl Read, connection: &str, id: &str) -> Result<Option<Message>> {
    r.one(
        seen_by(connection)
            .columns([
                ("m", "space_id"),
                ("m", "external_id"),
                ("m", "thread_key"),
                ("d", "source_id"),
            ])
            .and_where(Expr::col(("m", "id")).eq(id)),
        |row| {
            Ok(Message {
                space: row.get(0)?,
                external: row.get(1)?,
                thread: row.get(2)?,
                source: row.get(3)?,
            })
        },
    )
}

pub fn attachment(
    r: &mut impl Read,
    connection: &str,
    reference: &str,
) -> Result<Option<(String, String)>> {
    let dialect = r.db().dialect();
    let attachment = dialect
        .json_rows(Expr::col(("m", "attachments")), "a")
        .and_where(
            dialect
                .json_field(Expr::col(("a", "value")), "reference_id")
                .eq(reference),
        )
        .take();
    let row: Option<(String, String)> = r.one(
        seen_by(connection)
            .columns([("m", "space_id"), ("m", "attachments")])
            .and_where(Expr::exists(attachment))
            .limit(1),
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    Ok(row.and_then(|(space, raw)| {
        let attachments: Vec<Value> = serde_json::from_str(&raw).ok()?;
        let record = attachments
            .iter()
            .find(|attachment| attachment["reference_id"] == reference)?;
        Some((space, record["key"].as_str()?.into()))
    }))
}

fn seen_by(connection: &str) -> SelectStatement {
    Query::select()
        .from_as("chan_messages", "m")
        .join_as(
            JoinType::InnerJoin,
            "chan_message_seen",
            "d",
            Expr::col(("d", "message_id")).eq(Expr::col(("m", "id"))),
        )
        .and_where(Expr::col(("d", "connection_id")).eq(connection))
        .and_where(Expr::col(("m", "purged")).is_null())
        .take()
}

pub struct Reference<'a> {
    pub id: &'a str,
    pub ws: &'a str,
    pub connection: &'a str,
    pub account: &'a str,
    pub kind: &'a str,
    pub external: &'a str,
    pub conversation: &'a str,
    pub source: &'a str,
    pub original_type: &'a str,
    pub metadata: &'a Value,
    pub expires: i64,
}

pub fn save(w: &mut W<'_>, reference: &Reference<'_>, now: i64) -> Result<()> {
    w.exec(
        Query::insert()
            .into_table("connector_refs")
            .columns([
                "id",
                "workspace_id",
                "connection_id",
                "account_id",
                "kind",
                "external_id",
                "conversation_id",
                "source_message_id",
                "original_type",
                "metadata",
                "expires",
                "created",
                "updated",
            ])
            .values_panic([
                reference.id.into(),
                reference.ws.into(),
                reference.connection.into(),
                reference.account.into(),
                reference.kind.into(),
                reference.external.into(),
                reference.conversation.into(),
                reference.source.into(),
                reference.original_type.into(),
                reference.metadata.to_string().into(),
                reference.expires.into(),
                now.into(),
                now.into(),
            ])
            .on_conflict(
                OnConflict::column("id")
                    .update_columns(["metadata", "expires", "updated"])
                    .to_owned(),
            ),
    )?;
    Ok(())
}
