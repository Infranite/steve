//! Reconciling the Hub's inbox with the Node's streams (N1-7): before turns are planned, every head
//! of the Node's snapshot is compared with what the Hub holds of that stream and epoch.
//!
//! - The Hub holds less than the Node produced: the rest is pulled by replay into the inbox, and the
//!   coordinator waits (bounded) until it is applied.
//! - The Hub holds less than the Node already dropped (`gap`: the Hub's database went back, a
//!   restored backup): what is missing cannot be had any more. The stream is faulted, its session
//!   noted and held.
//! - The Hub holds more than the Node produced in the same epoch (`ahead`: the Node's journal went
//!   back): faulted the same way, once the Node's current head (read after the Hub's) confirms it:
//!   events pushed while the snapshot was read move the Hub past the snapshot's head.
//! - The Hub holds more than the Node heard acknowledged (an ack lost with a Hub restart): the Node
//!   is told now, so it drops its copy.
//! - An epoch the Hub holds that the Node no longer has is left as it is: a new journal's epoch is
//!   another stream.
//!
//! A remote Node's replayed events go through the same check as its pushed ones (its own work
//! only, [`ReplayCheck`]); a refused batch stops that stream's replay, not the reconcile. An event
//! too big for a normal page is asked for alone in the largest page the transport carries; one
//! that does not fit there either is left to push. A stream whose events are not applied within
//! [`APPLY_WAIT`] holds its session only: reconcile completes for the rest of the Node.
//!
//! [`catch_up_streams`] is the one place for it: the coordinator's reconcile and the link's
//! catch-up (a Node with no coordinator running) both call it.

use super::*;
use std::time::Instant;

/// One replay page: what one message over a remote Node's link carries (a 1 MiB page would not
/// fit in its frame).
const REPLAY_BYTES: u32 = steve_node_wire::envelope::BULK_CAP as u32;
/// How long reconcile waits for what the Hub holds of the Node's heads to be applied. Past it, the
/// sessions of the streams still behind are held (with a note) until their streams catch up;
/// reconcile completes for the rest.
pub(super) const APPLY_WAIT: Duration = Duration::from_secs(30);

/// The Node's ledger and stream heads, as of the one moment of one snapshot (all its pages).
pub struct NodeSnapshot {
    /// By operation id.
    pub entries: HashMap<String, SnapshotEntry>,
    pub heads: Vec<StreamHead>,
}

/// Reads every page of one snapshot; a snapshot that expired while it was paged is read again
/// (three times at most).
pub async fn read_snapshot(node: &dyn Node, limit: u32) -> ApiResult<NodeSnapshot> {
    let mut entries: HashMap<String, SnapshotEntry> = HashMap::new();
    let (mut cursor, mut restarts) = (None, 0);
    loop {
        match node
            .snapshot(PageRequest {
                cursor: cursor.clone(),
                limit,
            })
            .await
        {
            Ok(page) => {
                for e in page.entries {
                    entries.insert(e.receipt.operation.to_string(), e);
                }
                match page.next {
                    Some(c) => cursor = Some(c),
                    // every page shows the same heads
                    None => {
                        return Ok(NodeSnapshot {
                            entries,
                            heads: page.heads,
                        });
                    }
                }
            }
            Err(e) if e.code == ErrorCode::Expired && restarts < 3 => {
                restarts += 1;
                entries.clear();
                cursor = None;
            }
            Err(e) => return Err(e),
        }
    }
}

/// Takes a replayed batch into the Hub's inbox and answers what is durable ([`NodeRuntime::receive`]).
/// `Denied`: the batch is refused (another Node's work); nothing of it was taken in.
pub type Receive<'a> = &'a (dyn Fn(&EventBatch) -> ApiResult<EventAck> + Sync);

/// What a remote Node's replayed batch must pass before it reaches the inbox: the link's guard
/// (its own sessions, attempts and operations only). `Err`: refused, nothing taken in.
pub type ReplayCheck = Arc<dyn Fn(&EventBatch) -> ApiResult<()> + Send + Sync>;

