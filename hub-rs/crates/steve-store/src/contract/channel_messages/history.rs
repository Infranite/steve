//! A bounded chronological read of already stored messages.

use sea_query::{Expr, ExprTrait, Func, JoinType, Order, Query, Values};
use serde_json::Value;

use crate::{Read, Result};

pub struct Line {
    pub at: i64,
    pub sender: String,
    pub out: bool,
    pub text: Option<String>,
    pub attachments: Vec<Value>,
    pub history: Option<String>,
}

pub fn lines(r: &mut impl Read, space: &str, from: i64, to: i64, limit: u64) -> Result<Vec<Line>> {
    let name = Func::coalesce([
        Expr::cust_with_expr("NULLIF(?, '')", Expr::col(("a", "display_name"))),
        Expr::cust_with_expr("NULLIF(?, '')", Expr::col(("p", "name"))),
        Expr::col(("a", "handle_display")),
        Expr::cust("''"),
    ]);
    r.all(
        Query::select()
            .from_as("chan_messages", "m")
            .column(("m", "created"))
            .expr(name)
            .columns([
                ("m", "direction"),
                ("m", "text"),
                ("m", "attachments"),
                ("m", "history"),
            ])
            .join_as(
                JoinType::LeftJoin,
                "accounts",
                "a",
                Expr::col(("a", "id")).equals(("m", "sender_account_id")),
            )
            .join_as(
                JoinType::LeftJoin,
                "parties",
                "p",
                Expr::col(("p", "id")).equals(("m", "sender_party_id")),
            )
            .and_where(Expr::col(("m", "space_id")).eq(space))
            .and_where(Expr::col(("m", "created")).between(from, to))
            .and_where(
                Expr::col(("m", "direction"))
                    .eq("in")
                    .or(Expr::col(("m", "status")).is_in(["sent", "recalled"])),
            )
            .order_by_columns([(("m", "created"), Order::Asc), (("m", "id"), Order::Asc)])
            .limit(limit),
        |row| {
            let attachments = row
                .get::<Option<String>>(4)?
                .and_then(|raw| serde_json::from_str(&raw).ok())
                .unwrap_or_default();
            Ok(Line {
                at: row.get(0)?,
                sender: row.get(1)?,
                out: row.get::<String>(2)? == "out",
                text: row.get(3)?,
                attachments,
                history: row.get(5)?,
            })
        },
    )
}

pub struct PartyMessage {
    pub space: String,
    pub name: String,
    pub text: Option<String>,
    pub created: i64,
}

pub fn by_party(
    r: &mut impl Read,
    ws: &str,
    party: &str,
    before: Option<i64>,
    limit: u64,
) -> Result<Vec<PartyMessage>> {
    let parties = crate::parties::merged_ids(r, party)?;
    let ids = r.db().dialect().text_array(&parties)?;
    let mut query = Query::select();
    query
        .columns([
            ("m", "space_id"),
            ("s", "name"),
            ("m", "text"),
            ("m", "created"),
        ])
        .from_as("chan_messages", "m")
        .join_as(
            JoinType::InnerJoin,
            "spaces",
            "s",
            Expr::col(("s", "id")).equals(("m", "space_id")),
        )
        .and_where(Expr::col(("m", "workspace_id")).eq(ws))
        .and_where(Expr::col(("m", "direction")).eq("in"))
        .and_where(Expr::col(("m", "sender_party_id")).in_subquery(ids))
        .order_by_columns([(("m", "created"), Order::Desc), (("m", "id"), Order::Desc)])
        .limit(limit);
    if let Some(before) = before {
        query.and_where(Expr::col(("m", "created")).lt(before));
    }
    r.all(&query, |row| {
        Ok(PartyMessage {
            space: row.get(0)?,
            name: row.get(1)?,
            text: row.get(2)?,
            created: row.get(3)?,
        })
    })
}

pub struct ThreadItem {
    pub item: String,
    pub raiser: Option<String>,
    pub sender: Option<String>,
}

pub fn open_thread_item(r: &mut impl Read, ws: &str, message: &str) -> Result<Option<ThreadItem>> {
    let Some(source) = super::ingress::source(r, ws, message)? else {
        return Ok(None);
    };
    let Some(thread) = source.thread.filter(|thread| !thread.is_empty()) else {
        return Ok(None);
    };
    let mut found = None;
    r.db().query(
        "SELECT item, raiser FROM (
           SELECT cm.work_item_id AS item, cm.sender_party_id AS raiser, cm.created AS at
             FROM chan_messages cm WHERE cm.space_id = ?1 AND cm.thread_key = ?2
                AND cm.id <> ?3 AND cm.work_item_id IS NOT NULL
           UNION ALL
           SELECT t.target_item_id, cm.sender_party_id, t.created
             FROM thread_dispatch_refs t JOIN messages m ON m.id = t.origin_message_id
             JOIN chan_messages cm ON cm.id = json_extract(m.meta, '$.chan.message')
            WHERE t.workspace_id = ?4 AND t.effect_kind = 'create_item' AND t.target_item_id IS NOT NULL
                AND json_valid(m.meta)
                AND cm.space_id = ?1 AND cm.thread_key = ?2 AND cm.id <> ?3)
          WHERE EXISTS (SELECT 1 FROM items i WHERE i.id = item AND i.workspace_id = ?4 AND i.phase != 'closed')
          ORDER BY at DESC LIMIT 1",
        &Values(vec![source.space.into(), thread.into(), message.into(), ws.into()]),
        &mut |row| {
            found = Some(ThreadItem {
                item: row.get(0)?, raiser: row.get(1)?, sender: source.party.clone(),
            });
            Ok(())
        },
    )?;
    Ok(found)
}

