use sea_query::{
    CommonTableExpression, Expr, ExprTrait, Func, JoinType, Order, Query, SelectStatement,
    UnionType, WithClause,
};
use serde::Serialize;

use super::items::decision_row;
use super::reports::{dimensions, join_items, range};
use super::{
    DecisionReads, DecisionRow, Entity, ItemHistory, ItemStats, MarkReads, ReportScope,
    SessionReads, TurnReads, UsageReads,
};
use crate::{Read, Result, Tokens};

pub type WorkAt = (String, i64, String, String);
pub type WaitingCard = (DecisionRow, Option<String>, String, String);
pub type TokenGroup = (String, String, Tokens, i64, f64);

pub struct ItemView {
    pub id: String,
    pub no: i64,
    pub title: String,
    pub status: String,
    pub bot: Option<String>,
    pub project: Option<String>,
    pub created: i64,
    pub done: Option<i64>,
    pub lead: Option<i64>,
    pub cycle: Option<i64>,
    pub wait: Option<i64>,
    pub agent: Option<i64>,
    pub turns: i64,
    pub reworks: i64,
    pub first_pass: Option<bool>,
}

pub struct TreeItem {
    pub id: String,
    pub parent: Option<String>,
    pub depth: i64,
    pub agent: Option<i64>,
    pub first_turn: Option<i64>,
    pub done: Option<i64>,
    pub dropped: Option<i64>,
}

pub struct ItemHead {
    pub ws: String,
    pub no: i64,
    pub title: String,
    pub status: String,
}

pub struct ItemTurn {
    pub id: String,
    pub session: String,
    pub started: i64,
    pub ended: Option<i64>,
    pub stop_reason: Option<String>,
    pub attempt: i64,
    pub transient: bool,
    pub tools: i64,
    pub tool_errors: i64,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ItemMark {
    pub ts: i64,
    pub kind: String,
    pub from_v: Option<String>,
    pub to_v: Option<String>,
    pub actor: String,
    pub r#ref: Option<String>,
}

pub struct BotReads<'a, C>(pub &'a mut C);

impl<C: Read> BotReads<'_, C> {
    pub fn names(&mut self) -> Result<Vec<(String, String)>> {
        self.0
            .all(Query::select().columns(["id", "name"]).from("bots"), |r| {
                Ok((r.get(0)?, r.get(1)?))
            })
    }
}

pub struct NodeReads<'a, C>(pub &'a mut C);

impl<C: Read> NodeReads<'_, C> {
    pub fn names(&mut self) -> Result<Vec<(String, String)>> {
        self.0.all(
            Query::select().columns(["node_id", "name"]).from("nodes"),
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
    }
}

impl<C: Read> ItemHistory<'_, C> {
    pub fn project_names(&mut self, ws: &str) -> Result<Vec<(String, String)>> {
        self.0.all(
            Query::select()
                .columns(["id", "name"])
                .from("projects")
                .and_where(Expr::col("workspace_id").eq(ws)),
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
    }

    pub fn head(&mut self, id: &str) -> Result<Option<ItemHead>> {
        self.0.one(
            Query::select()
                .columns(["workspace_id", "no", "title"])
                .expr(Expr::cust("COALESCE(outcome, phase)"))
                .from("items")
                .and_where(Expr::col("id").eq(id)),
            |r| {
                Ok(ItemHead {
                    ws: r.get(0)?,
                    no: r.get(1)?,
                    title: r.get(2)?,
                    status: r.get(3)?,
                })
            },
        )
    }

    pub fn created_at(&mut self, scope: ReportScope<'_>) -> Result<Vec<WorkAt>> {
        let mut q = Query::select();
        q.columns(["id", "created", "bot_id", "project_id"].map(|c| ("i", c)))
            .from_as("items", "i")
            .and_where(Expr::col(("i", "workspace_id")).eq(scope.ws))
            .and_where(Expr::col(("i", "kind")).eq("task"))
            .and_where(range(Expr::col(("i", "created")), scope));
        dimensions(&mut q, "i", scope);
        self.0.all(&q, work_at)
    }
}

