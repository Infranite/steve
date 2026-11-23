use sea_query::{Expr, ExprTrait, Func, JoinType, OnConflict, Order, Query, SelectStatement};

use crate::{Read, Result, W, now};

pub struct MergeProposals<'a, C>(pub &'a mut C);

pub struct NewMergeProposal<'a> {
    pub id: &'a str,
    pub workspace: &'a str,
    pub from: &'a str,
    pub into: &'a str,
    pub account: &'a str,
    pub reason: &'a str,
    pub confidence: f64,
}

pub struct Proposal {
    pub id: String,
    pub ws: String,
    pub from: String,
    pub into: String,
    pub account: Option<String>,
    pub reason: String,
    pub decision: Option<String>,
    pub status: String,
}

fn proposals() -> SelectStatement {
    Query::select()
        .from("merge_proposals")
        .columns([
            "id",
            "workspace_id",
            "from_id",
            "into_id",
            "account_id",
            "reason",
            "decision_id",
            "status",
        ])
        .take()
}

fn proposal(row: &super::Row<'_>) -> Result<Proposal> {
    Ok(Proposal {
        id: row.get(0)?,
        ws: row.get(1)?,
        from: row.get(2)?,
        into: row.get(3)?,
        account: row.get(4)?,
        reason: row.get(5)?,
        decision: row.get(6)?,
        status: row.get(7)?,
    })
}

impl<C: Read> MergeProposals<'_, C> {
    pub fn get(&mut self, id: &str) -> Result<Option<Proposal>> {
        self.0
            .one(proposals().and_where(Expr::col("id").eq(id)), proposal)
    }

    pub fn pending(&mut self, ws: &str) -> Result<Vec<Proposal>> {
        let order = self.0.db().dialect().insertion_order(None);
        self.0.all(
            proposals()
                .and_where(Expr::col("workspace_id").eq(ws))
                .and_where(Expr::col("status").eq("open"))
                .order_by("created", Order::Desc)
                .order_by_expr(order, Order::Desc),
            proposal,
        )
    }

    pub fn involving(&mut self, ws: &str, id: &str) -> Result<Vec<Proposal>> {
        self.0.all(
            proposals()
                .and_where(Expr::col("workspace_id").eq(ws))
                .and_where(Expr::col("status").eq("open"))
                .and_where(Expr::col("from_id").eq(id).or(Expr::col("into_id").eq(id))),
            proposal,
        )
    }

    pub fn rejected(&mut self, ws: &str, from: &str, into: &str) -> Result<bool> {
        Ok(self
            .0
            .one(
                Query::select()
                    .from("merge_proposals")
                    .column("id")
                    .and_where(Expr::col("workspace_id").eq(ws))
                    .and_where(Expr::col("from_id").eq(from))
                    .and_where(Expr::col("into_id").eq(into))
                    .and_where(Expr::col("status").eq("rejected"))
                    .limit(1),
                |_| Ok(()),
            )?
            .is_some())
    }
}

impl MergeProposals<'_, W<'_>> {
    pub fn insert(&mut self, p: &NewMergeProposal<'_>) -> Result<bool> {
        Ok(self.0.exec(
            Query::insert()
                .into_table("merge_proposals")
                .columns([
                    "id",
                    "workspace_id",
                    "from_id",
                    "into_id",
                    "account_id",
                    "reason",
                    "confidence",
                    "status",
                    "created",
                ])
                .values_panic([
                    p.id.into(),
                    p.workspace.into(),
                    p.from.into(),
                    p.into.into(),
                    p.account.into(),
                    p.reason.into(),
                    p.confidence.into(),
                    "open".into(),
                    now().into(),
                ])
                .on_conflict(OnConflict::new().do_nothing().to_owned()),
        )? != 0)
    }

    pub fn set_status(&mut self, id: &str, status: &str) -> Result<()> {
        self.0.exec(
            Query::update()
                .table("merge_proposals")
                .value("status", status)
                .value("resolved", (status != "open").then(now))
                .and_where(Expr::col("id").eq(id)),
        )?;
        Ok(())
    }

    pub fn set_decision(&mut self, id: &str, decision: &str) -> Result<()> {
        self.0.exec(
            Query::update()
                .table("merge_proposals")
                .value("decision_id", decision)
                .and_where(Expr::col("id").eq(id)),
        )?;
        Ok(())
    }
}

