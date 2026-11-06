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
//! The attempt row (`outbox_leases`) holds attempts, the last error, the back-off and since when the
//! delivery has been failing. A [`ConsumeError::Reject`] makes it dead at once; [`ConsumeError::Retry`]
//! failures only once they have lasted [`DEAD_AFTER_MS`] (a Wi‑Fi drop or a short GitHub outage must
//! not lose an effect for good). Dead is never retried by itself and never removed by [`prune`]; a
//! person looks at it ([`dead`]) and sends it again ([`revive`]). Rows are scoped: the receipt carries
//! the event's workspace, and consumers check that the objects they touch belong to it.
//!
//! One Hub process owns the database, so there is no lease ownership or fencing: two passes of the
//! same consumer must not run at once (the Hub runs one dispatcher job, [`Dispatcher::pump`] is not
//! re-entrant). Were they to overlap, the receipt still keeps `apply` to once; only `effect` would run
//! twice, which it must tolerate anyway. Several Hubs on one database would need the lease back.
//!
//! A consumer receives the events created after its first registration ([`Dispatcher::register`],
//! `since_seq`); history is replayed only by an explicit backfill. Register before anything can emit.

use std::sync::Arc;

use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{Result, Store, StoreError, Tx, uid};

/// How long [`ConsumeError::Retry`] failures may last before the delivery is dead.
pub const DEAD_AFTER_MS: i64 = 24 * 3_600_000;
/// Attempts after which other retry loops that borrow [`backoff`] give up (GitHub's effects and
/// webhook deliveries); the outbox itself dies by time ([`DEAD_AFTER_MS`]).
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
    /// Transient: tried again after a back-off, dead once failing for [`DEAD_AFTER_MS`].
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

/// The due deliveries of consumer `?1` (topic `?2`) at `?3`: not received, not dead, not backing off;
/// `?4` binds the caller's LIMIT.
const DUE: &str = "SELECT e.seq, e.id, e.workspace_id, e.topic, e.version, e.payload, e.created
       FROM outbox_events e JOIN outbox_consumers k ON k.id = ?1 AND k.enabled = 1
      WHERE e.topic = ?2 AND e.seq > k.since_seq
        AND NOT EXISTS (SELECT 1 FROM outbox_receipts r WHERE r.event_seq = e.seq AND r.consumer_id = ?1)
        AND NOT EXISTS (SELECT 1 FROM outbox_leases l WHERE l.event_seq = e.seq AND l.consumer_id = ?1
                          AND (l.dead = 1 OR l.next_at > ?3))
        AND ?4 = ?4
      ORDER BY e.seq";

/// What one [`Dispatcher::pump`] did.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Pumped {
    pub done: usize,
    pub retried: usize,
    pub dead: usize,
    /// The consumers (in a workspace) that had no dead delivery before this pass and have one now:
    /// what the owner hears about once, until [`revive`] clears them.
    pub newly_dead: Vec<DeadGroup>,
}

impl Pumped {
    pub fn any(&self) -> bool {
        self.done + self.retried + self.dead > 0
    }
}

/// Dead deliveries of one consumer (in one workspace, for a notice; over all of them, for the view).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DeadGroup {
    pub consumer: String,
    pub topic: String,
    /// `None` when grouped over every workspace.
    pub workspace_id: Option<String>,
    pub count: i64,
    pub last_error: Option<String>,
    /// When the newest of them was given up on.
    pub updated: i64,
}

/// Delivers events to consumers. One per process, and one pass at a time (see the module docs).
pub struct Dispatcher {
    consumers: Vec<Arc<dyn Consumer>>,
}

enum Failed {
    Store(StoreError),
    Consume(ConsumeError),
}

impl From<StoreError> for Failed {
    fn from(e: StoreError) -> Self {
        Self::Store(e)
    }
}

