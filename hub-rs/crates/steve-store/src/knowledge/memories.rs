use sea_query::{Condition, Expr, ExprTrait, JoinType, OnConflict, Order, Query, SelectStatement};
use steve_types::{ConfigEvent, DomainEvent};

use crate::base::{
    Level, ObjRef, Viewer, Visibility,
    mems::{Mem, MemFilter, MemPatch, NewMem},
};
use crate::contract::Row;
use crate::{Read, Result, W, now, uid};

pub struct Memories<'a, C>(pub &'a mut C);

const COLUMNS: [&str; 16] = [
    "id",
    "workspace_id",
    "kind",
    "text",
    "source_type",
    "source_id",
    "source_label",
    "visibility",
    "level",
    "status",
    "valid_until",
    "writer",
    "decision_id",
    "created_by",
    "created",
    "updated",
];

fn row(r: &Row<'_>) -> Result<Mem> {
    Ok(Mem {
        id: r.get(0)?,
        workspace_id: r.get(1)?,
        kind: r.get(2)?,
        text: r.get(3)?,
        source_type: r.get(4)?,
        source_id: r.get(5)?,
        source_label: r.get(6)?,
        visibility: Visibility::from_db(&r.get::<String>(7)?),
        level: Level::from_db(&r.get::<String>(8)?),
        status: r.get(9)?,
        valid_until: r.get(10)?,
        writer: r.get(11)?,
        decision_id: r.get(12)?,
        created_by: r.get(13)?,
        created: r.get(14)?,
        updated: r.get(15)?,
        about: Vec::new(),
    })
}

fn query(viewer: &Viewer, filter: &MemFilter) -> SelectStatement {
    let mut q = Query::select();
    q.columns(COLUMNS.map(|col| ("m", col)))
        .from_as("mems", "m")
        .and_where(Expr::col(("m", "workspace_id")).eq(viewer.ws.as_str()))
        .order_by(("m", "updated"), Order::Desc)
        .order_by(("m", "id"), Order::Asc);
    if viewer.is_service() {
        q.and_where(Expr::col(("m", "visibility")).eq("workspace"));
    }
    if !filter.include_never {
        q.and_where(Expr::col(("m", "level")).ne("never"));
    }
    if !filter.statuses.is_empty() {
        q.and_where(Expr::col(("m", "status")).is_in(filter.statuses.iter().map(String::as_str)));
    }
    if filter.limit > 0 {
        q.limit(filter.limit as u64);
    }
    q.take()
}

