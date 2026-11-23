use sea_query::{Expr, ExprTrait, Func, JoinType, OnConflict, Order, Query, SelectStatement};
use serde::Serialize;

use crate::{Read, Result, W, base::Level, now, uid};

pub struct Attributes<'a, C>(pub &'a mut C);

/// An `attr_defs` row; `workspace_id = '*'` for the built-in ones.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct AttrDef {
    pub id: String,
    #[serde(skip)]
    pub workspace_id: String,
    /// `contact` | `org` | `both`
    pub applies: String,
    pub key: String,
    pub label: String,
    /// `text` | `phone` | `date` | `address` | `url` | `enum`
    #[serde(rename = "type")]
    pub kind: String,
    pub options: Vec<String>,
    pub default_level: Level,
    /// `builtin` | `me` | `steve` | `plugin:<id>`
    pub source: String,
    /// `active` | `proposed`
    pub status: String,
    pub builtin: bool,
    pub created: i64,
}

/// A definition to insert.
#[derive(Debug, Clone)]
pub struct NewDef {
    pub applies: String,
    pub key: String,
    pub label: String,
    pub kind: String,
    pub options: Vec<String>,
    pub default_level: Level,
    pub source: String,
    pub status: String,
}

/// An `attr_values` row.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct AttrValue {
    pub id: String,
    pub workspace_id: String,
    pub party_id: String,
    pub key: String,
    pub label: String,
    pub value: String,
    pub source: String,
    pub source_label: String,
    pub source_ref: Option<String>,
    pub level: Level,
    /// `candidate` | `confirmed` | `stale`
    pub status: String,
    pub decision_id: Option<String>,
    pub created: i64,
    pub updated: i64,
}

/// A value to insert.
#[derive(Debug, Clone)]
pub struct NewValue {
    pub party_id: String,
    pub key: String,
    pub label: String,
    pub value: String,
    pub source: String,
    pub source_label: String,
    pub source_ref: Option<String>,
    pub level: Level,
    pub status: String,
    pub decision_id: Option<String>,
}

impl AttrDef {
    pub fn applies_to(&self, kind: &str) -> bool {
        self.applies == "both" || self.applies == kind
    }
}

const DEF_COLS: [&str; 11] = [
    "id",
    "workspace_id",
    "applies",
    "key",
    "label",
    "type",
    "options",
    "default_level",
    "source",
    "status",
    "created",
];
const VALUE_COLS: [&str; 14] = [
    "id",
    "workspace_id",
    "party_id",
    "key",
    "label",
    "value",
    "source",
    "source_label",
    "source_ref",
    "level",
    "status",
    "decision_id",
    "created",
    "updated",
];

fn definitions() -> SelectStatement {
    Query::select().from("attr_defs").columns(DEF_COLS).take()
}

fn values() -> SelectStatement {
    Query::select()
        .from("attr_values")
        .columns(VALUE_COLS)
        .take()
}

fn definition(row: &super::Row<'_>) -> Result<AttrDef> {
    let ws: String = row.get(1)?;
    let options: Option<String> = row.get(6)?;
    Ok(AttrDef {
        id: row.get(0)?,
        builtin: ws == "*",
        workspace_id: ws,
        applies: row.get(2)?,
        key: row.get(3)?,
        label: row.get(4)?,
        kind: row.get(5)?,
        options: options
            .and_then(|v| serde_json::from_str(&v).ok())
            .unwrap_or_default(),
        default_level: Level::from_db(&row.get::<String>(7)?),
        source: row.get(8)?,
        status: row.get(9)?,
        created: row.get(10)?,
    })
}

fn attribute(row: &super::Row<'_>) -> Result<AttrValue> {
    Ok(AttrValue {
        id: row.get(0)?,
        workspace_id: row.get(1)?,
        party_id: row.get(2)?,
        key: row.get(3)?,
        label: row.get(4)?,
        value: row.get(5)?,
        source: row.get(6)?,
        source_label: row.get(7)?,
        source_ref: row.get(8)?,
        level: Level::from_db(&row.get::<String>(9)?),
        status: row.get(10)?,
        decision_id: row.get(11)?,
        created: row.get(12)?,
        updated: row.get(13)?,
    })
}

