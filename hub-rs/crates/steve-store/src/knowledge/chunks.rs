use crate::base::{Level, Visibility};
use crate::contract::{Q, Row};
use crate::{Read, Result, W};
use sea_query::{Expr, ExprTrait, JoinType, Order, Query, SelectStatement};

pub struct Chunks<'a, C>(pub &'a mut C);

/// One candidate a store found, before the Hub ranks it.
#[derive(Debug, Clone, PartialEq)]
pub struct Candidate {
    pub obj_type: String,
    pub obj_id: String,
    pub seq: i64,
    pub heading: String,
    pub text: String,
    pub source_kind: String,
    pub label: String,
    pub level: Level,
    pub visibility: Visibility,
    pub created_by: String,
    pub updated: i64,
    /// The store's score, larger = better; only compared within one [`Pass`].
    pub base: f64,
    pub title: Option<String>,
    pub doc_ref: Option<String>,
}

/// Source kinds excluded from default knowledge recall (#550): work items and chat logs are
/// searched via dedicated record search rather than mixed into knowledge retrieval.
pub const EXCLUDED_KNOWLEDGE_KINDS: [&str; 2] = ["items", "chat"];

pub struct ChunkFilter<'a> {
    pub ws: &'a str,
    pub workspace_only: bool,
    pub kinds: Option<&'a [String]>,
    pub paused_docs: &'a [String],
    pub paused_mems: bool,
    pub paused_bot_mems: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ObjectChunk {
    pub seq: i64,
    pub heading: String,
    pub text: String,
    pub visibility: String,
    pub level: String,
    pub created_by: String,
    pub source_kind: String,
}

pub struct ChunkMeta<'a> {
    pub ws: &'a str,
    pub kind: &'a str,
    pub id: &'a str,
    pub visibility: &'a Visibility,
    pub level: Level,
    pub created_by: &'a str,
    pub source_kind: &'a str,
    pub label: &'a str,
    pub updated: i64,
}

pub struct IndexChunk<'a> {
    pub seq: i64,
    pub heading: &'a str,
    pub text: String,
    pub start: i64,
    pub end: i64,
    pub terms: String,
}

pub type Piece = (i64, String, Option<i64>, Option<i64>);

fn candidate(r: &Row<'_>, base: Option<f64>) -> Result<Candidate> {
    Ok(Candidate {
        obj_type: r.get(0)?,
        obj_id: r.get(1)?,
        seq: r.get(2)?,
        heading: r.get(3)?,
        text: r.get(4)?,
        source_kind: r.get(5)?,
        label: r.get(6)?,
        level: Level::from_db(&r.get::<String>(7)?),
        visibility: Visibility::from_db(&r.get::<String>(8)?),
        created_by: r.get(9)?,
        updated: r.get(10)?,
        title: r.get(11)?,
        doc_ref: r.get(12)?,
        base: match base {
            Some(score) => score,
            None => -r.get::<f64>(13)?,
        },
    })
}

fn scope(query: &mut SelectStatement, filter: &ChunkFilter<'_>) {
    const COLS: [&str; 11] = [
        "obj_type",
        "obj_id",
        "seq",
        "heading",
        "text",
        "source_kind",
        "label",
        "level",
        "visibility",
        "created_by",
        "updated",
    ];
    query
        .columns(COLS.map(|col| ("c", col)))
        .columns([("d", "title"), ("d", "ref")])
        .join_as(
            JoinType::LeftJoin,
            "kb_docs",
            "d",
            Expr::col(("d", "id"))
                .equals(("c", "obj_id"))
                .and(Expr::col(("c", "obj_type")).eq("chunk")),
        )
        .and_where(Expr::col(("c", "workspace_id")).eq(filter.ws))
        .and_where(Expr::col(("c", "level")).ne("never"));
    if filter.workspace_only {
        query.and_where(Expr::col(("c", "visibility")).eq("workspace"));
    }
    if let Some(kinds) = filter.kinds {
        query.and_where(Expr::col(("c", "source_kind")).is_in(kinds.iter().map(String::as_str)));
    } else {
        query.and_where(Expr::col(("c", "source_kind")).is_not_in(EXCLUDED_KNOWLEDGE_KINDS));
    }
    if filter.paused_mems {
        query.and_where(Expr::col(("c", "obj_type")).ne("mem"));
    }
    if filter.paused_bot_mems {
        query.and_where(Expr::col(("c", "obj_type")).ne("bot_mem"));
    }
    if !filter.paused_docs.is_empty() {
        let paused = Query::select()
            .column("id")
            .from("kb_docs")
            .and_where(Expr::col("source_id").is_in(filter.paused_docs.iter().map(String::as_str)))
            .take();
        query.and_where(
            Expr::col(("c", "obj_type"))
                .ne("chunk")
                .or(Expr::col(("c", "obj_id")).not_in_subquery(paused)),
        );
    }
}

