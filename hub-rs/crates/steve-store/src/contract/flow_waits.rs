use sea_query::{Cond, Expr, ExprTrait, JoinType, OnConflict, Order, Query, SelectStatement};
use serde_json::Value;

use crate::{Result, W};

pub struct FlowWaits<'a, C>(pub &'a mut C);

pub struct NewWait<'a> {
    pub run: &'a str,
    pub node: &'a str,
    pub ws: &'a str,
    pub mode: &'a str,
    pub until: Option<i64>,
    pub matcher: &'a Value,
    pub decision: Option<&'a str>,
    pub now: i64,
}

impl FlowWaits<'_, W<'_>> {
    pub fn remove(&mut self, run: &str, node: &str) -> Result<bool> {
        Ok(self.0.exec(
            Query::delete()
                .from_table("flow_waits")
                .and_where(Expr::col("run_id").eq(run))
                .and_where(Expr::col("node_id").eq(node)),
        )? > 0)
    }

    pub fn clear(&mut self, run: &str) -> Result<()> {
        self.0.exec(
            Query::delete()
                .from_table("flow_waits")
                .and_where(Expr::col("run_id").eq(run)),
        )?;
        Ok(())
    }

    pub fn put(&mut self, row: &NewWait<'_>) -> Result<()> {
        let wait = crate::wait_repo::default_flow_wait_contract(
            self.0, row.ws, row.mode, row.now, row.until,
        )?;
        let wait_owner = wait.owner.to_code();
        self.0.exec(
            Query::insert()
                .into_table("flow_waits")
                .columns([
                    "run_id",
                    "node_id",
                    "workspace_id",
                    "mode",
                    "until",
                    "match",
                    "decision_id",
                    "created",
                    "wait_owner",
                    "wait_started_at",
                    "wait_deadline",
                    "wait_on_timeout",
                ])
                .values_panic([
                    row.run.into(),
                    row.node.into(),
                    row.ws.into(),
                    row.mode.into(),
                    Some(wait.deadline.0).into(),
                    row.matcher.to_string().into(),
                    row.decision.into(),
                    row.now.into(),
                    wait_owner.as_str().into(),
                    wait.started_at.0.into(),
                    wait.deadline.0.into(),
                    wait.on_timeout.as_str().into(),
                ])
                .on_conflict(
                    OnConflict::columns(["run_id", "node_id"])
                        .update_columns([
                            "workspace_id",
                            "mode",
                            "until",
                            "match",
                            "decision_id",
                            "created",
                            "wait_owner",
                            "wait_started_at",
                            "wait_deadline",
                            "wait_on_timeout",
                        ])
                        .to_owned(),
                ),
        )?;
        Ok(())
    }
}

pub struct EventWait {
    pub run: String,
    pub node: String,
    pub matcher: Option<String>,
}

pub struct WaitRow {
    pub mode: String,
    pub matcher: Value,
    pub decision: Option<String>,
    pub wait: steve_types::WaitContract,
}

pub struct DueWait {
    pub run: String,
    pub node: String,
    pub mode: String,
    pub matcher: Option<String>,
    pub decision: Option<String>,
}

fn deadlines() -> SelectStatement {
    Query::select()
        .from_as("flow_waits", "w")
        .join_as(
            JoinType::InnerJoin,
            "flow_runs",
            "r",
            Expr::col(("r", "id")).equals(("w", "run_id")),
        )
        .and_where(Expr::col(("w", "until")).is_not_null())
        .and_where(Expr::col(("w", "mode")).is_in(["delay", "until", "event", "decision", "flow"]))
        .and_where(Expr::col(("r", "status")).eq("waiting"))
        .take()
}

fn due(dialect: super::Dialect, now: i64) -> SelectStatement {
    let at = Expr::col(("w", "until"));
    deadlines()
        .cond_where(
            Cond::any()
                .add(dialect.invalid_integer(at.clone()))
                .add(at.lte(now)),
        )
        .take()
}

