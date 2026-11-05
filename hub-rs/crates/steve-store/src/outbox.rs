//! The durable outbox (migration 21): a message is written in the same transaction as the change that
//! caused it and delivered at least once afterwards, so a crash between commit and send loses nothing.
//!
//! A domain records a fact in the same [`Store::transaction`] as the state change it describes
//! ([`emit`]); a [`Dispatcher`] hands it to every registered [`Consumer`] at least once, after the
//! commit. What makes "at least once" safe is the receipt, not the lease:
//!
//! - [`Consumer::effect`] runs outside any transaction (a subprocess, a network call) and must be
//!   idempotent: it may run again after a crash or a lost lease;
//! - [`Consumer::apply`] runs inside the transaction that inserts the receipt `(event, consumer)`; the
//!   receipt goes first, so a second delivery finds it and changes nothing, and a failing `apply` rolls
//!   the receipt back with the consumer's own writes;
//! - what `apply` starts after the commit ([`AfterCommit`]) is not covered: a consumer that needs it to
//!   survive a crash stores it as state its own recovery picks up (a flow run left `running`).
//!
//! The lease (`outbox_leases`) only says who runs it now: claimed for [`LEASE_MS`], re-checked before
//! the receipt commits (fencing), and holding attempts, the last error and the back-off. A
//! [`ConsumeError::Reject`] or [`MAX_ATTEMPTS`] failures make it dead: never retried by itself and never
//! removed by [`prune`]; a person decides. Rows are scoped: the receipt carries the event's workspace,
//! and consumers check that the objects they touch belong to it.
//!
//! A consumer receives the events created after its first registration ([`Dispatcher::register`],
//! `since_seq`); history is replayed only by an explicit backfill. Register before anything can emit.

use std::sync::Arc;

use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{Result, Store, StoreError, Tx, uid};

/// How long a claimed delivery stays with its dispatcher before another may take it over.
pub const LEASE_MS: i64 = 60_000;
/// Failed attempts after which a delivery is retained as dead for investigation.
pub const MAX_ATTEMPTS: i64 = 8;
/// Deliveries claimed per consumer and pass.
pub const BATCH: usize = 50;
/// The longest back-off between attempts.
pub const BACKOFF_MAX_MS: i64 = 10 * 60_000;
/// Fully delivered events are kept this long (candidate value), then [`prune`]d.
pub const KEEP_MS: i64 = 7 * 24 * 3_600_000;

/// A committed fact.
#[derive(Debug, Clone, PartialEq)]
pub struct Event {
    pub seq: i64,
    /// `evt_…`, stable: consumers key their own idempotency on it.
    pub id: String,
    pub workspace_id: String,
    pub topic: String,
    pub version: i64,
    pub payload: Value,
    pub created: i64,
}

/// Records `payload` under `topic` in the caller's transaction; delivered only if it commits.
pub fn emit(tx: &Tx, ws: &str, topic: &str, version: i64, payload: &Value) -> Result<Event> {
    let id = uid("evt");
    let created = crate::now();
    tx.execute(
        "INSERT INTO outbox_events (id, workspace_id, topic, version, payload, created) VALUES (?, ?, ?, ?, ?, ?)",
        params![id, ws, topic, version, payload.to_string(), created],
    )?;
    tx.mark_outbox();
    Ok(Event {
        seq: tx.last_insert_rowid(),
        id,
        workspace_id: ws.into(),
        topic: topic.into(),
        version,
        payload: payload.clone(),
        created,
    })
}

/// Why a delivery did not go through.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConsumeError {
    /// Transient: tried again after a back-off, dead after [`MAX_ATTEMPTS`].
    Retry(String),
    /// Never valid (malformed, another workspace's object): dead at once.
    Reject(String),
}

impl From<StoreError> for ConsumeError {
    fn from(e: StoreError) -> Self {
        Self::Retry(e.to_string())
    }
}

impl From<rusqlite::Error> for ConsumeError {
    fn from(e: rusqlite::Error) -> Self {
        Self::Retry(e.to_string())
    }
}

