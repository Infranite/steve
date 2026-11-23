use sea_query::{Expr, ExprTrait, Func, Query, SelectStatement};
use serde_json::Value;

use crate::{Read, Result, W, now};

pub struct Revisions<'a, C>(pub &'a mut C);

fn object(ws: &str, kind: &str, id: &str) -> SelectStatement {
    Query::select()
        .from("revisions")
        .and_where(Expr::col("workspace_id").eq(ws))
        .and_where(Expr::col("object_type").eq(kind))
        .and_where(Expr::col("object_id").eq(id))
        .to_owned()
}

impl<C: Read> Revisions<'_, C> {
    pub fn field_written(
        &mut self,
        ws: &str,
        kind: &str,
        id: &str,
        path: &'static str,
        sync: bool,
    ) -> Result<bool> {
        let dialect = self.0.db().dialect();
        let patch = Expr::col("patch");
        let origin: Expr = Func::coalesce([
            dialect.json_path(patch.clone(), "$.after.source"),
            Expr::value(""),
        ])
        .into();
        let origin = if sync {
            origin.eq("sync")
        } else {
            origin.ne("sync")
        };
        Ok(self
            .0
            .one(
                object(ws, kind, id)
                    .column("id")
                    .and_where(dialect.json_valid(patch.clone()))
                    .and_where(dialect.json_type(patch, path).is_not_null())
                    .and_where(origin)
                    .limit(1),
                |_| Ok(()),
            )?
            .is_some())
    }

    pub fn job_patches(&mut self, ws: &str, id: &str, org: &str) -> Result<Vec<Value>> {
        let dialect = self.0.db().dialect();
        let patch = Expr::col("patch");
        self.0.all(
            object(ws, "party", id)
                .column("patch")
                .and_where(dialect.json_valid(patch.clone()))
                .and_where(dialect.json_path(patch, "$.after.job.org_id").eq(org)),
            |row| Ok(row.get::<super::Json<Value>>(0)?.0),
        )
    }

    pub fn removed_attribute(&mut self, ws: &str, id: &str, key: &str) -> Result<bool> {
        let dialect = self.0.db().dialect();
        let patch = Expr::col("patch");
        Ok(self
            .0
            .one(
                object(ws, "party", id)
                    .column("id")
                    .and_where(dialect.json_valid(patch.clone()))
                    .and_where(
                        dialect
                            .json_path(patch.clone(), "$.before.attr.key")
                            .eq(key),
                    )
                    .and_where(dialect.json_type(patch, "$.after.attr").eq("null"))
                    .limit(1),
                |_| Ok(()),
            )?
            .is_some())
    }

    pub fn count(&mut self, ws: &str, kind: &str, id: &str) -> Result<i64> {
        Ok(self
            .0
            .one(
                object(ws, kind, id).expr(Func::count(Expr::col("id"))),
                |row| row.get(0),
            )?
            .unwrap_or_default())
    }
}

impl Revisions<'_, W<'_>> {
    #[allow(clippy::too_many_arguments)]
    pub fn append(
        &mut self,
        ws: &str,
        kind: &str,
        id: &str,
        version: i64,
        patch: &Value,
        author: &str,
    ) -> Result<()> {
        self.0.exec(
            Query::insert()
                .into_table("revisions")
                .columns([
                    "workspace_id",
                    "object_type",
                    "object_id",
                    "version",
                    "patch",
                    "author",
                    "created",
                ])
                .values_panic([
                    ws.into(),
                    kind.into(),
                    id.into(),
                    version.into(),
                    patch.to_string().into(),
                    author.into(),
                    now().into(),
                ]),
        )?;
        Ok(())
    }
}
