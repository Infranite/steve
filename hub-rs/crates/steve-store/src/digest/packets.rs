use std::cmp::max;

use sea_query::{Asterisk, Expr, ExprTrait, Func, Order, Query, SelectStatement, UpdateStatement};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::contract::WorkspaceSettings;
use crate::contract::{Json, Q, Row};
use crate::{R, Read, Result, StoreError, W, uid};

/// The workspace's batching settings.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Batching {
    /// The module runs (windows are cut) at all.
    pub enabled: bool,
    pub every_min: i64,
    pub packets: i64,
    /// A chunk's token budget.
    pub chunk_tokens: i64,
    /// Lines a chunk repeats from the one before.
    pub overlap: usize,
    /// Messages a window carries from the one before, for context.
    pub context: usize,
    /// Without a cursor: `0` processes all stored messages; a positive value limits initial lookback.
    /// No connector history is fetched.
    pub backlog_days: i64,
    pub lease_min: i64,
    pub max_attempts: i64,
}

impl Default for Batching {
    fn default() -> Self {
        Self {
            enabled: true,
            every_min: 30,
            packets: 20,
            chunk_tokens: 6000,
            overlap: 2,
            context: 3,
            backlog_days: 0,
            lease_min: 120,
            max_attempts: 3,
        }
    }
}

impl Batching {
    pub fn every_ms(&self) -> i64 {
        self.every_min.clamp(1, 7 * 24 * 60) * 60_000
    }
    pub fn lease_ms(&self) -> i64 {
        self.lease_min.clamp(1, 7 * 24 * 60) * 60_000
    }
}

/// A packet to write.
pub struct NewPacket<'a> {
    pub ws: &'a str,
    pub conv: &'a str,
    pub seq: i64,
    pub kind: &'a str,
    pub trigger: &'a str,
    pub first_id: &'a str,
    pub last_id: &'a str,
    pub first_at: i64,
    pub last_at: i64,
    pub count: i64,
    pub priority: i64,
    pub facts: &'a Value,
    pub candidates: &'a Value,
}

/// A packet as stored.
#[derive(Debug, Clone, Serialize)]
pub struct Packet {
    pub id: String,
    pub ws: String,
    pub conv: String,
    pub seq: i64,
    pub kind: String,
    pub trigger: String,
    pub first_id: String,
    pub last_id: String,
    pub first_at: i64,
    pub last_at: i64,
    pub count: i64,
    pub tokens: i64,
    pub priority: i64,
    pub facts: Value,
    pub candidates: Value,
    pub state: String,
    pub batch_id: Option<String>,
    pub lease_until: Option<i64>,
    pub attempts: i64,
    pub error: Option<String>,
    pub created: i64,
    pub updated: i64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Chunk {
    pub n: i64,
    pub first_pos: i64,
    pub last_pos: i64,
    /// Lines repeated from before (the window's context for the first chunk).
    pub overlap: i64,
    pub tokens: i64,
    pub text: String,
}

/// One chunk as stored.
#[derive(Debug, Clone, Serialize)]
pub struct ChunkRow {
    pub n: i64,
    pub first_pos: i64,
    pub last_pos: i64,
    pub overlap: i64,
    pub tokens: i64,
    pub text: String,
    pub state: String,
    pub attempts: i64,
    pub error: Option<String>,
}

const PACKET_COLUMNS: [&str; 22] = [
    "id",
    "workspace_id",
    "conv",
    "seq",
    "kind",
    "trigger",
    "first_id",
    "last_id",
    "first_at",
    "last_at",
    "count",
    "tokens",
    "priority",
    "facts",
    "candidates",
    "state",
    "batch_id",
    "lease_until",
    "attempts",
    "error",
    "created",
    "updated",
];

fn packet(r: &Row<'_>) -> Result<Packet> {
    Ok(Packet {
        id: r.get(0)?,
        ws: r.get(1)?,
        conv: r.get(2)?,
        seq: r.get(3)?,
        kind: r.get(4)?,
        trigger: r.get(5)?,
        first_id: r.get(6)?,
        last_id: r.get(7)?,
        first_at: r.get(8)?,
        last_at: r.get(9)?,
        count: r.get(10)?,
        tokens: r.get(11)?,
        priority: r.get(12)?,
        facts: r.get::<Json<_>>(13)?.0,
        candidates: r.get::<Json<_>>(14)?.0,
        state: r.get(15)?,
        batch_id: r.get(16)?,
        lease_until: r.get(17)?,
        attempts: r.get(18)?,
        error: r.get(19)?,
        created: r.get(20)?,
        updated: r.get(21)?,
    })
}

pub struct Packets<'a, C>(&'a mut C);

