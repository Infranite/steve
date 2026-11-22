use sea_query::{Expr, ExprTrait, JoinType, Order, Query, SubQueryStatement};
use serde_json::Value;

use crate::{Read, Result};

pub struct OutboundFacts {
    pub on_behalf: String,
    pub status: String,
    pub external: bool,
    pub mode: Option<String>,
    pub verdict: Option<String>,
    pub composed: Option<Value>,
}

pub fn outbound_facts(c: &mut impl Read, ws: &str, message: &str) -> Result<Option<OutboundFacts>> {
    let verdict: SubQueryStatement = Query::select()
        .column("verdict")
        .from("reply_basis")
        .and_where(Expr::col("message_id").eq(Expr::col(("m", "id"))))
        .order_by("created", Order::Desc)
        .limit(1)
        .take()
        .into();
    c.one(
        Query::select()
            .columns([
                ("m", "on_behalf"),
                ("m", "status"),
                ("s", "has_external"),
                ("cs", "mode"),
            ])
            .expr(Expr::from(verdict))
            .column(("m", "composed"))
            .from_as("chan_messages", "m")
            .join_as(
                JoinType::InnerJoin,
                "spaces",
                "s",
                Expr::col(("s", "id")).eq(Expr::col(("m", "space_id"))),
            )
            .join_as(
                JoinType::LeftJoin,
                "channel_settings",
                "cs",
                Expr::col(("cs", "connection_id")).eq(Expr::col(("s", "connection_id"))),
            )
            .and_where(Expr::col(("m", "id")).eq(message))
            .and_where(Expr::col(("m", "workspace_id")).eq(ws)),
        |r| {
            Ok(OutboundFacts {
                on_behalf: r.get(0)?,
                status: r.get(1)?,
                external: r.get::<i64>(2)? != 0,
                mode: r.get(3)?,
                verdict: r.get(4)?,
                composed: r
                    .get::<Option<String>>(5)?
                    .and_then(|s| serde_json::from_str(&s).ok()),
            })
        },
    )
}
