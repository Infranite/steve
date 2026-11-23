use sea_query::{Expr, ExprTrait, Order, Query, SelectStatement};
use serde::Serialize;
use serde_json::Value;

use crate::contract::{Json, Q, Row};
use crate::{R, Read, Result, W, uid};

#[derive(Debug, Serialize)]
pub struct Change {
    pub id: String,
    pub run: String,
    pub writer: String,
    pub op: String,
    pub target: String,
    pub packet: Option<String>,
    pub before: Value,
    pub after: Value,
    pub state: String,
    pub created: i64,
}

impl Change {
    pub fn is_todo(&self) -> bool {
        self.op == "todo" || self.op.starts_with("todo_")
    }
}

pub struct NewChange<'a> {
    pub ws: &'a str,
    pub run: &'a str,
    pub writer: &'a str,
    pub op: &'a str,
    pub target: &'a str,
    pub packet: Option<&'a str>,
    pub before: &'a Value,
    pub after: &'a Value,
    pub now: i64,
}

pub struct Changes<'a, C>(&'a mut C);

impl R<'_> {
    pub fn digest_changes(&mut self) -> Changes<'_, Self> {
        Changes(self)
    }
}

impl W<'_> {
    pub fn digest_changes(&mut self) -> Changes<'_, Self> {
        Changes(self)
    }
}

fn changes(ws: &str) -> SelectStatement {
    Query::select()
        .columns([
            "id",
            "run_id",
            "writer",
            "op",
            "target",
            "packet_id",
            "before",
            "after",
            "state",
            "created",
        ])
        .from("digest_changes")
        .and_where(Expr::col("workspace_id").eq(ws))
        .take()
}

fn row(r: &Row<'_>) -> Result<Change> {
    Ok(Change {
        id: r.get(0)?,
        run: r.get(1)?,
        writer: r.get(2)?,
        op: r.get(3)?,
        target: r.get(4)?,
        packet: r.get(5)?,
        before: r.get::<Json<_>>(6)?.0,
        after: r.get::<Json<_>>(7)?.0,
        state: r.get(8)?,
        created: r.get(9)?,
    })
}

impl<C: Read> Changes<'_, C> {
    pub fn get(&mut self, ws: &str, id: &str) -> Result<Option<Change>> {
        self.0
            .one(changes(ws).and_where(Expr::col("id").eq(id)), row)
    }

    /// Newest first by default. With an offset, reads a report in chronological order.
    pub fn list(
        &mut self,
        ws: &str,
        run: Option<&str>,
        packet: Option<&str>,
        limit: usize,
        after: Option<usize>,
    ) -> Result<Vec<Change>> {
        let mut q = changes(ws);
        if let Some(run) = run {
            q.and_where(Expr::col("run_id").eq(run));
        }
        if let Some(packet) = packet {
            q.and_where(Expr::col("packet_id").eq(packet));
        }
        let order = if let Some(after) = after {
            q.offset(after.min(i64::MAX as usize) as u64);
            Order::Asc
        } else {
            Order::Desc
        };
        self.0.all(
            q.order_by("created", order.clone())
                .order_by("id", order)
                .limit(limit.clamp(1, 500) as u64),
            row,
        )
    }
}

impl Changes<'_, W<'_>> {
    pub fn record(&mut self, change: NewChange<'_>) -> Result<String> {
        let id = uid("dch");
        self.0.exec(
            Query::insert()
                .into_table("digest_changes")
                .columns([
                    "id",
                    "workspace_id",
                    "run_id",
                    "writer",
                    "op",
                    "target",
                    "packet_id",
                    "before",
                    "after",
                    "created",
                    "updated",
                ])
                .values_panic([
                    id.as_str().into(),
                    change.ws.into(),
                    change.run.into(),
                    change.writer.into(),
                    change.op.into(),
                    change.target.into(),
                    change.packet.into(),
                    change.before.to_string().into(),
                    change.after.to_string().into(),
                    change.now.into(),
                    change.now.into(),
                ]),
        )?;
        super::emit(self.0, change.ws, &id);
        Ok(id)
    }

    pub fn mark_undone(&mut self, ws: &str, id: &str, now: i64) -> Result<()> {
        self.0.exec(
            Query::update()
                .table("digest_changes")
                .value("state", "undone")
                .value("updated", now)
                .and_where(Expr::col("workspace_id").eq(ws))
                .and_where(Expr::col("id").eq(id)),
        )?;
        super::emit(self.0, ws, id);
        Ok(())
    }

    pub fn set_segment_run(
        &mut self,
        ws: &str,
        segment: &str,
        run: &str,
        opened: bool,
    ) -> Result<()> {
        let Some((id, mut after)) = self.0.one(
            Query::select()
                .columns(["id", "after"])
                .from("digest_changes")
                .and_where(Expr::col("workspace_id").eq(ws))
                .and_where(Expr::col("op").eq("segment"))
                .and_where(Expr::col("target").eq(segment)),
            |r| Ok((r.get::<String>(0)?, r.get::<Json<Value>>(1)?.0)),
        )?
        else {
            return Ok(());
        };
        after["opened"] = Value::Bool(opened);
        self.0.exec(
            Query::update()
                .table("digest_changes")
                .value("run_id", run)
                .value("after", after.to_string())
                .and_where(Expr::col("id").eq(id)),
        )?;
        Ok(())
    }
}
