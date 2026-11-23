use super::sealed::Q;
use sea_query::{Expr, ExprTrait, OnConflict, Order, Query};

use crate::{Read, Result, W, base::ObjRef, now};

pub struct ProjectRows<'a, C>(pub &'a mut C);
pub struct TagRows<'a, C>(pub &'a mut C);

impl<C: Read> ProjectRows<'_, C> {
    pub fn contains(&mut self, ws: &str, id: &str) -> Result<bool> {
        self.0
            .one(
                Query::select()
                    .column("id")
                    .from("projects")
                    .and_where(Expr::col("workspace_id").eq(ws))
                    .and_where(Expr::col("id").eq(id)),
                |_| Ok(()),
            )
            .map(|row| row.is_some())
    }

    pub fn first_named(&mut self, ws: &str, name: &str) -> Result<Option<String>> {
        self.0.one(
            Query::select()
                .from("projects")
                .column("id")
                .and_where(Expr::col("workspace_id").eq(ws))
                .and_where(Expr::col("name").eq(name))
                .and_where(Expr::col("archived").eq(0_i64))
                .limit(1),
            |row| row.get(0),
        )
    }
}

impl<C: Read> TagRows<'_, C> {
    pub fn of(&mut self, ws: &str, object: &crate::base::ObjRef) -> Result<Vec<String>> {
        self.0.all(
            Query::select()
                .from("entity_tags")
                .column("tag_id")
                .and_where(Expr::col("workspace_id").eq(ws))
                .and_where(Expr::col("object_type").eq(&object.kind))
                .and_where(Expr::col("object_id").eq(&object.id))
                .order_by("created", Order::Asc)
                .order_by("tag_id", Order::Asc),
            |row| row.get(0),
        )
    }

    pub fn contains(&mut self, ws: &str, id: &str) -> Result<bool> {
        self.0
            .one(
                Query::select()
                    .column("id")
                    .from("tags")
                    .and_where(Expr::col("workspace_id").eq(ws))
                    .and_where(Expr::col("id").eq(id)),
                |_| Ok(()),
            )
            .map(|row| row.is_some())
    }
}

impl TagRows<'_, W<'_>> {
    pub fn attach(&mut self, ws: &str, tag: &str, target: &ObjRef) -> Result<()> {
        self.attach_at(ws, tag, target, now())
    }

    fn attach_at(&mut self, ws: &str, tag: &str, target: &ObjRef, at: i64) -> Result<()> {
        self.0.exec(
            Query::insert()
                .into_table("entity_tags")
                .columns([
                    "workspace_id",
                    "tag_id",
                    "object_type",
                    "object_id",
                    "created",
                ])
                .values_panic([
                    ws.into(),
                    tag.into(),
                    target.kind.as_str().into(),
                    target.id.as_str().into(),
                    at.into(),
                ])
                .on_conflict(OnConflict::new().do_nothing().to_owned()),
        )?;
        Ok(())
    }

    pub fn replace(&mut self, ws: &str, target: &ObjRef, ids: &[String]) -> Result<Vec<String>> {
        let mut kept = Vec::new();
        for id in ids {
            if !kept.contains(id)
                && self
                    .0
                    .one(
                        Query::select()
                            .from("tags")
                            .column("id")
                            .and_where(Expr::col("workspace_id").eq(ws))
                            .and_where(Expr::col("id").eq(id))
                            .and_where(Expr::col("cat").ne(crate::kinds::KIND_CAT))
                            .limit(1),
                        |_| Ok(()),
                    )?
                    .is_some()
            {
                kept.push(id.clone());
            }
        }
        for id in self.of(ws, target)? {
            if !kept.contains(&id) {
                self.detach(ws, &id, target)?;
            }
        }
        let created = now();
        for (index, id) in kept.iter().enumerate() {
            self.attach_at(ws, id, target, created + index as i64)?;
        }
        Ok(kept)
    }

    fn detach(&mut self, ws: &str, tag: &str, target: &ObjRef) -> Result<()> {
        self.0.exec(
            Query::delete()
                .from_table("entity_tags")
                .and_where(Expr::col("workspace_id").eq(ws))
                .and_where(Expr::col("object_type").eq(&target.kind))
                .and_where(Expr::col("object_id").eq(&target.id))
                .and_where(Expr::col("tag_id").eq(tag)),
        )?;
        Ok(())
    }

    pub fn move_between(
        &mut self,
        ws: &str,
        from: &ObjRef,
        into: &ObjRef,
        only: Option<&[String]>,
    ) -> Result<Vec<String>> {
        let tags: Vec<_> = self
            .of(ws, from)?
            .into_iter()
            .filter(|tag| only.is_none_or(|ids| ids.contains(tag)))
            .collect();
        for tag in &tags {
            self.attach(ws, tag, into)?;
            self.detach(ws, tag, from)?;
        }
        Ok(tags)
    }
}
