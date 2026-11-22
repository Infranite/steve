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