const BATCH_PREFIX: &str = "digest:batch:";
const SCHEDULE_KEY: &str = "digest:schedule";

impl R<'_> {
    pub fn digest_packets(&mut self) -> Packets<'_, Self> {
        Packets(self)
    }
}

impl W<'_> {
    pub fn digest_packets(&mut self) -> Packets<'_, Self> {
        Packets(self)
    }
}

fn packets(ws: &str) -> SelectStatement {
    Query::select()
        .from("digest_packets")
        .and_where(Expr::col("workspace_id").eq(ws))
        .take()
}

fn update_packets(ws: &str) -> UpdateStatement {
    Query::update()
        .table("digest_packets")
        .and_where(Expr::col("workspace_id").eq(ws))
        .take()
}

fn update_chunks(id: &str) -> UpdateStatement {
    Query::update()
        .table("digest_chunks")
        .and_where(Expr::col("packet_id").eq(id))
        .and_where(Expr::col("state").ne("done"))
        .take()
}

impl<C: Read> Packets<'_, C> {
    pub fn schedule(&mut self, ws: &str) -> Result<Option<String>> {
        WorkspaceSettings(self.0).get(ws, SCHEDULE_KEY)
    }

    pub fn batch_owner(&mut self, ws: &str, batch: &str) -> Result<Option<String>> {
        WorkspaceSettings(self.0).get(ws, &format!("{BATCH_PREFIX}{batch}"))
    }

    pub fn batching(&mut self, ws: &str) -> Result<Batching> {
        Ok(WorkspaceSettings(self.0)
            .get(ws, "digest")?
            .and_then(|value| serde_json::from_str(&value).ok())
            .unwrap_or_default())
    }

    /// Workspaces with queued or leased packets and their earliest lease expiration.
    pub fn waiting_workspaces(&mut self) -> Result<Vec<(String, Option<i64>)>> {
        self.0.all(
            Query::select()
                .column("workspace_id")
                .expr(Func::min(Expr::cust(
                    "CASE WHEN state = 'leased' THEN lease_until END",
                )))
                .from("digest_packets")
                .and_where(Expr::col("state").is_in(["queued", "leased"]))
                .group_by_col("workspace_id"),
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
    }

    pub fn get(&mut self, ws: &str, id: &str) -> Result<Option<Packet>> {
        self.0.one(
            packets(ws)
                .columns(PACKET_COLUMNS)
                .and_where(Expr::col("id").eq(id)),
            packet,
        )
    }

    /// The most urgent packets in a workspace, optionally filtered by state and conversation.
    pub fn list(
        &mut self,
        ws: &str,
        state: Option<&str>,
        conv: Option<&str>,
        limit: usize,
    ) -> Result<Vec<Packet>> {
        let mut q = packets(ws);
        if let Some(state) = state {
            q.and_where(Expr::col("state").eq(state));
        }
        if let Some(conv) = conv {
            q.and_where(Expr::col("conv").eq(conv));
        }
        let state_first = Expr::case(Expr::col("state").eq("leased"), 0i64)
            .case(Expr::col("state").eq("queued"), 1i64)
            .case(Expr::col("state").eq("failed"), 2i64)
            .finally(3i64);
        self.0.all(
            q.columns(PACKET_COLUMNS)
                .order_by_expr(state_first.into(), Order::Asc)
                .order_by("priority", Order::Desc)
                .order_by("created", Order::Desc)
                .limit(limit as u64),
            packet,
        )
    }

    pub fn chunks(&mut self, id: &str) -> Result<Vec<ChunkRow>> {
        self.0.all(
            Query::select()
                .columns([
                    "n",
                    "first_pos",
                    "last_pos",
                    "overlap",
                    "tokens",
                    "text",
                    "state",
                    "attempts",
                    "error",
                ])
                .from("digest_chunks")
                .and_where(Expr::col("packet_id").eq(id))
                .order_by("n", Order::Asc),
            |r| {
                Ok(ChunkRow {
                    n: r.get(0)?,
                    first_pos: r.get(1)?,
                    last_pos: r.get(2)?,
                    overlap: r.get(3)?,
                    tokens: r.get(4)?,
                    text: r.get(5)?,
                    state: r.get(6)?,
                    attempts: r.get(7)?,
                    error: r.get(8)?,
                })
            },
        )
    }

    /// The earliest start time for a queued batch and the reason that set it.
    pub fn due_at(
        &mut self,
        ws: &str,
        now: i64,
        b: &Batching,
    ) -> Result<Option<(i64, &'static str)>> {
        let Some((count, Some(oldest))) = self.0.one(
            packets(ws)
                .expr(Func::count(Expr::col(Asterisk)))
                .expr(Func::min(Expr::col("created")))
                .and_where(Expr::col("state").eq("queued")),
            |r| Ok((r.get::<i64>(0)?, r.get::<Option<i64>>(1)?)),
        )?
        else {
            return Ok(None);
        };
        Ok(Some(if count >= max(b.packets, 1) {
            (now, "packets")
        } else {
            (oldest.saturating_add(b.every_ms()), "every")
        }))
    }

    pub fn due(&mut self, ws: &str, now: i64, b: &Batching) -> Result<Option<&'static str>> {
        Ok(self
            .due_at(ws, now, b)?
            .filter(|(at, _)| *at <= now)
            .map(|(_, reason)| reason))
    }

    /// Queue state counts and the oldest waiting packet.
    pub fn stats(&mut self, ws: &str, conv: Option<&str>) -> Result<Value> {
        let mut q = packets(ws);
        if let Some(conv) = conv {
            q.and_where(Expr::col("conv").eq(conv));
        }
        let rows = self.0.all(
            q.column("state")
                .expr(Func::count(Expr::col(Asterisk)))
                .expr(Func::sum(Expr::col("tokens")))
                .expr(Func::min(Expr::col("created")))
                .group_by_col("state"),
            |r| {
                Ok((
                    r.get::<String>(0)?,
                    r.get::<i64>(1)?,
                    r.get::<i64>(2)?,
                    r.get::<i64>(3)?,
                ))
            },
        )?;
        let mut out =
            json!({"queued": 0, "leased": 0, "done": 0, "failed": 0, "oldest_queued": null});
        let mut waiting_tokens = 0;
        for (state, count, tokens, oldest) in rows {
            if state == "queued" || state == "leased" {
                waiting_tokens += tokens;
            }
            if state == "queued" {
                out["oldest_queued"] = json!(oldest);
            }
            out[state] = json!(count);
        }
        out["waiting_tokens"] = json!(waiting_tokens);
        Ok(out)
    }
}

