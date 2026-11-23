use sea_query::{Expr, ExprTrait, Query};

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