pub fn thread_root(r: &mut impl Read, space: &str, message: &str) -> Result<Option<String>> {
    let row: Option<(Option<String>, Option<String>)> = r.one(
        Query::select()
            .columns(["thread_key", "external_id"])
            .from("chan_messages")
            .and_where(Expr::col("space_id").eq(space))
            .and_where(Expr::col("id").eq(message)),
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    let Some((key, external)) = row else {
        return Ok(None);
    };
    let Some(key) = key.or(external).filter(|key| !key.is_empty()) else {
        return Ok(Some(message.into()));
    };
    let mut base = Query::select();
    base.column("id")
        .from("chan_messages")
        .and_where(Expr::col("space_id").eq(space))
        .and_where(
            Expr::col("direction")
                .eq("in")
                .or(Expr::col("status").is_in(["sent", "recalled"])),
        )
        .limit(1);
    if let Some(id) = r.one(
        base.clone()
            .and_where(Expr::col("external_id").eq(key.as_str())),
        |row| row.get(0),
    )? {
        return Ok(Some(id));
    }
    Ok(Some(
        r.one(
            base.and_where(Expr::col("thread_key").eq(key))
                .order_by_columns([("created", Order::Asc), ("id", Order::Asc)]),
            |row| row.get(0),
        )?
        .unwrap_or_else(|| message.into()),
    ))
}

#[derive(Debug)]
pub struct ThreadLine {
    pub id: String,
    pub external_id: Option<String>,
    pub thread_key: Option<String>,
    pub reply_to: Option<String>,
    pub at: i64,
    pub sender: String,
    pub out: bool,
    pub status: String,
}

fn visible() -> Expr {
    Expr::col(("m", "direction"))
        .eq("in")
        .or(Expr::col(("m", "status")).is_in(["sent", "recalled"]))
}

fn visible_line() -> sea_query::SelectStatement {
    Query::select()
        .columns([
            ("m", "id"),
            ("m", "external_id"),
            ("m", "thread_key"),
            ("m", "reply_to"),
            ("m", "created"),
        ])
        .expr(Func::coalesce([
            Expr::cust_with_expr("NULLIF(?, '')", Expr::col(("a", "display_name"))),
            Expr::cust_with_expr("NULLIF(?, '')", Expr::col(("p", "name"))),
            Expr::col(("a", "handle_display")),
            Expr::Constant("".into()),
        ]))
        .columns([("m", "direction"), ("m", "status")])
        .from_as("chan_messages", "m")
        .join_as(
            JoinType::LeftJoin,
            "accounts",
            "a",
            Expr::col(("a", "id")).equals(("m", "sender_account_id")),
        )
        .join_as(
            JoinType::LeftJoin,
            "parties",
            "p",
            Expr::col(("p", "id")).equals(("m", "sender_party_id")),
        )
        .and_where(visible())
        .take()
}

fn decode_thread(row: &crate::contract::Row<'_>) -> Result<ThreadLine> {
    Ok(ThreadLine {
        id: row.get(0)?,
        external_id: row.get(1)?,
        thread_key: row.get(2)?,
        reply_to: row.get(3)?,
        at: row.get(4)?,
        sender: row.get(5)?,
        out: row.get::<String>(6)? == "out",
        status: row.get(7)?,
    })
}

pub fn resolve_visible(
    r: &mut impl Read,
    ws: &str,
    space: &str,
    query: &str,
) -> Result<Option<String>> {
    r.one(
        Query::select()
            .column(("m", "id"))
            .from_as("chan_messages", "m")
            .and_where(Expr::col(("m", "workspace_id")).eq(ws))
            .and_where(Expr::col(("m", "space_id")).eq(space))
            .and_where(
                Expr::col(("m", "id"))
                    .eq(query)
                    .or(Expr::col(("m", "external_id")).eq(query)),
            )
            .and_where(visible())
            .limit(1),
        |row| row.get(0),
    )
}

pub fn visible_message(r: &mut impl Read, id: &str) -> Result<Option<ThreadLine>> {
    r.one(
        visible_line().and_where(Expr::col(("m", "id")).eq(id)),
        decode_thread,
    )
}

pub fn replies(
    r: &mut impl Read,
    space: &str,
    root: &str,
    thread: Option<&str>,
    reference: Option<&str>,
    limit: u64,
) -> Result<Vec<ThreadLine>> {
    if thread.is_none() && reference.is_none() {
        return Ok(Vec::new());
    }
    r.all(
        visible_line()
            .and_where(Expr::col(("m", "space_id")).eq(space))
            .and_where(Expr::col(("m", "id")).ne(root))
            .and_where(
                Expr::col(("m", "thread_key"))
                    .eq(thread)
                    .or(Expr::col(("m", "reply_to")).eq(reference)),
            )
            .order_by_columns([(("m", "created"), Order::Asc), (("m", "id"), Order::Asc)])
            .limit(limit),
        decode_thread,
    )
}

pub fn quoted(r: &mut impl Read, space: &str, reference: &str) -> Result<Option<ThreadLine>> {
    r.one(
        visible_line()
            .and_where(Expr::col(("m", "space_id")).eq(space))
            .and_where(Expr::col(("m", "external_id")).eq(reference))
            .limit(1),
        decode_thread,
    )
}
