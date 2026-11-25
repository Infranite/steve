//! The durable outbox (migration 21): a message is written in the same transaction as the change that
//! caused it and delivered at least once afterwards, so a crash between commit and send loses nothing.
//!
//! A domain records a fact in the same [`Store::write`] as the state change it describes
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
//! not lose an effect for good). Dead is never retried by itself and never removed by retention; a
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

mod leases;
mod receipts;

use std::sync::Arc;

use rusqlite::Connection;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{Result, Store, StoreError, uid};

/// How long [`ConsumeError::Retry`] failures may last before the delivery is dead.
pub const DEAD_AFTER_MS: i64 = 24 * 3_600_000;
/// Attempts after which other retry loops that borrow [`backoff`] give up (GitHub's effects and
/// webhook deliveries); the outbox itself dies by time ([`DEAD_AFTER_MS`]).
pub const MAX_ATTEMPTS: i64 = 8;
/// Deliveries claimed per consumer and pass.
pub const BATCH: usize = 50;
/// The longest back-off between attempts.
pub const BACKOFF_MAX_MS: i64 = 10 * 60_000;
/// Fully delivered events are kept this long (candidate value), then removed by background retention.
pub use steve_store_sqlite::retention::policies::OUTBOX_KEEP_MS as KEEP_MS;

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

/// Records a durable fact inside the caller's write transaction.
pub fn append(
    w: &mut crate::W<'_>,
    ws: &str,
    topic: &str,
    version: i64,
    payload: &Value,
) -> Result<Event> {
    use crate::contract::Q;
    let event = append_on(w.db(), ws, topic, version, payload)?;
    w.outbox_changed();
    Ok(event)
}

fn append_on(
    db: &mut dyn crate::contract::Backend,
    ws: &str,
    topic: &str,
    version: i64,
    payload: &Value,
) -> Result<Event> {
    let id = uid("evt");
    let created = crate::now();
    let statement = sea_query::Query::insert()
        .into_table("outbox_events")
        .columns([
            "id",
            "workspace_id",
            "topic",
            "version",
            "payload",
            "created",
        ])
        .values_panic([
            id.as_str().into(),
            ws.into(),
            topic.into(),
            version.into(),
            payload.to_string().into(),
            created.into(),
        ])
        .returning_col("seq")
        .take();
    let (sql, values) = db.dialect().render(&statement)?;
    let mut seq = None;
    db.query(&sql, &values, &mut |row| {
        seq = Some(row.get(0)?);
        Ok(())
    })?;
    Ok(Event {
        seq: crate::repo::must(seq)?,
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
    /// In the receipt's transaction: only `w`, no other store call (the write lock is held), no I/O.
    fn apply(
        &self,
        _w: &mut crate::W<'_>,
        _ev: &Event,
    ) -> std::result::Result<Applied, ConsumeError> {
        Ok(Applied::noted(""))
    }
}

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
        store.write(|w| {
            for consumer in &self.consumers {
                leases::register(w, consumer.as_ref())?;
            }
            Ok(())
        })
    }

    /// Earliest due timestamp (ms) across all registered, enabled consumers, ignoring dead rows.
    /// `Some(0)` means an unattempted delivery is due immediately; `None` means nothing is pending.
    pub fn next_due(&self, store: &Store) -> Result<Option<i64>> {
        store.read(|r| {
            let mut earliest: Option<i64> = None;
            for c in &self.consumers {
                if let Some(at) = leases::next_due(r, c.as_ref())? {
                    earliest = Some(earliest.map_or(at, |prev| prev.min(at)));
                }
            }
            Ok(earliest)
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
        if !store.read(|r| leases::has_due(r, c, now))? {
            return Ok(Vec::new());
        }
        store.write(|w| leases::claim(w, c, now))
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
        let done = store.write(|w| -> std::result::Result<Option<AfterCommit>, Failed> {
            let fresh = receipts::claim(w, c.id(), ev, now)?;
            let after = if fresh {
                let a = c.apply(w, ev).map_err(Failed::Consume)?;
                receipts::outcome(w, c.id(), ev.seq, &a.outcome)?;
                a.after
            } else {
                None
            };
            receipts::release(w, c.id(), ev.seq)?;
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
        let (dead, first) = store.write(|w| leases::fail(w, c.id(), ev, now, reject, &msg))?;
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
pub(crate) fn stats_on(c: &Connection) -> Result<Stats> {
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
pub(crate) fn dead_on(c: &Connection) -> Result<Vec<DeadGroup>> {
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
    store.write(|w| leases::revive(w, consumer, crate::now()))
}

/// Whether consumer `id` was ever registered (and not unregistered).
pub(crate) fn is_registered_on(c: &Connection, id: &str) -> Result<bool> {
    Ok(c.query_row(
        "SELECT EXISTS (SELECT 1 FROM outbox_consumers WHERE id = ?)",
        [id],
        |r| r.get(0),
    )?)
}

/// Removes consumer `id` with everything it owed and its receipts: a consumer the Hub no longer
/// runs, whose backlog would otherwise be kept forever. Registering it again starts it afresh.
pub fn unregister(store: &Store, id: &str) -> Result<()> {
    store.write(|w| leases::unregister(w, id))
}

pub(crate) fn latest_sequence(r: &mut impl crate::Read) -> Result<i64> {
    use sea_query::{Expr, Func, Query};
    Ok(r.one(
        Query::select()
            .expr(Func::max(Expr::col("seq")))
            .from("outbox_events"),
        |row| row.get::<Option<i64>>(0),
    )?
    .flatten()
    .unwrap_or_default())
}

#[cfg(feature = "testkit")]
pub(crate) fn record_test_receipt(
    w: &mut crate::W<'_>,
    event: &Event,
    consumer: &str,
    outcome: &str,
) -> Result<()> {
    if !receipts::claim(w, consumer, event, 1)? {
        return Err(StoreError::Rejected("outbox receipt already exists".into()));
    }
    receipts::outcome(w, consumer, event.seq, outcome)
}

pub fn stats(r: &mut impl crate::Read) -> Result<Stats> {
    stats_on(r.connection())
}

pub fn dead(r: &mut impl crate::Read) -> Result<Vec<DeadGroup>> {
    dead_on(r.connection())
}

pub fn is_registered(r: &mut impl crate::Read, id: &str) -> Result<bool> {
    is_registered_on(r.connection(), id)
}
