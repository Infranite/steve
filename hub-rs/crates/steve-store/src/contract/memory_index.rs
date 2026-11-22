use crate::contract::Q;
use sea_query::{Expr, ExprTrait, OnConflict, Order, Query};
use serde::{Deserialize, Serialize};
use steve_types::{ConfigEvent, DomainEvent};

use crate::base::{ObjRef, Viewer, Visibility};
use crate::{R, Read, Result, W};

/// The message range a memory rests on.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Range {
    pub conv: String,
    pub first_id: String,
    pub last_id: String,
    pub first_at: i64,
    pub last_at: i64,
}

/// What indexing added (so undo removes exactly that).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Added {
    pub mem: String,
    /// `(type, id)` rows of `mem_about` that were not there.
    pub about: Vec<(String, String)>,
    pub range: Option<Range>,
}

#[derive(Debug, Serialize)]
pub struct IndexedRange {
    #[serde(flatten)]
    pub range: Range,
    pub packet_id: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct Index {
    pub about: Vec<ObjRef>,
    pub ranges: Vec<IndexedRange>,
}

pub struct MemoryIndex<'a, C>(&'a mut C);

impl R<'_> {
    pub fn memory_index(&mut self) -> MemoryIndex<'_, Self> {
        MemoryIndex(self)
    }
}

impl W<'_> {
    pub fn memory_index(&mut self) -> MemoryIndex<'_, Self> {
        MemoryIndex(self)
    }
}

fn memory(ws: &str, mem: &str) -> Expr {
    Expr::col("workspace_id")
        .eq(ws)
        .and(Expr::col("id").eq(mem))
}

fn reference(ws: &str, mem: &str) -> Expr {
    Expr::col("workspace_id")
        .eq(ws)
        .and(Expr::col("mem_id").eq(mem))
}

impl<C: Read> MemoryIndex<'_, C> {
    pub fn exists(&mut self, ws: &str, mem: &str) -> Result<bool> {
        Ok(self
            .0
            .one(
                Query::select()
                    .column("id")
                    .from("mems")
                    .and_where(memory(ws, mem)),
                |_| Ok(()),
            )?
            .is_some())
    }

    pub fn visible(&mut self, viewer: &Viewer, mem: &str) -> Result<bool> {
        Ok(self
            .0
            .one(
                Query::select()
                    .columns(["visibility", "created_by"])
                    .from("mems")
                    .and_where(memory(&viewer.ws, mem)),
                |row| {
                    let visibility: String = row.get(0)?;
                    let author: String = row.get(1)?;
                    Ok(viewer.sees(&Visibility::from_db(&visibility), &author))
                },
            )?
            .unwrap_or(false))
    }

    /// Reads the index once the caller has checked visibility in this snapshot.
    pub fn get(&mut self, ws: &str, mem: &str) -> Result<Index> {
        let about = self.0.all(
            Query::select()
                .columns(["object_type", "object_id"])
                .from("mem_about")
                .and_where(reference(ws, mem))
                .order_by("object_type", Order::Asc)
                .order_by("object_id", Order::Asc),
            |r| {
                Ok(ObjRef {
                    kind: r.get(0)?,
                    id: r.get(1)?,
                })
            },
        )?;
        let ranges = self.0.all(
            Query::select()
                .columns([
                    "conv",
                    "first_id",
                    "last_id",
                    "first_at",
                    "last_at",
                    "packet_id",
                ])
                .from("mem_ranges")
                .and_where(reference(ws, mem))
                .order_by("first_at", Order::Asc)
                .order_by("conv", Order::Asc)
                .order_by("first_id", Order::Asc),
            |r| {
                Ok(IndexedRange {
                    range: Range {
                        conv: r.get(0)?,
                        first_id: r.get(1)?,
                        last_id: r.get(2)?,
                        first_at: r.get(3)?,
                        last_at: r.get(4)?,
                    },
                    packet_id: r.get(5)?,
                })
            },
        )?;
        Ok(Index { about, ranges })
    }

    /// A merge may retarget a link, or later indexing may rely on the original conversation link.
    pub fn can_unlink(&mut self, ws: &str, a: &Added) -> Result<bool> {
        let current = self.get(ws, &a.mem)?;
        let about_unchanged = a.about.iter().all(|(kind, id)| {
            current.about.iter().any(|r| &r.kind == kind && &r.id == id)
                && !current.ranges.iter().any(|r| {
                    r.range.conv == format!("{kind}:{id}")
                        && a.range.as_ref().is_none_or(|original| {
                            original.conv != r.range.conv || original.first_id != r.range.first_id
                        })
                })
        });
        Ok(about_unchanged
            && a.range
                .as_ref()
                .is_none_or(|original| current.ranges.iter().any(|r| &r.range == original)))
    }
}