impl<C: Read> Memories<'_, C> {
    pub fn subjects(&mut self, id: &str) -> Result<Vec<ObjRef>> {
        self.0.all(
            Query::select()
                .columns(["object_type", "object_id"])
                .from("mem_about")
                .and_where(Expr::col("mem_id").eq(id))
                .order_by("rowid", Order::Asc),
            |row| {
                Ok(ObjRef {
                    kind: row.get(0)?,
                    id: row.get(1)?,
                })
            },
        )
    }

    pub fn get(&mut self, id: &str) -> Result<Option<Mem>> {
        let found = self.0.one(
            Query::select()
                .columns(COLUMNS)
                .from("mems")
                .and_where(Expr::col("id").eq(id)),
            row,
        )?;
        found
            .map(|mut found| {
                found.about = self.subjects(id)?;
                Ok(found)
            })
            .transpose()
    }

    pub fn visible(&mut self, viewer: &Viewer, id: &str) -> Result<Option<Mem>> {
        Ok(self
            .get(id)?
            .filter(|m| m.workspace_id == viewer.ws && viewer.sees(&m.visibility, &m.created_by)))
    }

    fn list(&mut self, q: &SelectStatement) -> Result<Vec<Mem>> {
        let mut rows = self.0.all(q, row)?;
        for m in &mut rows {
            m.about = self.subjects(&m.id)?;
        }
        Ok(rows)
    }

    pub fn all(&mut self, viewer: &Viewer, filter: &MemFilter) -> Result<Vec<Mem>> {
        self.list(&query(viewer, filter))
    }

    pub fn about(
        &mut self,
        viewer: &Viewer,
        about: &ObjRef,
        filter: &MemFilter,
    ) -> Result<Vec<Mem>> {
        self.list(
            query(viewer, filter)
                .join_as(
                    JoinType::InnerJoin,
                    "mem_about",
                    "a",
                    Expr::col(("a", "mem_id")).equals(("m", "id")),
                )
                .and_where(Expr::col(("a", "workspace_id")).eq(viewer.ws.as_str()))
                .and_where(Expr::col(("a", "object_type")).eq(about.kind.as_str()))
                .and_where(Expr::col(("a", "object_id")).eq(about.id.as_str())),
        )
    }

    pub fn ids(&mut self, ws: &str) -> Result<Vec<String>> {
        self.0.all(
            Query::select()
                .column("id")
                .from("mems")
                .and_where(Expr::col("workspace_id").eq(ws)),
            |row| row.get(0),
        )
    }

    pub fn anchors(
        &mut self,
        viewer: &Viewer,
        anchors: &[ObjRef],
        limit: usize,
        at: i64,
    ) -> Result<Vec<Mem>> {
        if limit == 0 || anchors.is_empty() {
            return Ok(Vec::new());
        }
        let targets = anchors.iter().fold(Condition::any(), |condition, anchor| {
            condition.add(
                Expr::col("object_type")
                    .eq(anchor.kind.as_str())
                    .and(Expr::col("object_id").eq(anchor.id.as_str())),
            )
        });
        let about = Query::select()
            .column("mem_id")
            .from("mem_about")
            .and_where(Expr::col("mem_id").equals(("m", "id")))
            .cond_where(targets)
            .take();
        self.list(
            query(
                viewer,
                &MemFilter {
                    statuses: vec!["confirmed".into()],
                    limit: limit as i64,
                    ..Default::default()
                },
            )
            .and_where(Expr::col(("m", "kind")).is_in(["rule", "preference"]))
            .and_where(
                Expr::col(("m", "valid_until"))
                    .is_null()
                    .or(Expr::col(("m", "valid_until")).gt(at)),
            )
            .and_where(Expr::exists(about)),
        )
    }

    pub fn candidates_before(
        &mut self,
        ws: &str,
        before: i64,
    ) -> Result<Vec<(String, Option<String>)>> {
        let q = Query::select()
            .columns([("m", "id"), ("m", "decision_id")])
            .from_as("mems", "m")
            .join_as(
                JoinType::LeftJoin,
                "mem_candidates",
                "c",
                Expr::col(("c", "mem_id")).equals(("m", "id")),
            )
            .and_where(Expr::col(("m", "workspace_id")).eq(ws))
            .and_where(Expr::col(("m", "status")).eq("candidate"))
            .and_where(Expr::col(("m", "created")).lt(before))
            .and_where(
                Expr::col(("c", "created"))
                    .is_null()
                    .or(Expr::col(("c", "created")).lt(before)),
            )
            .order_by(("m", "created"), Order::Asc)
            .take();
        self.0.all(&q, |row| Ok((row.get(0)?, row.get(1)?)))
    }
}

fn changed(w: &mut W<'_>, ws: &str, id: &str) {
    w.emit(DomainEvent::from(ConfigEvent::Mm {
        id: id.into(),
        ws: ws.into(),
    }));
}

pub struct BotMemories<'a, C>(pub &'a mut C);

impl<C: Read> BotMemories<'_, C> {
    pub fn bot(&mut self, id: &str) -> Result<Option<String>> {
        self.0.one(
            Query::select()
                .column("bot_id")
                .from("bot_memories")
                .and_where(Expr::col("id").eq(id)),
            |r| r.get(0),
        )
    }
}

impl<C: Read> Memories<'_, C> {
    pub fn updated_since(&mut self, ws: &str, since: i64) -> Result<Vec<(String, i64)>> {
        self.0.all(
            Query::select()
                .columns(["id", "updated"])
                .from("mems")
                .and_where(Expr::col("workspace_id").eq(ws))
                .and_where(Expr::col("updated").gt(since))
                .order_by("updated", Order::Asc),
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
    }

    pub fn indexable(&mut self, ws: &str, now: i64) -> Result<Vec<String>> {
        self.0.all(
            Query::select()
                .column("id")
                .from("mems")
                .and_where(Expr::col("workspace_id").eq(ws))
                .and_where(Expr::col("status").eq("confirmed"))
                .and_where(Expr::col("level").ne("never"))
                .and_where(
                    Expr::col("valid_until")
                        .is_null()
                        .or(Expr::col("valid_until").gt(now)),
                ),
            |r| r.get(0),
        )
    }
}

impl<C: Read> BotMemories<'_, C> {
    pub fn in_workspace(&mut self, ws: &str) -> Result<Vec<(String, String, String)>> {
        self.0.all(
            Query::select()
                .columns(["id", "bot_id", "text"])
                .from("bot_memories")
                .and_where(Expr::col("workspace_id").eq(ws))
                .order_by("id", Order::Asc),
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
    }
}

