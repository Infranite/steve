use sea_query::{Expr, ExprTrait, Func, JoinType, Query, SelectStatement};
use serde::Deserialize;

use crate::contract::{Dialect, Json};
use crate::{Read, Result, Tokens};

use super::{DailyFilter, DailyKey};

#[derive(Clone, Copy)]
pub enum Entity<'a> {
    Item(&'a str),
    Session(&'a str),
    Turn(&'a str),
}

pub enum CoverageScope<'a> {
    Report {
        days: DailyFilter<'a>,
        turns: Option<(i64, i64)>,
    },
    Entity(Entity<'a>),
}

pub struct CoverageRows<'a, C>(pub &'a mut C);

#[derive(Deserialize)]
pub struct Unspent {
    pub model: String,
    pub tokens: Tokens,
}

pub enum Attribution {
    Meter,
    CostOnly,
}

pub struct Meter {
    pub key: DailyKey,
    pub session: Option<String>,
    pub at: i64,
    pub tokens: Tokens,
    pub cost: Option<f64>,
    pub pending: bool,
    pub attribution: Attribution,
    pub unspent: Vec<Unspent>,
}

pub struct MissingTurn {
    pub ws: String,
    pub at: i64,
    pub harness: String,
    pub model: String,
    pub bot: String,
    pub project: String,
    pub node: String,
    pub session: String,
    pub pending: bool,
}

fn u(column: &'static str) -> Expr {
    Expr::col(("u", column))
}

fn t(column: &'static str) -> Expr {
    Expr::col(("t", column))
}

fn text(s: &'static str) -> Expr {
    Expr::Constant(s.into())
}

fn number(n: i64) -> Expr {
    Expr::Constant(n.into())
}

fn token_role(dialect: Dialect) -> Expr {
    Func::coalesce([
        dialect.json_field(u("detail"), "accounting_tokens"),
        number(1),
    ])
    .into()
}

fn entity_filter(entity: Entity<'_>, is_turn: bool) -> (&'static str, &str) {
    match entity {
        Entity::Item(id) => ("item_id", id),
        Entity::Session(id) => ("session_id", id),
        Entity::Turn(id) => (if is_turn { "id" } else { "turn_id" }, id),
    }
}

fn optional_filter(q: &mut SelectStatement, e: Expr, value: Option<&str>) {
    if let Some(value) = value {
        q.and_where(e.eq(value));
    }
}

impl<C: Read> CoverageRows<'_, C> {
    pub fn meters(&mut self, scope: &CoverageScope<'_>) -> Result<Vec<Meter>> {
        let dialect = self.0.db().dialect();
        let unspent = dialect.json_field(u("detail"), "unspent_usage");
        let reports_unspent = dialect
            .json_field(u("detail"), "unspent_accounting")
            .eq(number(1));
        let pending = t("id")
            .is_not_null()
            .and(t("ended").is_null())
            .or(dialect.json_field(u("detail"), "pending").eq(number(1)));
        let mut q = Query::select();
        q.columns(
            [
                "day",
                "source",
                "purpose",
                "harness",
                "model",
                "bot_id",
                "project_id",
                "node_id",
                "session_id",
                "input",
                "output",
                "cache_read",
                "cache_write",
                "cost_reported",
            ]
            .map(|column| ("u", column)),
        )
        .expr(Expr::case(pending, number(1)).finally(number(0)))
        .column(("u", "ts"))
        .expr(token_role(dialect))
        .expr(Expr::case(reports_unspent.clone(), unspent.clone()))
        .from_as("usage_records", "u")
        .join_as(
            JoinType::LeftJoin,
            "agent_turns",
            "t",
            t("id").eq(u("turn_id")),
        );
        match scope {
            CoverageScope::Report { days: f, .. } => {
                q.and_where(u("workspace_id").eq(f.ws))
                    .and_where(u("day").between(f.from, f.to));
                optional_filter(&mut q, u("project_id"), f.project);
                optional_filter(&mut q, u("bot_id"), f.bot);
                optional_filter(&mut q, u("source"), f.source);
                optional_filter(&mut q, u("node_id"), f.node);
                if let Some(model) = f.model {
                    let unspent_match = Query::select()
                        .column(("e", "value"))
                        .from_function(dialect.json_array_elements(unspent), "e")
                        .and_where(
                            dialect
                                .json_field(Expr::col(("e", "value")), "model")
                                .eq(model),
                        )
                        .take();
                    q.and_where(
                        u("model")
                            .eq(model)
                            .or(reports_unspent.and(Expr::exists(unspent_match))),
                    );
                }
            }
            CoverageScope::Entity(entity) => {
                let (field, id) = entity_filter(*entity, false);
                q.and_where(u(field).eq(id));
            }
        }
        self.0.all(&q, |r| {
            Ok(Meter {
                key: (
                    r.get(0)?,
                    r.get(1)?,
                    r.get(2)?,
                    r.get(3)?,
                    r.get(4)?,
                    r.get::<Option<String>>(5)?.unwrap_or_default(),
                    r.get::<Option<String>>(6)?.unwrap_or_default(),
                    r.get::<Option<String>>(7)?.unwrap_or_default(),
                ),
                session: r.get(8)?,
                tokens: Tokens {
                    input: r.get(9)?,
                    output: r.get(10)?,
                    cache_read: r.get(11)?,
                    cache_write: r.get(12)?,
                    reasoning: None,
                },
                cost: r.get(13)?,
                pending: r.get(14)?,
                at: r.get(15)?,
                attribution: match r.get::<i64>(16)? {
                    0 => Attribution::CostOnly,
                    _ => Attribution::Meter,
                },
                unspent: r
                    .get::<Option<Json<Vec<Unspent>>>>(17)?
                    .map(|j| j.0)
                    .unwrap_or_default(),
            })
        })
    }

    pub fn missing_turns(&mut self, scope: &CoverageScope<'_>) -> Result<Vec<MissingTurn>> {
        let model: Expr = Func::coalesce([
            t("model"),
            Expr::col(("s", "model")),
            Expr::col(("b", "model")),
            text(""),
        ])
        .into();
        let owned = Query::select()
            .column(("u", "id"))
            .from_as("usage_records", "u")
            .and_where(u("turn_id").eq(t("id")))
            .and_where(token_role(self.0.db().dialect()).ne(number(0)))
            .take();
        let mut q = Query::select();
        q.columns([("t", "workspace_id"), ("t", "started"), ("t", "harness")])
            .expr(model.clone())
            .columns([
                ("t", "bot_id"),
                ("t", "project_id"),
                ("t", "node_id"),
                ("t", "session_id"),
            ])
            .expr(t("ended").is_null())
            .from_as("agent_turns", "t")
            .join_as(
                JoinType::LeftJoin,
                "sessions",
                "s",
                Expr::col(("s", "id")).eq(t("session_id")),
            )
            .join_as(
                JoinType::LeftJoin,
                "bots",
                "b",
                Expr::col(("b", "id")).eq(t("bot_id")),
            )
            .and_where(Expr::exists(owned).not());
        match scope {
            CoverageScope::Report {
                days: f,
                turns: Some((lo, hi)),
            } if f.source.is_none_or(|s| s == "agent") => {
                q.and_where(t("workspace_id").eq(f.ws))
                    .and_where(t("started").gte(*lo))
                    .and_where(t("started").lt(*hi));
                optional_filter(&mut q, t("project_id"), f.project);
                optional_filter(&mut q, t("bot_id"), f.bot);
                optional_filter(&mut q, t("node_id"), f.node);
                optional_filter(&mut q, model, f.model);
            }
            CoverageScope::Report { .. } => return Ok(Vec::new()),
            CoverageScope::Entity(entity) => {
                let (field, id) = entity_filter(*entity, true);
                q.and_where(t(field).eq(id));
            }
        }
        self.0.all(&q, |r| {
            Ok(MissingTurn {
                ws: r.get(0)?,
                at: r.get(1)?,
                harness: r.get(2)?,
                model: r.get(3)?,
                bot: r.get::<Option<String>>(4)?.unwrap_or_default(),
                project: r.get::<Option<String>>(5)?.unwrap_or_default(),
                node: r.get::<Option<String>>(6)?.unwrap_or_default(),
                session: r.get(7)?,
                pending: r.get(8)?,
            })
        })
    }
}
