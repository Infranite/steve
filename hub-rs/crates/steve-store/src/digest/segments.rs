use crate::contract::Q;
use sea_query::{Expr, ExprTrait, Order, Query, SelectStatement};
use serde::Serialize;

use crate::contract::Json;
use crate::{R, Read, Result, W};

#[derive(Debug, Serialize)]
pub struct Segment {
    pub id: String,
    pub chunk: i64,
    pub first: i64,
    pub last: i64,
    pub summary: String,
    pub tags: Vec<String>,
    pub matter: Option<String>,
    pub chains: Vec<String>,
}

pub struct Segments<'a, C>(&'a mut C);

impl R<'_> {
    pub fn digest_segments(&mut self) -> Segments<'_, Self> {
        Segments(self)
    }
}

impl W<'_> {
    pub fn digest_segments(&mut self) -> Segments<'_, Self> {
        Segments(self)
    }
}

impl<C: Read> Segments<'_, C> {
    pub fn for_packet(
        &mut self,
        ws: &str,
        packet: &str,
        after: usize,
        limit: usize,
    ) -> Result<Vec<Segment>> {
        self.0.all(
            Query::select()
                .columns([
                    "id",
                    "chunk",
                    "first_pos",
                    "last_pos",
                    "summary",
                    "tags",
                    "matter_id",
                    "chains",
                ])
                .from("digest_segments")
                .and_where(Expr::col("workspace_id").eq(ws))
                .and_where(Expr::col("packet_id").eq(packet))
                .order_by("chunk", Order::Asc)
                .order_by("first_pos", Order::Asc)
                .order_by("id", Order::Asc)
                .offset(after.min(i64::MAX as usize) as u64)
                .limit(limit as u64),
            |r| {
                Ok(Segment {
                    id: r.get(0)?,
                    chunk: r.get(1)?,
                    first: r.get(2)?,
                    last: r.get(3)?,
                    summary: r.get(4)?,
                    tags: r.get::<Json<_>>(5)?.0,
                    matter: r.get(6)?,
                    chains: r.get::<Json<_>>(7)?.0,
                })
            },
        )
    }

    pub fn matters_by_chains<'a>(
        &mut self,
        ws: &str,
        conv: &str,
        chains: impl IntoIterator<Item = &'a str>,
    ) -> Result<Vec<String>> {
        let mut chains = chains.into_iter().peekable();
        if chains.peek().is_none() {
            return Ok(Vec::new());
        }
        let matching: SelectStatement = Query::select()
            .column(("chain", "value"))
            .from_function(
                self.0
                    .db()
                    .dialect()
                    .json_array_elements(Expr::col(("digest_segments", "chains"))),
                "chain",
            )
            .and_where(Expr::col(("chain", "value")).is_in(chains))
            .take();
        self.0.all(
            Query::select()
                .distinct()
                .column("matter_id")
                .from("digest_segments")
                .and_where(Expr::col("workspace_id").eq(ws))
                .and_where(Expr::col("conv").eq(conv))
                .and_where(Expr::col("matter_id").is_not_null())
                .and_where(Expr::exists(matching)),
            |r| r.get(0),
        )
    }
}

#[derive(Serialize)]
pub struct TimelineSegment {
    pub id: String,
    pub conv: String,
    pub packet: String,
    pub first_id: String,
    pub last_id: String,
    pub first_at: i64,
    pub last_at: i64,
    pub summary: String,
    pub tags: Vec<String>,
    pub writer: String,
}

pub struct SegmentInput<'a> {
    pub ws: &'a str,
    pub packet: &'a str,
    pub conv: &'a str,
    pub chunk: i64,
    pub first: i64,
    pub last: i64,
    pub first_id: &'a str,
    pub last_id: &'a str,
    pub first_at: i64,
    pub last_at: i64,
    pub summary: &'a str,
    pub tags: &'a [String],
    pub chains: &'a std::collections::BTreeSet<String>,
    pub parties: &'a std::collections::BTreeSet<String>,
    pub writer: &'a str,
    pub now: i64,
}

fn for_matter(id: &str) -> SelectStatement {
    Query::select()
        .from("digest_segments")
        .and_where(Expr::col("matter_id").eq(id))
        .take()
}

fn matching(ws: &str, segment: &str) -> Expr {
    Expr::col("workspace_id")
        .eq(ws)
        .and(Expr::col("id").eq(segment))
}

impl<'a, C: Read> Segments<'a, C> {
    pub fn new(read: &'a mut C) -> Self {
        Self(read)
    }

    pub fn assignment(&mut self, ws: &str, segment: &str) -> Result<Option<Option<String>>> {
        self.0.one(
            Query::select()
                .column("matter_id")
                .from("digest_segments")
                .and_where(matching(ws, segment)),
            |r| r.get(0),
        )
    }

    pub fn stats(&mut self, id: &str) -> Result<Vec<(Vec<String>, i64, i64)>> {
        self.0.all(
            for_matter(id).columns(["parties", "first_at", "last_at"]),
            |r| Ok((r.get::<Json<_>>(0)?.0, r.get(1)?, r.get(2)?)),
        )
    }