impl Packets<'_, W<'_>> {
    pub fn bind_schedule(&mut self, ws: &str, schedule: Option<&str>, now: i64) -> Result<()> {
        let mut settings = WorkspaceSettings(self.0);
        match schedule {
            Some(id) => settings.set(ws, SCHEDULE_KEY, id, now),
            None => settings.remove(ws, [SCHEDULE_KEY]),
        }
    }

    pub fn set_batch_owner(&mut self, ws: &str, batch: &str, run: &str, now: i64) -> Result<()> {
        WorkspaceSettings(self.0).set(ws, &format!("{BATCH_PREFIX}{batch}"), run, now)
    }

    /// A lease is the only reason to retain the identity of its batch's run.
    pub fn prune_batch_owners(&mut self, ws: &str) -> Result<()> {
        let active = self
            .0
            .all(
                packets(ws)
                    .column("batch_id")
                    .distinct()
                    .and_where(Expr::col("state").eq("leased"))
                    .and_where(Expr::col("batch_id").is_not_null()),
                |r| r.get::<String>(0),
            )?
            .into_iter()
            .collect::<std::collections::HashSet<_>>();
        let mut settings = WorkspaceSettings(self.0);
        let expired = settings
            .keys_starting_with(ws, BATCH_PREFIX)?
            .into_iter()
            .filter(|key| {
                key.strip_prefix(BATCH_PREFIX)
                    .is_none_or(|batch| !active.contains(batch))
            })
            .collect::<Vec<_>>();
        for keys in expired.chunks(500) {
            settings.remove(ws, keys.iter().map(String::as_str))?;
        }
        Ok(())
    }

