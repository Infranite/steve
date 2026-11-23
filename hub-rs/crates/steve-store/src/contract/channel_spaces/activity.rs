//! Where a contact appeared in channel traffic.

use sea_query::{Expr, ExprTrait, Func, JoinType, Order, Query, SelectStatement};

use crate::{Read, Result};

pub struct SeenAt {
    pub subject: String,
    pub space: String,
    pub at: i64,
}

fn seen_at(row: &crate::contract::Row<'_>) -> Result<SeenAt> {
    Ok(SeenAt {
        subject: row.get(0)?,
        space: row.get(1)?,
        at: row.get(2)?,
    })
}

fn sent(ws: &str) -> SelectStatement {
    Query::select()
        .from_as("chan_messages", "m")
        .and_where(Expr::col(("m", "workspace_id")).eq(ws))
        .and_where(Expr::col(("m", "direction")).eq("in"))
        .take()
}

pub fn sent_spaces(r: &mut impl Read, ws: &str, parties: &[String]) -> Result<Vec<SeenAt>> {
    let ids = r.db().dialect().text_array(parties)?;
    r.all(
        sent(ws)
            .columns([("m", "sender_party_id"), ("m", "space_id")])
            .expr(Func::max(Expr::col(("m", "created"))))
            .and_where(Expr::col(("m", "sender_party_id")).in_subquery(ids))
            .group_by_columns([("m", "sender_party_id"), ("m", "space_id")]),
        seen_at,
    )
}

pub fn member_spaces(r: &mut impl Read, ws: &str, accounts: &[String]) -> Result<Vec<SeenAt>> {
    let ids = r.db().dialect().text_array(accounts)?;
    r.all(
        Query::select()
            .columns(["account_id", "space_id"])
            .expr(Func::max(Expr::col("joined")))
            .from("space_members")
            .and_where(Expr::col("workspace_id").eq(ws))
            .and_where(Expr::col("account_id").in_subquery(ids))
            .group_by_columns(["account_id", "space_id"]),
        seen_at,
    )
}

pub fn mentioned_spaces(r: &mut impl Read, ws: &str, accounts: &[String]) -> Result<Vec<SeenAt>> {
    let dialect = r.db().dialect();
    let mentions = Expr::col(("m", "mentions"));
    let array =
        Expr::case(dialect.json_valid(mentions.clone()), mentions).finally(Expr::cust("'[]'"));
    let account = dialect.json_field(Expr::col(("j", "value")), "account_id");
    let ids = dialect.text_array(accounts)?;
    r.all(
        Query::select()
            .expr(account.clone())
            .column(("m", "space_id"))
            .expr(Func::max(Expr::col(("m", "created"))))
            .from_as("chan_messages", "m")
            .from_function(dialect.json_array_elements(array.into()), "j")
            .and_where(Expr::col(("m", "workspace_id")).eq(ws))
            .and_where(account.clone().in_subquery(ids))
            .add_group_by([account, Expr::col(("m", "space_id"))]),
        seen_at,
    )
}

pub struct Space {
    pub id: String,
    pub name: String,
    pub kind: String,
    pub attention: Option<String>,
}

pub fn spaces(r: &mut impl Read, ws: &str, ids: &[String]) -> Result<Vec<Space>> {
    let ids = r.db().dialect().text_array(ids)?;
    r.all(
        Query::select()
            .columns([
                ("s", "id"),
                ("s", "name"),
                ("s", "kind"),
                ("p", "attention"),
            ])
            .from_as("spaces", "s")
            .join_as(
                JoinType::LeftJoin,
                "space_prefs",
                "p",
                Expr::col(("p", "space_id")).eq(Expr::col(("s", "id"))),
            )
            .and_where(Expr::col(("s", "workspace_id")).eq(ws))
            .and_where(Expr::col(("s", "id")).in_subquery(ids)),
        |row| {
            Ok(Space {
                id: row.get(0)?,
                name: row.get(1)?,
                kind: row.get(2)?,
                attention: row.get(3)?,
            })
        },
    )
}

pub struct LastMessage {
    pub text: String,
    pub created: i64,
    pub space_id: String,
    pub space_name: String,
}

pub fn last_message(r: &mut impl Read, ws: &str, party: &str) -> Result<Option<LastMessage>> {
    r.one(
        sent(ws)
            .columns([("m", "text"), ("m", "created"), ("s", "id"), ("s", "name")])
            .join_as(
                JoinType::InnerJoin,
                "spaces",
                "s",
                Expr::col(("s", "id")).eq(Expr::col(("m", "space_id"))),
            )
            .and_where(Expr::col(("m", "sender_party_id")).eq(party))
            .and_where(Expr::from(Func::cust("trim").arg(Expr::col(("m", "text")))).ne(""))
            .order_by(("m", "created"), Order::Desc)
            .limit(1),
        |row| {
            Ok(LastMessage {
                text: row.get(0)?,
                created: row.get(1)?,
                space_id: row.get(2)?,
                space_name: row.get(3)?,
            })
        },
    )
}

pub fn message_counts(
    r: &mut impl Read,
    ws: &str,
    parties: &[String],
) -> Result<Vec<(String, i64)>> {
    let ids = r.db().dialect().text_array(parties)?;
    r.all(
        sent(ws)
            .column(("m", "sender_party_id"))
            .expr(Func::count(Expr::col(("m", "id"))))
            .and_where(Expr::col(("m", "sender_party_id")).in_subquery(ids))
            .group_by_col(("m", "sender_party_id")),
        |row| Ok((row.get(0)?, row.get(1)?)),
    )
}
