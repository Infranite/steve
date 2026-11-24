use crate::base::Level;
use crate::contract::Row;
use crate::{Read, Result, W, now, uid};
use sea_query::{Expr, ExprTrait, Func, Order, Query, SelectStatement};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;

pub struct KnowledgeUses<'a, C>(pub &'a mut C);
pub struct MemoryChanges<'a, C>(pub &'a mut C);

/// One stored hit (`kb_uses.hits[]`).
#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
pub struct UseHit {
    pub n: usize,
    pub obj_type: String,
    pub obj_id: String,
    #[serde(default)]
    pub seq: i64,
    #[serde(default)]
    pub score: f32,
    pub level: Level,
    #[serde(default)]
    pub label: String,
}

/// One `kb_uses` row.
#[derive(Debug, Clone, PartialEq)]
pub struct UseRow {
    pub id: String,
    pub query: String,
    pub hits: Vec<UseHit>,
    pub created: i64,
}

pub struct UsedPiece {
    pub consumer_type: String,
    pub consumer_id: String,
    pub query: String,
    pub created: i64,
    pub kind: String,
    pub id: String,
    pub label: String,
}

impl<C: Read> KnowledgeUses<'_, C> {
    pub fn by_consumer(&mut self, ws: &str, kind: &str, id: &str) -> Result<Vec<UseRow>> {
        self.0.all(
            Query::select()
                .columns(["id", "query", "hits", "created"])
                .from("kb_uses")
                .and_where(Expr::col("workspace_id").eq(ws))
                .and_where(Expr::col("consumer_type").eq(kind))
                .and_where(Expr::col("consumer_id").eq(id))
                .order_by("created", Order::Asc)
                .order_by("rowid", Order::Asc),
            |r| {
                Ok(UseRow {
                    id: r.get(0)?,
                    query: r.get(1)?,
                    hits: serde_json::from_str(&r.get::<String>(2)?).unwrap_or_default(),
                    created: r.get(3)?,
                })
            },
        )
    }

    fn hits(&mut self, ws: &str) -> SelectStatement {
        let dialect = self.0.db().dialect();
        Query::select()
            .from_as("kb_uses", "u")
            .from_function(dialect.json_array_elements(Expr::col(("u", "hits"))), "j")
            .and_where(Expr::col(("u", "workspace_id")).eq(ws))
            .take()
    }

    pub fn counts(&mut self, ws: &str, obj_type: &str) -> Result<HashMap<String, (i64, i64)>> {
        let dialect = self.0.db().dialect();
        let id = dialect.json_field(Expr::col(("j", "value")), "obj_id");
        let kind = dialect.json_field(Expr::col(("j", "value")), "obj_type");
        let q = self
            .hits(ws)
            .expr(id.clone())
            .expr(Func::count(Expr::col(("u", "id"))))
            .expr(Func::max(Expr::col(("u", "created"))))
            .and_where(kind.eq(obj_type))
            .add_group_by([id])
            .take();
        Ok(self
            .0
            .all(&q, |r| {
                Ok((r.get::<Option<String>>(0)?, (r.get(1)?, r.get(2)?)))
            })?
            .into_iter()
            .filter_map(|(id, counts)| id.map(|id| (id, counts)))
            .collect())
    }

    pub fn recent_pieces(&mut self, ws: &str) -> Result<Vec<UsedPiece>> {
        let dialect = self.0.db().dialect();
        let field = |name| dialect.json_field(Expr::col(("j", "value")), name);
        let kind = field("obj_type");
        let q = self
            .hits(ws)
            .columns(["consumer_type", "consumer_id", "query", "created"].map(|c| ("u", c)))
            .exprs([kind.clone(), field("obj_id"), field("label")])
            .and_where(kind.is_in(["mem", "chunk"]))
            .order_by(("u", "created"), Order::Desc)
            .order_by(("u", "rowid"), Order::Desc)
            .take();
        self.0.all(&q, |r| {
            Ok(UsedPiece {
                consumer_type: r.get(0)?,
                consumer_id: r.get(1)?,
                query: r.get(2)?,
                created: r.get(3)?,
                kind: r.get::<Option<String>>(4)?.unwrap_or_default(),
                id: r.get::<Option<String>>(5)?.unwrap_or_default(),
                label: r.get::<Option<String>>(6)?.unwrap_or_default(),
            })
        })
    }
}

impl KnowledgeUses<'_, W<'_>> {
    pub fn record(
        &mut self,
        ws: &str,
        consumer: (&str, &str),
        query: &str,
        hits: &[UseHit],
    ) -> Result<String> {
        let id = uid("ku");
        self.0.exec(
            Query::insert()
                .into_table("kb_uses")
                .columns([
                    "id",
                    "workspace_id",
                    "consumer_type",
                    "consumer_id",
                    "query",
                    "hits",
                    "created",
                ])
                .values_panic([
                    id.as_str().into(),
                    ws.into(),
                    consumer.0.into(),
                    consumer.1.into(),
                    query.into(),
                    serde_json::to_string(hits)?.into(),
                    now().into(),
                ]),
        )?;
        Ok(id)
    }
}

pub struct MemoryChange {
    pub id: String,
    pub op: String,
    pub mem_id: String,
    pub targets: Vec<String>,
    pub before: Value,
    pub after: Value,
    pub reason: String,
    pub state: String,
    pub created: i64,
}

