use sea_query::{
    Expr, ExprTrait, Func, JoinType, OnConflict, Order, Query, SelectStatement, Value as SqlValue,
};
use serde_json::{Value, json};

use crate::base::{Level, ObjRef, Viewer, Visibility};
use crate::contract::Row;
use crate::knowledge::memory_files::{SourceRole, WriteMode};
use crate::{Read, Result, W};

const BUILTIN_KINDS: [&str; 4] = ["notes", "files", "mems", "bot_mems"];

pub struct Documents<'a, C>(pub &'a mut C);

/// A `kb_sources` row.
#[derive(Debug, Clone, PartialEq)]
pub struct SourceRow {
    pub id: String,
    pub workspace_id: String,
    pub kind: String,
    pub name: String,
    pub config: Value,
    pub visibility: Visibility,
    pub level: Level,
    pub paused: bool,
    pub status: String,
    pub error: Option<String>,
    pub stats: Value,
    pub last_sync: Option<i64>,
    pub revision: i64,
    pub created_by: String,
    pub created: i64,
    pub updated: i64,
    pub role: SourceRole,
    pub write_mode: WriteMode,
}

impl SourceRow {
    fn from_row(r: &Row<'_>) -> Result<Self> {
        Ok(Self {
            id: r.get(0)?,
            workspace_id: r.get(1)?,
            kind: r.get(2)?,
            name: r.get(3)?,
            config: json_or(r.get(4)?, json!({})),
            visibility: Visibility::from_db(&r.get::<String>(5)?),
            level: Level::from_db(&r.get::<String>(6)?),
            paused: r.get::<i64>(7)? != 0,
            status: r.get(8)?,
            error: r.get(9)?,
            stats: json_or(r.get(10)?, json!({})),
            last_sync: r.get(11)?,
            revision: r.get(12)?,
            created_by: r.get(13)?,
            created: r.get(14)?,
            updated: r.get(15)?,
            role: SourceRole::from_db(&r.get::<String>(16)?),
            write_mode: WriteMode::from_db(&r.get::<String>(17)?),
        })
    }
}

/// A `kb_docs` row.
#[derive(Debug, Clone, PartialEq)]
pub struct DocRow {
    pub id: String,
    pub workspace_id: String,
    pub source_id: String,
    pub r#ref: String,
    pub title: String,
    pub mime: String,
    pub body: Option<String>,
    pub file_id: Option<String>,
    pub sha256: String,
    pub size: i64,
    pub mtime: Option<i64>,
    /// `None` = follows the source.
    pub visibility: Option<Visibility>,
    pub level: Option<Level>,
    pub tags: Vec<String>,
    pub status: String,
    pub error: Option<String>,
    pub chunks: i64,
    pub created_by: String,
    pub created: i64,
    pub updated: i64,
    pub indexed: Option<i64>,
}

impl DocRow {
    fn from_row(r: &Row<'_>) -> Result<Self> {
        Ok(Self {
            id: r.get(0)?,
            workspace_id: r.get(1)?,
            source_id: r.get(2)?,
            r#ref: r.get(3)?,
            title: r.get(4)?,
            mime: r.get(5)?,
            body: r.get(6)?,
            file_id: r.get(7)?,
            sha256: r.get(8)?,
            size: r.get(9)?,
            mtime: r.get(10)?,
            visibility: r
                .get::<Option<String>>(11)?
                .map(|v| Visibility::from_db(&v)),
            level: r.get::<Option<String>>(12)?.map(|v| Level::from_db(&v)),
            tags: serde_json::from_str(&r.get::<String>(13)?).unwrap_or_default(),
            status: r.get(14)?,
            error: r.get(15)?,
            chunks: r.get(16)?,
            created_by: r.get(17)?,
            created: r.get(18)?,
            updated: r.get(19)?,
            indexed: r.get(20)?,
        })
    }

    /// Effective visibility: the document's own, else the source's.
    pub fn eff_visibility(&self, s: &SourceRow) -> Visibility {
        self.visibility
            .clone()
            .unwrap_or_else(|| s.visibility.clone())
    }

    pub fn eff_level(&self, s: &SourceRow) -> Level {
        self.level.unwrap_or(s.level)
    }
}

