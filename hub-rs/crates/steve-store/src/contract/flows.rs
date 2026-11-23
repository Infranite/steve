use crate::{Read, Result};
use sea_query::{Expr, ExprTrait, Order, Query};

pub fn has_origin(c: &mut impl Read, ws: &str, origin: &str) -> Result<bool> {
    c.one(
        Query::select()
            .expr(Expr::cust("1"))
            .from("flows")
            .and_where(Expr::col("workspace_id").eq(ws))
            .and_where(Expr::col("origin").eq(origin))
            .limit(1),
        |_| Ok(()),
    )
    .map(|row| row.is_some())
}

pub struct EventRun {
    pub id: String,
    pub flow_id: String,
    pub status: String,
    pub outcome: Option<String>,
    pub label: String,
    pub error: Option<String>,
    pub context: serde_json::Value,
    pub item_id: Option<String>,
}

pub fn event_runs(read: &mut impl Read, ws: &str, event: &str) -> Result<Vec<EventRun>> {
    read.all(
        Query::select()
            .columns([
                "id",
                "flow_id",
                "status",
                "outcome",
                "outcome_label",
                "error",
                "context",
                "work_item_id",
            ])
            .from("flow_runs")
            .and_where(Expr::col("workspace_id").eq(ws))
            .and_where(Expr::col("event_id").eq(event))
            .and_where(Expr::col("dry_run").eq(0_i64))
            .order_by_columns([("started", Order::Asc), ("id", Order::Asc)]),
        |r| {
            Ok(EventRun {
                id: r.get(0)?,
                flow_id: r.get(1)?,
                status: r.get(2)?,
                outcome: r.get(3)?,
                label: r.get(4)?,
                error: r.get(5)?,
                context: r.get::<crate::contract::Json<serde_json::Value>>(6)?.0,
                item_id: r.get(7)?,
            })
        },
    )
}

pub fn flow_labels(
    read: &mut impl Read,
    ws: &str,
    ids: &[&str],
) -> Result<Vec<(String, (String, bool))>> {
    let ids = read.db().dialect().text_array(ids)?;
    read.all(
        Query::select()
            .columns(["id", "name", "deleted"])
            .from("flows")
            .and_where(Expr::col("workspace_id").eq(ws))
            .and_where(Expr::col("id").in_subquery(ids)),
        |r| Ok((r.get(0)?, (r.get(1)?, r.get(2)?))),
    )
}

pub fn source_triage_bot(read: &mut impl Read, ws: &str, conn: &str) -> Result<Option<String>> {
    Ok(read
        .one(
            Query::select()
                .column("triage_bot")
                .from("source_settings")
                .and_where(Expr::col("workspace_id").eq(ws))
                .and_where(Expr::col("connection_id").eq(conn)),
            |r| r.get(0),
        )?
        .flatten())
}

pub fn outcome_label(c: &mut impl Read, ws: &str, id: &str) -> Result<Option<String>> {
    c.one(
        Query::select()
            .column("outcome_label")
            .from("ext_events")
            .and_where(Expr::col("workspace_id").eq(ws))
            .and_where(Expr::col("id").eq(id)),
        |r| r.get(0),
    )
}

pub fn channel_event_connection(
    r: &mut impl Read,
    ws: &str,
    event: &str,
    space: &str,
    message: &str,
) -> Result<Option<String>> {
    Ok(r.one(
        Query::select()
            .column("connection_id")
            .from("ext_events")
            .and_where(Expr::col("id").eq(event))
            .and_where(Expr::col("workspace_id").eq(ws))
            .and_where(Expr::col("space_id").eq(space))
            .and_where(Expr::col("message_id").eq(message)),
        |row| row.get(0),
    )?
    .flatten())
}

pub fn recorded_outcome(w: &mut crate::W<'_>, id: &str, outcome: &str) -> Result<()> {
    w.exec(
        Query::update()
            .table("ext_events")
            .value("outcome", outcome)
            .and_where(Expr::col("id").eq(id))
            .and_where(Expr::col("outcome").eq("recorded")),
    )?;
    Ok(())
}

pub fn event_item(w: &mut crate::W<'_>, ws: &str, id: &str, item: &str, label: &str) -> Result<()> {
    w.exec(
        Query::update()
            .table("ext_events")
            .value("outcome", "item")
            .value("outcome_label", label)
            .value("work_item_id", item)
            .and_where(Expr::col("id").eq(id))
            .and_where(Expr::col("workspace_id").eq(ws)),
    )?;
    Ok(())
}
