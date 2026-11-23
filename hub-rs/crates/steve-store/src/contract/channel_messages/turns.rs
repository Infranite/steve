//! Claims on groups of inbound messages handled in one turn.

use sea_query::{Expr, ExprTrait, Query};
use serde_json::Value;

use crate::contract::Q;
use crate::{Result, StoreError, W};

pub struct Claimed {
    pub id: String,
    pub text: String,
    pub attachments: Value,
}

pub fn claim(w: &mut W<'_>, target: &str, ids: Vec<String>) -> Result<Vec<Claimed>> {
    let mut claimed = Vec::with_capacity(ids.len());
    for id in ids {
        let (text, attachments) = w
            .one(
                Query::select()
                    .columns(["text", "attachments"])
                    .from("chan_messages")
                    .and_where(Expr::col("id").eq(id.as_str())),
                |row| Ok((row.get::<Option<String>>(0)?, row.get::<Option<String>>(1)?)),
            )?
            .ok_or_else(|| StoreError::Rejected("message in the turn is missing".into()))?;
        if id != target {
            w.exec(
                Query::update()
                    .table("chan_messages")
                    .value("turn_of", target)
                    .and_where(Expr::col("id").eq(id.as_str())),
            )?;
        }
        claimed.push(Claimed {
            id,
            text: text.unwrap_or_default(),
            attachments: attachments
                .and_then(|s| serde_json::from_str(&s).ok())
                .unwrap_or(Value::Null),
        });
    }
    Ok(claimed)
}

pub fn release(w: &mut W<'_>, target: &str) -> Result<()> {
    w.exec(
        Query::update()
            .table("chan_messages")
            .value("turn_of", Option::<String>::None)
            .and_where(Expr::col("turn_of").eq(target)),
    )?;
    Ok(())
}