impl<C: Read> Attributes<'_, C> {
    pub fn definitions(&mut self, ws: &str) -> Result<Vec<AttrDef>> {
        let order = self.0.db().dialect().insertion_order(None);
        self.0.all(
            definitions()
                .and_where(Expr::col("workspace_id").is_in(["*", ws]))
                .order_by_expr(
                    Expr::col("workspace_id").ne(Expr::Constant("*".into())),
                    Order::Asc,
                )
                .order_by("created", Order::Asc)
                .order_by_expr(order, Order::Asc),
            definition,
        )
    }

    pub fn definition_by_key(&mut self, ws: &str, key: &str) -> Result<Option<AttrDef>> {
        self.0.one(
            definitions()
                .and_where(Expr::col("workspace_id").is_in(["*", ws]))
                .and_where(Expr::col("key").eq(key))
                .order_by_expr(
                    Expr::col("workspace_id").eq(Expr::Constant("*".into())),
                    Order::Desc,
                )
                .limit(1),
            definition,
        )
    }

    pub fn definition(&mut self, id: &str) -> Result<Option<AttrDef>> {
        self.0
            .one(definitions().and_where(Expr::col("id").eq(id)), definition)
    }

    pub fn definition_usage(&mut self, ws: &str, key: &str) -> Result<i64> {
        let query = Query::select()
            .distinct()
            .column(("v", "party_id"))
            .from_as("attr_values", "v")
            .join_as(
                JoinType::InnerJoin,
                "parties",
                "p",
                Expr::col(("p", "id")).equals(("v", "party_id")),
            )
            .and_where(Expr::col(("v", "workspace_id")).eq(ws))
            .and_where(Expr::col(("v", "key")).eq(key))
            .and_where(Expr::col(("p", "status")).eq("active"))
            .take();
        Ok(self
            .0
            .one(
                Query::select()
                    .from_subquery(query, "usage")
                    .expr(Func::count(Expr::col("party_id"))),
                |r| r.get(0),
            )?
            .unwrap_or(0))
    }

    pub fn values(&mut self, ws: &str, party: &str) -> Result<Vec<AttrValue>> {
        let order = self.0.db().dialect().insertion_order(None);
        self.0.all(
            values()
                .and_where(Expr::col("workspace_id").eq(ws))
                .and_where(Expr::col("party_id").eq(party))
                .order_by("created", Order::Asc)
                .order_by_expr(order, Order::Asc),
            attribute,
        )
    }

    pub fn value(&mut self, id: &str) -> Result<Option<AttrValue>> {
        self.0
            .one(values().and_where(Expr::col("id").eq(id)), attribute)
    }

    fn confirmed_siblings(&mut self, value: &AttrValue) -> Result<Vec<(String, String, String)>> {
        let order = self.0.db().dialect().insertion_order(None);
        self.0.all(
            Query::select()
                .from("attr_values")
                .columns(["id", "label", "value"])
                .and_where(Expr::col("workspace_id").eq(&value.workspace_id))
                .and_where(Expr::col("party_id").eq(&value.party_id))
                .and_where(Expr::col("key").eq(&value.key))
                .and_where(Expr::col("status").eq("confirmed"))
                .and_where(Expr::col("id").ne(&value.id))
                .order_by_expr(order, Order::Asc),
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
    }
}

fn row_update(table: &'static str, id: &str) -> sea_query::UpdateStatement {
    Query::update()
        .table(table)
        .and_where(Expr::col("id").eq(id))
        .take()
}

