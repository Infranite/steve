//! A session's append-only transcript, read from one snapshot.
use sea_query::{Alias, Expr, ExprTrait, Order, Query, SelectStatement};
use serde::Serialize;

use crate::contract::Row;
use crate::ingest::schema::{Events as E, Sessions as S};
use crate::{Event, Read, Result};

const PAGE_BYTES: usize = 256 * 1024;

pub struct Transcript<'a, C>(pub(super) &'a mut C);

#[derive(Debug, Serialize)]
pub struct TranscriptPage {
    pub events: Vec<Event>,
    /// Exclusive cursor in the direction of the request.
    pub next: Option<i64>,
}

#[derive(Debug, Serialize)]
pub struct TranscriptMarks {
    /// Inclusive upper bounds of ranges, including the snapshot's last event.
    pub marks: Vec<i64>,
}

fn entries(id: &str) -> SelectStatement {
    Query::select()
        .from(E::Table)
        .and_where(Expr::col(E::SessionId).eq(id))
        .to_owned()
}

fn decode(r: &Row<'_>) -> Result<Event> {
    let format: String = r.get(3)?;
    let (kind, data) = if format == "steve.v1" {
        let raw: String = r.get(4)?;
        (r.get(2)?, serde_json::from_str(&raw).unwrap_or_default())
    } else {
        (
            "system".into(),
            serde_json::json!({
                "code": "unsupported_format",
                "format": format,
                "text": format!("Unsupported transcript format: {format}"),
            }),
        )
    };
    Ok(Event {
        seq: r.get(0)?,
        ts: r.get(1)?,
        kind,
        data,
    })
}

