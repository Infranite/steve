use crate::contract::Q;
use std::collections::BTreeMap;

use sea_query::{
    Expr, ExprTrait, Func, OnConflict, Order, Query, SelectStatement, UpdateStatement,
};
use serde::Serialize;
use serde_json::Value;

use crate::contract::{Json, Row};
use crate::{Read, Result, StoreError, W, uid};

pub struct Matters<'a, C>(pub &'a mut C);

#[derive(Serialize)]
pub struct Matter {
    pub id: String,
    pub title: String,
    pub state: String,
    pub summary: String,
    pub first_at: Option<i64>,
    pub last_at: Option<i64>,
    pub updated: i64,
    #[serde(skip)]
    pub merged_into: Option<String>,
}

#[derive(Serialize)]
pub struct Event {
    pub op: String,
    pub other: Option<String>,
    pub detail: Value,
    pub actor: String,
    pub at: i64,
}

#[derive(Serialize)]
pub struct Participant {
    #[serde(rename = "id")]
    pub party: String,
    pub segments: i64,
    pub last_at: i64,
}

pub struct MatterEvent<'a> {
    pub op: &'a str,
    pub other: Option<&'a str>,
    pub detail: Value,
    pub actor: &'a str,
    pub now: i64,
}

const FIELDS: [&str; 8] = [
    "id",
    "title",
    "state",
    "summary",
    "first_at",
    "last_at",
    "updated",
    "merged_into",
];

fn scoped(ws: &str) -> SelectStatement {
    Query::select()
        .from("matters")
        .and_where(Expr::col("workspace_id").eq(ws))
        .take()
}

fn matter_row(r: &Row<'_>) -> Result<Matter> {
    Ok(Matter {
        id: r.get(0)?,
        title: r.get(1)?,
        state: r.get(2)?,
        summary: r.get(3)?,
        first_at: r.get(4)?,
        last_at: r.get(5)?,
        updated: r.get(6)?,
        merged_into: r.get(7)?,
    })
}

fn update(id: &str) -> UpdateStatement {
    Query::update()
        .table("matters")
        .and_where(Expr::col("id").eq(id))
        .take()
}

fn row_key(id: &str) -> SelectStatement {
    Query::select()
        .column("row")
        .from("matters_fts_rows")
        .and_where(Expr::col("matter_id").eq(id))
        .take()
}

