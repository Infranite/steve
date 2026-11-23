use sea_query::{Expr, ExprTrait, Func, JoinType, Order, Query, SelectStatement};

use crate::{Read, Result};

pub struct RealtimeReads<'a, C>(pub &'a mut C);

/// A workspace and its optional project and Bot filters.
pub struct RealtimeScope<'a> {
    pub workspace: &'a str,
    pub project: Option<&'a str>,
    pub bot: Option<&'a str>,
}

#[derive(Debug)]
pub struct Sample {
    pub at: i64,
    pub units: i64,
}

pub struct Attempt {
    pub session: String,
    pub started: i64,
    pub ended: Option<i64>,
    pub baseline: bool,
    pub confirmed_end: bool,
    pub unknown_at: Option<i64>,
    pub gap_at: Option<i64>,
    pub samples: Vec<Sample>,
}

fn in_workspace(ws: &str) -> SelectStatement {
    Query::select()
        .from_as("sessions", "s")
        .join_as(
            JoinType::LeftJoin,
            "items",
            "i",
            Expr::col(("i", "id"))
                .equals(("s", "item_id"))
                .and(Expr::col(("i", "workspace_id")).equals(("s", "workspace_id"))),
        )
        .and_where(Expr::col(("s", "workspace_id")).eq(ws))
        .and_where(Expr::col(("s", "readonly")).eq(0_i64))
        .take()
}

fn filter(q: &mut SelectStatement, expected: Option<&str>, field: Expr) {
    if let Some(expected) = expected {
        q.and_where(field.eq(expected));
    }
}

fn samples(id: &str) -> SelectStatement {
    Query::select()
        .from("realtime_output_samples")
        .and_where(Expr::col("attempt_id").eq(id))
        .take()
}

fn gap(id: &str) -> SelectStatement {
    // A literal matches the partial index predicate irrespective of bind-time planning.
    samples(id)
        .expr(Func::min(Expr::col("at")))
        .and_where(Expr::col("kind").eq(Expr::Constant("gap".into())))
        .take()
}

fn window(id: &str, lo: i64, at: i64) -> SelectStatement {
    samples(id)
        .columns(["at", "units"])
        .and_where(Expr::col("at").gt(lo))
        .and_where(Expr::col("at").lte(at))
        .order_by("at", Order::Asc)
        .order_by("seq", Order::Asc)
        .take()
}

fn preceding(id: &str, lo: i64) -> SelectStatement {
    samples(id)
        .column("at")
        .and_where(Expr::col("at").lte(lo))
        .order_by("at", Order::Desc)
        .limit(1)
        .take()
}

