//! Match copies of inbound messages and echoes of sent messages.

use sea_query::{Expr, ExprTrait, Func, JoinType, Order, Query, SelectStatement};

use crate::{Read, Result};

use super::ingress::{Found, SYNTHETIC_PREFIX};

/// A send whose provider timestamp is missing can have this additional clock skew.
const SEND_SLACK_MS: i64 = 30_000;

#[derive(Debug, Clone)]
pub struct Probe<'a> {
    pub ws: &'a str,
    pub conn: &'a str,
    pub space_id: &'a str,
    pub space_ext: &'a str,
    /// Your own connection may match copies seen through other identities of the chat.
    pub own: bool,
    pub key: &'a str,
    pub created: i64,
    pub tol: i64,
    /// A copy with a provider ID can only match a copy without one.
    pub has_id: bool,
    pub party: Option<&'a str>,
}

fn candidates(p: &Probe<'_>) -> SelectStatement {
    let already_seen = Query::select()
        .column("message_id")
        .from("chan_message_seen")
        .and_where(Expr::col("message_id").eq(Expr::col(("m", "id"))))
        .and_where(Expr::col("connection_id").eq(p.conn))
        .take();
    let mut q = Query::select();
    q.from_as("spaces", "s")
        .join_as(
            JoinType::InnerJoin,
            "chan_messages",
            "m",
            Expr::col(("m", "space_id")).eq(Expr::col(("s", "id"))),
        )
        .and_where(Expr::col(("s", "workspace_id")).eq(p.ws))
        .and_where(Expr::col(("s", "external_id")).eq(p.space_ext))
        .and_where(Expr::col(("m", "workspace_id")).eq(p.ws))
        .and_where(
            Expr::col(("m", "direction"))
                .eq("in")
                .or(Expr::col(("m", "status")).eq("sent")),
        )
        .and_where(
            Expr::from(Func::coalesce([
                Expr::col(("m", "fallback_from")),
                Expr::cust("''"),
            ]))
            .ne(p.conn),
        )
        .and_where(Expr::exists(already_seen).not());
    if !p.own {
        q.and_where(Expr::col(("s", "id")).eq(p.space_id).or(reached_owner()));
    }
    if p.has_id {
        q.and_where(
            Expr::col(("m", "external_id"))
                .is_null()
                .or(Expr::col(("m", "external_id")).like(format!("{SYNTHETIC_PREFIX}%"))),
        );
    }
    q
}

fn found(row: &crate::contract::Row<'_>) -> Result<Found> {
    Ok(Found {
        id: row.get(0)?,
        space_id: row.get(1)?,
        out: row.get::<String>(2)? == "out",
    })
}

/// A matching copy by content key. Prefer the closest timestamp, then the earliest stored copy.
pub fn by_keys(r: &mut impl Read, p: &Probe<'_>, keys: &[&str]) -> Result<Option<Found>> {
    if keys.is_empty() {
        return Ok(None);
    }
    let dialect = r.db().dialect();
    let at = Expr::from(Func::coalesce([
        Expr::col(("m", "ext_created")),
        Expr::case(
            Expr::col(("m", "direction")).eq(Expr::cust("'out'")),
            Expr::from(Func::coalesce([
                Expr::col(("m", "sent")),
                Expr::col(("m", "created")),
            ])),
        )
        .finally(Expr::col(("m", "created")))
        .into(),
    ]));
    let order = dialect.insertion_order(Some("m"));
    let slack = p.tol + SEND_SLACK_MS;
    let rows = r.all(
        candidates(p)
            .columns([
                ("m", "id"),
                ("m", "space_id"),
                ("m", "direction"),
                ("m", "ext_created"),
                ("m", "created"),
            ])
            .expr(at.clone())
            .expr(order.clone())
            .and_where(Expr::col(("m", "content_key")).in_subquery(dialect.text_array(keys)?))
            .and_where(at.between(p.created - slack, p.created + slack))
            .order_by(("m", "created"), Order::Desc)
            .order_by_expr(order, Order::Desc)
            .limit(100),
        |row| {
            let message = found(row)?;
            let provider_at = row.get::<Option<i64>>(3)?;
            let created = row.get::<i64>(4)?;
            let at = row.get::<i64>(5)?;
            let sequence = row.get::<i64>(6)?;
            let distance = (at - p.created).abs();
            let limit = if message.out && provider_at.is_none() {
                slack
            } else {
                p.tol
            };
            Ok((distance, created, sequence, limit, message))
        },
    )?;
    Ok(rows
        .into_iter()
        .filter(|(distance, _, _, limit, _)| distance <= limit)
        .min_by_key(|(distance, created, sequence, ..)| (*distance, *created, *sequence))
        .map(|(_, _, _, _, message)| message))
}

