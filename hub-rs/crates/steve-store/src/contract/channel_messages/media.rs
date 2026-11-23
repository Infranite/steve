//! Source records used to present and retrieve a message's attachments.

use sea_query::{Expr, ExprTrait, Query};
use serde_json::{Value, json};

use crate::{Read, Result};

pub struct Source {
    pub id: String,
    pub ws: String,
    pub space_id: String,
    pub external_id: Option<String>,
    pub direction: String,
    pub status: String,
    pub event_id: Option<String>,
    pub text: Option<String>,
    pub attachments: Value,
    pub purged: Option<i64>,
}

pub fn get(r: &mut impl Read, id: &str) -> Result<Option<Source>> {
    r.one(
        Query::select()
            .columns([
                "id",
                "workspace_id",
                "space_id",
                "external_id",
                "direction",
                "status",
                "event_id",
                "text",
                "attachments",
                "purged",
            ])
            .from("chan_messages")
            .and_where(Expr::col("id").eq(id)),
        |row| {
            let raw: Option<String> = row.get(8)?;
            Ok(Source {
                id: row.get(0)?,
                ws: row.get(1)?,
                space_id: row.get(2)?,
                external_id: row.get(3)?,
                direction: row.get(4)?,
                status: row.get(5)?,
                event_id: row.get(6)?,
                text: row.get(7)?,
                attachments: raw
                    .and_then(|s| serde_json::from_str(&s).ok())
                    .unwrap_or_else(|| json!([])),
                purged: row.get(9)?,
            })
        },
    )
}