/// How what the Hub lacks is pulled by replay.
#[derive(Clone, Copy)]
pub struct Pull<'a> {
    pub receive: Receive<'a>,
    /// The largest page the Node's transport carries: an event too big for a normal page is asked
    /// for alone in one this size.
    pub largest: u32,
}

/// What [`catch_up_streams`] did.
#[derive(Debug, Default)]
pub struct CaughtUp {
    /// Events pulled by replay.
    pub replayed: usize,
    /// Streams faulted now (gap or ahead).
    pub faulted: usize,
    /// Streams whose Hub copy reaches the Node's head: (stream, epoch, head). What is held up to
    /// there is applied before turns are planned.
    pub reached: Vec<(String, String, u64)>,
}

/// One head against the Hub's inbox.
enum Head {
    /// The Hub holds `hub` of it; replay from there up to the head (nothing to pull if equal).
    Behind { hub: u64 },
    /// The Hub holds `hub`, more than the head says: the Node's current head decides
    /// ([`confirm_ahead`]).
    Ahead { hub: u64 },
    /// Faulted, now or before.
    Faulted,
}

/// Compares the Node's `heads` with the Hub's inbox and brings the inbox up to them: faults what
/// cannot be reconciled (with a note on its session), tells the Node what the Hub holds that it
/// did not hear acknowledged, and, with `receive`, pulls what the Hub lacks by replay. Without
/// `receive` (no coordinator to take events) what the Hub lacks stays in the Node's journal for the
/// coordinator's own reconcile.
pub async fn catch_up_streams(
    store: &Store,
    node: &dyn Node,
    heads: &[StreamHead],
    pull_with: Option<Pull<'_>>,
) -> ApiResult<CaughtUp> {
    let internal = |e: StoreError| ApiError::new(ErrorCode::Internal, e.to_string());
    let node_id = node.id().clone();
    let mut out = CaughtUp::default();
    let looked: Vec<(StreamHead, Head, bool)> = store
        .transaction(|tx| {
            heads
                .iter()
                .map(|h| {
                    let (head, fresh) = compare_in(tx, &node_id, h)?;
                    Ok((h.clone(), head, fresh))
                })
                .collect::<Result<Vec<_>, StoreError>>()
        })
        .map_err(internal)?;
    for (h, head, fresh) in looked {
        if fresh {
            out.faulted += 1;
        }
        let (h, hub) = match head {
            Head::Faulted => continue,
            Head::Behind { hub } => (h, hub),
            Head::Ahead { hub } => match confirm_ahead(store, node, &h, hub, &mut out).await? {
                Some(now) => (now, hub),
                None => continue,
            },
        };
        if hub > h.acknowledged {
            // the Hub holds it, the Node never heard so (the ack was lost with a restart)
            tell(
                node,
                Watermark {
                    stream: h.stream.clone(),
                    epoch: h.epoch.clone(),
                    seq: hub,
                },
            )
            .await;
        }
        if hub >= h.produced {
            out.reached
                .push((h.stream.to_string(), h.epoch.to_string(), h.produced));
            continue;
        }
        let Some(with) = pull_with else { continue };
        if pull(store, node, &h, hub, with, &mut out).await? {
            out.reached
                .push((h.stream.to_string(), h.epoch.to_string(), h.produced));
        }
    }
    Ok(out)
}

/// The Hub held `hub` of `h`'s stream, more than the snapshot's head. The Node may have produced
/// (and pushed) more since the snapshot was read, so its current head decides: `hub` was read
/// before it, and a Node that still produced less in this epoch went back (`ahead`, faulted).
/// `Some`: the current head, which reaches `hub`. `None`: faulted, or the Node's journal is another
/// one now (left as it is).
async fn confirm_ahead(
    store: &Store,
    node: &dyn Node,
    h: &StreamHead,
    hub: u64,
    out: &mut CaughtUp,
) -> ApiResult<Option<StreamHead>> {
    let now = node
        .streams()
        .await?
        .into_iter()
        .find(|x| x.stream == h.stream);
    let Some(now) = now.filter(|x| x.epoch == h.epoch) else {
        return Ok(None);
    };
    if now.produced >= hub {
        return Ok(Some(now));
    }
    let fresh = store
        .transaction(|tx| fault_in(tx, node.id(), &now, hub, rt::Fault::Ahead))
        .map_err(|e| ApiError::new(ErrorCode::Internal, e.to_string()))?;
    if fresh {
        out.faulted += 1;
    }
    Ok(None)
}