impl<C: Read> Matters<'_, C> {
    pub fn get(&mut self, ws: &str, id: &str) -> Result<Option<Matter>> {
        self.0.one(
            scoped(ws).columns(FIELDS).and_where(Expr::col("id").eq(id)),
            matter_row,
        )
    }

    /// Walks the same bounded merge chain for direct reads and candidate lookup.
    pub fn resolve(&mut self, ws: &str, id: &str) -> Result<Option<Matter>> {
        let mut id = id.to_owned();
        for _ in 0..8 {
            match self.get(ws, &id)? {
                Some(mut row) if row.state == "merged" && row.merged_into.is_some() => {
                    id = row.merged_into.take().expect("merged target");
                }
                row => return Ok(row),
            }
        }
        Ok(None)
    }

    pub fn related_to_person(&mut self, ws: &str, party: &str) -> Result<Vec<String>> {
        self.0.all(
            scoped(ws)
                .column(("matters", "id"))
                .inner_join(
                    "matter_parties",
                    Expr::col(("matter_parties", "matter_id")).equals(("matters", "id")),
                )
                .and_where(Expr::col(("matter_parties", "party_id")).eq(party)),
            |r| r.get(0),
        )
    }

    pub fn list(
        &mut self,
        ws: &str,
        ids: Option<&[String]>,
        states: &[String],
        limit: usize,
    ) -> Result<Vec<Matter>> {
        if ids.is_some_and(|ids| ids.is_empty()) {
            return Ok(Vec::new());
        }
        let mut q = scoped(ws);
        if let Some(ids) = ids {
            q.and_where(Expr::col("id").is_in(ids.iter().map(String::as_str)));
        }
        if !states.is_empty() {
            q.and_where(Expr::col("state").is_in(states.iter().map(String::as_str)));
        }
        self.0.all(
            q.columns(FIELDS)
                .and_where(Expr::col("state").ne("merged"))
                .order_by_expr(
                    Func::coalesce([Expr::col("last_at"), Expr::col("updated")]).into(),
                    Order::Desc,
                )
                .order_by("id", Order::Asc)
                .limit(limit as u64),
            matter_row,
        )
    }

    pub fn recent_overlap(
        &mut self,
        ws: &str,
        people: &[&str],
        oldest: i64,
        newest: i64,
    ) -> Result<Vec<(String, i64)>> {
        if people.is_empty() {
            return Ok(Vec::new());
        }
        self.0.all(
            Query::select()
                .column(("matters", "id"))
                .expr(Func::count(Expr::col(("matter_parties", "party_id"))))
                .from("matters")
                .inner_join(
                    "matter_parties",
                    Expr::col(("matter_parties", "matter_id")).equals(("matters", "id")),
                )
                .and_where(Expr::col(("matters", "workspace_id")).eq(ws))
                .and_where(Expr::col(("matters", "state")).eq("open"))
                .and_where(Expr::col(("matter_parties", "party_id")).is_in(people.iter().copied()))
                .and_where(
                    Expr::from(Func::coalesce([
                        Expr::col(("matters", "last_at")),
                        Expr::col(("matters", "updated")),
                    ]))
                    .between(oldest, newest),
                )
                .group_by_col(("matters", "id")),
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
    }

    pub fn search(
        &mut self,
        ws: &str,
        text: &str,
        states: &[&str],
        limit: usize,
    ) -> Result<Vec<(String, f64)>> {
        let dialect = self.0.db().dialect();
        let score = dialect.fulltext_score("matters_fts");
        self.0.all(
            Query::select()
                .column(("matters", "id"))
                .expr(score.clone())
                .from("matters_fts")
                .inner_join(
                    "matters_fts_rows",
                    Expr::col(("matters_fts_rows", "row")).equals(("matters_fts", "rowid")),
                )
                .inner_join(
                    "matters",
                    Expr::col(("matters", "id")).equals(("matters_fts_rows", "matter_id")),
                )
                .and_where(Expr::col(("matters", "workspace_id")).eq(ws))
                .and_where(Expr::col(("matters", "state")).is_in(states.iter().copied()))
                .and_where(dialect.fulltext_match("matters_fts", text))
                .order_by_expr(score.into(), Order::Asc)
                .limit(limit as u64),
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
    }

    pub fn people(&mut self, id: &str, limit: Option<u64>) -> Result<Vec<Participant>> {
        let mut q = Query::select();
        q.columns(["party_id", "messages", "last_at"])
            .from("matter_parties")
            .and_where(Expr::col("matter_id").eq(id))
            .order_by("messages", Order::Desc)
            .order_by("last_at", Order::Desc);
        if let Some(limit) = limit {
            q.limit(limit);
        }
        self.0.all(&q, |r| {
            Ok(Participant {
                party: r.get(0)?,
                segments: r.get(1)?,
                last_at: r.get(2)?,
            })
        })
    }

    pub fn events(&mut self, id: &str) -> Result<Vec<Event>> {
        self.0.all(
            Query::select()
                .columns(["op", "other", "detail", "actor", "created"])
                .from("matter_events")
                .and_where(Expr::col("matter_id").eq(id))
                .order_by("created", Order::Desc)
                .order_by("id", Order::Desc)
                .limit(50),
            |r| {
                Ok(Event {
                    op: r.get(0)?,
                    other: r.get(1)?,
                    detail: r.get::<Json<Value>>(2)?.0,
                    actor: r.get(3)?,
                    at: r.get(4)?,
                })
            },
        )
    }

    pub fn separated(&mut self, ws: &str, id: &str) -> Result<Vec<String>> {
        self.0.all(
            Query::select()
                .columns(["a", "b"])
                .from("matter_not_same")
                .and_where(Expr::col("workspace_id").eq(ws))
                .and_where(Expr::col("a").eq(id).or(Expr::col("b").eq(id))),
            |r| {
                let a: String = r.get(0)?;
                if a == id { r.get(1) } else { Ok(a) }
            },
        )
    }

    pub fn not_same(&mut self, ws: &str, a: &str, b: &str) -> Result<bool> {
        let (a, b) = pair(a, b);
        Ok(self
            .0
            .one(
                Query::select()
                    .column("a")
                    .from("matter_not_same")
                    .and_where(Expr::col("workspace_id").eq(ws))
                    .and_where(Expr::col("a").eq(a))
                    .and_where(Expr::col("b").eq(b)),
                |_| Ok(()),
            )?
            .is_some())
    }
}

fn pair<'a>(a: &'a str, b: &'a str) -> (&'a str, &'a str) {
    if a <= b { (a, b) } else { (b, a) }
}

impl Matters<'_, W<'_>> {
    pub fn create(&mut self, ws: &str, title: &str, actor: &str, now: i64) -> Result<String> {
        let id = uid("mtr");
        self.0.exec(
            Query::insert()
                .into_table("matters")
                .columns([
                    "id",
                    "workspace_id",
                    "title",
                    "created_by",
                    "created",
                    "updated",
                ])
                .values_panic([
                    id.as_str().into(),
                    ws.into(),
                    title.into(),
                    actor.into(),
                    now.into(),
                    now.into(),
                ]),
        )?;
        Ok(id)
    }

    pub fn record(&mut self, ws: &str, id: &str, e: MatterEvent<'_>) -> Result<()> {
        self.0.exec(
            Query::insert()
                .into_table("matter_events")
                .columns([
                    "workspace_id",
                    "matter_id",
                    "op",
                    "other",
                    "detail",
                    "actor",
                    "created",
                ])
                .values_panic([
                    ws.into(),
                    id.into(),
                    e.op.into(),
                    e.other.into(),
                    e.detail.to_string().into(),
                    e.actor.into(),
                    e.now.into(),
                ]),
        )?;
        super::emit(self.0, ws, id);
        Ok(())
    }