/// Run after the receipt committed (spawning what the consumer stored). Lost on a crash.
pub type AfterCommit = Box<dyn FnOnce() + Send>;

/// What a delivery did: `outcome` goes on the receipt (“没有监听”, “evt_…”), so a delivery that
/// changed nothing is still accounted for.
pub struct Applied {
    pub outcome: String,
    pub after: Option<AfterCommit>,
}

impl Applied {
    pub fn noted(outcome: impl Into<String>) -> Self {
        Self {
            outcome: outcome.into(),
            after: None,
        }
    }

    pub fn then(outcome: impl Into<String>, after: impl FnOnce() + Send + 'static) -> Self {
        Self {
            outcome: outcome.into(),
            after: Some(Box::new(after)),
        }
    }
}

/// Where a consumer's first registration starts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Since {
    /// Every event of its topic still in the outbox: for consumers shipped with their topic, so
    /// nothing emitted before the registration (by this version) is skipped.
    Start,
    /// From the current end: a consumer added later; earlier events only by an explicit backfill.
    Now,
}

/// One subscriber of one topic.
pub trait Consumer: Send + Sync {
    /// Stable id (`automation.item_trigger`); receipts and the registration are keyed on it.
    fn id(&self) -> &'static str;
    fn topic(&self) -> &'static str;
    /// Where its first registration starts.
    fn since(&self) -> Since {
        Since::Now
    }
    /// Outside any transaction, before the receipt; idempotent.
    fn effect(&self, _ev: &Event) -> std::result::Result<(), ConsumeError> {
        Ok(())
    }
    /// In the receipt's transaction: only `tx`, no other store call (the write lock is held), no I/O.
    fn apply(&self, _tx: &Tx, _ev: &Event) -> std::result::Result<Applied, ConsumeError> {
        Ok(Applied::noted(""))
    }
}

/// The due deliveries of consumer `?1` (topic `?2`) at `?3` for dispatcher `?4`: not received, not dead,
/// not backing off, not leased to another dispatcher; `?5` binds the caller's LIMIT.
const DUE: &str = "SELECT e.seq, e.id, e.workspace_id, e.topic, e.version, e.payload, e.created
       FROM outbox_events e JOIN outbox_consumers k ON k.id = ?1 AND k.enabled = 1
      WHERE e.topic = ?2 AND e.seq > k.since_seq
        AND NOT EXISTS (SELECT 1 FROM outbox_receipts r WHERE r.event_seq = e.seq AND r.consumer_id = ?1)
        AND NOT EXISTS (SELECT 1 FROM outbox_leases l WHERE l.event_seq = e.seq AND l.consumer_id = ?1
                          AND (l.dead = 1 OR l.next_at > ?3 OR (l.lease_until > ?3 AND l.owner IS NOT ?4)))
        AND ?5 = ?5
      ORDER BY e.seq";

/// What one [`Dispatcher::pump`] did.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Pumped {
    pub done: usize,
    pub retried: usize,
    pub dead: usize,
    /// Claimed, but another dispatcher held it by the time it would commit.
    pub fenced: usize,
}

impl Pumped {
    pub fn any(&self) -> bool {
        self.done + self.retried + self.dead + self.fenced > 0
    }
}

/// Delivers events to consumers. One per process; `owner` tells its leases apart from another
/// process's during a release switch.
pub struct Dispatcher {
    owner: String,
    consumers: Vec<Arc<dyn Consumer>>,
}

enum Failed {
    Store(StoreError),
    Consume(ConsumeError),
    Fenced,
}

impl From<StoreError> for Failed {
    fn from(e: StoreError) -> Self {
        Self::Store(e)
    }
}

impl Dispatcher {
    pub fn new(consumers: Vec<Arc<dyn Consumer>>) -> Self {
        Self {
            owner: uid("disp"),
            consumers,
        }
    }

    pub fn owner(&self) -> &str {
        &self.owner
    }

    pub fn consumers(&self) -> impl Iterator<Item = &dyn Consumer> {
        self.consumers.iter().map(|c| c.as_ref())
    }

