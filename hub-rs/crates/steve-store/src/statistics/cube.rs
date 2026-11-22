use std::collections::BTreeMap;

use sea_query::{Expr, ExprTrait, Func, JoinType, Order, Query, SelectStatement};

use crate::contract::{Json, Q, Row, WorkspaceSettings};
use crate::{Read, Result, Tokens, W};

pub struct DailyRows<'a, C>(pub &'a mut C);
pub struct UsageFacts<'a, C>(pub &'a mut C);
pub struct TurnFacts<'a, C>(pub &'a mut C);
pub struct GatewayCalls<'a, C>(pub &'a mut C);
pub struct StatsSettings<'a, C>(pub &'a mut C);

/// A day, source, purpose, harness, model, Bot and project.
pub type DailyKey = (String, String, String, String, String, String, String);

#[derive(Debug, Default, Clone)]
pub struct Measures {
    pub input: i64,
    pub output: i64,
    pub cache_read: i64,
    pub cache_write: i64,
    pub reasoning: i64,
    pub hit_base: i64,
    pub cost_reported: f64,
    pub calls: i64,
    pub failed_calls: i64,
    pub turns: i64,
    pub turn_ms: i64,
    pub turn_errors: i64,
    pub retries: i64,
    pub interrupted: i64,
    pub tools: i64,
    pub tool_errors: i64,
    pub tool_kinds: BTreeMap<String, i64>,
}

#[derive(Default)]
pub struct DailyFilter<'a> {
    pub ws: &'a str,
    pub from: &'a str,
    pub to: &'a str,
    pub project: Option<&'a str>,
    pub bot: Option<&'a str>,
    pub model: Option<&'a str>,
    pub source: Option<&'a str>,
}

pub struct UsageFact {
    pub id: i64,
    pub at: i64,
    pub day: String,
    pub source: String,
    pub purpose: String,
    pub harness: String,
    pub model: String,
    pub turn: Option<String>,
    pub bot: String,
    pub project: String,
    pub tokens: Tokens,
    pub cost_reported: f64,
    pub ok: bool,
}

pub struct TurnFact {
    pub id: String,
    pub harness: String,
    pub bot: String,
    pub project: String,
    pub attempt: i64,
    pub started: i64,
    pub ended: i64,
    pub reason: Option<String>,
    pub transient: bool,
    pub tools: i64,
    pub tool_errors: i64,
    pub tool_kinds: BTreeMap<String, i64>,
    pub fallback_model: String,
    pub model: Option<String>,
}

const DIMENSIONS: [&str; 7] = [
    "day",
    "source",
    "purpose",
    "harness",
    "model",
    "bot_id",
    "project_id",
];
const MEASURES: [&str; 17] = [
    "input",
    "output",
    "cache_read",
    "cache_write",
    "reasoning",
    "hit_base",
    "cost_reported",
    "calls",
    "failed_calls",
    "turns",
    "turn_ms",
    "turn_errors",
    "retries",
    "interrupted",
    "tools",
    "tool_errors",
    "tool_kinds",
];

fn filter(q: &mut SelectStatement, field: Expr, value: Option<&str>) {
    if let Some(v) = value {
        q.and_where(field.eq(v));
    }
}

fn read_daily(r: &Row<'_>) -> Result<(DailyKey, Measures)> {
    Ok((
        (
            r.get(0)?,
            r.get(1)?,
            r.get(2)?,
            r.get(3)?,
            r.get(4)?,
            r.get(5)?,
            r.get(6)?,
        ),
        Measures {
            input: r.get(7)?,
            output: r.get(8)?,
            cache_read: r.get(9)?,
            cache_write: r.get(10)?,
            reasoning: r.get(11)?,
            hit_base: r.get(12)?,
            cost_reported: r.get(13)?,
            calls: r.get(14)?,
            failed_calls: r.get(15)?,
            turns: r.get(16)?,
            turn_ms: r.get(17)?,
            turn_errors: r.get(18)?,
            retries: r.get(19)?,
            interrupted: r.get(20)?,
            tools: r.get(21)?,
            tool_errors: r.get(22)?,
            tool_kinds: r.get::<Json<_>>(23)?.0,
        },
    ))
}

impl<C: Read> DailyRows<'_, C> {
    pub fn range(&mut self, f: DailyFilter<'_>) -> Result<Vec<(DailyKey, Measures)>> {
        let mut q = Query::select();
        q.columns(DIMENSIONS)
            .columns(MEASURES)
            .from("stat_daily")
            .and_where(Expr::col("workspace_id").eq(f.ws))
            .and_where(Expr::col("day").between(f.from, f.to));
        for (field, value) in [
            ("project_id", f.project),
            ("bot_id", f.bot),
            ("model", f.model),
            ("source", f.source),
        ] {
            filter(&mut q, Expr::col(field), value);
        }
        self.0.all(&q, read_daily)
    }

    pub fn model_totals(&mut self, ws: &str) -> Result<Vec<(String, i64)>> {
        let units = Expr::col("input")
            .add(Expr::col("output"))
            .add(Expr::col("cache_read"))
            .add(Expr::col("cache_write"));
        self.0.all(
            Query::select()
                .column("model")
                .expr(Func::sum(units))
                .from("stat_daily")
                .and_where(Expr::col("workspace_id").eq(ws))
                .group_by_col("model"),
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
    }
}