    /// Writes a packet and its chunks together.
    pub fn insert(&mut self, p: &NewPacket<'_>, chunks: &[Chunk], now: i64) -> Result<String> {
        let id = uid("dpk");
        let tokens: i64 = chunks.iter().map(|c| c.tokens).sum();
        self.0.exec(
            Query::insert()
                .into_table("digest_packets")
                .columns(
                    PACKET_COLUMNS
                        .into_iter()
                        .take(15)
                        .chain(["created", "updated"]),
                )
                .values_panic([
                    id.as_str().into(),
                    p.ws.into(),
                    p.conv.into(),
                    p.seq.into(),
                    p.kind.into(),
                    p.trigger.into(),
                    p.first_id.into(),
                    p.last_id.into(),
                    p.first_at.into(),
                    p.last_at.into(),
                    p.count.into(),
                    tokens.into(),
                    p.priority.into(),
                    p.facts.to_string().into(),
                    p.candidates.to_string().into(),
                    now.into(),
                    now.into(),
                ]),
        )?;
        for c in chunks {
            self.0.exec(
                Query::insert()
                    .into_table("digest_chunks")
                    .columns([
                        "packet_id",
                        "n",
                        "first_pos",
                        "last_pos",
                        "overlap",
                        "tokens",
                        "text",
                        "updated",
                    ])
                    .values_panic([
                        id.as_str().into(),
                        c.n.into(),
                        c.first_pos.into(),
                        c.last_pos.into(),
                        c.overlap.into(),
                        c.tokens.into(),
                        c.text.as_str().into(),
                        now.into(),
                    ]),
            )?;
        }
        super::emit(self.0, p.ws, &id);
        Ok(id)
    }

    pub fn release_expired(&mut self, ws: &str, now: i64) -> Result<u64> {
        let b = self.batching(ws)?;
        let state = Expr::case(Expr::col("attempts").gte(max(b.max_attempts, 1)), "failed")
            .finally("queued");
        self.0.exec(
            update_packets(ws)
                .value("state", state)
                .value("batch_id", None::<String>)
                .value("lease_until", None::<i64>)
                .value("error", "lease expired")
                .value("updated", now)
                .and_where(Expr::col("state").eq("leased"))
                .and_where(Expr::col("lease_until").lte(now)),
        )
    }

    /// Leases only the oldest unfinished packet in any given conversation.
    pub fn lease_matching(
        &mut self,
        ws: &str,
        now: i64,
        b: &Batching,
        max: usize,
        mut visible: impl FnMut(&Packet) -> Result<bool>,
    ) -> Result<(String, Vec<String>)> {
        self.release_expired(ws, now)?;
        let batch = uid("dbt");
        let older = Query::select()
            .from_as("digest_packets", "older")
            .column(("older", "id"))
            .and_where(Expr::col(("older", "workspace_id")).eq(ws))
            .and_where(Expr::col(("older", "conv")).equals(("digest_packets", "conv")))
            .and_where(Expr::col(("older", "seq")).lt(Expr::col(("digest_packets", "seq"))))
            .and_where(Expr::col(("older", "state")).ne("done"))
            .take();
        let mut eligible = packets(ws);
        eligible
            .columns(PACKET_COLUMNS)
            .and_where(Expr::col("state").eq("queued"))
            .and_where(Expr::exists(older).not())
            .order_by("priority", Order::Desc)
            .order_by("created", Order::Asc)
            .order_by("conv", Order::Asc)
            .order_by("seq", Order::Asc);
        let mut ids = Vec::new();
        let max = max.min(500);
        let page_size = 32;
        let mut offset = 0;
        // Scan beyond the first page when sources that have become private no longer qualify.
        while ids.len() < max {
            let rows = self
                .0
                .all(eligible.limit(page_size).offset(offset), packet)?;
            let last = (rows.len() as u64) < page_size;
            for p in rows {
                if visible(&p)? {
                    ids.push(p.id);
                    if ids.len() == max {
                        break;
                    }
                }
            }
            if last {
                break;
            }
            offset += page_size;
        }
        if !ids.is_empty() {
            self.0.exec(
                update_packets(ws)
                    .value("state", "leased")
                    .value("attempts", Expr::col("attempts").add(1i64))
                    .value("batch_id", batch.as_str())
                    .value("lease_until", now + b.lease_ms())
                    .value("updated", now)
                    .and_where(Expr::col("id").is_in(ids.iter().map(String::as_str))),
            )?;
            for id in &ids {
                super::emit(self.0, ws, id);
            }
        }
        Ok((batch, ids))
    }

