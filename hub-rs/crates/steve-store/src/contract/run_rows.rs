use sea_query::{Cond, Expr, ExprTrait, Func, Iden, JoinType, Query};

use super::Q;
use crate::{Read, Result, W, uid};
use serde_json::Value;

pub struct RunRows<'a, C>(pub &'a mut C);

pub struct StartupRecord {
    pub id: String,
    pub context: Value,
    pub unreadable: bool,
}

/// One `flow_runs` row.
#[derive(Debug, Clone, PartialEq)]
pub struct RunRow {
    pub id: String,
    pub workspace_id: String,
    pub flow_id: String,
    pub flow_version: i64,
    pub trigger_kind: String,
    pub status: String,
    pub outcome: Option<String>,
    pub outcome_label: String,
    pub cursor: Option<String>,
    pub context: Value,
    pub error: Option<String>,
    pub retry_at: Option<i64>,
    pub attempts: i64,
    pub work_item_id: Option<String>,
    pub cost: f64,
    pub started: i64,
    pub ended: Option<i64>,
    /// The event that started it (event-triggered runs).
    pub event_id: Option<String>,
    pub dry_run: bool,
    /// The run whose `flow` node called this one, and how deep that nesting is.
    pub parent_run: Option<String>,
    pub depth: i64,
}

impl RunRow {
    /// The planned time this run stands for (`context.at`); the start time for older rows.
    pub fn at(&self) -> i64 {
        self.context
            .get("at")
            .and_then(Value::as_i64)
            .unwrap_or(self.started)
    }
}

fn run_query() -> sea_query::SelectStatement {
    Query::select()
        .columns([
            RunColumn::Id,
            RunColumn::WorkspaceId,
            RunColumn::FlowId,
            RunColumn::FlowVersion,
            RunColumn::TriggerKind,
            RunColumn::Status,
            RunColumn::Outcome,
            RunColumn::OutcomeLabel,
            RunColumn::Cursor,
            RunColumn::Context,
            RunColumn::Error,
            RunColumn::RetryAt,
            RunColumn::Attempts,
            RunColumn::WorkItemId,
            RunColumn::Cost,
            RunColumn::Started,
            RunColumn::Ended,
            RunColumn::EventId,
            RunColumn::DryRun,
            RunColumn::ParentRun,
            RunColumn::Depth,
        ])
        .from("flow_runs")
        .take()
}

fn run(r: &super::Row<'_>) -> Result<RunRow> {
    Ok(RunRow {
        id: r.get(0)?,
        workspace_id: r.get(1)?,
        flow_id: r.get(2)?,
        flow_version: r.get(3)?,
        trigger_kind: r.get(4)?,
        status: r.get(5)?,
        outcome: r.get(6)?,
        outcome_label: r.get(7)?,
        cursor: r.get(8)?,
        context: serde_json::from_str(&r.get::<String>(9)?)
            .unwrap_or_else(|_| serde_json::json!({})),
        error: r.get(10)?,
        retry_at: r.get(11)?,
        attempts: r.get(12)?,
        work_item_id: r.get(13)?,
        cost: r.get(14)?,
        started: r.get(15)?,
        ended: r.get(16)?,
        event_id: r.get(17)?,
        dry_run: r.get(18)?,
        parent_run: r.get(19)?,
        depth: r.get(20)?,
    })
}

#[derive(Iden)]
enum RunColumn {
    Id,
    WorkspaceId,
    FlowId,
    FlowVersion,
    TriggerKind,
    Status,
    OutcomeLabel,
    Error,
    Cursor,
    Context,
    Started,
    Ended,
    RetryAt,
    Attempts,
    Outcome,
    WorkItemId,
    Cost,
    EventId,
    DryRun,
    ParentRun,
    Depth,
}

/// A row to insert.
#[derive(Debug, Clone)]
pub struct NewRun<'a> {
    pub ws: &'a str,
    pub flow_id: &'a str,
    pub flow_version: i64,
    pub trigger_kind: &'a str,
    /// Initial run status (including a terminal startup failure).
    pub status: &'a str,
    pub outcome_label: &'a str,
    /// A terminal startup rejection is inserted with its error, never patched afterwards.
    pub error: Option<&'a str>,
    /// Next node to run (`None` for skipped rows).
    pub cursor: Option<&'a str>,
    pub context: &'a Value,
    pub now: i64,
}