impl Memories<'_, W<'_>> {
    pub fn create(&mut self, ws: &str, m: &NewMem) -> Result<Mem> {
        let id = uid("mm");
        let at = now();
        self.0.exec(
            Query::insert()
                .into_table("mems")
                .columns(COLUMNS)
                .values_panic([
                    id.as_str().into(),
                    ws.into(),
                    m.kind.as_str().into(),
                    m.text.as_str().into(),
                    m.source_type.as_str().into(),
                    m.source_id.as_deref().into(),
                    m.source_label.as_str().into(),
                    m.visibility.code().into(),
                    m.level.code().into(),
                    m.status.as_str().into(),
                    m.valid_until.into(),
                    m.writer.as_str().into(),
                    m.decision_id.as_deref().into(),
                    m.created_by.as_str().into(),
                    at.into(),
                    at.into(),
                ]),
        )?;
        for a in &m.about {
            self.0.exec(
                Query::insert()
                    .into_table("mem_about")
                    .columns(["mem_id", "workspace_id", "object_type", "object_id"])
                    .values_panic([
                        id.as_str().into(),
                        ws.into(),
                        a.kind.as_str().into(),
                        a.id.as_str().into(),
                    ])
                    .on_conflict(
                        OnConflict::columns(["mem_id", "object_type", "object_id"])
                            .do_nothing()
                            .to_owned(),
                    ),
            )?;
        }
        changed(self.0, ws, &id);
        crate::repo::must(self.get(&id)?)
    }

    pub fn patch(&mut self, id: &str, patch: &MemPatch) -> Result<Option<Mem>> {
        let Some(old) = self.get(id)? else {
            return Ok(None);
        };
        let mut q = Query::update();
        q.table("mems").and_where(Expr::col("id").eq(id));
        let fields: [(&str, Option<sea_query::Value>); 6] = [
            ("text", patch.text.as_deref().map(Into::into)),
            ("level", patch.level.map(|level| level.code().into())),
            ("status", patch.status.as_deref().map(Into::into)),
            (
                "visibility",
                patch.visibility.as_ref().map(|v| v.code().into()),
            ),
            (
                "decision_id",
                patch.decision_id.as_ref().map(|id| id.as_deref().into()),
            ),
            ("valid_until", patch.valid_until.map(Into::into)),
        ];
        if fields.iter().all(|(_, value)| value.is_none()) {
            return Ok(Some(old));
        }
        for (field, value) in fields {
            if let Some(value) = value {
                q.value(field, value);
            }
        }
        self.0.exec(q.value("updated", now()))?;
        changed(self.0, &old.workspace_id, id);
        self.get(id)
    }

    pub fn add_subject(&mut self, ws: &str, id: &str, target: &ObjRef) -> Result<()> {
        self.0.exec(
            Query::insert()
                .into_table("mem_about")
                .columns(["mem_id", "workspace_id", "object_type", "object_id"])
                .values_panic([
                    id.into(),
                    ws.into(),
                    target.kind.as_str().into(),
                    target.id.as_str().into(),
                ])
                .on_conflict(
                    OnConflict::columns(["mem_id", "object_type", "object_id"])
                        .do_nothing()
                        .to_owned(),
                ),
        )?;
        Ok(())
    }

    pub fn remove_subject(&mut self, id: &str, target: &ObjRef) -> Result<()> {
        self.0.exec(
            Query::delete()
                .from_table("mem_about")
                .and_where(Expr::col("mem_id").eq(id))
                .and_where(Expr::col("object_type").eq(target.kind.as_str()))
                .and_where(Expr::col("object_id").eq(target.id.as_str())),
        )?;
        Ok(())
    }

    pub fn set_source(
        &mut self,
        id: &str,
        kind: &str,
        source: Option<&str>,
        label: &str,
    ) -> Result<()> {
        self.0.exec(
            Query::update()
                .table("mems")
                .value("source_type", kind)
                .value("source_id", source)
                .value("source_label", label)
                .value("updated", now())
                .and_where(Expr::col("id").eq(id)),
        )?;
        Ok(())
    }

    pub fn delete(&mut self, id: &str) -> Result<bool> {
        let Some(old) = self.get(id)? else {
            return Ok(false);
        };
        self.0.exec(
            Query::delete()
                .from_table("mem_about")
                .and_where(Expr::col("mem_id").eq(id)),
        )?;
        self.0.exec(
            Query::delete()
                .from_table("mems")
                .and_where(Expr::col("id").eq(id)),
        )?;
        changed(self.0, &old.workspace_id, id);
        Ok(true)
    }
}