impl Attributes<'_, W<'_>> {
    pub fn insert_definition(&mut self, ws: &str, d: &NewDef) -> Result<AttrDef> {
        let id = uid("ad");
        self.0.exec(
            Query::insert()
                .into_table("attr_defs")
                .columns(DEF_COLS)
                .values_panic([
                    id.as_str().into(),
                    ws.into(),
                    d.applies.as_str().into(),
                    d.key.as_str().into(),
                    d.label.as_str().into(),
                    d.kind.as_str().into(),
                    serde_json::to_string(&d.options)?.into(),
                    d.default_level.code().into(),
                    d.source.as_str().into(),
                    d.status.as_str().into(),
                    now().into(),
                ]),
        )?;
        crate::repo::must(self.definition(&id)?)
    }

    pub fn restore_definition(&mut self, row: &AttrDef) -> Result<()> {
        self.0.exec(
            Query::insert()
                .into_table("attr_defs")
                .columns(DEF_COLS)
                .values_panic([
                    row.id.as_str().into(),
                    row.workspace_id.as_str().into(),
                    row.applies.as_str().into(),
                    row.key.as_str().into(),
                    row.label.as_str().into(),
                    row.kind.as_str().into(),
                    serde_json::to_string(&row.options)?.into(),
                    row.default_level.code().into(),
                    row.source.as_str().into(),
                    row.status.as_str().into(),
                    row.created.into(),
                ])
                .on_conflict(OnConflict::new().do_nothing().to_owned()),
        )?;
        Ok(())
    }

    pub fn set_definition_status(
        &mut self,
        id: &str,
        status: &str,
        only: Option<&str>,
    ) -> Result<()> {
        let mut q = row_update("attr_defs", id);
        q.value("status", status);
        if let Some(status) = only {
            q.and_where(Expr::col("status").eq(status));
        }
        self.0.exec(&q)?;
        Ok(())
    }

    pub fn delete_proposal(&mut self, id: &str) -> Result<()> {
        self.0.exec(
            Query::delete()
                .from_table("attr_defs")
                .and_where(Expr::col("id").eq(id))
                .and_where(Expr::col("status").eq("proposed")),
        )?;
        Ok(())
    }

    pub fn update_definition(
        &mut self,
        id: &str,
        label: Option<&str>,
        options: Option<&[String]>,
        level: Option<Level>,
    ) -> Result<()> {
        let columns = [
            ("label", label.map(Expr::value)),
            (
                "options",
                options
                    .map(serde_json::to_string)
                    .transpose()?
                    .map(Expr::value),
            ),
            ("default_level", level.map(|v| Expr::value(v.code()))),
        ];
        if columns.iter().any(|(_, v)| v.is_some()) {
            self.0.exec(
                row_update("attr_defs", id).values(
                    columns
                        .into_iter()
                        .filter_map(|(key, v)| v.map(|expr| (key, expr))),
                ),
            )?;
        }
        Ok(())
    }

    pub fn delete_definition(&mut self, id: &str) -> Result<()> {
        self.0.exec(
            Query::delete()
                .from_table("attr_defs")
                .and_where(Expr::col("id").eq(id)),
        )?;
        Ok(())
    }

    pub fn insert_value(&mut self, ws: &str, v: &NewValue) -> Result<AttrValue> {
        let id = uid("av");
        let at = now();
        self.0.exec(
            Query::insert()
                .into_table("attr_values")
                .columns(VALUE_COLS)
                .values_panic([
                    id.as_str().into(),
                    ws.into(),
                    v.party_id.as_str().into(),
                    v.key.as_str().into(),
                    v.label.as_str().into(),
                    v.value.as_str().into(),
                    v.source.as_str().into(),
                    v.source_label.as_str().into(),
                    v.source_ref.as_deref().into(),
                    v.level.code().into(),
                    v.status.as_str().into(),
                    v.decision_id.as_deref().into(),
                    at.into(),
                    at.into(),
                ]),
        )?;
        crate::repo::must(self.value(&id)?)
    }

