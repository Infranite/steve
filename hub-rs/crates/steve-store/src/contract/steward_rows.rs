use sea_query::{Expr, ExprTrait, OnConflict, Order, Query, SelectStatement};
use serde::{Deserialize, Serialize};

use super::Row;
use crate::{Read, Result, W, now};

pub struct Stewards<'a, C>(pub &'a mut C);

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Steward {
    pub workspace_id: String,
    pub object_type: String,
    pub object_id: String,
    pub person_id: String,
    pub role: String,
    /// `manual` | `connect` | `sync` | `chat` | `inferred` | `seed`
    pub source: String,
    pub confidence: f64,
    /// `confirmed` | `proposed`
    pub status: String,
    pub source_ref: Option<String>,
    pub decision_id: Option<String>,
    pub created: i64,
    pub updated: i64,
}

impl Steward {
    /// A confirmed row created now.
    pub fn new(
        ws: &str,
        object_type: &str,
        object_id: &str,
        person: &str,
        role: &str,
        source: &str,
    ) -> Self {
        let t = now();
        Self {
            workspace_id: ws.into(),
            object_type: object_type.into(),
            object_id: object_id.into(),
            person_id: person.into(),
            role: role.into(),
            source: source.into(),
            confidence: 1.0,
            status: "confirmed".into(),
            source_ref: None,
            decision_id: None,
            created: t,
            updated: t,
        }
    }

    pub fn key(&self) -> Key {
        Key {
            object_type: self.object_type.clone(),
            object_id: self.object_id.clone(),
            person_id: self.person_id.clone(),
            role: self.role.clone(),
        }
    }
}

/// The primary key of a row within a workspace.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Key {
    pub object_type: String,
    pub object_id: String,
    pub person_id: String,
    pub role: String,
}

const COLS: [&str; 12] = [
    "workspace_id",
    "object_type",
    "object_id",
    "person_id",
    "role",
    "source",
    "confidence",
    "status",
    "source_ref",
    "decision_id",
    "created",
    "updated",
];

fn row(r: &Row<'_>) -> Result<Steward> {
    Ok(Steward {
        workspace_id: r.get(0)?,
        object_type: r.get(1)?,
        object_id: r.get(2)?,
        person_id: r.get(3)?,
        role: r.get(4)?,
        source: r.get(5)?,
        confidence: r.get(6)?,
        status: r.get(7)?,
        source_ref: r.get(8)?,
        decision_id: r.get(9)?,
        created: r.get(10)?,
        updated: r.get(11)?,
    })
}

fn rows() -> SelectStatement {
    Query::select().from("stewards").columns(COLS).take()
}

fn identity(ws: &str, key: &Key) -> sea_query::Condition {
    sea_query::Cond::all()
        .add(Expr::col("workspace_id").eq(ws))
        .add(Expr::col("object_type").eq(&key.object_type))
        .add(Expr::col("object_id").eq(&key.object_id))
        .add(Expr::col("person_id").eq(&key.person_id))
        .add(Expr::col("role").eq(&key.role))
}

fn subject(ws: &str, kind: &str, id: &str) -> SelectStatement {
    rows()
        .and_where(Expr::col("workspace_id").eq(ws))
        .and_where(Expr::col("object_type").eq(kind))
        .and_where(Expr::col("object_id").eq(id))
        .take()
}