/// One step of a run.
#[derive(Debug, Clone)]
pub struct NewStep<'a> {
    pub node_id: &'a str,
    /// `ok | skipped | waiting | failed`
    pub status: &'a str,
    pub branch: Option<&'a str>,
    pub input: &'a Value,
    pub output: &'a Value,
    pub ms: f64,
    pub error: Option<&'a str>,
    pub started: i64,
}

pub struct TriageRun {
    pub id: String,
    pub flow: String,
    pub status: String,
    pub outcome: Option<String>,
    pub label: String,
    pub item: Option<String>,
    pub context: Value,
}

#[derive(serde::Serialize)]
pub struct RunStep {
    pub seq: i64,
    pub node_id: String,
    pub status: String,
    pub branch: Option<String>,
    pub input: Option<Value>,
    pub output: Option<Value>,
    pub ms: f64,
    pub error: Option<String>,
}

fn waiting() -> Expr {
    Expr::col("status").eq("waiting")
}

fn retry_due(dialect: super::Dialect, now: i64) -> sea_query::Condition {
    Cond::all()
        .add(waiting())
        .add(Expr::col("retry_at").is_not_null())
        .add(
            Cond::any()
                .add(dialect.invalid_integer_columns("retry_at", &[]))
                .add(Expr::col("retry_at").lte(now)),
        )
}