    pub fn restore_value(&mut self, row: &AttrValue) -> Result<()> {
        self.0.exec(
            Query::insert()
                .into_table("attr_values")
                .columns(VALUE_COLS)
                .values_panic([
                    row.id.as_str().into(),
                    row.workspace_id.as_str().into(),
                    row.party_id.as_str().into(),
                    row.key.as_str().into(),
                    row.label.as_str().into(),
                    row.value.as_str().into(),
                    row.source.as_str().into(),
                    row.source_label.as_str().into(),
                    row.source_ref.as_deref().into(),
                    row.level.code().into(),
                    row.status.as_str().into(),
                    row.decision_id.as_deref().into(),
                    row.created.into(),
                    row.updated.into(),
                ])
                .on_conflict(OnConflict::column("id").do_nothing().to_owned()),
        )?;
        Ok(())
    }

    pub fn restore_confirmed(&mut self, id: &str) -> Result<()> {
        self.0.exec(
            row_update("attr_values", id)
                .value("status", "confirmed")
                .value("updated", now())
                .and_where(Expr::col("status").eq("stale")),
        )?;
        Ok(())
    }

    pub fn move_value(&mut self, id: &str, from: &str, into: &str) -> Result<()> {
        self.0.exec(
            row_update("attr_values", id)
                .value("party_id", into)
                .value("updated", now())
                .and_where(Expr::col("party_id").eq(from)),
        )?;
        Ok(())
    }

    pub fn set_manual_source(&mut self, id: &str, label: &str) -> Result<()> {
        self.0.exec(
            row_update("attr_values", id)
                .value("source", "me")
                .value("source_label", label),
        )?;
        Ok(())
    }

    pub fn update_value(
        &mut self,
        id: &str,
        value: Option<&str>,
        level: Option<Level>,
        status: Option<&str>,
    ) -> Result<()> {
        let columns = [
            ("value", value.map(Expr::value)),
            ("level", level.map(|level| Expr::value(level.code()))),
            ("status", status.map(Expr::value)),
        ];
        if columns.iter().any(|(_, v)| v.is_some()) {
            self.0.exec(
                row_update("attr_values", id)
                    .values(
                        columns
                            .into_iter()
                            .filter_map(|(key, v)| v.map(|expr| (key, expr))),
                    )
                    .value("updated", now()),
            )?;
        }
        Ok(())
    }

    /// Marks superseded facts stale and returns their identities for undo.
    pub fn stale_siblings(&mut self, v: &AttrValue) -> Result<Vec<String>> {
        let same_scope = |other: &str| {
            fn scope(text: &str) -> Option<(String, String)> {
                let value: serde_json::Value = serde_json::from_str(text).ok()?;
                Some((
                    value.get("kind")?.as_str()?.into(),
                    value.get("id")?.as_str()?.into(),
                ))
            }
            scope(&v.value).is_some_and(|current| Some(current) == scope(other))
        };
        let ids: Vec<_> = self
            .confirmed_siblings(v)?
            .into_iter()
            .filter(|(_, label, text)| match v.key.as_str() {
                "call" | "call_external" => true,
                "call_ext_scope" => same_scope(text),
                _ => label == &v.label,
            })
            .map(|(id, _, _)| id)
            .collect();
        let at = now();
        for id in &ids {
            self.0.exec(
                row_update("attr_values", id)
                    .value("status", "stale")
                    .value("updated", at),
            )?;
        }
        Ok(ids)
    }

    pub fn delete_value(&mut self, id: &str) -> Result<bool> {
        Ok(self.0.exec(
            Query::delete()
                .from_table("attr_values")
                .and_where(Expr::col("id").eq(id)),
        )? != 0)
    }

    pub fn delete_party_values(&mut self, ws: &str, party: &str) -> Result<usize> {
        Ok(self.0.exec(
            Query::delete()
                .from_table("attr_values")
                .and_where(Expr::col("workspace_id").eq(ws))
                .and_where(Expr::col("party_id").eq(party)),
        )? as usize)
    }
}