impl<C: Read> RealtimeReads<'_, C> {
    #[cfg(feature = "testkit")]
    pub fn record_count(&mut self) -> Result<i64> {
        Ok(self
            .0
            .one(
                Query::select()
                    .expr(Func::count(Expr::col("attempt_id")))
                    .from("realtime_output_attempts"),
                |r| r.get(0),
            )?
            .unwrap_or_default())
    }

    pub fn attempts(&mut self, scope: RealtimeScope<'_>, lo: i64, at: i64) -> Result<Vec<Attempt>> {
        let observed = Expr::col(("r", "attempt_id"));
        let ended = Expr::col(("r", "ended"));
        let started: Expr =
            Func::coalesce([Expr::col(("r", "started")), Expr::col(("a", "created"))]).into();
        let unknown = Expr::col(("a", "state"))
            .eq(Expr::Constant("unknown".into()))
            .or(Expr::col(("a", "state"))
                .eq(Expr::Constant("finished".into()))
                .and(observed.clone().is_not_null())
                .and(ended.clone().is_null()));
        let mut q = in_workspace(scope.workspace);
        q.join_as(
            JoinType::InnerJoin,
            "runtime_turns",
            "t",
            Expr::col(("t", "session_id")).equals(("s", "id")),
        )
        .join_as(
            JoinType::InnerJoin,
            "runtime_attempts",
            "a",
            Expr::col(("a", "turn_id")).equals(("t", "turn_id")),
        )
        .join_as(
            JoinType::LeftJoin,
            "realtime_output_attempts",
            "r",
            observed
                .clone()
                .equals(("a", "attempt_id"))
                .and(Expr::col(("r", "workspace_id")).equals(("s", "workspace_id"))),
        )
        .columns([("r", "attempt_id"), ("t", "session_id")])
        .expr(started.clone())
        .columns([("r", "ended"), ("r", "baseline")])
        .expr(Expr::case(unknown, Expr::col(("a", "updated"))))
        .and_where(Func::coalesce([Expr::col(("r", "not_started")), Expr::value(0_i64)]).eq(0_i64))
        .and_where(started.lte(at))
        .and_where(ended.clone().is_null().or(ended.gt(lo)));
        filter(
            &mut q,
            scope.project,
            Expr::case(observed.clone().is_null(), Expr::col(("i", "project_id")))
                .finally(Expr::col(("r", "project_id")))
                .into(),
        );
        filter(
            &mut q,
            scope.bot,
            Expr::case(observed.is_null(), Expr::col(("s", "bot_id")))
                .finally(Expr::col(("r", "bot_id")))
                .into(),
        );

        let rows = self.0.all(&q, |r| {
            let id: Option<String> = r.get(0)?;
            let ended: Option<i64> = r.get(3)?;
            Ok((
                id,
                Attempt {
                    session: r.get(1)?,
                    started: r.get(2)?,
                    ended,
                    baseline: r.get::<Option<bool>>(4)?.unwrap_or(false),
                    confirmed_end: ended.is_some(),
                    unknown_at: r.get(5)?,
                    gap_at: None,
                    samples: Vec::new(),
                },
            ))
        })?;
        let mut out = Vec::with_capacity(rows.len());
        for (id, mut attempt) in rows {
            if let Some(id) = id {
                attempt.gap_at = self
                    .0
                    .one(&gap(&id), |r| r.get::<Option<i64>>(0))?
                    .flatten();
                attempt.samples = self.0.all(&window(&id, lo, at), |r| {
                    Ok(Sample {
                        at: r.get(0)?,
                        units: r.get(1)?,
                    })
                })?;
                if let Some(before) = self.0.one(&preceding(&id, lo), |r| r.get(0))? {
                    attempt.samples.push(Sample {
                        at: before,
                        units: 0,
                    });
                }
            }
            out.push(attempt);
        }
        out.extend(self.untracked(&scope, lo, at)?);
        Ok(out)
    }

    fn untracked(&mut self, scope: &RealtimeScope<'_>, lo: i64, at: i64) -> Result<Vec<Attempt>> {
        let created = Expr::col(("s", "created"));
        let status = Expr::col(("s", "status"));
        let last = self
            .0
            .db()
            .dialect()
            .greatest(created.clone(), Expr::col(("s", "updated")));
        let ended = status.clone().eq(Expr::Constant("ended".into()));
        let turns = Query::select()
            .column("turn_id")
            .from("runtime_turns")
            .and_where(Expr::col("session_id").equals(("s", "id")))
            .take();
        let mut q = in_workspace(scope.workspace);
        q.columns([("s", "id"), ("s", "created")])
            .expr(Expr::case(ended.clone(), last.clone()))
            .and_where(created.lte(at))
            .and_where(
                status
                    .is_in(["running", "starting", "busy"])
                    .or(ended.and(last.gt(lo))),
            )
            .and_where(Expr::exists(turns).not());
        filter(&mut q, scope.project, Expr::col(("i", "project_id")));
        filter(&mut q, scope.bot, Expr::col(("s", "bot_id")));
        self.0.all(&q, |r| {
            Ok(Attempt {
                session: r.get(0)?,
                started: r.get(1)?,
                ended: r.get(2)?,
                baseline: false,
                confirmed_end: false,
                unknown_at: None,
                gap_at: None,
                samples: Vec::new(),
            })
        })
    }
}

#[cfg(test)]
#[path = "realtime_query_tests.rs"]
mod query_tests;
