use crate::contract::Q;
use std::path::Path;

use sea_query::{Expr, ExprTrait, Query};

use crate::{BotRows, ReplyBot, Result, Store};

#[derive(Default)]
pub struct Route {
    pub bot: Option<ReplyBot>,
    pub harness: Option<String>,
    pub effort: Option<String>,
    pub compose: Option<serde_json::Value>,
}

pub fn reply_route(path: &Path, ws: &str) -> Result<Route> {
    Store::read_file(path, |r| {
        let bot = BotRows(r).reply_profile(ws, None)?;
        let (harness, effort) = match &bot {
            Some(bot) => r
                .one(
                    Query::select()
                        .columns(["harness", "reasoning_effort"])
                        .from("bots")
                        .and_where(Expr::col("id").eq(&bot.id)),
                    |row| {
                        Ok((
                            Some(row.get(0)?),
                            row.get::<Option<String>>(1)?
                                .filter(|effort| !effort.is_empty()),
                        ))
                    },
                )?
                .unwrap_or_default(),
            None => (None, None),
        };
        let compose = r
            .one(
                Query::select()
                    .column("value")
                    .from("settings")
                    .and_where(Expr::col("workspace_id").eq(ws))
                    .and_where(Expr::col("key").eq("models.compose")),
                |row| row.get::<String>(0),
            )?
            .and_then(|value| serde_json::from_str(&value).ok());
        Ok(Route {
            bot,
            harness,
            effort,
            compose,
        })
    })
}