fn target(kind: &str, id: &str) -> Expr {
    Expr::col("obj_type")
        .eq(kind)
        .and(Expr::col("obj_id").eq(id))
}

impl<C: Read> Chunks<'_, C> {
    pub fn chunk_in_scope(
        &mut self,
        filter: &ChunkFilter<'_>,
        kind: &str,
        id: &str,
        seq: i64,
    ) -> Result<Option<Candidate>> {
        let mut q = Query::select();
        q.from_as("kb_chunks", "c");
        scope(&mut q, filter);
        q.and_where(Expr::col(("c", "obj_type")).eq(kind))
            .and_where(Expr::col(("c", "obj_id")).eq(id))
            .and_where(Expr::col(("c", "seq")).eq(seq))
            .limit(1);
        self.0.one(&q, |r| candidate(r, Some(0.0)))
    }

    pub fn containing(
        &mut self,
        filter: &ChunkFilter<'_>,
        text: &str,
        limit: usize,
    ) -> Result<Vec<Candidate>> {
        let dialect = self.0.db().dialect();
        let mut q = Query::select();
        q.from_as("kb_chunks", "c");
        scope(&mut q, filter);
        q.and_where(
            Expr::col(("c", "obj_type"))
                .is_in(["mem", "bot_mem"])
                .and(dialect.contains(Expr::col(("c", "text")), Expr::value(text)))
                .or(Expr::col(("c", "obj_type"))
                    .eq("chunk")
                    .and(Expr::col(("c", "seq")).eq(0_i64))
                    .and(dialect.contains(Expr::col(("d", "title")), Expr::value(text)))),
        )
        .order_by(("c", "updated"), Order::Desc)
        .limit(limit as u64);
        self.0.all(&q, |r| candidate(r, Some(1.0)))
    }

    pub fn fulltext(
        &mut self,
        filter: &ChunkFilter<'_>,
        terms: &str,
        limit: usize,
    ) -> Result<Vec<Candidate>> {
        let dialect = self.0.db().dialect();
        let mut q = Query::select();
        q.from("kb_fts").join_as(
            JoinType::InnerJoin,
            "kb_chunks",
            "c",
            Expr::col(("c", "id")).equals(("kb_fts", "rowid")),
        );
        scope(&mut q, filter);
        q.expr_as(dialect.knowledge_score(), "score")
            .and_where(dialect.fulltext_match("kb_fts", terms))
            .order_by("score", Order::Asc)
            .limit(limit as u64);
        self.0.all(&q, |r| candidate(r, None))
    }

    pub fn workspace(&mut self, kind: &str, id: &str) -> Result<Option<String>> {
        self.0.one(
            Query::select()
                .column("workspace_id")
                .from("kb_chunks")
                .and_where(target(kind, id))
                .limit(1),
            |row| row.get(0),
        )
    }

    pub fn object_ids(&mut self, ws: &str, kind: &str) -> Result<Vec<String>> {
        self.0.all(
            Query::select()
                .distinct()
                .column("obj_id")
                .from("kb_chunks")
                .and_where(Expr::col("workspace_id").eq(ws))
                .and_where(Expr::col("obj_type").eq(kind)),
            |r| r.get(0),
        )
    }

    pub fn texts(&mut self, ws: &str, kind: &str) -> Result<Vec<(String, (String, String))>> {
        self.0.all(
            Query::select()
                .columns(["obj_id", "text", "label"])
                .from("kb_chunks")
                .and_where(Expr::col("workspace_id").eq(ws))
                .and_where(Expr::col("obj_type").eq(kind)),
            |r| Ok((r.get(0)?, (r.get(1)?, r.get(2)?))),
        )
    }

    pub fn mirror_objects(&mut self) -> Result<Vec<(String, String, String)>> {
        self.0.all(
            Query::select()
                .distinct()
                .columns(["workspace_id", "obj_type", "obj_id"])
                .from("kb_chunks")
                .and_where(Expr::col("level").ne("never")),
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
    }

    pub fn mirror_chunks(&mut self, ws: &str, kind: &str, id: &str) -> Result<Vec<ObjectChunk>> {
        self.0.all(
            Query::select()
                .columns([
                    "seq",
                    "heading",
                    "text",
                    "visibility",
                    "level",
                    "created_by",
                    "source_kind",
                ])
                .from("kb_chunks")
                .and_where(Expr::col("workspace_id").eq(ws))
                .and_where(target(kind, id))
                .and_where(Expr::col("level").ne("never"))
                .order_by("seq", Order::Asc),
            |r| {
                Ok(ObjectChunk {
                    seq: r.get(0)?,
                    heading: r.get(1)?,
                    text: r.get(2)?,
                    visibility: r.get(3)?,
                    level: r.get(4)?,
                    created_by: r.get(5)?,
                    source_kind: r.get(6)?,
                })
            },
        )
    }

    pub fn document_pieces(&mut self, id: &str, from: i64) -> Result<Vec<Piece>> {
        self.0.all(
            Query::select()
                .columns(["seq", "text", "start_at", "end_at"])
                .from("kb_chunks")
                .and_where(target("chunk", id))
                .and_where(Expr::col("seq").gte(from))
                .and_where(Expr::col("level").ne("never"))
                .order_by("seq", Order::Asc),
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
    }

    pub fn disclosure(
        &mut self,
        ws: &str,
        kind: &str,
        id: &str,
        seq: i64,
    ) -> Result<Option<(String, Level, String)>> {
        self.0.one(
            Query::select()
                .columns(["text", "level", "visibility"])
                .from("kb_chunks")
                .and_where(Expr::col("workspace_id").eq(ws))
                .and_where(target(kind, id))
                .and_where(Expr::col("seq").eq(seq)),
            |r| Ok((r.get(0)?, Level::from_db(&r.get::<String>(1)?), r.get(2)?)),
        )
    }
}

impl Chunks<'_, W<'_>> {
    pub fn append(&mut self, m: &ChunkMeta<'_>, title: &str, c: IndexChunk<'_>) -> Result<()> {
        let query = Query::insert()
            .into_table("kb_chunks")
            .columns([
                "workspace_id",
                "obj_type",
                "obj_id",
                "seq",
                "heading",
                "text",
                "start_at",
                "end_at",
                "visibility",
                "level",
                "created_by",
                "source_kind",
                "label",
                "updated",
            ])
            .values_panic([
                m.ws.into(),
                m.kind.into(),
                m.id.into(),
                c.seq.into(),
                c.heading.into(),
                c.text.into(),
                c.start.into(),
                c.end.into(),
                m.visibility.code().into(),
                m.level.code().into(),
                m.created_by.into(),
                m.source_kind.into(),
                m.label.into(),
                m.updated.into(),
            ])
            .returning_col("id")
            .take();
        let id: i64 = crate::repo::must(self.0.one(&query, |r| r.get(0))?)?;
        self.0.exec(
            Query::insert()
                .into_table("kb_fts")
                .columns(["rowid", "seg", "title"])
                .values_panic([id.into(), c.terms.into(), title.into()]),
        )?;
        Ok(())
    }

    pub fn remove(&mut self, kind: &str, id: &str) -> Result<usize> {
        let ids = Query::select()
            .column("id")
            .from("kb_chunks")
            .and_where(target(kind, id))
            .take();
        self.0.exec(
            Query::delete()
                .from_table("kb_fts")
                .and_where(Expr::col("rowid").in_subquery(ids.clone())),
        )?;
        self.0.exec(
            Query::delete()
                .from_table("kb_vectors")
                .and_where(Expr::col("chunk_id").in_subquery(ids)),
        )?;
        Ok(self.0.exec(
            Query::delete()
                .from_table("kb_chunks")
                .and_where(target(kind, id)),
        )? as usize)
    }

    pub fn relabel(&mut self, kind: &str, id: &str, label: &str) -> Result<bool> {
        Ok(self.0.exec(
            Query::update()
                .table("kb_chunks")
                .value("label", label)
                .and_where(target(kind, id))
                .and_where(Expr::col("label").ne(label)),
        )? > 0)
    }

    pub fn rescope(
        &mut self,
        kind: &str,
        id: &str,
        visibility: &Visibility,
        level: Level,
    ) -> Result<usize> {
        self.rescope_matching(target(kind, id), visibility, level)
    }

    pub fn rescope_bot_memories(
        &mut self,
        ws: &str,
        vis: &Visibility,
        level: Level,
    ) -> Result<usize> {
        self.rescope_matching(
            Expr::col("workspace_id")
                .eq(ws)
                .and(Expr::col("obj_type").eq("bot_mem")),
            vis,
            level,
        )
    }

    fn rescope_matching(
        &mut self,
        predicate: Expr,
        vis: &Visibility,
        level: Level,
    ) -> Result<usize> {
        let scope = vis.code();
        let level = level.code();
        Ok(self.0.exec(
            Query::update()
                .table("kb_chunks")
                .value("visibility", scope.as_str())
                .value("level", level)
                .and_where(predicate)
                .and_where(
                    Expr::col("visibility")
                        .ne(scope.as_str())
                        .or(Expr::col("level").ne(level)),
                ),
        )? as usize)
    }
}