impl<C: Read> RunRows<'_, C> {
    pub fn work_items(&mut self, ws: &str, flow: &str) -> Result<Vec<String>> {
        self.0.all(
            Query::select()
                .distinct()
                .column("work_item_id")
                .from("flow_runs")
                .and_where(Expr::col("workspace_id").eq(ws))
                .and_where(Expr::col("flow_id").eq(flow))
                .and_where(Expr::col("work_item_id").is_not_null()),
            |r| r.get(0),
        )
    }

    pub fn latest_start(&mut self, ws: &str, flow: &str) -> Result<Option<i64>> {
        Ok(self
            .0
            .one(
                Query::select()
                    .expr(Func::max(Expr::col("started")))
                    .from("flow_runs")
                    .and_where(Expr::col("workspace_id").eq(ws))
                    .and_where(Expr::col("flow_id").eq(flow)),
                |r| r.get::<Option<i64>>(0),
            )?
            .flatten())
    }

    pub fn activity(&mut self, ws: &str, flow: &str, since: i64) -> Result<(i64, Option<i64>)> {
        let query = || {
            Query::select()
                .from("flow_runs")
                .and_where(Expr::col("workspace_id").eq(ws))
                .and_where(Expr::col("flow_id").eq(flow))
                .and_where(Expr::col("dry_run").eq(0_i64))
                .and_where(Expr::col("status").ne("skipped"))
                .take()
        };
        let count = self
            .0
            .one(
                query()
                    .expr(Func::count(Expr::col("id")))
                    .and_where(Expr::col("started").gte(since)),
                |r| r.get(0),
            )?
            .unwrap_or(0);
        let latest = self
            .0
            .one(query().expr(Func::max(Expr::col("started"))), |r| r.get(0))?
            .flatten();
        Ok((count, latest))
    }

    pub fn inspect_startup<T>(
        &mut self,
        ws: &str,
        flow: &str,
        source: &str,
        include_unattributed: bool,
        mut select: impl FnMut(StartupRecord) -> Option<T>,
    ) -> Result<Option<T>> {
        let dialect = self.0.db().dialect();
        let order = dialect.insertion_order(None);
        let context = Expr::col("context");
        let text = context.clone().cast_as("TEXT");
        let context_type = dialect.storage_type(context);
        let valid = dialect
            .json_valid(text.clone())
            .and(dialect.json_type(text.clone(), "$").eq("object"));
        let attributed = dialect
            .json_field(text.clone(), "source")
            .eq(source)
            .and(dialect.json_type(text.clone(), "$.startup").is_not_null());
        let mut query = Query::select();
        query
            .from("flow_runs")
            .expr(order.clone())
            .column("id")
            .expr(text)
            .expr(context_type)
            .and_where(Expr::col("workspace_id").eq(ws))
            .and_where(Expr::col("flow_id").eq(flow))
            .and_where(
                Expr::case(valid, attributed)
                    .finally(include_unattributed)
                    .into(),
            )
            .order_by_expr(order, sea_query::Order::Desc);
        if !include_unattributed {
            query.limit(1);
        }
        let mut result = None;
        self.0.scan(&query, |row| {
            if result.is_none() {
                let order: i64 = row.get(0)?;
                let id = row.get::<String>(1).ok();
                let context = row
                    .get::<String>(2)
                    .ok()
                    .and_then(|raw| serde_json::from_str(&raw).ok());
                let unreadable =
                    id.is_none() || context.is_none() || row.get::<String>(3)? != "text";
                result = select(StartupRecord {
                    id: id.unwrap_or_else(|| format!("rowid:{order}")),
                    context: context.unwrap_or(Value::Null),
                    unreadable,
                });
            }
            Ok(())
        })?;
        Ok(result)
    }

    pub fn latest_context(
        &mut self,
        ws: &str,
        flow: &str,
    ) -> Result<Option<(Option<String>, Value)>> {
        let tie = self.0.db().dialect().insertion_order(None);
        self.0.one(
            Query::select()
                .columns(["id", "context"])
                .from("flow_runs")
                .and_where(Expr::col("workspace_id").eq(ws))
                .and_where(Expr::col("flow_id").eq(flow))
                .order_by_expr(tie, sea_query::Order::Desc)
                .limit(1),
            |row| {
                Ok((
                    row.get(0)?,
                    row.get::<String>(1)
                        .ok()
                        .and_then(|raw| serde_json::from_str(&raw).ok())
                        .unwrap_or(Value::Null),
                ))
            },
        )
    }

    pub fn interrupted(&mut self, schedules: bool) -> Result<Vec<String>> {
        let kind = Expr::col(("f", "kind"));
        self.0.all(
            Query::select()
                .column(("r", "id"))
                .from_as("flow_runs", "r")
                .join_as(
                    JoinType::InnerJoin,
                    "flows",
                    "f",
                    Expr::col(("f", "id")).equals(("r", "flow_id")),
                )
                .and_where(Expr::col(("r", "status")).eq("running"))
                .and_where(if schedules {
                    kind.eq("schedule")
                } else {
                    kind.ne("schedule")
                })
                .order_by(("r", "started"), sea_query::Order::Asc),
            |row| row.get(0),
        )
    }

    pub fn item_origins(
        &mut self,
        ws: &str,
        flow: &str,
        offset: u64,
        count: u64,
    ) -> Result<Vec<(String, Option<String>)>> {
        self.0.all(
            Query::select()
                .columns(["id", "work_item_id"])
                .from("flow_runs")
                .and_where(Expr::col("workspace_id").eq(ws))
                .and_where(Expr::col("flow_id").eq(flow))
                .and_where(Expr::col("dry_run").eq(0_i64))
                .order_by("started", sea_query::Order::Desc)
                .order_by("id", sea_query::Order::Asc)
                .limit(count)
                .offset(offset),
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
    }

    pub fn valid_wait_state(&mut self, id: &str, node: &str) -> Result<bool> {
        let raw = self.0.one(
            Query::select()
                .column("context")
                .from("flow_runs")
                .and_where(Expr::col("id").eq(id))
                .and_where(Expr::col(RunColumn::Cursor).eq(node)),
            |row| row.get::<String>(0),
        )?;
        Ok(raw.is_some_and(|raw| serde_json::from_str::<Value>(&raw).is_ok_and(|v| v.is_object())))
    }

    pub fn next_retry(&mut self, now: i64) -> Result<Option<i64>> {
        super::next_deadline(
            self.0,
            Query::select()
                .from("flow_runs")
                .and_where(waiting())
                .and_where(Expr::col("retry_at").is_not_null()),
            Expr::col("retry_at"),
            now,
        )
    }

    pub fn by_event(&mut self, ws: &str, event: &str) -> Result<Vec<RunRow>> {
        let tie = self.0.db().dialect().insertion_order(None);
        self.0.all(
            run_query()
                .and_where(Expr::col("workspace_id").eq(ws))
                .and_where(Expr::col("event_id").eq(event))
                .and_where(Expr::col("dry_run").eq(0_i64))
                .order_by("started", sea_query::Order::Asc)
                .order_by_expr(tie, sea_query::Order::Asc),
            run,
        )
    }

    pub fn steps(&mut self, id: &str) -> Result<Vec<RunStep>> {
        self.0.all(
            Query::select()
                .columns([
                    "seq", "node_id", "status", "branch", "input", "output", "ms", "error",
                ])
                .from("flow_run_steps")
                .and_where(Expr::col("run_id").eq(id))
                .order_by("seq", sea_query::Order::Asc),
            |row| {
                Ok(RunStep {
                    seq: row.get(0)?,
                    node_id: row.get(1)?,
                    status: row.get(2)?,
                    branch: row.get(3)?,
                    input: row.get::<Option<super::Json<_>>>(4)?.map(|json| json.0),
                    output: row.get::<Option<super::Json<_>>>(5)?.map(|json| json.0),
                    ms: row.get(6)?,
                    error: row.get(7)?,
                })
            },
        )
    }

    pub fn get(&mut self, id: &str) -> Result<Option<RunRow>> {
        self.0
            .one(run_query().and_where(Expr::col("id").eq(id)), run)
    }

    pub fn active(&mut self, ws: &str, flow: &str) -> Result<bool> {
        self.0
            .one(
                Query::select()
                    .column("id")
                    .from("flow_runs")
                    .and_where(Expr::col("workspace_id").eq(ws))
                    .and_where(Expr::col("flow_id").eq(flow))
                    .and_where(Expr::col("status").is_in(["running", "waiting"]))
                    .limit(1),
                |_| Ok(()),
            )
            .map(|found| found.is_some())
    }

    pub fn last_real(
        &mut self,
        ws: &str,
        flow: &str,
        except: Option<&str>,
    ) -> Result<Option<RunRow>> {
        let tie = self.0.db().dialect().insertion_order(None);
        let mut q = run_query();
        q.and_where(Expr::col("workspace_id").eq(ws))
            .and_where(Expr::col("flow_id").eq(flow))
            .and_where(Expr::col("status").ne("skipped"))
            .order_by("started", sea_query::Order::Desc)
            .order_by_expr(tie, sea_query::Order::Desc)
            .limit(1);
        if let Some(except) = except {
            q.and_where(Expr::col("id").ne(except));
        }
        self.0.one(&q, run)
    }

    pub fn page(
        &mut self,
        ws: &str,
        flow: &str,
        before: i64,
        limit: u64,
        dry: Option<bool>,
    ) -> Result<Vec<RunRow>> {
        let tie = self.0.db().dialect().insertion_order(None);
        let mut q = run_query();
        q.and_where(Expr::col("workspace_id").eq(ws))
            .and_where(Expr::col("flow_id").eq(flow))
            .and_where(Expr::col("started").lt(before))
            .order_by("started", sea_query::Order::Desc)
            .order_by_expr(tie, sea_query::Order::Desc)
            .limit(limit);
        if let Some(dry) = dry {
            q.and_where(Expr::col("dry_run").eq(dry));
        }
        self.0.all(&q, run)
    }

    pub fn due_retries(&mut self, now: i64, limit: u64) -> Result<Vec<(String, String)>> {
        let due = retry_due(self.0.db().dialect(), now);
        self.0.all(
            Query::select()
                .columns(["id", "workspace_id"])
                .from("flow_runs")
                .cond_where(due)
                .order_by_columns([
                    ("retry_at", sea_query::Order::Asc),
                    ("id", sea_query::Order::Asc),
                ])
                .limit(limit),
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
    }

    pub fn triage_runs(&mut self, ws: &str, event: &str) -> Result<Vec<TriageRun>> {
        let tie = self.0.db().dialect().insertion_order(None);
        self.0.all(
            Query::select()
                .columns([
                    "id",
                    "flow_id",
                    "status",
                    "outcome",
                    "outcome_label",
                    "work_item_id",
                    "context",
                ])
                .from("flow_runs")
                .and_where(Expr::col("workspace_id").eq(ws))
                .and_where(Expr::col("event_id").eq(event))
                .and_where(Expr::col("dry_run").eq(0_i64))
                .order_by("started", sea_query::Order::Asc)
                .order_by_expr(tie, sea_query::Order::Asc),
            |row| {
                Ok(TriageRun {
                    id: row.get(0)?,
                    flow: row.get(1)?,
                    status: row.get(2)?,
                    outcome: row.get(3)?,
                    label: row.get(4)?,
                    item: row.get(5)?,
                    context: row.get::<super::Json<_>>(6)?.0,
                })
            },
        )
    }

    pub fn flow_ids(&mut self, ws: &str, runs: &[String]) -> Result<Vec<String>> {
        let mut flows = std::collections::BTreeSet::new();
        for chunk in runs.chunks(500) {
            flows.extend(
                self.0.all(
                    Query::select()
                        .distinct()
                        .column("flow_id")
                        .from("flow_runs")
                        .and_where(Expr::col("workspace_id").eq(ws))
                        .and_where(Expr::col("id").is_in(chunk.iter().map(String::as_str))),
                    |row| row.get::<String>(0),
                )?,
            );
        }
        Ok(flows.into_iter().collect())
    }

    pub fn belongs_to(&mut self, ws: &str, flow: &str, run: &str) -> Result<bool> {
        self.0
            .one(
                Query::select()
                    .column(("r", "id"))
                    .from_as("flow_runs", "r")
                    .join_as(
                        JoinType::InnerJoin,
                        "flows",
                        "f",
                        Expr::col(("f", "id")).eq(Expr::col(("r", "flow_id"))).and(
                            Expr::col(("f", "workspace_id")).eq(Expr::col(("r", "workspace_id"))),
                        ),
                    )
                    .and_where(Expr::col(("r", "workspace_id")).eq(ws))
                    .and_where(Expr::col(("r", "flow_id")).eq(flow))
                    .and_where(Expr::col(("r", "id")).eq(run)),
                |_| Ok(()),
            )
            .map(|row| row.is_some())
    }

    pub fn last_real_start(&mut self, ws: &str, flow: &str, except: &str) -> Result<Option<i64>> {
        Ok(self
            .0
            .one(
                Query::select()
                    .expr(Func::max(Expr::col("started")))
                    .from("flow_runs")
                    .and_where(Expr::col("workspace_id").eq(ws))
                    .and_where(Expr::col("flow_id").eq(flow))
                    .and_where(Expr::col("status").ne("skipped"))
                    .and_where(Expr::col("id").ne(except)),
                |row| row.get::<Option<i64>>(0),
            )?
            .flatten())
    }
}

impl RunRows<'_, W<'_>> {
    pub fn set_context(&mut self, id: &str, context: &Value) -> Result<()> {
        self.0.exec(
            Query::update()
                .table("flow_runs")
                .value("context", context.to_string())
                .and_where(Expr::col("id").eq(id)),
        )?;
        Ok(())
    }

    pub fn refresh_start_failure(
        &mut self,
        ws: &str,
        flow: &str,
        run: &str,
        error: &str,
        context: &Value,
        at: i64,
    ) -> Result<()> {
        self.0.exec(
            Query::update()
                .table("flow_runs")
                .value("error", error)
                .value("outcome_label", error)
                .value("context", context.to_string())
                .value("started", at)
                .value("ended", at)
                .and_where(Expr::col("id").eq(run)),
        )?;
        self.emit(ws, flow, run);
        Ok(())
    }

    pub fn save(&mut self, run: &RunRow) -> Result<()> {
        self.0.exec(
            Query::update()
                .table("flow_runs")
                .value(RunColumn::Status, run.status.as_str())
                .value(RunColumn::Outcome, run.outcome.as_deref())
                .value(RunColumn::OutcomeLabel, run.outcome_label.as_str())
                .value(RunColumn::Cursor, run.cursor.as_deref())
                .value(RunColumn::Context, run.context.to_string())
                .value(RunColumn::Error, run.error.as_deref())
                .value(RunColumn::RetryAt, run.retry_at)
                .value(RunColumn::Attempts, run.attempts)
                .value(RunColumn::WorkItemId, run.work_item_id.as_deref())
                .value(RunColumn::Cost, run.cost)
                .value(RunColumn::Ended, run.ended)
                .and_where(Expr::col(RunColumn::Id).eq(run.id.as_str())),
        )?;
        self.emit(&run.workspace_id, &run.flow_id, &run.id);
        Ok(())
    }

    pub fn emit(&mut self, ws: &str, flow: &str, run: &str) {
        emit_run(self.0, ws, flow, run);
    }

    pub fn claim_retry(&mut self, id: &str, now: i64, max_retries: i64) -> Result<bool> {
        let due = retry_due(self.0.dialect(), now);
        let row = self.0.one(
            Query::select()
                .columns([
                    RunColumn::RetryAt,
                    RunColumn::Cursor,
                    RunColumn::Attempts,
                    RunColumn::Context,
                ])
                .from("flow_runs")
                .and_where(Expr::col("id").eq(id))
                .cond_where(due),
            |row| {
                Ok((
                    row.get::<i64>(0)?,
                    row.get::<Option<String>>(1)?,
                    row.get::<i64>(2)?,
                    row.get::<String>(3)?,
                ))
            },
        )?;
        let Some((retry_at, cursor, attempts, context)) = row else {
            return Ok(false);
        };
        let valid_context =
            serde_json::from_str::<Value>(&context).is_ok_and(|value| value.is_object());
        if cursor.is_none() || attempts > max_retries || !valid_context {
            return self.fail_due(id, now, "Invalid stored retry state");
        }
        Ok(self.0.exec(
            Query::update()
                .table("flow_runs")
                .value("status", "running")
                .value("retry_at", Option::<i64>::None)
                .and_where(Expr::col("id").eq(id))
                .and_where(waiting())
                .and_where(Expr::col("retry_at").eq(retry_at)),
        )? == 1)
    }

    /// A wait or retry with unusable stored state is terminal.
    pub fn fail_due(&mut self, id: &str, now: i64, reason: &str) -> Result<bool> {
        let scope = self.0.one(
            Query::select()
                .columns(["workspace_id", "flow_id"])
                .from("flow_runs")
                .and_where(Expr::col("id").eq(id))
                .and_where(waiting()),
            |row| Ok((row.get::<String>(0)?, row.get::<String>(1)?)),
        )?;
        let Some((ws, flow)) = scope else {
            return Ok(false);
        };
        self.0.exec(
            Query::update()
                .table("flow_runs")
                .value("status", "failed")
                .value("retry_at", Option::<i64>::None)
                .value("error", reason)
                .value("outcome_label", reason)
                .value("ended", now)
                .and_where(Expr::col("id").eq(id))
                .and_where(waiting()),
        )?;
        self.0.exec(
            Query::delete()
                .from_table("flow_waits")
                .and_where(Expr::col("run_id").eq(id)),
        )?;
        emit_run(self.0, &ws, &flow, id);
        Ok(true)
    }

    pub fn insert(&mut self, run: &NewRun<'_>) -> Result<String> {
        let id = uid("run");
        let ended = (run.status != "running").then_some(run.now);
        self.0.exec(
            Query::insert()
                .into_table("flow_runs")
                .columns([
                    RunColumn::Id,
                    RunColumn::WorkspaceId,
                    RunColumn::FlowId,
                    RunColumn::FlowVersion,
                    RunColumn::TriggerKind,
                    RunColumn::Status,
                    RunColumn::OutcomeLabel,
                    RunColumn::Error,
                    RunColumn::Cursor,
                    RunColumn::Context,
                    RunColumn::Started,
                    RunColumn::Ended,
                ])
                .values_panic([
                    id.as_str().into(),
                    run.ws.into(),
                    run.flow_id.into(),
                    run.flow_version.into(),
                    run.trigger_kind.into(),
                    run.status.into(),
                    run.outcome_label.into(),
                    run.error.into(),
                    run.cursor.into(),
                    run.context.to_string().into(),
                    run.now.into(),
                    ended.into(),
                ]),
        )?;
        emit_run(self.0, run.ws, run.flow_id, &id);
        Ok(id)
    }

    /// Event and subflow provenance for a newly opened run.
    pub fn set_origin(
        &mut self,
        id: &str,
        event: Option<&str>,
        dry: bool,
        parent: Option<&str>,
        depth: i64,
    ) -> Result<()> {
        self.0.exec(
            Query::update()
                .table("flow_runs")
                .value("event_id", event)
                .value("dry_run", dry)
                .value("parent_run", parent)
                .value("depth", depth)
                .and_where(Expr::col("id").eq(id)),
        )?;
        Ok(())
    }

    pub fn add_step(&mut self, ws: &str, run: &str, step: &NewStep<'_>) -> Result<i64> {
        let last = self
            .0
            .one(
                Query::select()
                    .expr(Func::max(Expr::col("seq")))
                    .from("flow_run_steps")
                    .and_where(Expr::col("run_id").eq(run)),
                |row| row.get::<Option<i64>>(0),
            )?
            .flatten();
        let seq = last.unwrap_or(0) + 1;
        self.0.exec(
            Query::insert()
                .into_table("flow_run_steps")
                .columns([
                    "run_id",
                    "seq",
                    "workspace_id",
                    "node_id",
                    "status",
                    "branch",
                    "input",
                    "output",
                    "ms",
                    "error",
                    "started",
                ])
                .values_panic([
                    run.into(),
                    seq.into(),
                    ws.into(),
                    step.node_id.into(),
                    step.status.into(),
                    step.branch.into(),
                    step.input.to_string().into(),
                    step.output.to_string().into(),
                    step.ms.into(),
                    step.error.into(),
                    step.started.into(),
                ]),
        )?;
        Ok(seq)
    }
}

fn emit_run(w: &mut W<'_>, ws: &str, flow: &str, run: &str) {
    use steve_types::{AutomationEvent, DomainEvent};
    w.emit(DomainEvent::from(AutomationEvent::Run {
        id: run.into(),
        flow_id: flow.into(),
        ws: ws.into(),
    }));
    w.emit(DomainEvent::from(AutomationEvent::Flow {
        id: flow.into(),
        ws: ws.into(),
    }));
}

impl RunRows<'_, crate::W<'_>> {
    pub fn purge_event_data(&mut self, events: &[String], marker: &str) -> Result<u64> {
        let runs = Query::select()
            .column("id")
            .from("flow_runs")
            .and_where(Expr::col("event_id").is_in(events.iter().map(String::as_str)))
            .take();
        self.0.exec(
            Query::update()
                .table("flow_run_steps")
                .value("input", marker)
                .value("output", marker)
                .and_where(Expr::col("run_id").in_subquery(runs)),
        )?;
        self.0.exec(
            Query::update()
                .table("flow_runs")
                .value("context", marker)
                .and_where(Expr::col("event_id").is_in(events.iter().map(String::as_str))),
        )
    }
}

fn expired_schedules(dialect: super::Dialect, keep: i64, cut: i64) -> sea_query::SelectStatement {
    let context = Expr::col(("r", "context"));
    let valid = dialect.json_valid(context.clone());
    let source = Expr::case(valid.clone(), dialect.json_field(context.clone(), "source"));
    let has_startup = Expr::case(valid, dialect.json_type(context, "$.startup").is_not_null())
        .finally(Expr::Constant(false.into()));
    let row_order = dialect.insertion_order(Some("r"));
    let for_flows = || {
        Query::select()
            .from_as("flow_runs", "r")
            .join_as(
                JoinType::InnerJoin,
                "flows",
                "f",
                Expr::col(("f", "id")).eq(Expr::col(("r", "flow_id"))),
            )
            .take()
    };
    // The latest record for each source retains its startup budget.
    let anchors = for_flows()
        .expr(Func::max(row_order.clone()))
        .and_where(Expr::col(("f", "deleted")).eq(0_i64))
        .and_where(has_startup.into())
        .group_by_columns([("r", "workspace_id"), ("r", "flow_id")])
        .add_group_by([source.into()])
        .take();
    let window = sea_query::WindowStatement::partition_by(("r", "flow_id"))
        .order_by(("r", "started"), sea_query::Order::Desc)
        .order_by_expr(row_order.clone(), sea_query::Order::Desc)
        .take();
    let ranked = for_flows()
        .columns([("r", "id"), ("r", "started"), ("r", "status")])
        .expr_as(row_order, "run_order")
        .expr_window_as(Expr::cust("ROW_NUMBER()"), window, "rank")
        .and_where(Expr::col(("f", "kind")).eq("schedule"))
        .take();
    Query::select()
        .column("id")
        .from_subquery(ranked, "ranked")
        .and_where(Expr::col("rank").gt(keep))
        .and_where(Expr::col("started").lt(cut))
        .and_where(Expr::col("status").is_not_in(["running", "waiting"]))
        .and_where(Expr::col("run_order").not_in_subquery(anchors))
        .take()
}

impl RunRows<'_, crate::W<'_>> {
    pub fn prune_schedules(&mut self, keep: i64, cut: i64) -> Result<usize> {
        let expired = expired_schedules(self.0.dialect(), keep, cut);
        self.0.exec(
            Query::delete()
                .from_table("flow_run_steps")
                .and_where(Expr::col("run_id").in_subquery(expired.clone())),
        )?;
        Ok(self.0.exec(
            Query::delete()
                .from_table("flow_runs")
                .and_where(Expr::col("id").in_subquery(expired)),
        )? as usize)
    }
}