pub struct NewMemoryChange<'a> {
    pub run: &'a str,
    pub writer: &'a str,
    pub op: &'a str,
    pub mem: &'a str,
    pub targets: &'a [String],
    pub before: &'a Value,
    pub after: &'a Value,
    pub reason: &'a str,
    pub state: &'a str,
}

fn change_row(r: &Row<'_>) -> Result<MemoryChange> {
    let json = |s: String| serde_json::from_str(&s).unwrap_or(Value::Null);
    Ok(MemoryChange {
        id: r.get(0)?,
        op: r.get(1)?,
        mem_id: r.get(2)?,
        targets: serde_json::from_str(&r.get::<String>(3)?).unwrap_or_default(),
        before: json(r.get(4)?),
        after: json(r.get(5)?),
        reason: r.get(6)?,
        state: r.get(7)?,
        created: r.get(8)?,
    })
}

fn workspace(ws: &str) -> SelectStatement {
    Query::select()
        .from("mem_changes")
        .and_where(Expr::col("workspace_id").eq(ws))
        .take()
}

const CHANGE_COLUMNS: [&str; 9] = [
    "id", "op", "mem_id", "targets", "before", "after", "reason", "state", "created",
];

impl<C: Read> MemoryChanges<'_, C> {
    pub fn get(&mut self, ws: &str, id: &str) -> Result<Option<MemoryChange>> {
        self.0.one(
            workspace(ws)
                .columns(CHANGE_COLUMNS)
                .and_where(Expr::col("id").eq(id)),
            change_row,
        )
    }

    pub fn runs(
        &mut self,
        ws: &str,
        run: Option<&str>,
        limit: usize,
    ) -> Result<Vec<(String, String, i64)>> {
        let mut q = workspace(ws);
        q.columns(["run_id", "writer"])
            .expr_as(Func::max(Expr::col("created")), "at")
            .group_by_columns(["run_id", "writer"]);
        match run {
            Some(run) => {
                q.and_where(Expr::col("run_id").eq(run));
            }
            None => {
                q.order_by("at", Order::Desc).limit(limit as u64);
            }
        }
        self.0.all(&q, |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
    }

    pub fn of_run(&mut self, ws: &str, run: &str, writer: &str) -> Result<Vec<MemoryChange>> {
        self.0.all(
            workspace(ws)
                .columns(CHANGE_COLUMNS)
                .and_where(Expr::col("run_id").eq(run))
                .and_where(Expr::col("writer").eq(writer))
                .order_by("created", Order::Asc)
                .order_by("rowid", Order::Asc),
            change_row,
        )
    }

    /// Unresolved `flag` conflicts created after `since`.
    pub fn open_flags_since(&mut self, ws: &str, since: i64) -> Result<usize> {
        let n: i64 = self
            .0
            .one(
                workspace(ws)
                    .expr(Func::count(Expr::col("id")))
                    .and_where(Expr::col("op").eq("flag"))
                    .and_where(Expr::col("state").eq("open"))
                    .and_where(Expr::col("created").gt(since)),
                |r| r.get(0),
            )?
            .unwrap_or(0);
        Ok(usize::try_from(n).unwrap_or(0))
    }

    /// Start time of the newest non-skipped run of `flow_id`.
    pub fn last_steward_run_at(&mut self, ws: &str, flow_id: &str) -> Result<Option<i64>> {
        self.0.one(
            Query::select()
                .column("started")
                .from("flow_runs")
                .and_where(Expr::col("workspace_id").eq(ws))
                .and_where(Expr::col("flow_id").eq(flow_id))
                .and_where(Expr::col("status").ne("skipped"))
                .order_by("started", Order::Desc)
                .limit(1),
            |r| r.get(0),
        )
    }

    /// Number of skipped runs recorded for `flow_id`.
    pub fn skipped_steward_runs(&mut self, ws: &str, flow_id: &str) -> Result<i64> {
        Ok(self
            .0
            .one(
                Query::select()
                    .expr(Func::count(Expr::col("id")))
                    .from("flow_runs")
                    .and_where(Expr::col("workspace_id").eq(ws))
                    .and_where(Expr::col("flow_id").eq(flow_id))
                    .and_where(Expr::col("status").eq("skipped")),
                |r| r.get(0),
            )?
            .unwrap_or(0))
    }
}

impl MemoryChanges<'_, W<'_>> {
    pub fn record(&mut self, ws: &str, c: NewMemoryChange<'_>) -> Result<String> {
        let id = uid("mch");
        let at = now();
        self.0.exec(
            Query::insert()
                .into_table("mem_changes")
                .columns([
                    "id",
                    "workspace_id",
                    "run_id",
                    "writer",
                    "op",
                    "mem_id",
                    "targets",
                    "before",
                    "after",
                    "reason",
                    "state",
                    "created",
                    "updated",
                ])
                .values_panic([
                    id.as_str().into(),
                    ws.into(),
                    c.run.into(),
                    c.writer.into(),
                    c.op.into(),
                    c.mem.into(),
                    serde_json::to_string(c.targets)?.into(),
                    c.before.to_string().into(),
                    c.after.to_string().into(),
                    c.reason.into(),
                    c.state.into(),
                    at.into(),
                    at.into(),
                ]),
        )?;
        Ok(id)
    }

    pub fn set_state(&mut self, id: &str, state: &str, before: Option<&Value>) -> Result<()> {
        let mut q = Query::update();
        q.table("mem_changes")
            .value("state", state)
            .value("updated", now())
            .and_where(Expr::col("id").eq(id));
        if let Some(before) = before {
            q.value("before", before.to_string());
        }
        self.0.exec(&q)?;
        Ok(())
    }
}