    /// Registers the consumers not yet known (from [`Consumer::since`]); known ones keep their place.
    /// A known id with another topic is refused: a consumer that changes what it means takes a new id
    /// (or an explicit migration), so claiming and pruning never disagree about what it owes.
    pub fn register(&self, store: &Store) -> Result<()> {
        store.transaction(|tx| {
            for c in &self.consumers {
                let known: Option<String> = tx
                    .query_row(
                        "SELECT topic FROM outbox_consumers WHERE id = ?",
                        [c.id()],
                        |r| r.get(0),
                    )
                    .optional()?;
                match known {
                    Some(t) if t == c.topic() => {}
                    Some(t) => {
                        return Err(StoreError::Hook(format!(
                            "outbox consumer {} is registered for {t}, not {}",
                            c.id(),
                            c.topic()
                        )));
                    }
                    None => {
                        let since: i64 = match c.since() {
                            Since::Start => 0,
                            Since::Now => tx.query_row(
                                "SELECT coalesce(max(seq), 0) FROM outbox_events",
                                [],
                                |r| r.get(0),
                            )?,
                        };
                        tx.execute(
                            "INSERT INTO outbox_consumers (id, topic, since_seq, enabled, registered) VALUES (?, ?, ?, 1, ?)",
                            params![c.id(), c.topic(), since, crate::now()],
                        )?;
                    }
                }
            }
            Ok(())
        })
    }

    /// One pass: every consumer, up to [`BATCH`] due deliveries each.
    pub fn pump(&self, store: &Store, now: i64) -> Result<Pumped> {
        let Some(_activity) = store.try_activity("outbox") else {
            return Ok(Pumped::default());
        };
        let mut out = Pumped::default();
        for c in &self.consumers {
            for ev in self.claim(store, c.as_ref(), now)? {
                self.deliver(store, c.as_ref(), &ev, now, &mut out)?;
            }
        }
        Ok(out)
    }

    /// Leases the due deliveries of `c`: not received, not dead, not backing off, not leased to
    /// another dispatcher.
    fn claim(&self, store: &Store, c: &dyn Consumer, now: i64) -> Result<Vec<Event>> {
        // idle passes stay off the write lock: a pooled read first, the claim re-checks under the lock
        let due = store.reader(|r| -> Result<bool> {
            let mut st = r.prepare_cached(&format!("SELECT EXISTS ({DUE} LIMIT 1)"))?;
            Ok(
                st.query_row(params![c.id(), c.topic(), now, self.owner, 1_i64], |r| {
                    r.get(0)
                })?,
            )
        })?;
        if !due {
            return Ok(Vec::new());
        }
        store.transaction(|tx| {
            let evs = {
                let mut st = tx.prepare_cached(&format!("{DUE} LIMIT ?5"))?;
                st.query_map(
                    params![c.id(), c.topic(), now, self.owner, BATCH as i64],
                    event_row,
                )?
                .collect::<rusqlite::Result<Vec<_>>>()?
            };
            for e in &evs {
                tx.execute(
                    "INSERT INTO outbox_leases (event_seq, consumer_id, owner, lease_until, attempts, updated)
                     VALUES (?1, ?2, ?3, ?4, 1, ?5)
                     ON CONFLICT (event_seq, consumer_id) DO UPDATE
                       SET owner = ?3, lease_until = ?4, attempts = attempts + 1, updated = ?5",
                    params![e.seq, c.id(), self.owner, now + LEASE_MS, now],
                )?;
            }
            Ok(evs)
        })
    }

