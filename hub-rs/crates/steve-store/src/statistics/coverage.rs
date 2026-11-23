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
    Uncertain,
}

pub struct Meter {
    pub key: DailyKey,
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
    pub pending: bool,
}

fn u(column: &'static str) -> Expr {
    Expr::col(("u", column))
}

fn peer(column: &'static str) -> Expr {
    Expr::col(("p", column))
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

fn subquery(q: SelectStatement) -> Expr {
    Expr::SubQuery(None, Box::new(q.into()))
}

fn without_empty(e: Expr) -> Expr {
    Func::cust("nullif").arg(e).arg(text("")).into()
}

fn has_tokens(column: fn(&'static str) -> Expr) -> Expr {
    column("input")
        .is_not_null()
        .or(column("output").is_not_null())
        .or(column("cache_read").is_not_null())
        .or(column("cache_write").is_not_null())
}

/// Stable turn/call IDs identify paired token and cost rows. Equal timestamps alone do not.
fn token_role(dialect: Dialect) -> Expr {
    let detail = |key| dialect.json_field(u("detail"), key);
    let peer_detail = |key| dialect.json_field(peer("detail"), key);
    let call = without_empty(detail("call_id"));
    let peer_call = without_empty(peer_detail("call_id"));
    let same_turn = u("turn_id")
        .is_not_null()
        .and(peer("turn_id").eq(u("turn_id")));
    let same_call = call
        .clone()
        .is_not_null()
        .and(peer_call.clone().eq(detail("call_id")));
    let owned = same_turn.or(same_call);
    let uncertain = u("turn_id")
        .is_null()
        .and(peer("turn_id").is_null())
        .and(call.is_null())
        .and(peer_call.is_null())
        .and(peer("ts").eq(u("ts")));

    let mut candidates = Query::select();
    let certainty = Expr::case(owned.clone(), number(0)).finally(number(-1));
    candidates
        .expr(Func::max(certainty))
        .from_as("usage_records", "p")
        .and_where(peer("id").ne(u("id")))
        .and_where(peer_detail("scope").eq(text("all")))
        .and_where(has_tokens(peer))
        .and_where(owned.or(uncertain));
    for key in [
        "workspace_id",
        "day",
        "source",
        "purpose",
        "harness",
        "model",
    ] {
        candidates.and_where(peer(key).eq(u(key)));
    }
    for key in ["item_id", "session_id", "bot_id", "project_id"] {
        candidates.and_where(dialect.null_safe_eq(peer(key), u(key)));
    }
    let cost_only = has_tokens(u)
        .not()
        .and(u("cost_reported").is_not_null())
        .and(detail("scope").eq(text("main")));
    let role = Expr::case(
        cost_only,
        Func::coalesce([subquery(candidates.take()), number(1)]),
    )
    .finally(number(1));
    Func::coalesce([detail("accounting_tokens"), role.into()]).into()
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
                ),
                tokens: Tokens {
                    input: r.get(7)?,
                    output: r.get(8)?,
                    cache_read: r.get(9)?,
                    cache_write: r.get(10)?,
                    reasoning: None,
                },
                cost: r.get(11)?,
                pending: r.get(12)?,
                at: r.get(13)?,
                attribution: match r.get::<i64>(14)? {
                    0 => Attribution::CostOnly,
                    n if n < 0 => Attribution::Uncertain,
                    _ => Attribution::Meter,
                },
                unspent: r
                    .get::<Option<Json<Vec<Unspent>>>>(15)?
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
            .columns([("t", "bot_id"), ("t", "project_id")])
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
                pending: r.get(6)?,
            })
        })
    }
}