impl<C: Read> MarkReads<'_, C> {
    pub fn completed_at(&mut self, scope: ReportScope<'_>) -> Result<Vec<WorkAt>> {
        let mut q = Query::select();
        q.distinct()
            .columns([
                ("m", "item_id"),
                ("m", "ts"),
                ("i", "bot_id"),
                ("i", "project_id"),
            ])
            .from_as("item_marks", "m")
            .and_where(Expr::col(("m", "workspace_id")).eq(scope.ws))
            .and_where(Expr::col(("m", "kind")).eq("status"))
            .and_where(Expr::col(("m", "to_v")).eq("completed"))
            .and_where(range(Expr::col(("m", "ts")), scope))
            .order_by(("m", "ts"), Order::Asc);
        join_items(&mut q, "m", scope);
        self.0.all(&q, work_at)
    }

    pub fn for_item(&mut self, id: &str) -> Result<Vec<ItemMark>> {
        self.0.all(
            Query::select()
                .columns(["ts", "kind", "from_v", "to_v", "actor", "ref"])
                .from("item_marks")
                .and_where(Expr::col("item_id").eq(id))
                .order_by("ts", Order::Asc)
                .order_by("id", Order::Asc),
            |r| {
                Ok(ItemMark {
                    ts: r.get(0)?,
                    kind: r.get(1)?,
                    from_v: r.get(2)?,
                    to_v: r.get(3)?,
                    actor: r.get(4)?,
                    r#ref: r.get(5)?,
                })
            },
        )
    }
}

fn work_at(r: &crate::contract::Row<'_>) -> Result<WorkAt> {
    Ok((
        r.get(0)?,
        r.get(1)?,
        r.get::<Option<String>>(2)?.unwrap_or_default(),
        r.get::<Option<String>>(3)?.unwrap_or_default(),
    ))
}

impl<C: Read> TurnReads<'_, C> {
    pub fn active_at(&mut self, scope: ReportScope<'_>) -> Result<Vec<WorkAt>> {
        let mut q = Query::select();
        q.columns(["item_id", "started", "bot_id", "project_id", "session_id"].map(|c| ("t", c)))
            .from_as("agent_turns", "t")
            .and_where(Expr::col(("t", "workspace_id")).eq(scope.ws))
            .and_where(range(Expr::col(("t", "started")), scope));
        dimensions(&mut q, "t", scope);
        self.0.all(&q, |r| {
            let item: Option<String> = r.get(0)?;
            let id = match item {
                Some(id) => id,
                None => format!("session:{}", r.get::<String>(4)?),
            };
            Ok((
                id,
                r.get(1)?,
                r.get::<Option<String>>(2)?.unwrap_or_default(),
                r.get::<Option<String>>(3)?.unwrap_or_default(),
            ))
        })
    }

    pub fn details(&mut self, id: &str, limit: u64) -> Result<Vec<ItemTurn>> {
        self.0.all(
            Query::select()
                .columns([
                    "id",
                    "session_id",
                    "started",
                    "ended",
                    "stop_reason",
                    "attempt",
                    "transient",
                    "tools",
                    "tool_errors",
                ])
                .from("agent_turns")
                .and_where(Expr::col("item_id").eq(id))
                .order_by("started", Order::Asc)
                .limit(limit),
            |r| {
                Ok(ItemTurn {
                    id: r.get(0)?,
                    session: r.get(1)?,
                    started: r.get(2)?,
                    ended: r.get(3)?,
                    stop_reason: r.get(4)?,
                    attempt: r.get(5)?,
                    transient: r.get(6)?,
                    tools: r.get(7)?,
                    tool_errors: r.get(8)?,
                })
            },
        )
    }

    pub fn session_count(&mut self, id: &str) -> Result<i64> {
        Ok(self
            .0
            .one(
                Query::select()
                    .expr(Func::count(Expr::col("id")))
                    .from("agent_turns")
                    .and_where(Expr::col("session_id").eq(id))
                    .and_where(Expr::col("attempt").eq(0_i64)),
                |r| r.get(0),
            )?
            .unwrap_or_default())
    }
}