impl Dispatcher {
    pub fn new(consumers: Vec<Arc<dyn Consumer>>) -> Self {
        Self { consumers }
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

    /// One pass: every consumer, up to [`BATCH`] due deliveries each. Never two at once.
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

    /// Takes the due deliveries of `c` (not received, not dead, not backing off) and counts the
    /// attempt before it is made, so one cut short by a crash still counts.
    fn claim(&self, store: &Store, c: &dyn Consumer, now: i64) -> Result<Vec<Event>> {
        // idle passes stay off the write lock: a pooled read first, the claim re-checks under the lock
        let due = store.reader(|r| -> Result<bool> {
            let mut st = r.prepare_cached(&format!("SELECT EXISTS ({DUE} LIMIT 1)"))?;
            Ok(st.query_row(params![c.id(), c.topic(), now, 1_i64], |r| r.get(0))?)
        })?;
        if !due {
            return Ok(Vec::new());
        }
        store.transaction(|tx| {
            let evs = {
                let mut st = tx.prepare_cached(&format!("{DUE} LIMIT ?4"))?;
                st.query_map(params![c.id(), c.topic(), now, BATCH as i64], event_row)?
                    .collect::<rusqlite::Result<Vec<_>>>()?
            };
            for e in &evs {
                tx.execute(
                    "INSERT INTO outbox_leases (event_seq, consumer_id, attempts, updated)
                     VALUES (?1, ?2, 1, ?3)
                     ON CONFLICT (event_seq, consumer_id) DO UPDATE
                       SET attempts = attempts + 1, updated = ?3",
                    params![e.seq, c.id(), now],
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
            Err(Failed::Consume(e)) => self.failed(store, c, ev, e, now, out),
            Err(Failed::Store(e)) => {
                self.failed(store, c, ev, ConsumeError::Retry(e.to_string()), now, out)
            }
        }
    }

    /// Records a failed attempt: back off, or dead (a rejection, or failing for [`DEAD_AFTER_MS`]).
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
        let (dead, first) = store.transaction(|tx| -> Result<(bool, bool)> {
            let row: Option<(i64, Option<i64>)> = tx
                .query_row(
                    "SELECT attempts, failing_since FROM outbox_leases WHERE event_seq = ? AND consumer_id = ?",
                    params![ev.seq, c.id()],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .optional()?;
            let (attempts, since) = row.unwrap_or((1, None));
            let since = since.unwrap_or(now);
            let dead = reject || now - since >= DEAD_AFTER_MS;
            // the first dead delivery of this consumer in this workspace since the last revive
            let first = dead
                && !tx.query_row(
                    "SELECT EXISTS (SELECT 1 FROM outbox_leases l JOIN outbox_events e ON e.seq = l.event_seq
                      WHERE l.consumer_id = ? AND l.dead = 1 AND e.workspace_id = ? AND l.event_seq <> ?)",
                    params![c.id(), ev.workspace_id, ev.seq],
                    |r| r.get::<_, bool>(0),
                )?;
            tx.execute(
                "INSERT INTO outbox_leases (event_seq, consumer_id, attempts, last_error, next_at, dead, failing_since, updated)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
                 ON CONFLICT (event_seq, consumer_id) DO UPDATE
                   SET last_error = ?4, next_at = ?5, dead = ?6, failing_since = ?7, updated = ?8",
                params![ev.seq, c.id(), attempts, msg, now + backoff(attempts), dead, since, now],
            )?;
            Ok((dead, first))
        })?;
        if dead {
            out.dead += 1;
            if first {
                out.newly_dead.push(DeadGroup {
                    consumer: c.id().into(),
                    topic: c.topic().into(),
                    workspace_id: Some(ev.workspace_id.clone()),
                    count: 1,
                    last_error: Some(msg.clone()),
                    updated: now,
                });
            }
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

/// The dead deliveries, one group per consumer over every workspace, newest first.
pub fn dead(c: &Connection) -> Result<Vec<DeadGroup>> {
    let mut st = c.prepare_cached(
        "SELECT l.consumer_id, coalesce(k.topic, ''), count(*), max(l.updated),
                (SELECT x.last_error FROM outbox_leases x WHERE x.consumer_id = l.consumer_id AND x.dead = 1
                  ORDER BY x.updated DESC, x.event_seq DESC LIMIT 1)
           FROM outbox_leases l LEFT JOIN outbox_consumers k ON k.id = l.consumer_id
          WHERE l.dead = 1
          GROUP BY l.consumer_id
          ORDER BY max(l.updated) DESC",
    )?;
    let rows = st
        .query_map([], |r| {
            Ok(DeadGroup {
                consumer: r.get(0)?,
                topic: r.get(1)?,
                workspace_id: None,
                count: r.get(2)?,
                updated: r.get(3)?,
                last_error: r.get(4)?,
            })
        })?
        .collect::<rusqlite::Result<_>>()?;
    Ok(rows)
}

/// Sends the dead deliveries (of `consumer`, or all) again: attempts and the failure clock start
/// over, so a delivery that keeps failing has another [`DEAD_AFTER_MS`]. Returns how many.
pub fn revive(store: &Store, consumer: Option<&str>) -> Result<usize> {
    let n = store.transaction(|tx| -> Result<usize> {
        Ok(tx.execute(
            "UPDATE outbox_leases SET dead = 0, attempts = 0, next_at = 0, failing_since = NULL, updated = ?1
              WHERE dead = 1 AND (?2 IS NULL OR consumer_id = ?2)",
            params![crate::now(), consumer],
        )?)
    })?;
    if n > 0 {
        store.outbox_wake().notify_one();
    }
    Ok(n)
}

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

/// Deletes events older than `keep_ms` that every registered consumer of their topic (paused ones
/// included) has a receipt for, with those receipts. Owed, failing or dead deliveries are never removed.
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