    pub fn recent_summaries(&mut self, id: &str) -> Result<Vec<String>> {
        self.0.all(
            for_matter(id)
                .column("summary")
                .order_by("last_at", Order::Desc)
                .limit(30),
            |r| r.get(0),
        )
    }

    pub fn count(&mut self, id: &str) -> Result<i64> {
        Ok(self
            .0
            .one(for_matter(id).expr(Expr::col("id").count()), |r| r.get(0))?
            .unwrap_or_default())
    }

    pub fn conversations(&mut self, id: &str) -> Result<Vec<String>> {
        self.0
            .all(for_matter(id).column("conv").distinct(), |r| r.get(0))
    }

    pub fn conversation_matters(&mut self, ws: &str, conv: &str) -> Result<Vec<String>> {
        self.0.all(
            Query::select()
                .column("matter_id")
                .distinct()
                .from("digest_segments")
                .and_where(Expr::col("workspace_id").eq(ws))
                .and_where(Expr::col("conv").eq(conv))
                .and_where(Expr::col("matter_id").is_not_null()),
            |r| r.get(0),
        )
    }

    pub fn matters_at(&mut self, ws: &str, conv: &str, at: i64, id: &str) -> Result<Vec<String>> {
        self.0.all(
            Query::select()
                .column("matter_id")
                .from("digest_segments")
                .and_where(Expr::col("workspace_id").eq(ws))
                .and_where(Expr::col("conv").eq(conv))
                .and_where(Expr::col("matter_id").is_not_null())
                .and_where(
                    Expr::tuple([Expr::col("first_at"), Expr::col("first_id")])
                        .lte(Expr::tuple([Expr::value(at), Expr::value(id)])),
                )
                .and_where(
                    Expr::tuple([Expr::col("last_at"), Expr::col("last_id")])
                        .gte(Expr::tuple([Expr::value(at), Expr::value(id)])),
                ),
            |r| r.get(0),
        )
    }

    pub fn timeline(&mut self, id: &str) -> Result<Vec<TimelineSegment>> {
        self.0.all(
            for_matter(id)
                .columns([
                    "id",
                    "conv",
                    "packet_id",
                    "first_id",
                    "last_id",
                    "first_at",
                    "last_at",
                    "summary",
                    "tags",
                    "writer",
                ])
                .order_by("first_at", Order::Asc)
                .order_by("id", Order::Asc),
            |r| {
                Ok(TimelineSegment {
                    id: r.get(0)?,
                    conv: r.get(1)?,
                    packet: r.get(2)?,
                    first_id: r.get(3)?,
                    last_id: r.get(4)?,
                    first_at: r.get(5)?,
                    last_at: r.get(6)?,
                    summary: r.get(7)?,
                    tags: r.get::<Json<_>>(8)?.0,
                    writer: r.get(9)?,
                })
            },
        )
    }
}

impl Segments<'_, W<'_>> {
    pub fn insert(&mut self, s: SegmentInput<'_>) -> Result<String> {
        let id = crate::uid("dsg");
        self.0.exec(
            Query::insert()
                .into_table("digest_segments")
                .columns([
                    "id",
                    "workspace_id",
                    "packet_id",
                    "conv",
                    "chunk",
                    "first_pos",
                    "last_pos",
                    "first_id",
                    "last_id",
                    "first_at",
                    "last_at",
                    "summary",
                    "tags",
                    "chains",
                    "parties",
                    "writer",
                    "created",
                ])
                .values_panic([
                    id.as_str().into(),
                    s.ws.into(),
                    s.packet.into(),
                    s.conv.into(),
                    s.chunk.into(),
                    s.first.into(),
                    s.last.into(),
                    s.first_id.into(),
                    s.last_id.into(),
                    s.first_at.into(),
                    s.last_at.into(),
                    s.summary.into(),
                    serde_json::to_string(s.tags)?.into(),
                    serde_json::to_string(s.chains)?.into(),
                    serde_json::to_string(s.parties)?.into(),
                    s.writer.into(),
                    s.now.into(),
                ]),
        )?;
        Ok(id)
    }

    pub fn assign(&mut self, ws: &str, segment: &str, matter: Option<&str>) -> Result<()> {
        self.0.exec(
            Query::update()
                .table("digest_segments")
                .value("matter_id", matter)
                .and_where(matching(ws, segment)),
        )?;
        Ok(())
    }

    pub fn move_all(&mut self, ws: &str, from: &str, into: &str) -> Result<Vec<String>> {
        let ids = self.0.all(
            for_matter(from)
                .column("id")
                .and_where(Expr::col("workspace_id").eq(ws)),
            |r| r.get(0),
        )?;
        self.0.exec(
            Query::update()
                .table("digest_segments")
                .value("matter_id", into)
                .and_where(Expr::col("workspace_id").eq(ws))
                .and_where(Expr::col("matter_id").eq(from)),
        )?;
        Ok(ids)
    }

    pub fn remove(&mut self, ws: &str, segment: &str) -> Result<()> {
        self.0.exec(
            Query::delete()
                .from_table("digest_segments")
                .and_where(matching(ws, segment)),
        )?;
        Ok(())
    }
}
