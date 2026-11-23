//! Channel behavior and history counters scoped to a connection.

use sea_query::{Expr, ExprTrait, JoinType, OnConflict, Query};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::contract::Json;

use crate::{Read, Result, W};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Mode {
    /// You (`你本人`): an account of your “我”.
    #[serde(rename = "self")]
    Own,
    /// An app bot of the own organization.
    Bot,
}

impl Mode {
    pub fn code(self) -> &'static str {
        match self {
            Self::Own => "self",
            Self::Bot => "bot",
        }
    }

    pub fn parse(s: &str) -> Self {
        if s == "self" { Self::Own } else { Self::Bot }
    }
}

/// One `channel_settings` row.
#[derive(Debug, Clone, PartialEq)]
pub struct ChannelSettings {
    pub connection_id: String,
    pub mode: Mode,
    pub duty: String,
    pub rules: Value,
    pub trigger: Value,
    pub revision: i64,
    /// The undo window after approval, in seconds. None uses the sender's default delay.
    pub send_delay_s: Option<i64>,
    /// On a personal account, hours before an unanswered card expires.
    pub card_ttl_h: Option<i64>,
    pub late_ms: Option<i64>,
    pub before: i64,
    pub late: i64,
    pub history_at: Option<i64>,
}

pub fn get(r: &mut impl Read, ws: &str, conn: &str) -> Result<Option<ChannelSettings>> {
    r.one(
        Query::select()
            .columns([
                "connection_id",
                "mode",
                "duty",
                "rules",
                "trigger",
                "revision",
                "send_delay_s",
                "card_ttl_h",
                "late_ms",
                "history_before",
                "history_late",
                "history_at",
            ])
            .from("channel_settings")
            .and_where(Expr::col("workspace_id").eq(ws))
            .and_where(Expr::col("connection_id").eq(conn)),
        |row| {
            Ok(ChannelSettings {
                connection_id: row.get(0)?,
                mode: Mode::parse(&row.get::<String>(1)?),
                duty: row.get(2)?,
                rules: row
                    .get::<Option<Json<Value>>>(3)?
                    .map_or_else(|| json!([]), |j| j.0),
                trigger: row
                    .get::<Option<Json<Value>>>(4)?
                    .map_or(Value::Null, |j| j.0),
                revision: row.get(5)?,
                send_delay_s: row.get(6)?,
                card_ttl_h: row.get(7)?,
                late_ms: row.get(8)?,
                before: row.get(9)?,
                late: row.get(10)?,
                history_at: row.get(11)?,
            })
        },
    )
}

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

/// Connections whose configured identity is a Bot.
pub fn bot_connections(r: &mut impl Read, ws: &str) -> Result<Vec<String>> {
    r.all(
        Query::select()
            .column("connection_id")
            .from("channel_settings")
            .and_where(Expr::col("workspace_id").eq(ws))
            .and_where(Expr::col("mode").eq("bot")),
        |row| row.get(0),
    )
}

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

/// Connected accounts configured to act as the user's own identity.
pub fn personal_accounts(r: &mut impl Read, ws: &str) -> Result<Vec<String>> {
    r.all(
        Query::select()
            .column(("c", "account_id"))
            .from_as("channel_settings", "s")
            .join_as(
                JoinType::InnerJoin,
                "connections",
                "c",
                Expr::col(("c", "id")).equals(("s", "connection_id")),
            )
            .and_where(Expr::col(("s", "workspace_id")).eq(ws))
            .and_where(Expr::col(("s", "mode")).eq("self"))
            .and_where(Expr::col(("c", "status")).ne("disconnected")),
        |row| row.get(0),
    )
}

fn configurable(
    dialect: crate::contract::Dialect,
    viewer: &crate::base::Viewer,
) -> sea_query::SelectStatement {
    let settings = Query::select()
        .column("connection_id")
        .from("channel_settings")
        .and_where(Expr::col("connection_id").equals(("c", "id")))
        .take();
    let cap = dialect
        .json_rows(Expr::col(("c", "caps")), "cap")
        .and_where(Expr::col(("cap", "value")).eq("msg.receive"))
        .take();
    Query::select()
        .column(("c", "id"))
        .from_as("connections", "c")
        .join_as(
            JoinType::InnerJoin,
            "accounts",
            "a",
            Expr::col(("a", "id")).equals(("c", "account_id")),
        )
        .join_as(
            JoinType::LeftJoin,
            "parties",
            "p",
            Expr::col(("p", "id")).equals(("a", "party_id")),
        )
        .and_where(Expr::col(("c", "workspace_id")).eq(&viewer.ws))
        .and_where(Expr::col(("c", "status")).ne("revoked"))
        .and_where(
            Expr::col(("p", "user_id"))
                .is_null()
                .or(Expr::col(("p", "user_id")).eq(&viewer.user)),
        )
        .and_where(Expr::exists(settings).or(Expr::exists(cap)))
        .take()
}

pub fn can_configure(r: &mut impl Read, viewer: &crate::base::Viewer, id: &str) -> Result<bool> {
    let query = configurable(r.db().dialect(), viewer)
        .and_where(Expr::col(("c", "id")).eq(id))
        .take();
    Ok(r.one(&query, |_| Ok(()))?.is_some())
}

pub fn configurable_ids(r: &mut impl Read, viewer: &crate::base::Viewer) -> Result<Vec<String>> {
    let query = configurable(r.db().dialect(), viewer)
        .order_by_columns([
            (("c", "created"), sea_query::Order::Asc),
            (("c", "id"), sea_query::Order::Asc),
        ])
        .take();
    r.all(&query, |row| row.get(0))
}