impl DailyRows<'_, W<'_>> {
    pub fn replace(
        &mut self,
        ws: &str,
        cells: impl IntoIterator<Item = (DailyKey, Measures)>,
    ) -> Result<usize> {
        self.0.exec(
            Query::delete()
                .from_table("stat_daily")
                .and_where(Expr::col("workspace_id").eq(ws)),
        )?;
        let mut n = 0;
        for ((day, source, purpose, harness, model, bot, project), m) in cells {
            self.0.exec(
                Query::insert()
                    .into_table("stat_daily")
                    .columns(
                        std::iter::once("workspace_id")
                            .chain(DIMENSIONS)
                            .chain(MEASURES),
                    )
                    .values_panic([
                        ws.into(),
                        day.into(),
                        source.into(),
                        purpose.into(),
                        harness.into(),
                        model.into(),
                        bot.into(),
                        project.into(),
                        m.input.into(),
                        m.output.into(),
                        m.cache_read.into(),
                        m.cache_write.into(),
                        m.reasoning.into(),
                        m.hit_base.into(),
                        m.cost_reported.into(),
                        m.calls.into(),
                        m.failed_calls.into(),
                        m.turns.into(),
                        m.turn_ms.into(),
                        m.turn_errors.into(),
                        m.retries.into(),
                        m.interrupted.into(),
                        m.tools.into(),
                        m.tool_errors.into(),
                        serde_json::to_string(&m.tool_kinds)?.into(),
                    ]),
            )?;
            n += 1;
        }
        Ok(n)
    }
}

fn usage_row(r: &Row<'_>) -> Result<UsageFact> {
    Ok(UsageFact {
        id: r.get(0)?,
        at: r.get(1)?,
        day: r.get(2)?,
        source: r.get(3)?,
        purpose: r.get(4)?,
        harness: r.get(5)?,
        model: r.get(6)?,
        turn: r.get(7)?,
        bot: r.get::<Option<String>>(8)?.unwrap_or_default(),
        project: r.get::<Option<String>>(9)?.unwrap_or_default(),
        tokens: Tokens {
            input: r.get(10)?,
            output: r.get(11)?,
            cache_read: r.get(12)?,
            cache_write: r.get(13)?,
            reasoning: r.get(14)?,
        },
        cost_reported: r.get::<Option<f64>>(15)?.unwrap_or_default(),
        ok: r.get(16)?,
    })
}

impl<C: Read> UsageFacts<'_, C> {
    /// Visits usage in recording order, including every model needed for oldest-turn attribution.
    pub fn scan(
        &mut self,
        ws: &str,
        days: Option<(&str, &str)>,
        project: Option<&str>,
        bot: Option<&str>,
        mut visit: impl FnMut(UsageFact) -> Result<()>,
    ) -> Result<()> {
        let mut q = Query::select();
        q.columns([
            "id",
            "ts",
            "day",
            "source",
            "purpose",
            "harness",
            "model",
            "turn_id",
            "bot_id",
            "project_id",
            "input",
            "output",
            "cache_read",
            "cache_write",
            "reasoning",
            "cost_reported",
            "ok",
        ])
        .from("usage_records")
        .and_where(Expr::col("workspace_id").eq(ws))
        .order_by("id", Order::Asc);
        if let Some((from, to)) = days {
            q.and_where(Expr::col("day").between(from, to));
        }
        filter(&mut q, Expr::col("project_id"), project);
        filter(&mut q, Expr::col("bot_id"), bot);
        self.0.scan(&q, |r| visit(usage_row(r)?))
    }
}

impl UsageFacts<'_, W<'_>> {
    pub fn recut(&mut self, ws: &str, days: Vec<(i64, String)>) -> Result<()> {
        for (id, day) in days {
            self.0.exec(
                Query::update()
                    .table("usage_records")
                    .value("day", day)
                    .and_where(Expr::col("workspace_id").eq(ws))
                    .and_where(Expr::col("id").eq(id)),
            )?;
        }
        Ok(())
    }
}

impl<C: Read> GatewayCalls<'_, C> {
    pub fn starts(&mut self, ws: &str) -> Result<Vec<(String, i64)>> {
        self.0.all(
            Query::select()
                .columns(["call_id", "started"])
                .from("gateway_calls")
                .and_where(Expr::col("workspace_id").eq(ws)),
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
    }
}

impl GatewayCalls<'_, W<'_>> {
    pub fn recut(&mut self, ws: &str, id: &str, day: &str) -> Result<()> {
        self.0.exec(
            Query::update()
                .table("gateway_calls")
                .value("day", day)
                .and_where(Expr::col("workspace_id").eq(ws))
                .and_where(Expr::col("call_id").eq(id)),
        )?;
        Ok(())
    }
}