impl<C: crate::Read> FlowWaits<'_, C> {
    pub fn next_deadline(&mut self, now: i64) -> Result<Option<i64>> {
        super::next_deadline(self.0, &mut deadlines(), Expr::col(("w", "until")), now)
    }

    pub fn due(&mut self, now: i64, limit: u64) -> Result<Vec<i64>> {
        let dialect = self.0.db().dialect();
        self.0.all(
            due(dialect, now)
                .expr(dialect.insertion_order(Some("w")))
                .order_by_columns([
                    (("w", "until"), Order::Asc),
                    (("w", "run_id"), Order::Asc),
                    (("w", "node_id"), Order::Asc),
                ])
                .limit(limit),
            |row| row.get(0),
        )
    }

    pub fn due_at(&mut self, locator: i64, now: i64) -> Result<Option<DueWait>> {
        let dialect = self.0.db().dialect();
        self.0.one(
            due(dialect, now)
                .columns([
                    ("w", "run_id"),
                    ("w", "node_id"),
                    ("w", "mode"),
                    ("w", "match"),
                    ("w", "until"),
                    ("w", "decision_id"),
                ])
                .and_where(dialect.insertion_order(Some("w")).eq(locator)),
            |row| {
                // Decode the deadline so malformed rows take the terminal failure path.
                row.get::<i64>(4)?;
                Ok(DueWait {
                    run: row.get(0)?,
                    node: row.get(1)?,
                    mode: row.get(2)?,
                    matcher: row.get(3)?,
                    decision: row.get(5)?,
                })
            },
        )
    }

    pub fn scope_at(&mut self, locator: i64) -> Result<Option<(String, String)>> {
        let key = self.0.db().dialect().insertion_order(None);
        self.0.one(
            Query::select()
                .columns(["workspace_id", "run_id"])
                .from("flow_waits")
                .and_where(key.eq(locator)),
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
    }

    pub fn get(&mut self, run: &str, node: &str) -> Result<Option<WaitRow>> {
        self.0.one(
            Query::select()
                .columns([
                    "mode",
                    "match",
                    "decision_id",
                    "wait_owner",
                    "wait_started_at",
                    "wait_deadline",
                    "wait_on_timeout",
                ])
                .from("flow_waits")
                .and_where(Expr::col("run_id").eq(run))
                .and_where(Expr::col("node_id").eq(node)),
            |row| {
                let wait = crate::wait_repo::decode_contract(
                    row.get(3)?,
                    row.get(4)?,
                    row.get(5)?,
                    row.get(6)?,
                )?;
                Ok(WaitRow {
                    mode: row.get(0)?,
                    matcher: row
                        .get::<Option<String>>(1)?
                        .and_then(|raw| serde_json::from_str(&raw).ok())
                        .unwrap_or(Value::Null),
                    decision: row.get(2)?,
                    wait,
                })
            },
        )
    }

    pub fn child_wait(&mut self, parent: &str, child: &str) -> Result<Option<String>> {
        let child_key = self
            .0
            .db()
            .dialect()
            .json_field(Expr::col("match"), "child");
        self.0.one(
            Query::select()
                .column("node_id")
                .from("flow_waits")
                .and_where(Expr::col("run_id").eq(parent))
                .and_where(Expr::col("mode").eq("flow"))
                .and_where(child_key.eq(child)),
            |row| row.get(0),
        )
    }

    pub fn matching_event(&mut self, ws: &str) -> Result<Vec<EventWait>> {
        self.0.all(
            Query::select()
                .columns([("w", "run_id"), ("w", "node_id"), ("w", "match")])
                .from_as("flow_waits", "w")
                .join_as(
                    JoinType::InnerJoin,
                    "flow_runs",
                    "r",
                    Expr::col(("r", "id")).equals(("w", "run_id")),
                )
                .and_where(Expr::col(("w", "workspace_id")).eq(ws))
                .and_where(Expr::col(("w", "mode")).eq("event"))
                .and_where(Expr::col(("r", "status")).eq("waiting"))
                .order_by(("w", "created"), Order::Asc),
            |row| {
                Ok(EventWait {
                    run: row.get(0)?,
                    node: row.get(1)?,
                    matcher: row.get(2)?,
                })
            },
        )
    }
}
