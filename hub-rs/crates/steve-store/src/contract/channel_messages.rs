use sea_query::{Expr, ExprTrait, JoinType, Order, Query, SubQueryStatement};
use serde_json::Value;

use crate::{Read, Result};

pub mod approval_context;
pub mod connector_refs;
pub mod delivery;
pub mod dialogue;
pub mod duplicates;
#[cfg(feature = "testkit")]
pub mod fixtures;
pub mod handoffs;
pub mod history;
pub mod ingress;
pub mod media;
pub mod outbound;
pub mod outputs;
pub mod presence;
pub mod projection;
pub mod provider;
pub mod queue;
pub mod reactions;
pub mod records;
pub mod retention;
pub mod starred;
pub mod transcript;
pub mod trigger;
pub mod turns;
pub mod voice;

pub fn emit(w: &mut crate::W<'_>, ws: &str, space: &str, id: &str) {
    w.emit(steve_types::ChannelsEvent::ChanMessage {
        id: id.into(),
        space_id: space.into(),
        ws: ws.into(),
    });
}

pub struct MessageLink {
    pub id: String,
    pub space: String,
    pub external: Option<String>,
}

pub fn space_of(r: &mut impl Read, ws: &str, message: &str) -> Result<Option<String>> {
    r.one(
        Query::select()
            .column("space_id")
            .from("chan_messages")
            .and_where(Expr::col("id").eq(message))
            .and_where(Expr::col("workspace_id").eq(ws)),
        |row| row.get(0),
    )
}

pub fn external_id(r: &mut impl Read, id: &str) -> Result<Option<String>> {
    Ok(r.one(
        Query::select()
            .column("external_id")
            .from("chan_messages")
            .and_where(Expr::col("id").eq(id)),
        |row| row.get(0),
    )?
    .flatten())
}

pub fn link_targets(r: &mut impl Read, ws: &str, ids: &[String]) -> Result<Vec<MessageLink>> {
    if ids.is_empty() {
        return Ok(vec![]);
    }
    let ids = r.db().dialect().text_array(ids)?;
    r.all(
        Query::select()
            .columns(["id", "space_id", "external_id"])
            .from("chan_messages")
            .and_where(Expr::col("workspace_id").eq(ws))
            .and_where(Expr::col("id").in_subquery(ids)),
        |row| {
            Ok(MessageLink {
                id: row.get(0)?,
                space: row.get(1)?,
                external: row.get(2)?,
            })
        },
    )
}

pub fn contains_external(r: &mut impl Read, spaces: &[String], external: &str) -> Result<bool> {
    if spaces.is_empty() {
        return Ok(false);
    }
    Ok(r.one(
        Query::select()
            .column("id")
            .from("chan_messages")
            .and_where(Expr::col("space_id").is_in(spaces.iter().map(String::as_str)))
            .and_where(Expr::col("external_id").eq(external))
            .limit(1),
        |_| Ok(()),
    )?
    .is_some())
}

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