/// When all clocks have millisecond precision, a unique copy by the same sender at the same time.
pub fn by_exact_time(r: &mut impl Read, p: &Probe<'_>) -> Result<Option<Found>> {
    let Some(party) = p.party.filter(|_| p.tol == 1) else {
        return Ok(None);
    };
    let at = Expr::from(Func::coalesce([
        Expr::col(("m", "ext_created")),
        Expr::case(
            Expr::col(("m", "direction")).eq("in"),
            Expr::col(("m", "created")),
        )
        .into(),
    ]));
    let mut rows = r.all(
        candidates(p)
            .columns([("m", "id"), ("m", "space_id"), ("m", "direction")])
            .and_where(Expr::col(("m", "sender_party_id")).eq(party))
            .and_where(at.eq(p.created))
            .limit(2),
        found,
    )?;
    Ok(if rows.len() == 1 { rows.pop() } else { None })
}

/// A locally assigned reference. Keep identical messages distinct even at the same timestamp.
pub fn new_id(r: &mut impl Read, space: &str, key: &str, created: i64) -> Result<String> {
    let prefix = format!("{SYNTHETIC_PREFIX}{key}:{created}:");
    let width = prefix.chars().count() as i64;
    let suffix = Expr::cust_with_exprs(
        "CAST(substr(?, ?) AS BIGINT)",
        [Expr::col("external_id"), Expr::Constant((width + 1).into())],
    );
    let previous: Option<i64> = r
        .one(
            Query::select()
                .expr(Func::max(suffix))
                .from("chan_messages")
                .and_where(Expr::col("space_id").eq(space))
                .and_where(Expr::cust_with_exprs(
                    "substr(?, 1, ?) = ?",
                    [
                        Expr::col("external_id"),
                        Expr::val(width),
                        Expr::val(prefix.as_str()),
                    ],
                )),
            |row| row.get(0),
        )?
        .flatten();
    Ok(format!("{prefix}{}", previous.map_or(0, |n| n + 1)))
}

// The caller uses m for the received message and s for the space.
fn reached_owner() -> Expr {
    let seen_by_owner = Query::select()
        .column("ordinal")
        .from("space_identities")
        .and_where(Expr::col("space_id").eq(Expr::col(("m", "space_id"))))
        .and_where(Expr::col("ordinal").is_not_null())
        .and_where(Expr::col("owner_user").is_not_null())
        .and_where(Expr::cust_with_exprs(
            "((? >> ?) & 1) = 1",
            [Expr::col(("m", "seen_by")), Expr::col("ordinal")],
        ))
        .take();
    let own_mode = Query::select()
        .column("mode")
        .from("channel_settings")
        .and_where(Expr::col("connection_id").eq(Expr::col(("s", "connection_id"))))
        .and_where(Expr::col("mode").eq("self"))
        .take();
    Expr::case(Expr::col(("m", "seen_by")).eq(0i64), Expr::exists(own_mode))
        .finally(Expr::exists(seen_by_owner))
        .into()
}

/// The same inbound provider message through a different connection of the workspace.
pub fn elsewhere(
    r: &mut impl Read,
    ws: &str,
    connection: &str,
    own: bool,
    chat: &str,
    external: &str,
) -> Result<Option<(String, String)>> {
    let mut rows = Query::select();
    rows.columns([("m", "id"), ("m", "space_id")])
        .from_as("chan_messages", "m")
        .join_as(
            JoinType::InnerJoin,
            "spaces",
            "s",
            Expr::col(("s", "id")).equals(("m", "space_id")),
        )
        .and_where(Expr::col(("s", "workspace_id")).eq(ws))
        .and_where(Expr::col(("s", "connection_id")).ne(connection))
        .and_where(Expr::col(("s", "external_id")).eq(chat))
        .and_where(Expr::col(("m", "external_id")).eq(external))
        .and_where(Expr::col(("m", "direction")).eq("in"))
        .order_by_columns([(("m", "created"), Order::Asc), (("m", "id"), Order::Asc)])
        .limit(1);
    if !own {
        rows.and_where(reached_owner());
    }
    r.one(&rows, |row| Ok((row.get(0)?, row.get(1)?)))
}