impl<C: Read> Transcript<'_, C> {
    pub fn exists(&mut self, ws: &str, id: &str) -> Result<bool> {
        Ok(self
            .0
            .one(
                Query::select()
                    .column(S::Id)
                    .from(S::Table)
                    .and_where(Expr::col(S::Id).eq(id))
                    .and_where(Expr::col(S::WorkspaceId).eq(ws)),
                |r| r.get::<String>(0),
            )?
            .is_some())
    }

    /// Take a bounded window, then extend its oldest end to the first prompt of its turn.
    /// Adjacent prompts can belong to the same turn (a merged queue).
    pub fn before(&mut self, id: &str, before: Option<i64>, limit: u32) -> Result<TranscriptPage> {
        let mut query = entries(id);
        if let Some(before) = before {
            query.and_where(Expr::col(E::Seq).lt(before));
        }
        let (edge, _) = self.edge(&query, Order::Desc, limit)?;
        let Some(edge) = edge else {
            return Ok(TranscriptPage {
                events: vec![],
                next: None,
            });
        };
        let start = self.0.one(
            entries(id)
                .column(E::Seq)
                .and_where(Expr::col(E::Seq).lte(edge))
                .and_where(Expr::col(E::Kind).eq("user"))
                .and_where(Expr::col(E::Format).eq("steve.v1"))
                .and_where(Expr::cust(
                    "coalesce((SELECT CASE WHEN prev.format='steve.v1' THEN prev.kind END FROM events prev
                    WHERE prev.session_id=events.session_id AND prev.seq<events.seq
                    ORDER BY prev.seq DESC LIMIT 1), '') <> 'user'",
                ))
                .order_by(E::Seq, Order::Desc)
                .limit(1),
            |r| r.get::<i64>(0),
        )?;
        // A prefix of notices has no prompt or mergeable entries. It can use the
        // bounded window directly instead of treating every notice as one long turn.
        let start = match start {
            Some(start) => Some(start),
            None => {
                let earlier_work = self.0.one(
                    entries(id)
                        .column(E::Seq)
                        .and_where(Expr::col(E::Seq).lt(edge))
                        .and_where(Expr::col(E::Format).eq("steve.v1"))
                        .and_where(Expr::col(E::Kind).is_in(["text", "tool"]))
                        .limit(1),
                    |r| r.get::<i64>(0),
                )?;
                earlier_work.is_none().then_some(edge)
            }
        };
        let next = if let Some(start) = start {
            query.and_where(Expr::col(E::Seq).gte(start));
            self.0
                .one(
                    entries(id)
                        .column(E::Seq)
                        .and_where(Expr::col(E::Seq).lt(start))
                        .limit(1),
                    |r| r.get::<i64>(0),
                )?
                .map(|_| start)
        } else {
            None
        };
        Ok(TranscriptPage {
            events: self.read(id, query)?,
            next,
        })
    }

    pub fn after(
        &mut self,
        id: &str,
        after: i64,
        through: Option<i64>,
        limit: u32,
    ) -> Result<TranscriptPage> {
        let mut query = entries(id);
        query.and_where(Expr::col(E::Seq).gt(after));
        if let Some(through) = through {
            query.and_where(Expr::col(E::Seq).lte(through));
        }
        let (edge, more) = self.edge(&query, Order::Asc, limit)?;
        let Some(edge) = edge else {
            return Ok(TranscriptPage {
                events: vec![],
                next: None,
            });
        };
        query.and_where(Expr::col(E::Seq).lte(edge));
        Ok(TranscriptPage {
            events: self.read(id, query)?,
            next: more.then_some(edge),
        })
    }

    pub fn marks(&mut self, id: &str, every: usize) -> Result<TranscriptMarks> {
        let seqs = self.0.all(
            entries(id).column(E::Seq).order_by(E::Seq, Order::Asc),
            |r| r.get::<i64>(0),
        )?;
        Ok(TranscriptMarks {
            marks: seqs
                .chunks(std::cmp::max(every, 1))
                .filter_map(|s| s.last().copied())
                .collect(),
        })
    }

    fn edge(
        &mut self,
        query: &SelectStatement,
        order: Order,
        limit: u32,
    ) -> Result<(Option<i64>, bool)> {
        let limit = std::cmp::max(limit, 1) as usize;
        let rows = self.0.all(
            query
                .clone()
                .column(E::Seq)
                .expr(Expr::cust(
                    "octet_length(events.data) + coalesce(
                    (SELECT sum(octet_length(p.data)) FROM event_payload_refs r
                     JOIN event_payloads p ON p.session_id=r.session_id AND p.hash=r.hash
                     WHERE r.event_seq=events.seq), 0)",
                ))
                .order_by(E::Seq, order)
                .limit(limit as u64 + 1),
            |r| Ok((r.get::<i64>(0)?, r.get::<i64>(1)?)),
        )?;
        let mut bytes = 0usize;
        let mut edge = None;
        for (index, (seq, size)) in rows.into_iter().enumerate() {
            bytes = bytes.saturating_add(size as usize + 64);
            if index == limit || (index > 0 && bytes > PAGE_BYTES) {
                return Ok((edge, true));
            }
            edge = Some(seq);
        }
        Ok((edge, false))
    }

    fn read(&mut self, id: &str, mut query: SelectStatement) -> Result<Vec<Event>> {
        let mut events = self.0.all(
            query
                .columns([E::Seq, E::Ts, E::Kind, E::Format, E::Data])
                .order_by(E::Seq, Order::Asc),
            decode,
        )?;
        for event in &mut events {
            if matches!(event.kind.as_str(), "tool_call" | "tool_call_update") {
                let refs = self.0.all(
                    Query::select().from(Alias::new("event_payload_refs"))
                        .exprs([Expr::cust("event_payload_refs.path"), Expr::cust("p.data"), Expr::cust("event_payload_refs.session_id")])
                        .join_as(sea_query::JoinType::LeftJoin, Alias::new("event_payloads"), Alias::new("p"),
                            Expr::cust("p.session_id=event_payload_refs.session_id AND p.hash=event_payload_refs.hash"))
                        .and_where(Expr::col(Alias::new("event_seq")).eq(event.seq)),
                    |r| Ok((r.get::<String>(0)?, r.get::<Option<String>>(1)?, r.get::<String>(2)?)),
                )?;
                for (path, raw, owner) in refs {
                    crate::event_payload::restore(id, &mut event.data, &path, raw, &owner)?;
                }
            }
        }
        Ok(events)
    }
}
