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