pub struct ContactMerges<'a, C>(pub &'a mut C);

#[derive(Debug, Clone, PartialEq)]
pub struct ContactMerge<T> {
    pub id: String,
    pub workspace_id: String,
    pub from_id: String,
    pub into_id: String,
    pub moved: T,
    pub actor: String,
    pub created: i64,
    pub split: Option<i64>,
}

impl<C: Read> ContactMerges<'_, C> {
    pub fn get<T: serde::de::DeserializeOwned + Default>(
        &mut self,
        id: &str,
    ) -> Result<Option<ContactMerge<T>>> {
        self.0.one(
            Query::select()
                .from("merges")
                .columns([
                    "id",
                    "workspace_id",
                    "from_id",
                    "into_id",
                    "moved",
                    "actor",
                    "created",
                    "split",
                ])
                .and_where(Expr::col("id").eq(id)),
            |row| {
                let moved: String = row.get(4)?;
                Ok(ContactMerge {
                    id: row.get(0)?,
                    workspace_id: row.get(1)?,
                    from_id: row.get(2)?,
                    into_id: row.get(3)?,
                    moved: serde_json::from_str(&moved).unwrap_or_default(),
                    actor: row.get(5)?,
                    created: row.get(6)?,
                    split: row.get(7)?,
                })
            },
        )
    }

    pub fn has_later<T>(&mut self, merge: &ContactMerge<T>) -> Result<bool> {
        let order = self.0.db().dialect().insertion_order(None);
        let before = Query::select()
            .from("merges")
            .expr(order.clone())
            .and_where(Expr::col("id").eq(&merge.id))
            .take();
        let newer = Expr::col("created")
            .gt(merge.created)
            .or(Expr::col("created").eq(merge.created).and(order.gt(before)));
        Ok(self
            .0
            .one(
                Query::select()
                    .from("merges")
                    .column("id")
                    .and_where(Expr::col("workspace_id").eq(&merge.workspace_id))
                    .and_where(Expr::col("into_id").eq(&merge.into_id))
                    .and_where(Expr::col("split").is_null())
                    .and_where(Expr::col("id").ne(&merge.id))
                    .and_where(newer)
                    .limit(1),
                |_| Ok(()),
            )?
            .is_some())
    }

    pub fn of_contact(
        &mut self,
        ws: &str,
        id: &str,
    ) -> Result<Vec<(String, String, i64, Option<i64>)>> {
        let order = self.0.db().dialect().insertion_order(Some("m"));
        self.0.all(
            Query::select()
                .from_as("merges", "m")
                .column(("m", "id"))
                .expr(Func::coalesce([
                    Expr::col(("p", "name")),
                    Expr::Constant("".into()),
                ]))
                .columns([("m", "created"), ("m", "split")])
                .join_as(
                    JoinType::LeftJoin,
                    "parties",
                    "p",
                    Expr::col(("p", "id")).equals(("m", "from_id")),
                )
                .and_where(Expr::col(("m", "workspace_id")).eq(ws))
                .and_where(Expr::col(("m", "into_id")).eq(id))
                .order_by(("m", "created"), Order::Desc)
                .order_by_expr(order, Order::Desc),
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
    }
}

impl ContactMerges<'_, W<'_>> {
    pub fn insert<T: serde::Serialize>(&mut self, merge: &ContactMerge<T>) -> Result<()> {
        self.0.exec(
            Query::insert()
                .into_table("merges")
                .columns([
                    "id",
                    "workspace_id",
                    "from_id",
                    "into_id",
                    "moved",
                    "actor",
                    "created",
                ])
                .values_panic([
                    merge.id.as_str().into(),
                    merge.workspace_id.as_str().into(),
                    merge.from_id.as_str().into(),
                    merge.into_id.as_str().into(),
                    serde_json::to_string(&merge.moved)?.into(),
                    merge.actor.as_str().into(),
                    merge.created.into(),
                ]),
        )?;
        Ok(())
    }

    pub fn split(&mut self, id: &str) -> Result<()> {
        self.0.exec(
            Query::update()
                .table("merges")
                .value("split", now())
                .and_where(Expr::col("id").eq(id)),
        )?;
        Ok(())
    }
}