    fn deliver(
        &self,
        store: &Store,
        c: &dyn Consumer,
        ev: &Event,
        now: i64,
        out: &mut Pumped,
    ) -> Result<()> {
        if let Err(e) = c.effect(ev) {
            return self.failed(store, c, ev, e, now, out);
        }
        let done = store.transaction(|tx| -> std::result::Result<Option<AfterCommit>, Failed> {
            // fencing: the lease must still be ours (a slow pass loses it to another dispatcher)
            let mine: Option<String> = tx
                .query_row(
                    "SELECT owner FROM outbox_leases WHERE event_seq = ? AND consumer_id = ?",
                    params![ev.seq, c.id()],
                    |r| r.get(0),
                )
                .optional()
                .map_err(StoreError::from)?
                .flatten();
            if mine.as_deref() != Some(self.owner.as_str()) {
                return Err(Failed::Fenced);
            }
            let fresh = tx
                .execute(
                    "INSERT OR IGNORE INTO outbox_receipts (event_seq, consumer_id, workspace_id, done, outcome) VALUES (?, ?, ?, ?, '')",
                    params![ev.seq, c.id(), ev.workspace_id, now],
                )
                .map_err(StoreError::from)?
                == 1;
            let after = if fresh {
                let a = c.apply(tx, ev).map_err(Failed::Consume)?;
                tx.execute(
                    "UPDATE outbox_receipts SET outcome = ? WHERE event_seq = ? AND consumer_id = ?",
                    params![a.outcome, ev.seq, c.id()],
                )
                .map_err(StoreError::from)?;
                a.after
            } else {
                None
            };
            tx.execute(
                "DELETE FROM outbox_leases WHERE event_seq = ? AND consumer_id = ?",
                params![ev.seq, c.id()],
            )
            .map_err(StoreError::from)?;
            Ok(after)
        });
        match done {
            Ok(after) => {
                out.done += 1;
                if let Some(f) = after {
                    f();
                }
                Ok(())
            }
            Err(Failed::Fenced) => {
                out.fenced += 1;
                Ok(())
            }
            Err(Failed::Consume(e)) => self.failed(store, c, ev, e, now, out),
            Err(Failed::Store(e)) => {
                self.failed(store, c, ev, ConsumeError::Retry(e.to_string()), now, out)
            }
        }
    }

    /// Records a failed attempt: back off, or dead.
    fn failed(
        &self,
        store: &Store,
        c: &dyn Consumer,
        ev: &Event,
        e: ConsumeError,
        now: i64,
        out: &mut Pumped,
    ) -> Result<()> {
        let (reject, msg) = match &e {
            ConsumeError::Reject(m) => (true, m.clone()),
            ConsumeError::Retry(m) => (false, m.clone()),
        };
        let dead = store.transaction(|tx| -> Result<bool> {
            let attempts: i64 = tx
                .query_row(
                    "SELECT attempts FROM outbox_leases WHERE event_seq = ? AND consumer_id = ? AND owner IS ?",
                    params![ev.seq, c.id(), self.owner],
                    |r| r.get(0),
                )
                .optional()?
                .unwrap_or(0);
            if attempts == 0 {
                // the lease went to another dispatcher meanwhile; it reports its own outcome
                return Ok(false);
            }
            let dead = reject || attempts >= MAX_ATTEMPTS;
            let next = now + backoff(attempts);
            tx.execute(
                "UPDATE outbox_leases SET owner = NULL, lease_until = 0, last_error = ?, next_at = ?, dead = ?, updated = ?
                  WHERE event_seq = ? AND consumer_id = ?",
                params![msg, next, dead, now, ev.seq, c.id()],
            )?;
            Ok(dead)
        })?;
        if dead {
            out.dead += 1;
            tracing::error!(consumer = c.id(), event = %ev.id, "outbox delivery dead: {msg}");
        } else {
            out.retried += 1;
            tracing::warn!(consumer = c.id(), event = %ev.id, "outbox delivery failed, will retry: {msg}");
        }
        Ok(())
    }
}

/// `2^attempts` seconds, at most [`BACKOFF_MAX_MS`].
pub fn backoff(attempts: i64) -> i64 {
    let s = 1_i64 << attempts.clamp(0, 20);
    (s * 1000).min(BACKOFF_MAX_MS)
}

fn event_row(r: &rusqlite::Row) -> rusqlite::Result<Event> {
    let payload: String = r.get(5)?;
    Ok(Event {
        seq: r.get(0)?,
        id: r.get(1)?,
        workspace_id: r.get(2)?,
        topic: r.get(3)?,
        version: r.get(4)?,
        payload: serde_json::from_str(&payload).unwrap_or(Value::Null),
        created: r.get(6)?,
    })
}

