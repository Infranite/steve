//! Session lifecycle, idle release, and maintenance counters.

use sea_query::{Expr, ExprTrait, Func, JoinType, Order, Query, SelectStatement, UnionType};
use steve_node_api::ResumeHandle;

use crate::{Read, Result, StoreError, W};

pub fn idle(
    r: &mut impl Read,
    node: &str,
    before: i64,
    session: Option<&str>,
) -> Result<Vec<String>> {
    let queued = related("queued", "q");
    let cards = related("decisions", "d")
        .and_where(Expr::col(("d", "subject_type")).eq("tool_call"))
        .and_where(Expr::col(("d", "status")).eq("open"))
        .take();
    let turns = related("runtime_turns", "t")
        .and_where(
            Expr::col(("t", "state"))
                .ne("finished")
                .or(Expr::col(("t", "updated")).gt(before)),
        )
        .take();
    let mut q = Query::select();
    q.column(("s", "session_id"))
        .from_as("runtime_sessions", "s")
        .and_where(Expr::col(("s", "node_id")).eq(node))
        .and_where(Expr::col(("s", "phase")).eq("ready"))
        .and_where(Expr::col(("s", "active_attempt")).is_null())
        .and_where(Expr::col(("s", "updated")).lte(before))
        .and_where(Expr::exists(turns).not())
        .and_where(Expr::exists(queued).not())
        .and_where(Expr::exists(cards).not())
        .order_by(("s", "updated"), Order::Asc)
        .limit(64);
    if let Some(session) = session {
        q.and_where(Expr::col(("s", "session_id")).eq(session));
    }
    r.all(&q, |row| row.get(0))
}

fn related(table: &'static str, alias: &'static str) -> SelectStatement {
    Query::select()
        .column((alias, "session_id"))
        .from_as(table, alias)
        .and_where(Expr::col((alias, "session_id")).eq(Expr::col(("s", "session_id"))))
        .take()
}

pub fn busy(r: &mut impl Read, session: &str) -> Result<bool> {
    let occupied = r
        .one(
            Query::select()
                .column("session_id")
                .from("runtime_sessions")
                .and_where(Expr::col("session_id").eq(session))
                .and_where(Expr::col("active_attempt").is_not_null().or(
                    Expr::col("phase").is_in(["opening", "releasing", "active", "configuring"]),
                )),
            |_| Ok(()),
        )?
        .is_some();
    Ok(occupied || super::unfinished(r, session)?)
}

pub fn active_attempt(r: &mut impl Read, session: &str) -> Result<Option<String>> {
    r.one(
        Query::select()
            .column(("a", "attempt_id"))
            .from_as("runtime_sessions", "s")
            .join_as(
                JoinType::InnerJoin,
                "runtime_attempts",
                "a",
                Expr::col(("a", "attempt_id")).eq(Expr::col(("s", "active_attempt"))),
            )
            .and_where(Expr::col(("s", "session_id")).eq(session))
            .and_where(Expr::col(("a", "state")).ne("finished")),
        |row| row.get(0),
    )
}

pub fn resume(r: &mut impl Read, session: &str) -> Result<Option<ResumeHandle>> {
    Ok(r.one(
        Query::select()
            .column("node_resume")
            .from("runtime_sessions")
            .and_where(Expr::col("session_id").eq(session)),
        |row| row.get::<Option<String>>(0),
    )?
    .flatten()
    .and_then(|handle| ResumeHandle::try_from(handle).ok()))
}

pub fn reset_views(w: &mut W<'_>, session: &str) -> Result<()> {
    w.exec(
        Query::update()
            .table("runtime_sessions")
            .value("node_views", "[]")
            .and_where(Expr::col("session_id").eq(session)),
    )?;
    Ok(())
}

pub fn decision_session(r: &mut impl Read, id: &str) -> Result<Option<String>> {
    Ok(r.one(
        Query::select()
            .column("session_id")
            .from("decisions")
            .and_where(Expr::col("id").eq(id)),
        |row| row.get::<Option<String>>(0),
    )?
    .flatten())
}

/// True only when `id` is a waiting permission card belonging to a currently running Node turn
/// (neither the attempt nor its logical turn has finished).
pub fn waiting_card(r: &mut impl Read, id: &str) -> Result<bool> {
    Ok(r.one(
        Query::select()
            .column(("p", "decision_id"))
            .from_as("runtime_permissions", "p")
            .join_as(
                JoinType::InnerJoin,
                "runtime_attempts",
                "a",
                Expr::col(("a", "attempt_id")).eq(Expr::col(("p", "attempt_id"))),
            )
            .join_as(
                JoinType::InnerJoin,
                "runtime_turns",
                "t",
                Expr::col(("t", "turn_id")).eq(Expr::col(("a", "turn_id"))),
            )
            .and_where(Expr::col(("p", "decision_id")).eq(id))
            .and_where(Expr::col(("p", "state")).eq("waiting"))
            .and_where(Expr::col(("a", "state")).ne("finished"))
            .and_where(Expr::col(("t", "state")).ne("finished")),
        |_| Ok(()),
    )?
    .is_some())
}