/// Replays `h`'s stream from `hub` up to its head into the inbox, acknowledging each page. False:
/// it stopped short (the stream faulted, the Node's journal is another one now, a batch was
/// refused, or an event does not fit in any page: push delivers it or nothing does).
async fn pull(
    store: &Store,
    node: &dyn Node,
    h: &StreamHead,
    hub: u64,
    with: Pull<'_>,
    out: &mut CaughtUp,
) -> ApiResult<bool> {
    let internal = |e: StoreError| ApiError::new(ErrorCode::Internal, e.to_string());
    let mut from = hub;
    let mut budget = REPLAY_BYTES;
    while from < h.produced {
        let mark = Watermark {
            stream: h.stream.clone(),
            epoch: h.epoch.clone(),
            seq: from,
        };
        let batch = match node.replay(mark.clone(), budget).await {
            Ok(b) => {
                budget = REPLAY_BYTES;
                b
            }
            // the next event alone is bigger than a page: once more in the largest one
            Err(e) if e.code == ErrorCode::Exhausted && budget < with.largest => {
                budget = with.largest;
                continue;
            }
            Err(e) if e.code == ErrorCode::Exhausted => {
                tracing::warn!(
                    "node {}: replay of {} from {from}: {e}; left to push",
                    node.id(),
                    h.stream
                );
                return Ok(false);
            }
            Err(e) if e.code == ErrorCode::Gap => {
                // the Node dropped what the Hub lacks, or its journal changed since the snapshot:
                // what both hold now decides
                let now = node
                    .streams()
                    .await?
                    .into_iter()
                    .find(|x| x.stream == h.stream);
                let Some(now) = now.filter(|x| x.epoch == h.epoch) else {
                    return Ok(false);
                };
                let (head, fresh) = store
                    .transaction(|tx| compare_in(tx, node.id(), &now))
                    .map_err(internal)?;
                if fresh {
                    out.faulted += 1;
                }
                let hub = match head {
                    Head::Behind { hub } => hub,
                    Head::Ahead { hub } => {
                        match confirm_ahead(store, node, &now, hub, out).await? {
                            Some(_) => hub,
                            None => return Ok(false),
                        }
                    }
                    Head::Faulted => return Ok(false),
                };
                if hub > from {
                    from = hub;
                    continue;
                }
                return Err(e);
            }
            Err(e) => return Err(e),
        };
        let events: Vec<NodeEvent> = batch
            .events
            .into_iter()
            .filter(|e| {
                e.cursor.stream == h.stream && e.cursor.epoch == h.epoch && e.cursor.seq > from
            })
            .collect();
        if events.is_empty() {
            return Err(ApiError::new(
                ErrorCode::Internal,
                format!(
                    "replay of {} from {from} brought nothing below its head {}",
                    h.stream, h.produced
                ),
            ));
        }
        let n = events.len();
        let ack = match (with.receive)(&EventBatch { events }) {
            Ok(a) => a,
            Err(e) if e.code == ErrorCode::Denied => {
                tracing::warn!(
                    "node {}: replay of {} from {from} refused: {e}",
                    node.id(),
                    h.stream
                );
                return Ok(false);
            }
            Err(e) => return Err(e),
        };
        out.replayed += n;
        let next = ack
            .durable
            .iter()
            .find(|w| w.stream == h.stream && w.epoch == h.epoch)
            .map_or(from, |w| w.seq);
        if next <= from {
            return Err(ApiError::new(
                ErrorCode::Internal,
                format!("replay of {} from {from} was not taken in", h.stream),
            ));
        }
        tell(node, Watermark { seq: next, ..mark }).await;
        from = next;
    }
    Ok(true)
}

