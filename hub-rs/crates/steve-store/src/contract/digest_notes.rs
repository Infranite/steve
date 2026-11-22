use sea_query::{Expr, ExprTrait, Order, Query};
use serde::Serialize;

use crate::{Read, Result, W, uid};

pub struct DigestNotes<'a, C>(pub &'a mut C);

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct DigestEntry {
    pub id: String,
    pub event_id: Option<String>,
    pub run_id: Option<String>,
    pub text: String,
    pub created: i64,
}

impl<C: Read> DigestNotes<'_, C> {
    pub fn contains_pending(&mut self, ws: &str, text: &str) -> Result<bool> {
        self.0
            .one(
                Query::select()
                    .column("id")
                    .from("digest_entries")
                    .and_where(Expr::col("workspace_id").eq(ws))
                    .and_where(Expr::col("taken").is_null())
                    .and_where(Expr::col("text").eq(text))
                    .limit(1),
                |_| Ok(()),
            )
            .map(|row| row.is_some())
    }

    pub fn pending(&mut self, ws: &str, since: i64) -> Result<Vec<DigestEntry>> {
        let tie = self.0.db().dialect().insertion_order(None);
        self.0.all(
            Query::select()
                .columns(["id", "event_id", "run_id", "text", "created"])
                .from("digest_entries")
                .and_where(Expr::col("workspace_id").eq(ws))
                .and_where(Expr::col("taken").is_null())
                .and_where(Expr::col("created").gte(since))
                .order_by("created", Order::Asc)
                .order_by_expr(tie, Order::Asc),
            |row| {
                Ok(DigestEntry {
                    id: row.get(0)?,
                    event_id: row.get(1)?,
                    run_id: row.get(2)?,
                    text: row.get(3)?,
                    created: row.get(4)?,
                })
            },
        )
    }
}

impl DigestNotes<'_, W<'_>> {
    pub fn add(
        &mut self,
        ws: &str,
        text: &str,
        event_id: Option<&str>,
        run_id: Option<&str>,
        now: i64,
    ) -> Result<String> {
        let id = uid("dig");
        self.0.exec(
            Query::insert()
                .into_table("digest_entries")
                .columns([
                    "id",
                    "workspace_id",
                    "event_id",
                    "run_id",
                    "text",
                    "created",
                ])
                .values_panic([
                    id.as_str().into(),
                    ws.into(),
                    event_id.into(),
                    run_id.into(),
                    text.into(),
                    now.into(),
                ]),
        )?;
        Ok(id)
    }

    pub fn mark_taken(&mut self, ids: &[String], now: i64) -> Result<()> {
        for id in ids {
            self.0.exec(
                Query::update()
                    .table("digest_entries")
                    .value("taken", now)
                    .and_where(Expr::col("id").eq(id.as_str()))
                    .and_where(Expr::col("taken").is_null()),
            )?;
        }
        Ok(())
    }
}