    pub fn set_title(&mut self, id: &str, title: &str, now: i64) -> Result<()> {
        self.0
            .exec(update(id).value("title", title).value("updated", now))?;
        Ok(())
    }

    pub fn set_state(&mut self, id: &str, state: &str, now: i64) -> Result<()> {
        self.0
            .exec(update(id).value("state", state).value("updated", now))?;
        Ok(())
    }

    pub fn set_summary(&mut self, id: &str, summary: &str, now: i64) -> Result<()> {
        self.0
            .exec(update(id).value("summary", summary).value("updated", now))?;
        Ok(())
    }

    pub fn mark_merged(&mut self, from: &str, into: &str, now: i64) -> Result<()> {
        self.0.exec(
            update(from)
                .value("state", "merged")
                .value("merged_into", into)
                .value("updated", now),
        )?;
        Ok(())
    }

    pub fn separate(&mut self, ws: &str, a: &str, b: &str, actor: &str, now: i64) -> Result<()> {
        let (a, b) = pair(a, b);
        self.0.exec(
            Query::insert()
                .into_table("matter_not_same")
                .columns(["workspace_id", "a", "b", "actor", "created"])
                .values_panic([ws.into(), a.into(), b.into(), actor.into(), now.into()])
                .on_conflict(
                    OnConflict::columns(["workspace_id", "a", "b"])
                        .do_nothing()
                        .to_owned(),
                ),
        )?;
        Ok(())
    }

    pub fn clear_separation(&mut self, ws: &str, a: &str, b: &str) -> Result<()> {
        let (a, b) = pair(a, b);
        self.0.exec(
            Query::delete()
                .from_table("matter_not_same")
                .and_where(Expr::col("workspace_id").eq(ws))
                .and_where(Expr::col("a").eq(a))
                .and_where(Expr::col("b").eq(b)),
        )?;
        Ok(())
    }

    pub fn recount(&mut self, id: &str, now: i64) -> Result<()> {
        let segments = super::Segments::new(self.0).stats(id)?;
        let mut parties = BTreeMap::<String, (i64, i64)>::new();
        for (ids, _, end) in &segments {
            for party in ids {
                let stats = parties.entry(party.clone()).or_insert((0, 0));
                stats.0 += 1;
                stats.1 = std::cmp::max(stats.1, *end);
            }
        }
        self.0.exec(
            Query::delete()
                .from_table("matter_parties")
                .and_where(Expr::col("matter_id").eq(id)),
        )?;
        for (party, (count, at)) in parties {
            self.0.exec(
                Query::insert()
                    .into_table("matter_parties")
                    .columns(["matter_id", "party_id", "messages", "last_at"])
                    .values_panic([id.into(), party.into(), count.into(), at.into()]),
            )?;
        }
        self.0.exec(
            update(id)
                .value("first_at", segments.iter().map(|s| s.1).min())
                .value(
                    "last_at",
                    segments
                        .iter()
                        .map(|s| s.2)
                        .max()
                        .map(|at| std::cmp::max(at, 0)),
                )
                .value("updated", now),
        )?;
        Ok(())
    }

    pub fn search_document(&mut self, id: &str) -> Result<String> {
        let (title, summary): (String, String) = self
            .0
            .one(
                Query::select()
                    .columns(["title", "summary"])
                    .from("matters")
                    .and_where(Expr::col("id").eq(id)),
                |r| Ok((r.get(0)?, r.get(1)?)),
            )?
            .ok_or_else(|| StoreError::Rejected("unknown matter".into()))?;
        let summaries = super::Segments::new(self.0).recent_summaries(id)?;
        Ok(format!("{title}\n{summary}\n{}", summaries.join("\n")))
    }

    pub fn put_search_document(&mut self, id: &str, content: &str) -> Result<()> {
        self.0.exec(
            Query::insert()
                .into_table("matters_fts_rows")
                .columns(["matter_id"])
                .values_panic([id.into()])
                .on_conflict(OnConflict::column("matter_id").do_nothing().to_owned()),
        )?;
        let row: i64 = self
            .0
            .one(&row_key(id), |r| r.get(0))?
            .ok_or_else(|| StoreError::Rejected("matter search row missing".into()))?;
        self.0.exec(
            Query::delete()
                .from_table("matters_fts")
                .and_where(Expr::col("rowid").eq(row)),
        )?;
        self.0.exec(
            Query::insert()
                .into_table("matters_fts")
                .columns(["rowid", "seg"])
                .values_panic([row.into(), content.into()]),
        )?;
        Ok(())
    }

    pub fn remove_search_document(&mut self, id: &str) -> Result<()> {
        if let Some(row) = self.0.one(&row_key(id), |r| r.get::<i64>(0))? {
            self.0.exec(
                Query::delete()
                    .from_table("matters_fts")
                    .and_where(Expr::col("rowid").eq(row)),
            )?;
            self.0.exec(
                Query::delete()
                    .from_table("matters_fts_rows")
                    .and_where(Expr::col("row").eq(row)),
            )?;
        }
        Ok(())
    }
}