/// Acknowledges one stream to the Node (one at a time: the Node takes an ack all or nothing, and
/// another stream's epoch may have changed meanwhile). A refusal is only logged: the Node sends
/// again what it did not hear acknowledged, and it is stored once.
async fn tell(node: &dyn Node, w: Watermark) {
    let (stream, seq) = (w.stream.clone(), w.seq);
    if let Err(e) = node.acknowledge(EventAck { durable: vec![w] }).await {
        tracing::warn!("node {}: ack {stream} up to {seq}: {e}", node.id());
    }
}

/// One head against the inbox, in `tx`: faults it on a gap (the Hub's own value only grows, so a
/// head read before it cannot make one up); the Hub holding more than the head is
/// [`Head::Ahead`], for [`confirm_ahead`]. The bool: it faulted now.
fn compare_in(tx: &Tx, node: &NodeId, h: &StreamHead) -> Result<(Head, bool), StoreError> {
    let k = StreamKey {
        node: node.as_str(),
        stream: h.stream.as_str(),
        epoch: h.epoch.as_str(),
    };
    let fault: Option<Option<String>> = tx
        .query_row(
            "SELECT fault FROM node_streams WHERE node_id=? AND stream=? AND epoch=?",
            rusqlite::params![k.node, k.stream, k.epoch],
            |r| r.get(0),
        )
        .optional()?;
    if fault.as_ref().is_some_and(|f| f.is_some()) {
        return Ok((Head::Faulted, false));
    }
    // what the Hub holds: the durable prefix and what quarantine holds past it (acknowledged too)
    let hub = match fault {
        Some(_) => rt::acked_in(tx, k)?.max(0) as u64,
        None => 0,
    };
    if hub < h.acknowledged {
        let fresh = fault_in(tx, node, h, hub, rt::Fault::Gap)?;
        return Ok((Head::Faulted, fresh));
    }
    if hub > h.produced {
        return Ok((Head::Ahead { hub }, false));
    }
    Ok((Head::Behind { hub }, false))
}

/// Faults `h`'s stream (the Hub holds `hub` of it) and notes its session, once. True: faulted now.
fn fault_in(
    tx: &Tx,
    node: &NodeId,
    h: &StreamHead,
    hub: u64,
    fault: rt::Fault,
) -> Result<bool, StoreError> {
    let k = StreamKey {
        node: node.as_str(),
        stream: h.stream.as_str(),
        epoch: h.epoch.as_str(),
    };
    let note = match fault {
        // a rolled-back Node journal
        rt::Fault::Ahead => steve_i18n::t!("hub.node.ahead"),
        // a gap
        _ => steve_i18n::t!("hub.node.gap"),
    };
    let fresh = rt::stream_fault_in(tx, k, fault)?;
    if fresh {
        tracing::warn!(
            "node {node}: stream {} epoch {}: the Hub holds {hub}, the Node produced {} and dropped {}; faulted {fault:?}",
            k.stream,
            k.epoch,
            h.produced,
            h.acknowledged
        );
        if let Some(session) = stream_session_in(tx, node, k.stream)? {
            system_in(tx, &session, &note)?;
        }
    }
    Ok(fresh)
}

/// The session of this Node whose events `stream` carries, if it is one of its sessions.
fn stream_session_in(tx: &Tx, node: &NodeId, stream: &str) -> Result<Option<String>, StoreError> {
    let mut st = tx.prepare("SELECT session_id FROM runtime_sessions WHERE node_id=?")?;
    let sessions = st
        .query_map([node.as_str()], |r| r.get::<_, String>(0))?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(sessions.into_iter().find(|s| session_stream(s) == stream))
}

/// The stream that carries a session's events (the Node names it so: steve-node-api's contract).
pub(super) fn session_stream(session: &str) -> String {
    derived_id("session", session)
}

/// A stream reconcile stopped waiting for: (stream, epoch, the head its events are applied up to).
pub(super) type Lag = (String, String, u64);

