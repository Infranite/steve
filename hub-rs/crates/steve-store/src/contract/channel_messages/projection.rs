//! Channel message records for a conversation or space.

use sea_query::{Expr, ExprTrait, Func, Order, Query, SelectStatement, SubQueryStatement};
use serde_json::{Value, json};

use crate::contract::{Json, Row};
use crate::{Read, Result, accounts, parties};

pub struct Message {
    pub id: String,
    pub space_id: String,
    pub direction: String,
    pub sender_name: String,
    pub sender_party_id: Option<String>,
    pub sender_account_id: Option<String>,
    pub persona_id: Option<String>,
    pub on_behalf: String,
    pub text: Option<String>,
    pub attachments: Value,
    pub mentions: Value,
    pub status: String,
    pub thread_key: Option<String>,
    pub reply_to: Option<String>,
    pub event_id: Option<String>,
    pub work_item_id: Option<String>,
    pub decision_id: Option<String>,
    pub created: i64,
    pub sent: Option<i64>,
    pub purged: Option<i64>,
    pub history: Option<String>,
}

pub enum Before<'a> {
    Time(i64),
    Message(i64, &'a str),
}

fn select() -> SelectStatement {
    let name = |q: SelectStatement| Expr::from(SubQueryStatement::from(q));
    let sender_name = Func::coalesce([
        name(accounts::sender_name(Expr::col(("m", "sender_account_id")))),
        name(parties::sender_name(Expr::col(("m", "sender_party_id")))),
        Expr::cust("''"),
    ]);
    Query::select()
        .from_as("chan_messages", "m")
        .columns([("m", "id"), ("m", "space_id"), ("m", "direction")])
        .expr(sender_name)
        .columns([
            ("m", "sender_party_id"),
            ("m", "sender_account_id"),
            ("m", "persona_id"),
            ("m", "on_behalf"),
            ("m", "text"),
            ("m", "attachments"),
            ("m", "mentions"),
            ("m", "status"),
            ("m", "thread_key"),
            ("m", "reply_to"),
            ("m", "event_id"),
            ("m", "work_item_id"),
            ("m", "decision_id"),
            ("m", "created"),
            ("m", "sent"),
            ("m", "purged"),
            ("m", "history"),
        ])
        .take()
}

fn decode(r: &Row<'_>) -> Result<Message> {
    let array = |i| {
        r.get::<Option<Json<Value>>>(i)
            .map(|v| v.map_or_else(|| json!([]), |v| v.0))
    };
    Ok(Message {
        id: r.get(0)?,
        space_id: r.get(1)?,
        direction: r.get(2)?,
        sender_name: r.get(3)?,
        sender_party_id: r.get(4)?,
        sender_account_id: r.get(5)?,
        persona_id: r.get(6)?,
        on_behalf: r.get(7)?,
        text: r.get(8)?,
        attachments: array(9)?,
        mentions: array(10)?,
        status: r.get(11)?,
        thread_key: r.get(12)?,
        reply_to: r.get(13)?,
        event_id: r.get(14)?,
        work_item_id: r.get(15)?,
        decision_id: r.get(16)?,
        created: r.get(17)?,
        sent: r.get(18)?,
        purged: r.get(19)?,
        history: r.get(20)?,
    })
}

pub fn get(r: &mut impl Read, id: &str) -> Result<Option<Message>> {
    r.one(select().and_where(Expr::col(("m", "id")).eq(id)), decode)
}

pub fn page(
    r: &mut impl Read,
    space: &str,
    before: Option<Before<'_>>,
    limit: u64,
) -> Result<Vec<Message>> {
    let mut q = select();
    let created = Expr::col(("m", "created"));
    let id = Expr::col(("m", "id"));
    q.and_where(Expr::col(("m", "space_id")).eq(space));
    match before {
        Some(Before::Time(time)) => {
            q.and_where(created.lt(time));
        }
        Some(Before::Message(time, message)) => {
            q.and_where(
                created
                    .clone()
                    .lt(time)
                    .or(created.eq(time).and(id.lt(message))),
            );
        }
        None => {}
    }
    q.order_by_columns([(("m", "created"), Order::Desc), (("m", "id"), Order::Desc)])
        .limit(limit);
    r.all(&q, decode)
}
