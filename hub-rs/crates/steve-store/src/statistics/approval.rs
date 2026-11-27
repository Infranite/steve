//! Permission observations in the same workspace, time and item filters as the other reports.
use std::collections::BTreeMap;

use sea_query::{Expr, ExprTrait, Func, JoinType, Query};
use serde::Serialize;
use serde_json::Value;
use steve_types::ApprovalMode;

use super::{ReportScope, reports::range};
use crate::{Read, Result};

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct ModeCounts {
    pub sessions: i64,
    pub turns: i64,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct ApprovalStats {
    /// Distinct sessions with counted permission turns in the range.
    pub sessions: i64,
    pub turns: i64,
    /// Session and turn counts broken down by applied permission level (`ask`, `auto`, `full_access`).
    pub by_mode: BTreeMap<String, ModeCounts>,
    pub permissions: i64,
    pub permissions_by_resolver: BTreeMap<String, BTreeMap<String, i64>>,
    pub questions: i64,
}

pub fn report(r: &mut impl Read, scope: ReportScope<'_>) -> Result<ApprovalStats> {
    let mut result = ApprovalStats::default();
    for mode in ApprovalMode::ALL {
        result
            .by_mode
            .insert(mode.as_str().to_owned(), ModeCounts::default());
    }
    let mut turns = Query::select();
    turns
        .expr(Func::count(Expr::col(("a", "turn_id"))))
        .expr(Expr::cust("COUNT(DISTINCT a.session_id)"))
        .from_as("approval_turns", "a")
        .join_as(
            JoinType::InnerJoin,
            "sessions",
            "s",
            Expr::col(("s", "id")).eq(Expr::col(("a", "session_id"))),
        )
        .and_where(Expr::col(("s", "workspace_id")).eq(scope.ws))
        .and_where(range(Expr::col(("a", "created")), scope));
    let mut turns_by_mode = Query::select();
    turns_by_mode
        .column(("a", "mode"))
        .expr(Func::count(Expr::col(("a", "turn_id"))))
        .expr(Expr::cust("COUNT(DISTINCT a.session_id)"))
        .from_as("approval_turns", "a")
        .join_as(
            JoinType::InnerJoin,
            "sessions",
            "s",
            Expr::col(("s", "id")).eq(Expr::col(("a", "session_id"))),
        )
        .and_where(Expr::col(("s", "workspace_id")).eq(scope.ws))
        .and_where(range(Expr::col(("a", "created")), scope))
        .group_by_col(("a", "mode"));
    let mut permissions = Query::select();
    permissions
        .columns([("p", "resolver"), ("p", "state"), ("d", "result")])
        .from_as("runtime_permissions", "p")
        .join_as(
            JoinType::InnerJoin,
            "decisions",
            "d",
            Expr::col(("d", "id")).eq(Expr::col(("p", "decision_id"))),
        )
        .and_where(Expr::col(("d", "workspace_id")).eq(scope.ws))
        .and_where(range(Expr::col(("p", "created")), scope));
    let mut questions = Query::select();
    questions
        .expr(Func::count(Expr::col(("d", "id"))))
        .from_as("decisions", "d")
        .and_where(Expr::col(("d", "workspace_id")).eq(scope.ws))
        .and_where(Expr::col(("d", "kind")).eq("ask"))
        .and_where(Expr::col(("d", "subject_type")).eq("question"))
        .and_where(range(Expr::col(("d", "created")), scope));
    for (q, src, bot) in [
        (&mut turns, "s", "bot_id"),
        (&mut turns_by_mode, "s", "bot_id"),
        (&mut permissions, "d", "requested_by"),
        (&mut questions, "d", "requested_by"),
    ] {
        if let Some(p) = scope.project {
            q.join_as(
                JoinType::InnerJoin,
                "items",
                "i",
                Expr::col(("i", "id")).eq(Expr::col((src, "item_id"))),
            )
            .and_where(Expr::col(("i", "project_id")).eq(p));
        }
        if let Some(b) = scope.bot {
            q.and_where(Expr::col((src, bot)).eq(b));
        }
    }
    (result.turns, result.sessions) = r
        .one(&turns, |row| Ok((row.get(0)?, row.get(1)?)))?
        .unwrap_or_default();
    r.scan(&turns_by_mode, |row| {
        let mode: String = row.get(0)?;
        let turn_count: i64 = row.get(1)?;
        let session_count: i64 = row.get(2)?;
        result.by_mode.insert(
            mode,
            ModeCounts {
                sessions: session_count,
                turns: turn_count,
            },
        );
        Ok(())
    })?;
    result.questions = r.one(&questions, |row| row.get(0))?.unwrap_or_default();
    r.scan(&permissions, |row| {
        let resolver: String = row.get(0)?;
        let state: String = row.get(1)?;
        let decision: Value = row
            .get::<Option<String>>(2)?
            .and_then(|json| serde_json::from_str(&json).ok())
            .unwrap_or_default();
        let outcome = match state.as_str() {
            "waiting" | "answering" => "pending",
            "expired" => "expired",
            "refused" => "refused",
            "applied" if decision["optionId"] == "allow_once" => "allowed",
            "applied" => "rejected",
            _ => "unknown",
        };
        result.permissions += 1;
        *result
            .permissions_by_resolver
            .entry(resolver)
            .or_default()
            .entry(outcome.into())
            .or_default() += 1;
        Ok(())
    })?;
    Ok(result)
}

/// Active sessions by applied permission level (`ask`, `auto`, `full_access`), not sessions merely waiting for it.
pub fn applied_sessions_by_mode(r: &mut impl Read) -> Result<BTreeMap<ApprovalMode, i64>> {
    let mut counts: BTreeMap<ApprovalMode, i64> =
        ApprovalMode::ALL.into_iter().map(|m| (m, 0)).collect();
    let q = Query::select()
        .column("applied_mode")
        .expr(Func::count(Expr::col("session_id")))
        .from("runtime_sessions")
        .and_where(Expr::col("applied_mode").is_in(["ask", "auto", "full_access"]))
        .and_where(Expr::col("phase").is_in(["ready", "active", "configuring"]))
        .group_by_col("applied_mode")
        .to_owned();
    r.scan(&q, |row| {
        let raw: String = row.get(0)?;
        let count: i64 = row.get(1)?;
        if let Some(mode) = ApprovalMode::parse(&raw) {
            counts.insert(mode, count);
        }
        Ok(())
    })?;
    Ok(counts)
}
