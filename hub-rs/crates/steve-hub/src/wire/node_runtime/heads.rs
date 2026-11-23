//! Reconciling what the Hub applied with the Node's streams (N1-7): before turns are planned, every
//! head of the Node's snapshot is compared with what the Hub applied of that stream and epoch.
//!
//! - The Hub holds less than the Node produced: the rest is pulled by replay and applied as it
//!   comes.
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
//! that does not fit there either is left to push.
//!
//! [`catch_up_streams`] is the one place for it: the coordinator's reconcile and the link's
//! catch-up (a Node with no coordinator running) both call it.

use super::*;
use steve_store::{Fault, FaultNote};

/// One replay page: what one message over a remote Node's link carries (a 1 MiB page would not
/// fit in its frame).
const REPLAY_BYTES: u32 = steve_node_wire::envelope::BULK_CAP as u32;

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
    let mut heads: HashMap<String, StreamHead> = HashMap::new();
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
                for head in page.heads {
                    heads.insert(head.stream.as_str().to_string(), head);
                }
                match page.next {
                    Some(c) => cursor = Some(c),
                    None => {
                        let mut heads: Vec<_> = heads.into_values().collect();
                        heads.sort_by(|a, b| a.stream.as_str().cmp(b.stream.as_str()));
                        return Ok(NodeSnapshot { entries, heads });
                    }
                }
            }
            Err(e) if e.code == Code::Stale && restarts < 3 => {
                restarts += 1;
                entries.clear();
                heads.clear();
                cursor = None;
            }
            Err(e) => return Err(e),
        }
    }
}

/// Applies a replayed batch and answers what is applied ([`NodeRuntime::receive`]).
/// `Denied`: the batch is refused (another Node's work); nothing of it was taken in.
pub type Receive<'a> = &'a (dyn Fn(&EventBatch) -> ApiResult<EventAck> + Sync);

/// What a remote Node's replayed batch must pass before it is applied: the link's guard
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
}

/// One head against what the Hub applied.
enum Head {
    /// The Hub holds `hub` of it; replay from there up to the head (nothing to pull if equal).
    Behind { hub: u64 },
    /// The Hub holds `hub`, more than the head says: the Node's current head decides
    /// ([`confirm_ahead`]).
    Ahead { hub: u64 },
    /// Faulted, now or before.
    Faulted,
}