    /// Returns the current attempt count only if the caller still owns this lease.
    fn owns(&mut self, p: &Packet, now: i64) -> Result<i64> {
        self.0
            .one(
                packets(&p.ws)
                    .column("attempts")
                    .and_where(Expr::col("id").eq(p.id.as_str()))
                    .and_where(Expr::col("state").eq("leased"))
                    .and_where(Expr::col("batch_id").eq(p.batch_id.as_deref()))
                    .and_where(Expr::col("lease_until").gt(now)),
                |r| r.get(0),
            )?
            .ok_or_else(|| StoreError::Rejected("digest lease no longer owned".into()))
    }

    pub fn renew(&mut self, p: &Packet, b: &Batching, now: i64) -> Result<()> {
        self.owns(p, now)?;
        self.0.exec(
            update_packets(&p.ws)
                .value("lease_until", now + b.lease_ms())
                .value("updated", now)
                .and_where(Expr::col("id").eq(p.id.as_str())),
        )?;
        Ok(())
    }

    /// Finished chunks are immutable across retries.
    pub fn chunk_done(
        &mut self,
        p: &Packet,
        n: i64,
        ok: bool,
        error: Option<&str>,
        now: i64,
    ) -> Result<bool> {
        self.owns(p, now)?;
        Ok(self.0.exec(
            update_chunks(&p.id)
                .value("state", if ok { "done" } else { "failed" })
                .value("attempts", Expr::col("attempts").add(1i64))
                .value("error", error)
                .value("updated", now)
                .and_where(Expr::col("n").eq(n)),
        )? > 0)
    }

    /// Completes a packet only when every chunk is done. Failed attempts retain completed chunks.
    pub fn packet_done(
        &mut self,
        p: &Packet,
        ok: bool,
        error: Option<&str>,
        b: &Batching,
        now: i64,
    ) -> Result<String> {
        let attempts = self.owns(p, now)?;
        if ok
            && self
                .0
                .one(
                    Query::select()
                        .column("n")
                        .from("digest_chunks")
                        .and_where(Expr::col("packet_id").eq(p.id.as_str()))
                        .and_where(Expr::col("state").ne("done"))
                        .limit(1),
                    |_| Ok(()),
                )?
                .is_some()
        {
            return Err(StoreError::Rejected("digest chunks are unfinished".into()));
        }
        let state = if ok {
            "done"
        } else if attempts >= max(b.max_attempts, 1) {
            "failed"
        } else {
            "queued"
        };
        self.0.exec(
            update_packets(&p.ws)
                .value("state", state)
                .value("error", error)
                .value("batch_id", None::<String>)
                .value("lease_until", None::<i64>)
                .value("updated", now)
                .and_where(Expr::col("id").eq(p.id.as_str())),
        )?;
        if !ok {
            self.0.exec(
                update_chunks(&p.id)
                    .value("state", "queued")
                    .value("updated", now)
                    .and_where(Expr::col("state").eq("failed")),
            )?;
        }
        super::emit(self.0, &p.ws, &p.id);
        Ok(state.into())
    }

    /// Resets the attempt budget for a failed packet, preserving finished chunks and receipts.
    pub fn retry(&mut self, ws: &str, id: &str, now: i64) -> Result<bool> {
        let changed = self.0.exec(
            update_packets(ws)
                .value("state", "queued")
                .value("attempts", 0i64)
                .value("error", None::<String>)
                .value("updated", now)
                .and_where(Expr::col("id").eq(id))
                .and_where(Expr::col("state").eq("failed")),
        )? > 0;
        if changed {
            self.0.exec(
                update_chunks(id)
                    .value("state", "queued")
                    .value("error", None::<String>)
                    .value("updated", now),
            )?;
            super::emit(self.0, ws, id);
        }
        Ok(changed)
    }

    /// Returns the lease to the queue without spending an attempt.
    pub fn yield_lease(&mut self, p: &Packet, now: i64) -> Result<()> {
        let attempts = self.owns(p, now)?;
        self.0.exec(
            update_packets(&p.ws)
                .value("state", "queued")
                .value("attempts", max(attempts - 1, 0))
                .value("error", None::<String>)
                .value("batch_id", None::<String>)
                .value("lease_until", None::<i64>)
                .value("updated", now)
                .and_where(Expr::col("id").eq(p.id.as_str())),
        )?;
        self.0.exec(
            update_chunks(&p.id)
                .value("state", "queued")
                .value("updated", now)
                .and_where(Expr::col("state").eq("failed")),
        )?;
        super::emit(self.0, &p.ws, &p.id);
        Ok(())
    }
}