impl MemoryIndex<'_, W<'_>> {
    pub fn add(
        &mut self,
        ws: &str,
        mem: &str,
        about: Vec<(String, String)>,
        range: Option<&Range>,
        packet: Option<&str>,
        now: i64,
    ) -> Result<Added> {
        let mut added = Added {
            mem: mem.into(),
            ..Default::default()
        };
        for (kind, id) in about {
            let changed = self.0.exec(
                Query::insert()
                    .into_table("mem_about")
                    .columns(["mem_id", "workspace_id", "object_type", "object_id"])
                    .values_panic([
                        mem.into(),
                        ws.into(),
                        kind.as_str().into(),
                        id.as_str().into(),
                    ])
                    .on_conflict(
                        OnConflict::columns(["mem_id", "object_type", "object_id"])
                            .do_nothing()
                            .to_owned(),
                    ),
            )?;
            if changed != 0 {
                added.about.push((kind, id));
            }
        }
        if let Some(r) = range {
            let changed = self.0.exec(
                Query::insert()
                    .into_table("mem_ranges")
                    .columns([
                        "mem_id",
                        "workspace_id",
                        "conv",
                        "first_id",
                        "last_id",
                        "first_at",
                        "last_at",
                        "packet_id",
                    ])
                    .values_panic([
                        mem.into(),
                        ws.into(),
                        r.conv.as_str().into(),
                        r.first_id.as_str().into(),
                        r.last_id.as_str().into(),
                        r.first_at.into(),
                        r.last_at.into(),
                        packet.into(),
                    ])
                    .on_conflict(
                        OnConflict::columns(["mem_id", "conv", "first_id"])
                            .do_nothing()
                            .to_owned(),
                    ),
            )?;
            if changed != 0 {
                added.range = Some(r.clone());
            }
        }
        self.0.exec(
            Query::update()
                .table("mems")
                .value("updated", now)
                .and_where(memory(ws, mem)),
        )?;
        emit(self.0, ws, mem);
        Ok(added)
    }

    /// Removes the original links after `can_unlink` succeeds in the same transaction.
    pub fn unlink(&mut self, ws: &str, a: &Added) -> Result<()> {
        for (kind, id) in &a.about {
            self.0.exec(
                Query::delete()
                    .from_table("mem_about")
                    .and_where(reference(ws, &a.mem))
                    .and_where(Expr::col("object_type").eq(kind.as_str()))
                    .and_where(Expr::col("object_id").eq(id.as_str())),
            )?;
        }
        if let Some(r) = &a.range {
            self.0.exec(
                Query::delete()
                    .from_table("mem_ranges")
                    .and_where(reference(ws, &a.mem))
                    .and_where(Expr::col("conv").eq(r.conv.as_str()))
                    .and_where(Expr::col("first_id").eq(r.first_id.as_str())),
            )?;
        }
        emit(self.0, ws, &a.mem);
        Ok(())
    }
}

fn emit(w: &mut W<'_>, ws: &str, mem: &str) {
    w.emit(DomainEvent::from(ConfigEvent::Mm {
        id: mem.into(),
        ws: ws.into(),
    }));
}

#[derive(Serialize)]
pub struct VisibleMemory {
    pub id: String,
    pub text: String,
    pub status: String,
}

fn memory_columns() -> [(&'static str, &'static str); 3] {
    [("mems", "id"), ("mems", "text"), ("mems", "status")]
}

fn memory_row(r: &super::Row<'_>) -> Result<VisibleMemory> {
    Ok(VisibleMemory {
        id: r.get(0)?,
        text: r.get(1)?,
        status: r.get(2)?,
    })
}

fn visible_query(viewer: &Viewer) -> sea_query::SelectStatement {
    let mut q = Query::select();
    q.columns(memory_columns())
        .from("mems")
        .and_where(Expr::col(("mems", "workspace_id")).eq(viewer.ws.as_str()))
        .and_where(Expr::col(("mems", "status")).is_in(["confirmed", "candidate"]));
    if viewer.is_service() {
        q.and_where(Expr::col(("mems", "visibility")).eq("workspace"));
    }
    q
}

