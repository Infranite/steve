//! Workspace choices for a channel space's behavior.

use sea_query::{Expr, ExprTrait, JoinType, OnConflict, Query};
use serde_json::Value;

use crate::{Read, Result, W};

pub fn channel_mode(r: &mut impl Read, ws: &str, connection: &str) -> Result<Option<String>> {
    r.one(
        Query::select()
            .column("mode")
            .from("channel_settings")
            .and_where(Expr::col("workspace_id").eq(ws))
            .and_where(Expr::col("connection_id").eq(connection)),
        |row| row.get(0),
    )
}

pub fn chosen_voice(r: &mut impl Read, space: &str) -> Result<Option<String>> {
    Ok(r.one(
        Query::select()
            .column("voice")
            .from("space_prefs")
            .and_where(Expr::col("space_id").eq(space)),
        |row| row.get(0),
    )?
    .flatten())
}

pub fn bot_notices(r: &mut impl Read, space: &str) -> Result<Option<String>> {
    r.one(
        Query::select()
            .column("bot_notices")
            .from("space_prefs")
            .and_where(Expr::col("space_id").eq(space)),
        |row| row.get(0),
    )
}

pub struct Visibility {
    pub disclosure: String,
    pub attention: Option<String>,
}

pub fn visibility(r: &mut impl Read, space: &str) -> Result<Option<Visibility>> {
    r.one(
        Query::select()
            .columns([("s", "disclosure"), ("p", "attention")])
            .from_as("space_settings", "s")
            .join_as(
                JoinType::LeftJoin,
                "space_prefs",
                "p",
                Expr::col(("p", "space_id")).eq(Expr::col(("s", "space_id"))),
            )
            .and_where(Expr::col(("s", "space_id")).eq(space)),
        |row| {
            Ok(Visibility {
                disclosure: row.get(0)?,
                attention: row.get(1)?,
            })
        },
    )
}

pub enum Preference<'a> {
    Attention(&'a str),
    ReplyAsYou(&'a str),
    BotNotices(&'a str),
    Voice(Option<&'a str>),
}

pub fn set(w: &mut W<'_>, ws: &str, space: &str, change: Preference<'_>, now: i64) -> Result<()> {
    let (column, value): (&str, Expr) = match change {
        Preference::Attention(value) => ("attention", value.into()),
        Preference::ReplyAsYou(value) => ("reply_as_you", value.into()),
        Preference::BotNotices(value) => ("bot_notices", value.into()),
        Preference::Voice(value) => ("voice", value.into()),
    };
    w.exec(
        Query::insert()
            .into_table("space_prefs")
            .columns(["space_id", "workspace_id", column, "updated"])
            .values_panic([space.into(), ws.into(), value, now.into()])
            .on_conflict(
                OnConflict::column("space_id")
                    .update_columns([column, "updated"])
                    .to_owned(),
            ),
    )?;
    Ok(())
}

pub struct Settings<'a> {
    pub duty: &'a str,
    pub rules: Option<&'a Value>,
    pub muted: bool,
    pub collab: bool,
    pub memory_scope: &'a str,
    pub disclosure: &'a str,
    pub item_defaults: &'a Value,
    pub revision: i64,
}

pub fn configure(w: &mut W<'_>, space: &str, settings: &Settings<'_>, now: i64) -> Result<()> {
    w.exec(
        Query::update()
            .table("space_settings")
            .value("duty", settings.duty)
            .value("rules", settings.rules.map(Value::to_string))
            .value("muted", i64::from(settings.muted))
            .value("collab", i64::from(settings.collab))
            .value("memory_scope", settings.memory_scope)
            .value("disclosure", settings.disclosure)
            .value("item_defaults", settings.item_defaults.to_string())
            .value("configured", 1i64)
            .value("revision", settings.revision)
            .value("updated", now)
            .and_where(Expr::col("space_id").eq(space)),
    )?;
    Ok(())
}

pub fn set_flags(
    w: &mut W<'_>,
    space: &str,
    muted: bool,
    configured: bool,
    revision: i64,
    now: i64,
) -> Result<()> {
    w.exec(
        Query::update()
            .table("space_settings")
            .value("muted", i64::from(muted))
            .value("configured", i64::from(configured))
            .value("revision", revision)
            .value("updated", now)
            .and_where(Expr::col("space_id").eq(space)),
    )?;
    Ok(())
}

pub fn mute(w: &mut W<'_>, ws: &str, space: &str, now: i64) -> Result<bool> {
    let changed = w.exec(
        Query::update()
            .table("space_settings")
            .value("muted", 1i64)
            .value("updated", now)
            .and_where(Expr::col("space_id").eq(space))
            .and_where(Expr::col("muted").eq(0i64)),
    )? != 0;
    if changed {
        w.emit(steve_types::ChannelsEvent::Space {
            ws: ws.into(),
            id: space.into(),
        });
    }
    Ok(changed)
}