/// The outbox as healthz and the release check see it.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Stats {
    /// Deliveries a registered consumer still owes (paused and dead ones included).
    pub pending: i64,
    pub dead: i64,
    /// `created` of the oldest owed delivery.
    pub oldest: Option<i64>,
}

/// Owed deliveries over all registered consumers (a paused one still owes: pausing is not unsubscribing).
pub fn stats(c: &Connection) -> Result<Stats> {
    let (pending, oldest): (i64, Option<i64>) = c.query_row(
        "SELECT count(*), min(e.created) FROM outbox_events e
           JOIN outbox_consumers k ON k.topic = e.topic AND e.seq > k.since_seq
          WHERE 1
            AND NOT EXISTS (SELECT 1 FROM outbox_receipts r WHERE r.event_seq = e.seq AND r.consumer_id = k.id)",
        [],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    let dead: i64 = c.query_row(
        "SELECT count(*) FROM outbox_leases WHERE dead = 1",
        [],
        |r| r.get(0),
    )?;
    Ok(Stats {
        pending,
        dead,
        oldest,
    })
}

/// Deletes events older than `keep_ms` that every registered consumer of their topic (paused ones
/// included) has a receipt for, with those receipts. Owed, leased or dead deliveries are never removed.
/// Whether consumer `id` was ever registered (and not unregistered).
pub fn is_registered(c: &Connection, id: &str) -> Result<bool> {
    Ok(c.query_row(
        "SELECT EXISTS (SELECT 1 FROM outbox_consumers WHERE id = ?)",
        [id],
        |r| r.get(0),
    )?)
}

/// Removes consumer `id` with everything it owed and its receipts: a consumer the Hub no longer
/// runs, whose backlog would otherwise be kept forever. Registering it again starts it afresh.
pub fn unregister(store: &Store, id: &str) -> Result<()> {
    store.transaction(|tx| {
        tx.execute("DELETE FROM outbox_leases WHERE consumer_id = ?", [id])?;
        tx.execute("DELETE FROM outbox_receipts WHERE consumer_id = ?", [id])?;
        tx.execute("DELETE FROM outbox_consumers WHERE id = ?", [id])?;
        Ok(())
    })
}

pub fn prune(store: &Store, now: i64, keep_ms: i64) -> Result<usize> {
    // an event is done when no registered consumer of its topic still owes it and nothing is leased
    const DONE: &str = "e.created < ?1
        AND NOT EXISTS (SELECT 1 FROM outbox_leases l WHERE l.event_seq = e.seq)
        AND NOT EXISTS (SELECT 1 FROM outbox_consumers k WHERE k.topic = e.topic AND e.seq > k.since_seq
                          AND NOT EXISTS (SELECT 1 FROM outbox_receipts r WHERE r.event_seq = e.seq AND r.consumer_id = k.id))";
    // In batches, each its own transaction, so the writer lock is never held for a whole week of
    // events at once. The seqs are collected before deleting: removing receipts first would change
    // what DONE matches.
    let mut total = 0;
    loop {
        let n = store.transaction(|tx| {
            let seqs: Vec<i64> = {
                let mut st = tx.prepare_cached(&format!(
                    "SELECT e.seq FROM outbox_events e WHERE {DONE} ORDER BY e.seq LIMIT {PRUNE_BATCH}"
                ))?;
                st.query_map([now - keep_ms], |r| r.get(0))?
                    .collect::<rusqlite::Result<_>>()?
            };
            let mut receipts = tx.prepare_cached("DELETE FROM outbox_receipts WHERE event_seq = ?")?;
            let mut events = tx.prepare_cached("DELETE FROM outbox_events WHERE seq = ?")?;
            for seq in &seqs {
                receipts.execute([seq])?;
                events.execute([seq])?;
            }
            Ok::<_, StoreError>(seqs.len())
        })?;
        total += n;
        if n < PRUNE_BATCH {
            break;
        }
    }
    // prune runs hourly: a good moment to refresh the planner's statistics too
    if let Err(e) = store.optimize() {
        tracing::warn!(error = %e, "PRAGMA optimize failed");
    }
    Ok(total)
}

/// Events [`prune`] deletes per transaction.
const PRUNE_BATCH: usize = 500;