fn json_or(s: String, d: Value) -> Value {
    serde_json::from_str(&s).unwrap_or(d)
}

/// A new document (status `active`, not yet indexed).
#[derive(Debug, Clone, Default)]
pub struct NewDoc {
    pub id: String,
    pub source_id: String,
    pub r#ref: String,
    pub title: String,
    pub mime: String,
    pub body: Option<String>,
    pub file_id: Option<String>,
    pub size: i64,
    pub mtime: Option<i64>,
    pub visibility: Option<Visibility>,
    pub level: Option<Level>,
    pub tags: Vec<String>,
    pub created_by: String,
}

/// Filters of the document list.
#[derive(Debug, Clone, Default)]
pub struct DocFilter {
    pub source: Option<String>,
    /// Matches title and ref (path), case-insensitive substring.
    pub q: Option<String>,
    /// Only documents updated before this (paging, newest first).
    pub before: Option<i64>,
    pub limit: i64,
}

const SOURCE_COLUMNS: [&str; 18] = [
    "id",
    "workspace_id",
    "kind",
    "name",
    "config",
    "visibility",
    "level",
    "paused",
    "status",
    "error",
    "stats",
    "last_sync",
    "revision",
    "created_by",
    "created",
    "updated",
    "role",
    "write_mode",
];
const DOC_COLUMNS: [&str; 21] = [
    "id",
    "workspace_id",
    "source_id",
    "ref",
    "title",
    "mime",
    "body",
    "file_id",
    "sha256",
    "size",
    "mtime",
    "visibility",
    "level",
    "tags",
    "status",
    "error",
    "chunks",
    "created_by",
    "created",
    "updated",
    "indexed",
];

const SOURCES: &str = "kb_sources";
const DOCS: &str = "kb_docs";

fn source_projection() -> SelectStatement {
    Query::select().columns(SOURCE_COLUMNS).from(SOURCES).take()
}

fn doc_projection() -> SelectStatement {
    Query::select().columns(DOC_COLUMNS).from(DOCS).take()
}

