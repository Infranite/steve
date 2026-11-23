use sea_query::{Expr, ExprTrait, Func, JoinType, OnConflict, Order, Query};
use serde_json::Value;

use crate::{Read, Result, W};

pub struct AccountProfiles<'a, C>(pub &'a mut C);

pub struct Lookup {
    pub checked: Option<i64>,
    pub next_check: Option<i64>,
    pub source: Option<String>,
    pub gaps: Value,
}

pub struct LookupUpdate<'a> {
    pub account: &'a str,
    pub workspace: &'a str,
    pub at: i64,
    pub next: Option<i64>,
    pub attempts: i64,
    pub source: Option<&'a str>,
    pub gaps: &'a Value,
}

impl<C: Read> AccountProfiles<'_, C> {
    pub fn latest(&mut self, ws: &str, party: &str) -> Result<Option<Lookup>> {
        self.0.one(
            Query::select()
                .from_as("account_profiles", "p")
                .columns([
                    ("p", "checked"),
                    ("p", "next_check"),
                    ("p", "source"),
                    ("p", "gaps"),
                ])
                .join_as(
                    JoinType::InnerJoin,
                    "accounts",
                    "a",
                    Expr::col(("a", "id"))
                        .equals(("p", "account_id"))
                        .and(Expr::col(("a", "workspace_id")).equals(("p", "workspace_id"))),
                )
                .and_where(Expr::col(("p", "workspace_id")).eq(ws))
                .and_where(Expr::col(("a", "party_id")).eq(party))
                .and_where(Expr::col(("a", "status")).eq("active"))
                .order_by_expr(Expr::col(("p", "checked")).is_null(), Order::Asc)
                .order_by(("p", "checked"), Order::Desc)
                .limit(1),
            |row| {
                Ok(Lookup {
                    checked: row.get(0)?,
                    next_check: row.get(1)?,
                    source: row.get(2)?,
                    gaps: row
                        .get::<String>(3)
                        .ok()
                        .and_then(|text| serde_json::from_str(&text).ok())
                        .unwrap_or_default(),
                })
            },
        )
    }

    pub fn attempts(&mut self, id: &str) -> Result<i64> {
        Ok(self
            .0
            .one(
                Query::select()
                    .from("account_profiles")
                    .column("attempts")
                    .and_where(Expr::col("account_id").eq(id)),
                |row| row.get(0),
            )?
            .unwrap_or_default())
    }

    pub fn retry(&mut self, id: &str) -> Result<Option<Option<i64>>> {
        self.0.one(
            Query::select()
                .from("account_profiles")
                .column("next_check")
                .and_where(Expr::col("account_id").eq(id)),
            |row| row.get(0),
        )
    }
}

impl AccountProfiles<'_, W<'_>> {
    pub fn upsert(&mut self, data: &LookupUpdate<'_>) -> Result<()> {
        let source = Func::coalesce([
            Expr::col(("excluded", "source")),
            Expr::col(("account_profiles", "source")),
        ]);
        self.0.exec(
            Query::insert()
                .into_table("account_profiles")
                .columns([
                    "account_id",
                    "workspace_id",
                    "checked",
                    "next_check",
                    "attempts",
                    "source",
                    "gaps",
                ])
                .values_panic([
                    data.account.into(),
                    data.workspace.into(),
                    data.at.into(),
                    data.next.into(),
                    data.attempts.into(),
                    data.source.into(),
                    data.gaps.to_string().into(),
                ])
                .on_conflict(
                    OnConflict::column("account_id")
                        .update_columns(["checked", "next_check", "attempts", "gaps"])
                        .value("source", source)
                        .to_owned(),
                ),
        )?;
        Ok(())
    }

    pub fn mark_due(&mut self, ws: &str, at: i64) -> Result<usize> {
        Ok(self.0.exec(
            Query::update()
                .table("account_profiles")
                .value("next_check", at)
                .value("attempts", 0_i64)
                .and_where(Expr::col("workspace_id").eq(ws))
                .and_where(Expr::col("gaps").ne("{}")),
        )? as usize)
    }
}