impl<C: Read> Stewards<'_, C> {
    pub fn get(&mut self, ws: &str, key: &Key) -> Result<Option<Steward>> {
        self.0.one(rows().cond_where(identity(ws, key)), row)
    }

    pub fn of_object(&mut self, ws: &str, kind: &str, id: &str) -> Result<Vec<Steward>> {
        let role = Expr::col("role");
        let priority = Expr::case(
            role.clone().eq(Expr::Constant("owner".into())),
            Expr::Constant(0_i64.into()),
        )
        .case(
            role.eq(Expr::Constant("maintainer".into())),
            Expr::Constant(1_i64.into()),
        )
        .finally(Expr::Constant(2_i64.into()));
        self.0.all(
            subject(ws, kind, id)
                .order_by_expr(priority.into(), Order::Asc)
                .order_by("created", Order::Asc),
            row,
        )
    }

    pub fn of_person(&mut self, ws: &str, id: &str) -> Result<Vec<Steward>> {
        self.0.all(
            rows()
                .and_where(Expr::col("workspace_id").eq(ws))
                .and_where(Expr::col("person_id").eq(id))
                .order_by("object_type", Order::Asc)
                .order_by("created", Order::Asc),
            row,
        )
    }

    pub fn by_decision(&mut self, decision: &str) -> Result<Option<Steward>> {
        self.0
            .one(rows().and_where(Expr::col("decision_id").eq(decision)), row)
    }

    pub fn confirmed_objects(
        &mut self,
        ws: &str,
    ) -> Result<std::collections::HashSet<(String, String)>> {
        Ok(self
            .0
            .all(
                rows()
                    .clear_selects()
                    .columns(["object_type", "object_id"])
                    .and_where(Expr::col("workspace_id").eq(ws))
                    .and_where(Expr::col("role").eq("owner"))
                    .and_where(Expr::col("status").eq("confirmed")),
                |r| Ok((r.get(0)?, r.get(1)?)),
            )?
            .into_iter()
            .collect())
    }

    pub fn has_any(&mut self, ws: &str, kind: &str, id: &str) -> Result<bool> {
        Ok(self
            .0
            .one(
                subject(ws, kind, id)
                    .clear_selects()
                    .column("role")
                    .limit(1),
                |_| Ok(()),
            )?
            .is_some())
    }
}

impl Stewards<'_, W<'_>> {
    pub fn put(&mut self, row: &Steward) -> Result<()> {
        self.0.exec(
            Query::insert()
                .into_table("stewards")
                .columns(COLS)
                .values_panic([
                    row.workspace_id.as_str().into(),
                    row.object_type.as_str().into(),
                    row.object_id.as_str().into(),
                    row.person_id.as_str().into(),
                    row.role.as_str().into(),
                    row.source.as_str().into(),
                    row.confidence.into(),
                    row.status.as_str().into(),
                    row.source_ref.as_deref().into(),
                    row.decision_id.as_deref().into(),
                    row.created.into(),
                    row.updated.into(),
                ])
                .on_conflict(
                    OnConflict::columns([
                        "workspace_id",
                        "object_type",
                        "object_id",
                        "person_id",
                        "role",
                    ])
                    .update_columns([
                        "source",
                        "confidence",
                        "status",
                        "source_ref",
                        "decision_id",
                        "updated",
                    ])
                    .to_owned(),
                ),
        )?;
        Ok(())
    }

    pub fn delete(&mut self, ws: &str, key: &Key) -> Result<bool> {
        Ok(self.0.exec(
            Query::delete()
                .from_table("stewards")
                .cond_where(identity(ws, key)),
        )? != 0)
    }

    pub fn move_person(&mut self, ws: &str, key: &Key, person: &str) -> Result<()> {
        self.0.exec(
            Query::update()
                .table("stewards")
                .value("person_id", person)
                .cond_where(identity(ws, key)),
        )?;
        Ok(())
    }

    pub fn move_object(&mut self, ws: &str, key: &Key, object: &str) -> Result<()> {
        self.0.exec(
            Query::update()
                .table("stewards")
                .value("object_id", object)
                .cond_where(identity(ws, key)),
        )?;
        Ok(())
    }

    pub fn forget_party(&mut self, ws: &str, party: &str) -> Result<usize> {
        Ok(self.0.exec(
            Query::delete()
                .from_table("stewards")
                .and_where(Expr::col("workspace_id").eq(ws))
                .and_where(
                    Expr::col("person_id").eq(party).or(Expr::col("object_type")
                        .eq("party")
                        .and(Expr::col("object_id").eq(party))),
                ),
        )? as usize)
    }
}