fn by_decision_time(scope: ReportScope<'_>) -> SelectStatement {
    let mut q = Query::select();
    q.from_as("decisions", "d")
        .and_where(Expr::col(("d", "workspace_id")).eq(scope.ws))
        .and_where(range(Expr::col(("d", "created")), scope));
    if scope.has_item_filters() {
        join_items(&mut q, "d", scope);
    }
    q
}

impl<C: Read> DecisionReads<'_, C> {
    pub fn creation_times(&mut self, scope: ReportScope<'_>) -> Result<Vec<i64>> {
        self.0
            .all(by_decision_time(scope).column(("d", "created")), |r| {
                r.get(0)
            })
    }

    pub fn waiting(&mut self, scope: ReportScope<'_>) -> Result<Vec<WaitingCard>> {
        let mut q = Query::select();
        q.columns(
            [
                "id", "kind", "status", "created", "resolved", "result", "item_id",
            ]
            .map(|c| ("d", c)),
        )
        .columns([("i", "bot_id"), ("i", "project_id")])
        .from_as("decisions", "d")
        .and_where(Expr::col(("d", "workspace_id")).eq(scope.ws))
        .and_where(Expr::col(("d", "created")).lt(scope.hi))
        .and_where(
            Expr::col(("d", "resolved"))
                .is_null()
                .or(Expr::col(("d", "resolved")).gt(scope.lo)),
        );
        if scope.has_item_filters() {
            join_items(&mut q, "d", scope);
        } else {
            q.join_as(
                JoinType::LeftJoin,
                "items",
                "i",
                Expr::col(("i", "id")).equals(("d", "item_id")),
            );
        }
        self.0.all(&q, |r| {
            Ok((
                decision_row(r)?,
                r.get(6)?,
                r.get::<Option<String>>(7)?.unwrap_or_default(),
                r.get::<Option<String>>(8)?.unwrap_or_default(),
            ))
        })
    }
}

fn has_activity(table: &'static str, at: &'static str, scope: ReportScope<'_>) -> SelectStatement {
    Query::select()
        .column("item_id")
        .from(table)
        .and_where(Expr::col("item_id").equals(("i", "id")))
        .and_where(range(Expr::col(at), scope))
        .take()
}