impl<C: Read> MemoryIndex<'_, C> {
    pub fn new(handle: &mut C) -> MemoryIndex<'_, C> {
        MemoryIndex(handle)
    }

    pub fn about(
        &mut self,
        viewer: &Viewer,
        obj: &ObjRef,
        limit: u64,
    ) -> Result<Vec<VisibleMemory>> {
        self.0.all(
            visible_query(viewer)
                .inner_join(
                    "mem_about",
                    Expr::col(("mem_about", "mem_id")).equals(("mems", "id")),
                )
                .and_where(Expr::col(("mem_about", "workspace_id")).eq(viewer.ws.as_str()))
                .and_where(Expr::col(("mem_about", "object_type")).eq(obj.kind.as_str()))
                .and_where(Expr::col(("mem_about", "object_id")).eq(obj.id.as_str()))
                .order_by(("mems", "updated"), Order::Desc)
                .limit(limit),
            memory_row,
        )
    }

    pub fn search(
        &mut self,
        viewer: &Viewer,
        text: &str,
        now: i64,
        limit: u64,
    ) -> Result<Vec<(VisibleMemory, f64)>> {
        let dialect = self.0.db().dialect();
        let score = dialect.fulltext_score("kb_fts");
        self.0.all(
            visible_query(viewer)
                .expr(score.clone())
                .inner_join(
                    "kb_chunks",
                    Expr::col(("kb_chunks", "obj_id")).equals(("mems", "id")),
                )
                .inner_join(
                    "kb_fts",
                    Expr::col(("kb_fts", "rowid")).equals(("kb_chunks", "id")),
                )
                .and_where(Expr::col(("kb_chunks", "obj_type")).eq("mem"))
                .and_where(Expr::col(("mems", "level")).ne("never"))
                .and_where(
                    Expr::col(("mems", "valid_until")).is_null().or(Expr::col((
                        "mems",
                        "valid_until",
                    ))
                    .gt(now)),
                )
                .and_where(dialect.fulltext_match("kb_fts", text))
                .order_by_expr(score.into(), Order::Asc)
                .limit(limit),
            |r| Ok((memory_row(r)?, r.get(3)?)),
        )
    }
}

impl MemoryIndex<'_, W<'_>> {
    pub fn retarget(&mut self, ws: &str, kind: &str, from: &str, into: &str) -> Result<()> {
        let source = Expr::col("workspace_id")
            .eq(ws)
            .and(Expr::col("object_type").eq(kind))
            .and(Expr::col("object_id").eq(from));
        let ids = self.0.all(
            Query::select()
                .column("mem_id")
                .from("mem_about")
                .and_where(source.clone()),
            |r| r.get::<String>(0),
        )?;
        for id in ids {
            self.0.exec(
                Query::insert()
                    .into_table("mem_about")
                    .columns(["mem_id", "workspace_id", "object_type", "object_id"])
                    .values_panic([id.into(), ws.into(), kind.into(), into.into()])
                    .on_conflict(
                        OnConflict::columns(["mem_id", "object_type", "object_id"])
                            .do_nothing()
                            .to_owned(),
                    ),
            )?;
        }
        self.0
            .exec(Query::delete().from_table("mem_about").and_where(source))?;
        Ok(())
    }
}

impl<C: Read> MemoryIndex<'_, C> {
    pub fn visible_status(&mut self, viewer: &Viewer, id: &str) -> Result<Option<String>> {
        let mut q = Query::select();
        q.column("status")
            .from("mems")
            .and_where(memory(&viewer.ws, id));
        if viewer.is_service() {
            q.and_where(Expr::col("visibility").eq("workspace"));
        }
        self.0.one(&q, |r| r.get(0))
    }

    /// Outer `None` means the memory is not awaiting an answer.
    pub fn candidate_decision(&mut self, ws: &str, id: &str) -> Result<Option<Option<String>>> {
        self.0.one(
            Query::select()
                .column("decision_id")
                .from("mems")
                .and_where(memory(ws, id))
                .and_where(Expr::col("status").eq("candidate")),
            |r| r.get(0),
        )
    }
}

impl MemoryIndex<'_, W<'_>> {
    pub fn expire_candidate(&mut self, ws: &str, id: &str) -> Result<()> {
        let changed = self.0.exec(
            Query::update()
                .table("mems")
                .value("status", "stale")
                .value("updated", crate::now())
                .and_where(memory(ws, id))
                .and_where(Expr::col("status").eq("candidate")),
        )?;
        if changed != 0 {
            emit(self.0, ws, id);
        }
        Ok(())
    }
}
