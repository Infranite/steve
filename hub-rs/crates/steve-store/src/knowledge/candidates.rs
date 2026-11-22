use crate::base::Viewer;
use crate::{Read, Result, W};
use sea_query::{Expr, ExprTrait, Func, JoinType, OnConflict, Order, Query, SelectStatement};

pub struct CandidateRows<'a, C>(pub &'a mut C);

pub type CandidateInfo = (String, String, String, Option<String>);

pub struct CandidateOrigin<'a> {
    pub kind: &'a str,
    pub id: &'a str,
    pub label: &'a str,
    pub evidence: &'a str,
    pub supersedes: Option<&'a str>,
    pub created: i64,
}

fn candidates(viewer: &Viewer, status: &str) -> SelectStatement {
    let mut q = Query::select();
    q.from_as("mems", "m")
        .join_as(
            JoinType::InnerJoin,
            "mem_candidates",
            "k",
            Expr::col(("m", "id")).equals(("k", "mem_id")),
        )
        .and_where(Expr::col(("m", "workspace_id")).eq(viewer.ws.as_str()))
        .and_where(Expr::col(("m", "status")).eq(status));
    if viewer.is_service() {
        q.and_where(Expr::col(("m", "visibility")).eq("workspace"));
    }
    q.take()
}

impl<C: Read> CandidateRows<'_, C> {
    pub fn info(&mut self, id: &str) -> Result<Option<CandidateInfo>> {
        self.0.one(
            Query::select()
                .columns(["origin", "origin_label", "evidence", "supersedes"])
                .from("mem_candidates")
                .and_where(Expr::col("mem_id").eq(id)),
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
    }

    pub fn evidence(&mut self, id: &str) -> Result<Option<(String, String, String)>> {
        self.0.one(
            Query::select()
                .columns(["origin", "origin_id", "evidence"])
                .from("mem_candidates")
                .and_where(Expr::col("mem_id").eq(id)),
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
    }

    pub fn supersedes(&mut self, id: &str) -> Result<Vec<String>> {
        self.0.all(
            Query::select()
                .column("old_id")
                .from("mem_supersedes")
                .and_where(Expr::col("mem_id").eq(id))
                .order_by("rowid", Order::Asc),
            |r| r.get(0),
        )
    }

    pub fn pending(&mut self, viewer: &Viewer) -> Result<Vec<String>> {
        self.0.all(
            candidates(viewer, "candidate")
                .column(("m", "id"))
                .and_where(Expr::col(("k", "workspace_id")).eq(viewer.ws.as_str()))
                .order_by(("k", "created"), Order::Desc)
                .order_by(("m", "created"), Order::Desc)
                .order_by(("m", "id"), Order::Asc),
            |r| r.get(0),
        )
    }

    pub fn expired(&mut self, viewer: &Viewer, limit: usize) -> Result<Vec<(String, i64)>> {
        let confirmed = Query::select()
            .column(("b", "id"))
            .from_as("audit", "b")
            .and_where(Expr::col(("b", "target")).equals(("m", "id")))
            .and_where(Expr::col(("b", "action")).eq("mm.confirm"))
            .and_where(Expr::col(("b", "id")).gt(Expr::col(("a", "id"))))
            .take();
        self.0.all(
            candidates(viewer, "stale")
                .column(("m", "id"))
                .expr_as(Func::max(Expr::col(("a", "ts"))), "expired")
                .join_as(
                    JoinType::InnerJoin,
                    "audit",
                    "a",
                    Expr::col(("a", "workspace_id"))
                        .equals(("m", "workspace_id"))
                        .and(Expr::col(("a", "target")).equals(("m", "id")))
                        .and(Expr::col(("a", "action")).eq("mm.expire")),
                )
                .and_where(Expr::exists(confirmed).not())
                .group_by_col(("m", "id"))
                .order_by("expired", Order::Desc)
                .limit(limit as u64),
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
    }

    pub fn known(&mut self, ws: &str, user: &str) -> Result<Vec<(String, String, String)>> {
        self.0.all(
            Query::select()
                .columns(["id", "text", "status"])
                .from("mems")
                .and_where(Expr::col("workspace_id").eq(ws))
                .and_where(
                    Expr::col("status").eq("confirmed").or(Expr::col("status")
                        .eq("candidate")
                        .and(Expr::col("created_by").eq(user))),
                )
                .order_by("updated", Order::Desc)
                .order_by("id", Order::Asc)
                .limit(500),
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
    }
}

impl CandidateRows<'_, W<'_>> {
    pub fn record(&mut self, ws: &str, mem: &str, origin: CandidateOrigin<'_>) -> Result<()> {
        let fields = [
            "workspace_id",
            "origin",
            "origin_id",
            "origin_label",
            "evidence",
            "supersedes",
            "created",
        ];
        let mut q = Query::insert();
        q.into_table("mem_candidates")
            .columns(["mem_id"].into_iter().chain(fields))
            .values_panic([
                mem.into(),
                ws.into(),
                origin.kind.into(),
                origin.id.into(),
                origin.label.into(),
                origin.evidence.into(),
                origin.supersedes.into(),
                origin.created.into(),
            ])
            .on_conflict(
                OnConflict::column("mem_id")
                    .update_columns(fields)
                    .to_owned(),
            );
        self.0.exec(&q)?;
        Ok(())
    }

    pub fn remove(&mut self, mem: &str) -> Result<()> {
        for table in ["mem_candidates", "mem_supersedes"] {
            self.0.exec(
                Query::delete()
                    .from_table(table)
                    .and_where(Expr::col("mem_id").eq(mem)),
            )?;
        }
        Ok(())
    }

    pub fn restore(&mut self, mem: &str, at: i64) -> Result<()> {
        self.0.exec(
            Query::update()
                .table("mem_candidates")
                .value("created", at)
                .and_where(Expr::col("mem_id").eq(mem)),
        )?;
        Ok(())
    }

    pub fn supersede(&mut self, mem: &str, old: &str) -> Result<()> {
        self.0.exec(
            Query::insert()
                .into_table("mem_supersedes")
                .columns(["mem_id", "old_id"])
                .values_panic([mem.into(), old.into()])
                .on_conflict(
                    OnConflict::columns(["mem_id", "old_id"])
                        .do_nothing()
                        .to_owned(),
                ),
        )?;
        Ok(())
    }
}