impl<C: Read> TurnFacts<'_, C> {
    pub fn scan_ended(
        &mut self,
        ws: &str,
        window: Option<(i64, i64)>,
        project: Option<&str>,
        bot: Option<&str>,
        mut visit: impl FnMut(TurnFact) -> Result<()>,
    ) -> Result<()> {
        let mut q = Query::select();
        q.columns(
            [
                "id",
                "harness",
                "bot_id",
                "project_id",
                "attempt",
                "started",
                "ended",
                "stop_reason",
                "transient",
                "tools",
                "tool_errors",
                "tool_kinds",
            ]
            .map(|column| ("t", column)),
        )
        .columns([("s", "model"), ("b", "model"), ("t", "model")])
        .from_as("agent_turns", "t")
        .join_as(
            JoinType::LeftJoin,
            "sessions",
            "s",
            Expr::col(("s", "id")).equals(("t", "session_id")),
        )
        .join_as(
            JoinType::LeftJoin,
            "bots",
            "b",
            Expr::col(("b", "id")).equals(("t", "bot_id")),
        )
        .and_where(Expr::col(("t", "workspace_id")).eq(ws))
        .and_where(Expr::col(("t", "ended")).is_not_null());
        if let Some((lo, hi)) = window {
            q.and_where(Expr::col(("t", "started")).lt(hi))
                .and_where(Expr::col(("t", "ended")).gte(lo))
                .and_where(Expr::col(("t", "ended")).lt(hi));
        }
        filter(&mut q, Expr::col(("t", "project_id")), project);
        filter(&mut q, Expr::col(("t", "bot_id")), bot);
        self.0.scan(&q, |r| {
            let fallback_model = r
                .get::<Option<String>>(12)?
                .filter(|s| !s.is_empty())
                .or(r.get::<Option<String>>(13)?.filter(|s| !s.is_empty()))
                .unwrap_or_default();
            visit(TurnFact {
                id: r.get(0)?,
                harness: r.get(1)?,
                bot: r.get::<Option<String>>(2)?.unwrap_or_default(),
                project: r.get::<Option<String>>(3)?.unwrap_or_default(),
                attempt: r.get(4)?,
                started: r.get(5)?,
                ended: r.get(6)?,
                reason: r.get(7)?,
                transient: r.get(8)?,
                tools: r.get(9)?,
                tool_errors: r.get(10)?,
                tool_kinds: r.get::<Json<_>>(11)?.0,
                fallback_model,
                model: r.get(14)?,
            })
        })
    }
}

impl<C: Read> StatsSettings<'_, C> {
    pub fn timezone(&mut self, ws: &str) -> Result<Option<String>> {
        Ok(WorkspaceSettings(self.0)
            .get(ws, "stats.tz")?
            .and_then(|value| serde_json::from_str(&value).ok()))
    }

    pub fn prices(&mut self, ws: &str) -> Result<Option<serde_json::Value>> {
        Ok(WorkspaceSettings(self.0)
            .get(ws, "stats.prices")?
            .and_then(|value| serde_json::from_str(&value).ok()))
    }
}

#[derive(Clone, Copy)]
pub enum Setting {
    Prices,
    Timezone,
}

impl StatsSettings<'_, W<'_>> {
    pub fn save(
        &mut self,
        ws: &str,
        actor: &str,
        setting: Setting,
        value: Option<&serde_json::Value>,
        at: i64,
    ) -> Result<()> {
        let (key, kind) = match setting {
            Setting::Prices => ("stats.prices", "stats_prices"),
            Setting::Timezone => ("stats.tz", "stats_tz"),
        };
        let w = &mut *self.0;
        match value {
            Some(value) => WorkspaceSettings(w).set(ws, key, &value.to_string(), at)?,
            None => WorkspaceSettings(w).remove(ws, [key])?,
        }
        w.audit(ws, actor, key, ws, &serde_json::json!({"value": value}))?;
        let last: Option<i64> = w
            .one(
                Query::select()
                    .expr(Expr::col("version").max())
                    .from("revisions")
                    .and_where(Expr::col("workspace_id").eq(ws))
                    .and_where(Expr::col("object_type").eq(kind))
                    .and_where(Expr::col("object_id").eq(ws)),
                |row| row.get(0),
            )?
            .flatten();
        w.exec(
            Query::insert()
                .into_table("revisions")
                .columns([
                    "workspace_id",
                    "object_type",
                    "object_id",
                    "version",
                    "patch",
                    "author",
                    "created",
                ])
                .values_panic([
                    ws.into(),
                    kind.into(),
                    ws.into(),
                    (last.unwrap_or_default() + 1).into(),
                    serde_json::json!({key: value}).to_string().into(),
                    actor.into(),
                    at.into(),
                ]),
        )?;
        w.emit(steve_types::DomainEvent::from(
            steve_types::ConfigEvent::Settings {
                ws: ws.into(),
                id: ws.into(),
            },
        ));
        w.emit(crate::stats_changed(ws, None));
        Ok(())
    }
}