fn update(
    w: &mut W<'_>,
    table: &'static str,
    id: &str,
    fields: impl IntoIterator<Item = (&'static str, SqlValue)>,
) -> Result<()> {
    w.exec(
        Query::update()
            .table(table)
            .values(
                fields
                    .into_iter()
                    .map(|(field, value)| (field, Expr::from(value))),
            )
            .and_where(Expr::col("id").eq(id)),
    )?;
    Ok(())
}

fn remove(w: &mut W<'_>, table: &'static str, field: &'static str, id: &str) -> Result<()> {
    w.exec(
        Query::delete()
            .from_table(table)
            .and_where(Expr::col(field).eq(id)),
    )?;
    Ok(())
}

impl<C: Read> Documents<'_, C> {
    pub fn source(&mut self, id: &str) -> Result<Option<SourceRow>> {
        self.0.one(
            source_projection().and_where(Expr::col("id").eq(id)),
            SourceRow::from_row,
        )
    }

    pub fn source_of_kind(&mut self, ws: &str, kind: &str) -> Result<Option<SourceRow>> {
        self.0.one(
            source_projection()
                .and_where(Expr::col("workspace_id").eq(ws))
                .and_where(Expr::col("kind").eq(kind))
                .order_by("created", Order::Asc)
                .order_by("id", Order::Asc)
                .limit(1),
            SourceRow::from_row,
        )
    }

    pub fn sources(&mut self, ws: &str) -> Result<Vec<SourceRow>> {
        let mut rows = self.0.all(
            source_projection()
                .and_where(Expr::col("workspace_id").eq(ws))
                .order_by("created", Order::Asc)
                .order_by("id", Order::Asc),
            SourceRow::from_row,
        )?;
        rows.sort_by_key(|row| {
            BUILTIN_KINDS
                .iter()
                .position(|kind| *kind == row.kind)
                .unwrap_or(BUILTIN_KINDS.len())
        });
        Ok(rows)
    }

    pub fn has_builtins(&mut self, ws: &str) -> Result<bool> {
        let present = self.0.all(
            Query::select()
                .distinct()
                .column("kind")
                .from(SOURCES)
                .and_where(Expr::col("workspace_id").eq(ws))
                .and_where(Expr::col("kind").is_in(BUILTIN_KINDS)),
            |r| r.get::<String>(0),
        )?;
        Ok(present.len() == BUILTIN_KINDS.len())
    }

    /// Counts of active documents and chunks. Memory sources count their indexed entries.
    pub fn source_counts(&mut self, source: &SourceRow) -> Result<(i64, i64)> {
        let obj = match source.kind.as_str() {
            "mems" => Some("mem"),
            "bot_mems" => Some("bot_mem"),
            _ => None,
        };
        if let Some(kind) = obj {
            let count = self
                .0
                .one(
                    Query::select()
                        .expr(Func::count(Expr::col("id")))
                        .from("kb_chunks")
                        .and_where(Expr::col("workspace_id").eq(source.workspace_id.as_str()))
                        .and_where(Expr::col("obj_type").eq(kind)),
                    |row| row.get(0),
                )?
                .unwrap_or(0);
            return Ok((count, count));
        }
        Ok(self
            .0
            .one(
                Query::select()
                    .expr(Func::count(Expr::col("id")))
                    .expr(Func::sum(Expr::col("chunks")))
                    .from(DOCS)
                    .and_where(Expr::col("source_id").eq(source.id.as_str()))
                    .and_where(Expr::col("status").eq("active")),
                |row| Ok((row.get(0)?, row.get::<Option<i64>>(1)?.unwrap_or(0))),
            )?
            .unwrap_or_default())
    }

    pub fn paused(&mut self, ws: &str) -> Result<Vec<(String, String)>> {
        self.0.all(
            Query::select()
                .columns(["id", "kind"])
                .from(SOURCES)
                .and_where(Expr::col("workspace_id").eq(ws))
                .and_where(Expr::col("paused").eq(1_i64)),
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
    }

    pub fn doc(&mut self, id: &str) -> Result<Option<DocRow>> {
        self.0.one(
            doc_projection().and_where(Expr::col("id").eq(id)),
            DocRow::from_row,
        )
    }

    pub fn visible_doc(
        &mut self,
        viewer: &Viewer,
        id: &str,
    ) -> Result<Option<(DocRow, SourceRow)>> {
        let Some(doc) = self
            .doc(id)?
            .filter(|d| d.workspace_id == viewer.ws && d.status != "deleted")
        else {
            return Ok(None);
        };
        Ok(self
            .source(&doc.source_id)?
            .filter(|source| viewer.sees(&doc.eff_visibility(source), &doc.created_by))
            .map(|source| (doc, source)))
    }

    pub fn doc_by_ref(&mut self, source: &str, reference: &str) -> Result<Option<DocRow>> {
        self.0.one(
            doc_projection()
                .and_where(Expr::col("source_id").eq(source))
                .and_where(Expr::col("ref").eq(reference)),
            DocRow::from_row,
        )
    }

    pub fn doc_about(&mut self, id: &str) -> Result<Vec<ObjRef>> {
        self.0.all(
            Query::select()
                .columns(["object_type", "object_id"])
                .from("kb_doc_about")
                .and_where(Expr::col("doc_id").eq(id))
                .order_by("rowid", Order::Asc),
            |row| {
                Ok(ObjRef {
                    kind: row.get(0)?,
                    id: row.get(1)?,
                })
            },
        )
    }

    pub fn docs_visible(&mut self, viewer: &Viewer, filter: &DocFilter) -> Result<Vec<DocRow>> {
        let mut q = Query::select();
        q.columns(DOC_COLUMNS.map(|col| ("d", col)))
            .from_as(DOCS, "d")
            .join_as(
                JoinType::InnerJoin,
                SOURCES,
                "s",
                Expr::col(("s", "id")).equals(("d", "source_id")),
            )
            .and_where(Expr::col(("d", "workspace_id")).eq(viewer.ws.as_str()))
            .and_where(Expr::col(("d", "status")).ne("deleted"));
        if viewer.is_service() {
            let visibility: Expr = Func::coalesce([
                Expr::col(("d", "visibility")),
                Expr::col(("s", "visibility")),
            ])
            .into();
            q.and_where(visibility.eq("workspace"));
        }
        if let Some(source) = &filter.source {
            q.and_where(Expr::col(("d", "source_id")).eq(source.as_str()));
        }
        if let Some(text) = filter.q.as_deref().filter(|text| !text.is_empty()) {
            let contains = |col| {
                Expr::from(Func::cust("instr").args([
                    Func::lower(Expr::col(("d", col))).into(),
                    Func::lower(Expr::value(text)).into(),
                ]))
                .gt(0_i64)
            };
            q.and_where(contains("title").or(contains("ref")));
        }
        if let Some(before) = filter.before {
            q.and_where(Expr::col(("d", "updated")).lt(before));
        }
        q.order_by(("d", "updated"), Order::Desc)
            .order_by(("d", "id"), Order::Desc)
            .limit(if filter.limit > 0 {
                filter.limit as u64
            } else {
                50
            });
        self.0.all(&q, DocRow::from_row)
    }

    pub fn doc_chunks(&mut self, id: &str, limit: i64) -> Result<Vec<(i64, String, String)>> {
        let mut q = Query::select();
        q.columns(["seq", "heading", "text"])
            .from("kb_chunks")
            .and_where(Expr::col("obj_type").eq("chunk"))
            .and_where(Expr::col("obj_id").eq(id))
            .order_by("seq", Order::Asc);
        if limit >= 0 {
            q.limit(limit as u64);
        }
        self.0
            .all(&q, |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
    }

    pub fn uses_of(&mut self, ws: &str, id: &str) -> Result<i64> {
        let pattern = format!("%\"obj_id\":{}%", serde_json::to_string(id)?);
        Ok(self
            .0
            .one(
                Query::select()
                    .expr(Func::count(Expr::col("id")))
                    .from("kb_uses")
                    .and_where(Expr::col("workspace_id").eq(ws))
                    .and_where(Expr::col("hits").like(pattern)),
                |row| row.get(0),
            )?
            .unwrap_or(0))
    }

    pub fn mark(&mut self, ws: &str, name: &str) -> Result<Option<String>> {
        self.0.one(
            Query::select()
                .column("value")
                .from("kb_marks")
                .and_where(Expr::col("workspace_id").eq(ws))
                .and_where(Expr::col("name").eq(name)),
            |row| row.get(0),
        )
    }

    pub fn folder_sources(&mut self, ws: Option<&str>) -> Result<Vec<SourceRow>> {
        let mut q = source_projection();
        q.and_where(Expr::col("kind").eq("folder"))
            .order_by("created", Order::Asc)
            .order_by("id", Order::Asc);
        if let Some(ws) = ws {
            q.and_where(Expr::col("workspace_id").eq(ws));
        }
        self.0.all(&q, SourceRow::from_row)
    }

    pub fn memory_folder_sources(&mut self, ws: &str) -> Result<Vec<SourceRow>> {
        self.0.all(
            source_projection()
                .and_where(Expr::col("workspace_id").eq(ws))
                .and_where(Expr::col("kind").eq("folder"))
                .and_where(Expr::col("role").eq(SourceRole::Memory.code()))
                .order_by("created", Order::Asc)
                .order_by("id", Order::Asc),
            SourceRow::from_row,
        )
    }

    pub fn docs_of_source(&mut self, source: &str) -> Result<Vec<DocRow>> {
        self.0.all(
            doc_projection()
                .and_where(Expr::col("source_id").eq(source))
                .order_by("ref", Order::Asc),
            DocRow::from_row,
        )
    }
}

impl Documents<'_, W<'_>> {
    #[allow(clippy::too_many_arguments)]
    pub fn insert_source(
        &mut self,
        id: &str,
        ws: &str,
        kind: &str,
        name: &str,
        config: &Value,
        visibility: &Visibility,
        level: Level,
        role: SourceRole,
        write_mode: WriteMode,
        by: &str,
        now: i64,
    ) -> Result<()> {
        self.0.exec(
            Query::insert()
                .into_table(SOURCES)
                .columns([
                    "id",
                    "workspace_id",
                    "kind",
                    "name",
                    "config",
                    "visibility",
                    "level",
                    "role",
                    "write_mode",
                    "created_by",
                    "created",
                    "updated",
                ])
                .values_panic([
                    id.into(),
                    ws.into(),
                    kind.into(),
                    name.into(),
                    config.to_string().into(),
                    visibility.code().into(),
                    level.code().into(),
                    role.code().into(),
                    write_mode.code().into(),
                    by.into(),
                    now.into(),
                    now.into(),
                ]),
        )?;
        Ok(())
    }

    pub fn insert_doc(&mut self, ws: &str, doc: &NewDoc, now: i64) -> Result<()> {
        let mime = if doc.mime.is_empty() {
            "text/markdown"
        } else {
            doc.mime.as_str()
        };
        self.0.exec(
            Query::insert()
                .into_table(DOCS)
                .columns([
                    "id",
                    "workspace_id",
                    "source_id",
                    "ref",
                    "title",
                    "mime",
                    "body",
                    "file_id",
                    "size",
                    "mtime",
                    "visibility",
                    "level",
                    "tags",
                    "created_by",
                    "created",
                    "updated",
                ])
                .values_panic([
                    doc.id.as_str().into(),
                    ws.into(),
                    doc.source_id.as_str().into(),
                    doc.r#ref.as_str().into(),
                    doc.title.as_str().into(),
                    mime.into(),
                    doc.body.as_deref().into(),
                    doc.file_id.as_deref().into(),
                    doc.size.into(),
                    doc.mtime.into(),
                    doc.visibility.as_ref().map(Visibility::code).into(),
                    doc.level.map(Level::code).into(),
                    serde_json::to_string(&doc.tags)?.into(),
                    doc.created_by.as_str().into(),
                    now.into(),
                    now.into(),
                ]),
        )?;
        Ok(())
    }

    pub fn set_doc_text(&mut self, id: &str, title: &str, body: &str, now: i64) -> Result<()> {
        update(
            self.0,
            DOCS,
            id,
            [
                ("title", title.into()),
                ("body", body.into()),
                ("size", (body.len() as i64).into()),
                ("updated", now.into()),
            ],
        )
    }

    pub fn set_doc_scope(
        &mut self,
        id: &str,
        visibility: Option<&Visibility>,
        level: Option<Level>,
        now: i64,
    ) -> Result<()> {
        update(
            self.0,
            DOCS,
            id,
            [
                ("visibility", visibility.map(Visibility::code).into()),
                ("level", level.map(Level::code).into()),
                ("updated", now.into()),
            ],
        )
    }

    pub fn set_doc_indexed(
        &mut self,
        id: &str,
        hash: &str,
        chunks: i64,
        status: &str,
        error: Option<&str>,
        now: i64,
    ) -> Result<()> {
        update(
            self.0,
            DOCS,
            id,
            [
                ("sha256", hash.into()),
                ("chunks", chunks.into()),
                ("status", status.into()),
                ("error", error.into()),
                ("indexed", now.into()),
            ],
        )
    }

    pub fn delete_doc_row(&mut self, id: &str) -> Result<()> {
        remove(self.0, "kb_doc_about", "doc_id", id)?;
        remove(self.0, DOCS, "id", id)
    }

    pub fn set_doc_about(&mut self, ws: &str, id: &str, about: &[ObjRef]) -> Result<()> {
        remove(self.0, "kb_doc_about", "doc_id", id)?;
        for target in about {
            self.0.exec(
                Query::insert()
                    .into_table("kb_doc_about")
                    .columns(["doc_id", "workspace_id", "object_type", "object_id"])
                    .values_panic([
                        id.into(),
                        ws.into(),
                        target.kind.as_str().into(),
                        target.id.as_str().into(),
                    ])
                    .on_conflict(
                        OnConflict::columns(["doc_id", "object_type", "object_id"])
                            .do_nothing()
                            .to_owned(),
                    ),
            )?;
        }
        Ok(())
    }

    pub fn set_mark(&mut self, ws: &str, name: &str, value: &str, now: i64) -> Result<()> {
        self.0.exec(
            Query::insert()
                .into_table("kb_marks")
                .columns(["workspace_id", "name", "value", "updated"])
                .values_panic([ws.into(), name.into(), value.into(), now.into()])
                .on_conflict(
                    OnConflict::columns(["workspace_id", "name"])
                        .update_columns(["value", "updated"])
                        .to_owned(),
                ),
        )?;
        Ok(())
    }

    pub fn update_source(&mut self, source: &SourceRow, now: i64) -> Result<()> {
        update(
            self.0,
            SOURCES,
            &source.id,
            [
                ("name", source.name.as_str().into()),
                ("config", source.config.to_string().into()),
                ("visibility", source.visibility.code().into()),
                ("level", source.level.code().into()),
                ("role", source.role.code().into()),
                ("write_mode", source.write_mode.code().into()),
                ("paused", i64::from(source.paused).into()),
                ("revision", source.revision.into()),
                ("updated", now.into()),
            ],
        )
    }

    pub fn claim_sync(&mut self, id: &str, now: i64) -> Result<bool> {
        Ok(self.0.exec(
            Query::update()
                .table(SOURCES)
                .value("status", "syncing")
                .value("updated", now)
                .and_where(Expr::col("id").eq(id))
                .and_where(Expr::col("status").ne("syncing"))
                .and_where(Expr::col("paused").eq(0_i64)),
        )? == 1)
    }

    pub fn finish_sync(
        &mut self,
        id: &str,
        status: &str,
        error: Option<&str>,
        stats: Option<&Value>,
        now: i64,
    ) -> Result<()> {
        let mut fields = vec![
            ("status", status.into()),
            ("error", error.into()),
            ("updated", now.into()),
        ];
        if let Some(stats) = stats {
            fields.extend([
                ("stats", stats.to_string().into()),
                ("last_sync", now.into()),
            ]);
        }
        update(self.0, SOURCES, id, fields)
    }

    pub fn reset_syncing(&mut self) -> Result<usize> {
        Ok(self.0.exec(
            Query::update()
                .table(SOURCES)
                .value("status", "ok")
                .and_where(Expr::col("status").eq("syncing")),
        )? as usize)
    }

    pub fn delete_source_row(&mut self, id: &str) -> Result<()> {
        remove(self.0, SOURCES, "id", id)
    }

    pub fn set_doc_seen(&mut self, id: &str, size: i64, mtime: i64, now: i64) -> Result<()> {
        update(
            self.0,
            DOCS,
            id,
            [
                ("size", size.into()),
                ("mtime", mtime.into()),
                ("status", "active".into()),
                ("error", Option::<String>::None.into()),
                ("updated", now.into()),
            ],
        )
    }

    pub fn forget_seen(&mut self, id: &str) -> Result<()> {
        update(self.0, DOCS, id, [("mtime", Option::<i64>::None.into())])
    }

    pub fn set_doc_front(
        &mut self,
        id: &str,
        title: &str,
        tags: &[String],
        visibility: Option<&Visibility>,
        level: Option<Level>,
        now: i64,
    ) -> Result<()> {
        update(
            self.0,
            DOCS,
            id,
            [
                ("title", title.into()),
                ("tags", serde_json::to_string(tags)?.into()),
                ("visibility", visibility.map(Visibility::code).into()),
                ("level", level.map(Level::code).into()),
                ("updated", now.into()),
            ],
        )
    }

    pub fn reactivate_doc(&mut self, id: &str, title: &str, size: i64, now: i64) -> Result<()> {
        update(
            self.0,
            DOCS,
            id,
            [
                ("title", title.into()),
                ("size", size.into()),
                ("updated", now.into()),
                ("status", "active".into()),
                ("error", Option::<String>::None.into()),
            ],
        )
    }

    pub fn set_tags(&mut self, id: &str, tags: &[String]) -> Result<()> {
        update(
            self.0,
            DOCS,
            id,
            [("tags", serde_json::to_string(tags)?.into())],
        )
    }

    pub fn delete_mark(&mut self, ws: &str, name: &str) -> Result<()> {
        self.0.exec(
            Query::delete()
                .from_table("kb_marks")
                .and_where(Expr::col("workspace_id").eq(ws))
                .and_where(Expr::col("name").eq(name)),
        )?;
        Ok(())
    }
}