fn open_turns(node: Option<&str>) -> SelectStatement {
    let mut turns = Query::select();
    turns
        .from_as("runtime_turns", "t")
        .and_where(Expr::col(("t", "state")).ne("finished"));
    if let Some(node) = node {
        turns
            .join_as(
                JoinType::InnerJoin,
                "runtime_sessions",
                "s",
                Expr::col(("s", "session_id")).eq(Expr::col(("t", "session_id"))),
            )
            .and_where(Expr::col(("s", "node_id")).eq(node));
    }
    turns
}

fn configuring(node: Option<&str>) -> SelectStatement {
    let mut sessions = Query::select();
    sessions
        .from_as("runtime_sessions", "s")
        .and_where(Expr::col(("s", "phase")).eq("configuring"));
    if let Some(node) = node {
        sessions.and_where(Expr::col(("s", "node_id")).eq(node));
    }
    sessions
}

fn draining(node: &str) -> SelectStatement {
    let open = related("runtime_turns", "t")
        .and_where(Expr::col(("t", "state")).ne("finished"))
        .take();
    Query::select()
        .from_as("runtime_sessions", "s")
        .and_where(Expr::col(("s", "node_id")).eq(node))
        .and_where(Expr::col(("s", "phase")).is_in(["opening", "configuring", "releasing"]))
        .and_where(Expr::exists(open).not())
        .take()
}

fn count(r: &mut impl Read, q: &mut SelectStatement) -> Result<i64> {
    r.one(q.expr(Func::count(Expr::col(sea_query::Asterisk))), |row| {
        row.get(0)
    })?
    .ok_or_else(|| StoreError::Rejected("missing session count".into()))
}

pub fn running(r: &mut impl Read, node: Option<&str>) -> Result<i64> {
    Ok(count(r, &mut open_turns(node))? + count(r, &mut configuring(node))?)
}

pub fn working(r: &mut impl Read, node: &str) -> Result<i64> {
    Ok(count(r, &mut open_turns(Some(node)))? + count(r, &mut draining(node))?)
}

#[derive(serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkKind {
    NodeTurn,
    NodeSession,
}

pub struct Work {
    pub kind: WorkKind,
    pub session: String,
    pub item: Option<String>,
    pub attempt: Option<String>,
    pub state: String,
}

pub fn working_details(r: &mut impl Read, node: &str) -> Result<Vec<Work>> {
    let mut turns = open_turns(Some(node));
    let mut sessions = draining(node);
    let item_join = || Expr::col(("app", "id")).eq(Expr::col(("s", "session_id")));
    turns
        .columns([
            ("s", "session_id"),
            ("app", "item_id"),
            ("t", "active_attempt"),
            ("t", "state"),
        ])
        .expr(Expr::Constant(true.into()))
        .join_as(JoinType::LeftJoin, "sessions", "app", item_join());
    sessions
        .columns([
            ("s", "session_id"),
            ("app", "item_id"),
            ("s", "active_attempt"),
            ("s", "phase"),
        ])
        .expr(Expr::Constant(false.into()))
        .join_as(JoinType::LeftJoin, "sessions", "app", item_join());
    let union = turns.union(UnionType::All, sessions).take();
    r.all(
        Query::select()
            .expr(Expr::col(sea_query::Asterisk))
            .from_subquery(union, "work")
            .order_by("session_id", Order::Asc)
            .limit(100),
        |row| {
            Ok(Work {
                kind: if row.get::<bool>(4)? {
                    WorkKind::NodeTurn
                } else {
                    WorkKind::NodeSession
                },
                session: row.get(0)?,
                item: row.get(1)?,
                attempt: row.get(2)?,
                state: row.get(3)?,
            })
        },
    )
}

pub struct Health {
    pub sessions: i64,
    pub open: i64,
    pub running: i64,
    pub unknown: i64,
    pub parked: i64,
}

pub fn health(r: &mut impl Read, local: bool, remote: bool) -> Result<Health> {
    let mut parked = Query::select();
    parked
        .from_as("runtime_sessions", "s")
        .and_where(Expr::exists(related("queued", "q")));
    let node = Expr::col(("s", "node_id"));
    let parked = match (local, remote) {
        (true, true) => 0,
        (true, false) => count(r, parked.and_where(node.ne("local")))?,
        (false, true) => count(r, parked.and_where(node.eq("local")))?,
        (false, false) => count(r, &mut parked)?,
    };
    Ok(Health {
        sessions: count(r, Query::select().from("runtime_sessions"))?,
        open: count(
            r,
            Query::select()
                .from("runtime_sessions")
                .and_where(Expr::col("phase").ne("released")),
        )?,
        running: running(r, None)?,
        unknown: count(
            r,
            Query::select()
                .from("runtime_attempts")
                .and_where(Expr::col("state").eq("unknown")),
        )?,
        parked,
    })
}