impl<C: Read> ItemStats<'_, C> {
    pub fn completed_leads(&mut self, scope: ReportScope<'_>) -> Result<Vec<(i64, i64)>> {
        let mut q = Query::select();
        q.columns([("s", "done_at"), ("s", "lead_ms")])
            .from_as("item_stats", "s")
            .and_where(Expr::col(("s", "workspace_id")).eq(scope.ws))
            .and_where(range(Expr::col(("s", "done_at")), scope))
            .and_where(Expr::col(("s", "lead_ms")).is_not_null());
        join_items(&mut q, "s", scope);
        self.0.all(&q, |r| Ok((r.get(0)?, r.get(1)?)))
    }

    pub fn item_views(
        &mut self,
        scope: ReportScope<'_>,
        only: Option<&str>,
    ) -> Result<Vec<ItemView>> {
        let created = range(Expr::col(("i", "created")), scope);
        let done = range(Expr::col(("s", "done_at")), scope);
        let predicate = match only {
            Some("created") => created.clone(),
            Some("done") => Expr::exists(
                has_activity("item_marks", "ts", scope)
                    .and_where(Expr::col("kind").eq("status"))
                    .and_where(Expr::col("to_v").eq("completed"))
                    .take(),
            ),
            None => done
                .or(created)
                .or(Expr::exists(has_activity("agent_turns", "started", scope)))
                .or(Expr::exists(has_activity("item_marks", "ts", scope))),
            _ => return Ok(Vec::new()),
        };
        let mut q = Query::select();
        q.columns([("i", "id"), ("i", "no"), ("i", "title")])
            .expr(Expr::cust("COALESCE(i.outcome, i.phase)"))
            .columns([("i", "bot_id"), ("i", "project_id"), ("i", "created")])
            .columns(
                [
                    "done_at",
                    "lead_ms",
                    "cycle_ms",
                    "wait_ms",
                    "agent_ms",
                    "turns",
                    "reworks",
                    "first_pass",
                ]
                .map(|c| ("s", c)),
            )
            .from_as("item_stats", "s")
            .and_where(Expr::col(("s", "workspace_id")).eq(scope.ws))
            .and_where(Expr::col(("i", "kind")).eq("task"))
            .and_where(predicate);
        join_items(&mut q, "s", scope);
        self.0.all(&q, |r| {
            Ok(ItemView {
                id: r.get(0)?,
                no: r.get(1)?,
                title: r.get(2)?,
                status: r.get(3)?,
                bot: r.get(4)?,
                project: r.get(5)?,
                created: r.get(6)?,
                done: r.get(7)?,
                lead: r.get(8)?,
                cycle: r.get(9)?,
                wait: r.get(10)?,
                agent: r.get(11)?,
                turns: r.get(12)?,
                reworks: r.get(13)?,
                first_pass: r.get(14)?,
            })
        })
    }

    pub fn tree(&mut self, ws: &str, root: &str, depth: i64, limit: u64) -> Result<Vec<TreeItem>> {
        let dialect = self.0.db().dialect();
        let parent = Expr::col(("i", "parent"));
        let item: Expr = Expr::case(
            dialect.json_valid(parent.clone()),
            dialect.json_field(parent, "item"),
        )
        .into();
        let anchor = Query::select()
            .column("id")
            .expr(Expr::Constant(Option::<String>::None.into()))
            .expr(Expr::Constant(0_i64.into()))
            .from("items")
            .and_where(Expr::col("id").eq(root))
            .take();
        let children = Query::select()
            .columns([("i", "id"), ("t", "id")])
            .expr(Expr::col(("t", "depth")).add(Expr::Constant(1_i64.into())))
            .from_as("items", "i")
            .inner_join("t", item.eq(Expr::col(("t", "id"))))
            .and_where(Expr::col(("i", "workspace_id")).eq(ws))
            .and_where(Expr::col(("t", "depth")).lte(depth))
            .take();
        let walk = CommonTableExpression::new()
            .table_name("t")
            .columns(["id", "parent_id", "depth"])
            .query(anchor.clone().union(UnionType::Distinct, children).take())
            .to_owned();
        let recursive = WithClause::new().recursive(true).cte(walk).to_owned();
        let data = Query::select()
            .columns([("t", "id"), ("t", "parent_id"), ("t", "depth")])
            .columns(["agent_ms", "first_turn", "done_at", "dropped_at"].map(|c| ("s", c)))
            .from("t")
            .join_as(
                JoinType::LeftJoin,
                "item_stats",
                "s",
                Expr::col(("s", "item_id")).equals(("t", "id")),
            )
            .order_by(("t", "depth"), Order::Asc)
            .order_by(("t", "id"), Order::Asc)
            .limit(limit)
            .take()
            .with(recursive);
        self.0.all(&data, |r| {
            Ok(TreeItem {
                id: r.get(0)?,
                parent: r.get(1)?,
                depth: r.get(2)?,
                agent: r.get(3)?,
                first_turn: r.get(4)?,
                done: r.get(5)?,
                dropped: r.get(6)?,
            })
        })
    }
}