impl NodeRuntime {
    /// Brings the inbox up to the Node's heads ([`catch_up_streams`]), then waits until what the
    /// Hub holds up to them is applied, at most [`Self::apply_wait`]. The sessions of the streams
    /// still behind then are held with a note until their streams catch up
    /// ([`Self::caught_up_on`]); every other session goes on.
    pub(super) async fn catch_up(&self, heads: &[StreamHead]) -> ApiResult<CaughtUp> {
        let internal = |e: StoreError| ApiError::new(ErrorCode::Internal, e.to_string());
        let check = self.replay_check.get().cloned();
        let receive = |b: &EventBatch| {
            if let Some(check) = &check {
                check(b)?;
            }
            self.receive(b).map_err(internal)
        };
        let with = Pull {
            receive: &receive,
            largest: self.largest_replay.load(Ordering::Relaxed),
        };
        let out = catch_up_streams(&self.store, &*self.node, heads, Some(with)).await?;
        let deadline = Instant::now() + self.apply_wait();
        let behind = loop {
            let behind = self.unapplied(&out.reached).map_err(internal)?;
            if behind.is_empty() || Instant::now() >= deadline {
                break behind;
            }
            self.wake_apply.notify_one();
            tokio::time::sleep(Duration::from_millis(20)).await;
        };
        self.lag_on(behind).map_err(internal)?;
        Ok(out)
    }

    /// Holds the sessions of `behind`'s streams (noted once) and lets go of every other one held
    /// for lagging before.
    fn lag_on(&self, behind: Vec<(Lag, i64)>) -> Result<(), StoreError> {
        for ((stream, _, head), applied) in &behind {
            tracing::warn!(
                "node {}: stream {stream}: {applied} of {head} applied in time; its session is held until it catches up",
                self.node_id
            );
        }
        let was = self.lagging.lock().unwrap().clone();
        let lagging = self.tx(|tx| {
            let mut m = HashMap::new();
            for (lag, _) in &behind {
                if let Some(session) = stream_session_in(tx, &self.node_id, &lag.0)? {
                    if !was.contains_key(&session) {
                        system_in(tx, &session, &steve_i18n::t!("hub.node.lagging"))?;
                    }
                    m.insert(session, lag.clone());
                }
            }
            Ok(m)
        })?;
        *self.lagging.lock().unwrap() = lagging;
        // a stream applied up to its head meanwhile is not held: the apply worker may have found
        // it done before it was put here
        let lags: Vec<Lag> = self.lagging.lock().unwrap().values().cloned().collect();
        let done = self.unapplied(&lags)?;
        self.lagging
            .lock()
            .unwrap()
            .retain(|_, l| done.iter().any(|(d, _)| d == l));
        Ok(())
    }

    /// The apply worker found nothing more to apply on `stream` (all it holds is applied, or the
    /// stream faulted): a session held for lagging on it goes on (what waited is planned now).
    pub(super) fn caught_up_on(&self, stream: &str, epoch: &str) {
        let freed: Vec<String> = {
            let mut lagging = self.lagging.lock().unwrap();
            let freed = lagging
                .iter()
                .filter(|(_, (s, e, _))| s == stream && e == epoch)
                .map(|(k, _)| k.clone())
                .collect::<Vec<_>>();
            for k in &freed {
                lagging.remove(k);
            }
            freed
        };
        for s in freed {
            if let Err(e) = self.tx(|tx| self.plan_after_in(tx, &s)) {
                tracing::error!("plan {s} after its stream caught up: {e}");
            }
        }
    }

    /// Those of `reached` not applied up to their head yet, with what is applied (a faulted stream
    /// is not: it waits, and holds its session on its own).
    fn unapplied(&self, reached: &[Lag]) -> Result<Vec<(Lag, i64)>, StoreError> {
        self.store.reader(|c| {
            let mut out = vec![];
            for (stream, epoch, head) in reached {
                let row: Option<(i64, Option<String>)> = c
                    .query_row(
                        "SELECT applied, fault FROM node_streams WHERE node_id=? AND stream=? AND epoch=?",
                        rusqlite::params![self.node_id.as_str(), stream, epoch],
                        |r| Ok((r.get(0)?, r.get(1)?)),
                    )
                    .optional()?;
                match row {
                    Some((_, Some(_))) => {}
                    Some((applied, None)) if applied as u64 >= *head => {}
                    None if *head == 0 => {}
                    r => out.push((
                        (stream.clone(), epoch.clone(), *head),
                        r.map_or(0, |x| x.0),
                    )),
                }
            }
            Ok(out)
        })
    }
}