/// Compares the Node's `heads` with what the Hub applied and brings it up to them: faults what
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
    let internal = |e: StoreError| Error::new(Code::Internal, e.to_string());
    let node_id = node.id().clone();
    let mut out = CaughtUp::default();
    let looked: Vec<(StreamHead, Head, bool)> = store
        .write(|w| {
            heads
                .iter()
                .map(|h| {
                    let (head, fresh) = compare(w, &node_id, h)?;
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
            continue;
        }
        if let Some(with) = pull_with {
            pull(store, node, &h, hub, with, &mut out).await?;
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
        .write(|w| fault(w, node.id(), &now, hub, Fault::Ahead))
        .map_err(|e| Error::new(Code::Internal, e.to_string()))?;
    if fresh {
        out.faulted += 1;
    }
    Ok(None)
}

/// Replays `h`'s stream from `hub` up to its head, acknowledging each page applied. It may stop
/// short: the stream faulted, the Node's journal is another one now, a batch was refused, or an
/// event does not fit in any page (push delivers it or nothing does).
async fn pull(
    store: &Store,
    node: &dyn Node,
    h: &StreamHead,
    hub: u64,
    with: Pull<'_>,
    out: &mut CaughtUp,
) -> ApiResult<()> {
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
            Err(e) if e.code == Code::Exhausted && budget < with.largest => {
                budget = with.largest;
                continue;
            }
            Err(e) if e.code == Code::Exhausted => {
                tracing::warn!(
                    "node {}: replay of {} from {from}: {e}; left to push",
                    node.id(),
                    h.stream
                );
                return Ok(());
            }
            Err(e) if e.code == Code::Gap => {
                match recover_replay_gap(store, node, h, from, out, e).await? {
                    std::ops::ControlFlow::Continue(hub) => {
                        from = hub;
                        continue;
                    }
                    std::ops::ControlFlow::Break(()) => return Ok(()),
                }
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
            return Err(Error::new(
                Code::Internal,
                format!(
                    "replay of {} from {from} brought nothing below its head {}",
                    h.stream, h.produced
                ),
            ));
        }
        let n = events.len();
        let ack = match (with.receive)(&EventBatch { events }) {
            Ok(a) => a,
            Err(e) if e.code == Code::Forbidden => {
                tracing::warn!(
                    "node {}: replay of {} from {from} refused: {e}",
                    node.id(),
                    h.stream
                );
                return Ok(());
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
            return Err(Error::new(
                Code::Internal,
                format!("replay of {} from {from} was not taken in", h.stream),
            ));
        }
        tell(node, Watermark { seq: next, ..mark }).await;
        from = next;
    }
    Ok(())
}

async fn recover_replay_gap(
    store: &Store,
    node: &dyn Node,
    h: &StreamHead,
    from: u64,
    out: &mut CaughtUp,
    e: Error,
) -> ApiResult<std::ops::ControlFlow<(), u64>> {
    let internal = |e: StoreError| Error::new(Code::Internal, e.to_string());
    // the Node dropped what the Hub lacks, or its journal changed since the snapshot:
    // what both hold now decides
    let now = node
        .streams()
        .await?
        .into_iter()
        .find(|x| x.stream == h.stream);
    let Some(now) = now.filter(|x| x.epoch == h.epoch) else {
        return Ok(std::ops::ControlFlow::Break(()));
    };
    let (head, fresh) = store
        .write(|w| compare(w, node.id(), &now))
        .map_err(internal)?;
    if fresh {
        out.faulted += 1;
    }
    let hub = match head {
        Head::Behind { hub } => hub,
        Head::Ahead { hub } => match confirm_ahead(store, node, &now, hub, out).await? {
            Some(_) => hub,
            None => return Ok(std::ops::ControlFlow::Break(())),
        },
        Head::Faulted => return Ok(std::ops::ControlFlow::Break(())),
    };
    if hub > from {
        return Ok(std::ops::ControlFlow::Continue(hub));
    }
    Err(e)
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

/// One head against what the Hub applied, in `w`: faults it on a gap (the Hub's own value only
/// grows, so a head read before it cannot make one up); the Hub holding more than the head is
/// [`Head::Ahead`], for [`confirm_ahead`]. The bool: it faulted now.
fn compare(w: &mut W<'_>, node: &NodeId, h: &StreamHead) -> Result<(Head, bool), StoreError> {
    let st = w.streams().state(StreamKey {
        node: node.as_str(),
        stream: h.stream.as_str(),
        epoch: h.epoch.as_str(),
    })?;
    if st.is_some_and(|st| st.faulted) {
        return Ok((Head::Faulted, false));
    }
    let hub = st.map_or(0, |st| st.applied.max(0) as u64);
    if hub < h.acknowledged {
        let fresh = fault(w, node, h, hub, Fault::Gap)?;
        return Ok((Head::Faulted, fresh));
    }
    if hub > h.produced {
        return Ok((Head::Ahead { hub }, false));
    }
    Ok((Head::Behind { hub }, false))
}

/// Faults `h`'s stream (the Hub holds `hub` of it) and notes its session, once. True: faulted now.
fn fault(
    w: &mut W<'_>,
    node: &NodeId,
    h: &StreamHead,
    hub: u64,
    fault: Fault,
) -> Result<bool, StoreError> {
    let k = StreamKey {
        node: node.as_str(),
        stream: h.stream.as_str(),
        epoch: h.epoch.as_str(),
    };
    let note = match fault {
        // a rolled-back Node journal
        Fault::Ahead => steve_i18n::stored(|| steve_i18n::t!("hub.node.ahead")),
        // a gap
        _ => steve_i18n::stored(|| steve_i18n::t!("hub.node.gap")),
    };
    let session = steve_store::node_sessions::on_node(w, node.as_str())?
        .into_iter()
        .find(|s| session_stream(s) == k.stream);
    let fresh = w.streams().fault(
        k,
        fault,
        FaultNote {
            session: session.as_deref(),
            ..Default::default()
        },
    )?;
    if fresh {
        tracing::warn!(
            "node {node}: stream {} epoch {}: the Hub holds {hub}, the Node produced {} and dropped {}; faulted {fault:?}",
            k.stream,
            k.epoch,
            h.produced,
            h.acknowledged
        );
        if let Some(session) = &session {
            w.events()
                .add(session, "system", json!({ "text": note }), None, now())?;
        }
    }
    Ok(fresh)
}

/// The stream that carries a session's events (the Node names it so: steve-node-api's contract).
pub(super) fn session_stream(session: &str) -> String {
    derived_id("session", session)
}

impl NodeRuntime {
    /// Brings what the Hub applied up to the Node's heads ([`catch_up_streams`]): replayed events
    /// are applied as they come.
    pub(super) async fn catch_up(&self, heads: &[StreamHead]) -> ApiResult<CaughtUp> {
        let internal = |e: StoreError| Error::new(Code::Internal, e.to_string());
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
        catch_up_streams(&self.store, &*self.node, heads, Some(with)).await
    }
}