impl<C: Read> UsageReads<'_, C> {
    pub fn groups(
        &mut self,
        scope: Entity<'_>,
        by_turn: bool,
    ) -> Result<Vec<(String, TokenGroup)>> {
        let (field, id) = match scope {
            Entity::Item(id) => ("item_id", id),
            Entity::Session(id) => ("session_id", id),
            Entity::Turn(id) => ("turn_id", id),
        };
        let mut q = Query::select();
        if by_turn {
            q.column("turn_id").group_by_col("turn_id");
        } else {
            q.expr(Expr::Constant(Option::<String>::None.into()));
        }
        q.columns(["model", "day"])
            .from("usage_records")
            .and_where(Expr::col(field).eq(id));
        for field in ["input", "output", "cache_read", "cache_write"] {
            q.expr(Func::sum(Expr::col(field)));
        }
        let hit_base = Expr::col("input")
            .add(Expr::col("cache_read"))
            .add(Func::coalesce([
                Expr::col("cache_write"),
                Expr::Constant(0_i64.into()),
            ]));
        q.expr(Func::sum(hit_base))
            .expr(Func::sum(Expr::col("cost_reported")))
            .group_by_col("model")
            .group_by_col("day");
        self.0.all(&q, |r| {
            Ok((
                r.get::<Option<String>>(0)?.unwrap_or_default(),
                (
                    r.get(1)?,
                    r.get(2)?,
                    Tokens {
                        input: r.get(3)?,
                        output: r.get(4)?,
                        cache_read: r.get(5)?,
                        cache_write: r.get(6)?,
                        reasoning: None,
                    },
                    r.get::<Option<i64>>(7)?.unwrap_or_default(),
                    r.get::<Option<f64>>(8)?.unwrap_or_default(),
                ),
            ))
        })
    }

    /// Batched per-session `(model, day)` usage sums for session list enrichment.
    pub fn for_sessions(&mut self, ws: &str, ids: &[String]) -> Result<Vec<(String, TokenGroup)>> {
        const SESSION_BATCH_MAX: usize = 200;
        let mut out = Vec::new();
        for chunk in ids.chunks(SESSION_BATCH_MAX) {
            let mut q = Query::select();
            q.columns(["session_id", "model", "day"])
                .from("usage_records")
                .and_where(Expr::col("workspace_id").eq(ws))
                .and_where(Expr::col("session_id").is_in(chunk.iter().map(String::as_str)));
            for field in ["input", "output", "cache_read", "cache_write"] {
                q.expr(Func::sum(Expr::col(field)));
            }
            let hit_base = Expr::col("input")
                .add(Expr::col("cache_read"))
                .add(Func::coalesce([
                    Expr::col("cache_write"),
                    Expr::Constant(0_i64.into()),
                ]));
            q.expr(Func::sum(hit_base))
                .expr(Func::sum(Expr::col("cost_reported")))
                .group_by_col("session_id")
                .group_by_col("model")
                .group_by_col("day");
            let rows = self.0.all(&q, |r| {
                Ok((
                    r.get::<Option<String>>(0)?.unwrap_or_default(),
                    (
                        r.get(1)?,
                        r.get(2)?,
                        Tokens {
                            input: r.get(3)?,
                            output: r.get(4)?,
                            cache_read: r.get(5)?,
                            cache_write: r.get(6)?,
                            reasoning: None,
                        },
                        r.get::<Option<i64>>(7)?.unwrap_or_default(),
                        r.get::<Option<f64>>(8)?.unwrap_or_default(),
                    ),
                ))
            })?;
            out.extend(rows);
        }
        Ok(out)
    }
}

impl<C: Read> SessionReads<'_, C> {
    pub fn workspace(&mut self, id: &str) -> Result<Option<String>> {
        self.0.one(
            Query::select()
                .column("workspace_id")
                .from("sessions")
                .and_where(Expr::col("id").eq(id)),
            |r| r.get(0),
        )
    }

    pub fn titles(&mut self, ws: &str) -> Result<Vec<(String, String)>> {
        self.0.all(
            Query::select()
                .columns(["id", "title"])
                .from("sessions")
                .and_where(Expr::col("workspace_id").eq(ws)),
            |r| Ok((r.get(0)?, r.get::<Option<String>>(1)?.unwrap_or_default())),
        )
    }
}
