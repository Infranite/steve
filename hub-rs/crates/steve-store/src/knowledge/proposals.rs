use std::collections::HashMap;

use sea_query::{Expr, ExprTrait, Func, Order, Query, SelectStatement};
use serde::Serialize;

use crate::contract::Row;
use crate::{Read, Result, W, now, uid};

pub struct ProjectProposals<'a, C>(pub &'a mut C);

/// One project-level proposal (e.g. an `AGENTS.md` addition), kept apart from personal memories.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ProjectProposal {
    pub id: String,
    pub workspace_id: String,
    pub project_id: String,
    /// `agents_md`
    pub kind: String,
    pub text: String,
    pub reason: String,
    pub evidence: String,
    pub origin_kind: String,
    pub origin_id: Option<String>,
    pub origin_label: String,
    /// `pending` | `accepted` | `dismissed`
    pub status: String,
    pub decision_id: Option<String>,
    pub writer: String,
    pub created_by: String,
    pub created: i64,
    pub updated: i64,
}

#[derive(Debug, Clone)]
pub struct NewProjectProposal<'a> {
    pub project_id: &'a str,
    pub kind: &'a str,
    pub text: &'a str,
    pub reason: &'a str,
    pub evidence: &'a str,
    pub origin_kind: &'a str,
    pub origin_id: Option<&'a str>,
    pub origin_label: &'a str,
    pub writer: &'a str,
    pub created_by: &'a str,
}

const COLUMNS: [&str; 16] = [
    "id",
    "workspace_id",
    "project_id",
    "kind",
    "text",
    "reason",
    "evidence",
    "origin_kind",
    "origin_id",
    "origin_label",
    "status",
    "decision_id",
    "writer",
    "created_by",
    "created",
    "updated",
];

fn from_row(r: &Row<'_>) -> Result<ProjectProposal> {
    Ok(ProjectProposal {
        id: r.get(0)?,
        workspace_id: r.get(1)?,
        project_id: r.get(2)?,
        kind: r.get(3)?,
        text: r.get(4)?,
        reason: r.get(5)?,
        evidence: r.get(6)?,
        origin_kind: r.get(7)?,
        origin_id: r.get(8)?,
        origin_label: r.get(9)?,
        status: r.get(10)?,
        decision_id: r.get(11)?,
        writer: r.get(12)?,
        created_by: r.get(13)?,
        created: r.get(14)?,
        updated: r.get(15)?,
    })
}

fn select_all() -> SelectStatement {
    Query::select()
        .columns(COLUMNS)
        .from("project_proposals")
        .take()
}

impl<C: Read> ProjectProposals<'_, C> {
    pub fn get(&mut self, id: &str) -> Result<Option<ProjectProposal>> {
        self.0
            .one(select_all().and_where(Expr::col("id").eq(id)), from_row)
    }

    pub fn list(
        &mut self,
        ws: &str,
        project_id: Option<&str>,
        status: Option<&str>,
    ) -> Result<Vec<ProjectProposal>> {
        let mut q = select_all();
        q.and_where(Expr::col("workspace_id").eq(ws));
        if let Some(pid) = project_id.filter(|s| !s.is_empty()) {
            q.and_where(Expr::col("project_id").eq(pid));
        }
        if let Some(st) = status.filter(|s| !s.is_empty()) {
            q.and_where(Expr::col("status").eq(st));
        }
        q.order_by("updated", Order::Desc)
            .order_by("id", Order::Asc);
        self.0.all(&q, from_row)
    }

    pub fn known_texts(&mut self, ws: &str) -> Result<Vec<(String, String)>> {
        self.0.all(
            Query::select()
                .columns(["project_id", "text"])
                .from("project_proposals")
                .and_where(Expr::col("workspace_id").eq(ws))
                .and_where(Expr::col("status").is_in(["pending", "accepted"]))
                .order_by("updated", Order::Desc)
                .limit(500),
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
    }

    pub fn pending_counts_by_project(&mut self, ws: &str) -> Result<HashMap<String, i64>> {
        let rows = self.0.all(
            Query::select()
                .column("project_id")
                .expr(Func::count(Expr::col("id")))
                .from("project_proposals")
                .and_where(Expr::col("workspace_id").eq(ws))
                .and_where(Expr::col("status").eq("pending"))
                .group_by_col("project_id"),
            |r| Ok((r.get::<String>(0)?, r.get::<i64>(1)?)),
        )?;
        Ok(rows.into_iter().collect())
    }
}

impl ProjectProposals<'_, W<'_>> {
    pub fn create(&mut self, ws: &str, p: &NewProjectProposal<'_>) -> Result<ProjectProposal> {
        let id = uid("prp");
        let at = now();
        self.0.exec(
            Query::insert()
                .into_table("project_proposals")
                .columns(COLUMNS)
                .values_panic([
                    id.as_str().into(),
                    ws.into(),
                    p.project_id.into(),
                    p.kind.into(),
                    p.text.into(),
                    p.reason.into(),
                    p.evidence.into(),
                    p.origin_kind.into(),
                    p.origin_id.into(),
                    p.origin_label.into(),
                    "pending".into(),
                    Option::<String>::None.into(),
                    p.writer.into(),
                    p.created_by.into(),
                    at.into(),
                    at.into(),
                ]),
        )?;
        crate::repo::must(self.get(&id)?)
    }

    pub fn set_decision_id(&mut self, id: &str, decision_id: Option<&str>) -> Result<()> {
        self.0.exec(
            Query::update()
                .table("project_proposals")
                .value("decision_id", decision_id)
                .value("updated", now())
                .and_where(Expr::col("id").eq(id)),
        )?;
        Ok(())
    }

    pub fn set_status(&mut self, id: &str, status: &str) -> Result<()> {
        self.0.exec(
            Query::update()
                .table("project_proposals")
                .value("status", status)
                .value("updated", now())
                .and_where(Expr::col("id").eq(id)),
        )?;
        Ok(())
    }
}
