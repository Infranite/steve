//! Channel behavior and history counters scoped to a connection.

use sea_query::{Expr, ExprTrait, OnConflict, Query};
use serde_json::Value;

use crate::{Result, W};

pub fn initialize(
    w: &mut W<'_>,
    ws: &str,
    conn: &str,
    mode: &str,
    rules: &Value,
    now: i64,
) -> Result<()> {
    w.exec(
        Query::insert()
            .into_table("channel_settings")
            .columns(["connection_id", "workspace_id", "mode", "rules", "updated"])
            .values_panic([
                conn.into(),
                ws.into(),
                mode.into(),
                rules.to_string().into(),
                now.into(),
            ])
            .on_conflict(OnConflict::column("connection_id").do_nothing().to_owned()),
    )?;
    Ok(())
}

#[derive(Default)]
pub struct SettingsPatch<'a> {
    pub duty: Option<&'a str>,
    pub rules: Option<&'a Value>,
    pub trigger: Option<&'a Value>,
    pub send_delay_s: Option<Option<i64>>,
    pub card_ttl_h: Option<Option<i64>>,
    pub late_ms: Option<i64>,
    pub revision: i64,
}

pub fn patch(w: &mut W<'_>, conn: &str, patch: &SettingsPatch<'_>, now: i64) -> Result<()> {
    let mut update = Query::update();
    update
        .table("channel_settings")
        .value("revision", patch.revision)
        .value("updated", now)
        .and_where(Expr::col("connection_id").eq(conn));
    if let Some(value) = patch.duty {
        update.value("duty", value);
    }
    if let Some(value) = patch.rules {
        update.value("rules", value.to_string());
    }
    if let Some(value) = patch.trigger {
        update.value("trigger", value.to_string());
    }
    if let Some(value) = patch.send_delay_s {
        update.value("send_delay_s", value);
    }
    if let Some(value) = patch.card_ttl_h {
        update.value("card_ttl_h", value);
    }
    if let Some(value) = patch.late_ms {
        update.value("late_ms", value);
    }
    w.exec(&update)?;
    Ok(())
}

pub enum HistoryArrival {
    Before,
    Late,
}

pub fn count_history(w: &mut W<'_>, conn: &str, arrival: HistoryArrival, at: i64) -> Result<()> {
    let column = match arrival {
        HistoryArrival::Before => "history_before",
        HistoryArrival::Late => "history_late",
    };
    w.exec(
        Query::update()
            .table("channel_settings")
            .value(column, Expr::col(column).add(1i64))
            .value("history_at", at)
            .and_where(Expr::col("connection_id").eq(conn)),
    )?;
    Ok(())
}
