//! Reconciling the Hub's inbox with the Node's streams (N1-7): before turns are planned, every head
//! of the Node's snapshot is compared with what the Hub holds of that stream and epoch.
//!
//! - The Hub holds less than the Node produced: the rest is pulled by replay into the inbox, and the
//!   coordinator waits (bounded) until it is applied.
//! - The Hub holds less than the Node already dropped (`gap`: the Hub's database went back, a
//!   restored backup): what is missing cannot be had any more. The stream is faulted, its session
//!   noted and held.
//! - The Hub holds more than the Node produced in the same epoch (`ahead`: the Node's journal went
//!   back): faulted the same way.
//! - The Hub holds more than the Node heard acknowledged (an ack lost with a Hub restart): the Node
//!   is told now, so it drops its copy.
//! - An epoch the Hub holds that the Node no longer has is left as it is: a new journal's epoch is
//!   another stream.
//!
//! [`catch_up_streams`] is the one place for it: the coordinator's reconcile and the link's
//! catch-up (a Node with no coordinator running) both call it.

use super::*;
use std::time::Instant;

/// One replay page: what one message over a remote Node's link carries (a 1 MiB page would not
/// fit in its frame).
const REPLAY_BYTES: u32 = steve_node_wire::envelope::BULK_CAP as u32;
/// How long reconcile waits for what the Hub holds of the Node's heads to be applied. Past it,
/// reconcile fails and is tried again: nothing is planned on a half-applied stream.
pub(super) const APPLY_WAIT: Duration = Duration::from_secs(30);

/// What a gap leaves on the session.
const GAP: &str = "Hub 保存的 Node 事件少于 Node 已确认送达的部分（Hub 的数据可能回到了较早的状态），缺失的事件已无法取回；该会话暂停应用 Node 事件，等待处理，期间不会重新运行";
/// What a rolled-back Node journal leaves on the session.
const AHEAD: &str = "Hub 保存的 Node 事件多于 Node 现有的记录（Node 的记录可能回到了较早的状态）；该会话暂停应用 Node 事件，等待处理，期间不会重新运行";

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
pub type Receive<'a> = &'a (dyn Fn(&EventBatch) -> Result<EventAck, StoreError> + Sync);

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
    receive: Option<Receive<'_>>,
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
        let Head::Behind { hub } = head else { continue };
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
        let Some(receive) = receive else { continue };
        if pull(store, node, &h, hub, receive, &mut out).await? {
            out.reached
                .push((h.stream.to_string(), h.epoch.to_string(), h.produced));
        }
    }
    Ok(out)
}

/// Replays `h`'s stream from `hub` up to its head into the inbox, acknowledging each page. False:
/// it stopped short (the stream faulted, or the Node's journal is another one now).
async fn pull(
    store: &Store,
    node: &dyn Node,
    h: &StreamHead,
    hub: u64,
    receive: Receive<'_>,
    out: &mut CaughtUp,
) -> ApiResult<bool> {
    let internal = |e: StoreError| ApiError::new(ErrorCode::Internal, e.to_string());
    let mut from = hub;
    while from < h.produced {
        let mark = Watermark {
            stream: h.stream.clone(),
            epoch: h.epoch.clone(),
            seq: from,
        };
        let batch = match node.replay(mark.clone(), REPLAY_BYTES).await {
            Ok(b) => b,
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
                match head {
                    Head::Behind { hub } if hub > from => {
                        from = hub;
                        continue;
                    }
                    Head::Behind { .. } => return Err(e),
                    Head::Faulted => return Ok(false),
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
            return Err(ApiError::new(
                ErrorCode::Internal,
                format!(
                    "replay of {} from {from} brought nothing below its head {}",
                    h.stream, h.produced
                ),
            ));
        }
        out.replayed += events.len();
        let ack = receive(&EventBatch { events }).map_err(internal)?;
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

/// One head against the inbox, in `tx`: faults it when it cannot be reconciled. The bool: it
/// faulted now.
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
    let why = if hub < h.acknowledged {
        Some((rt::Fault::Gap, GAP))
    } else if hub > h.produced {
        Some((rt::Fault::Ahead, AHEAD))
    } else {
        None
    };
    let Some((fault, note)) = why else {
        return Ok((Head::Behind { hub }, false));
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
            system_in(tx, &session, note)?;
        }
    }
    Ok((Head::Faulted, fresh))
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

impl NodeRuntime {
    /// Brings the inbox up to the Node's heads ([`catch_up_streams`]), then waits until what the
    /// Hub holds up to them is applied, at most [`APPLY_WAIT`]: past it, an error (reconcile is
    /// tried again, nothing is planned meanwhile).
    pub(super) async fn catch_up(&self, heads: &[StreamHead]) -> ApiResult<CaughtUp> {
        let receive = |b: &EventBatch| self.receive(b);
        let out = catch_up_streams(&self.store, &*self.node, heads, Some(&receive)).await?;
        let deadline = Instant::now() + APPLY_WAIT;
        loop {
            let behind = self
                .unapplied(&out.reached)
                .map_err(|e| ApiError::new(ErrorCode::Internal, e.to_string()))?;
            let Some((stream, applied, head)) = behind else {
                return Ok(out);
            };
            if Instant::now() >= deadline {
                return Err(ApiError::new(
                    ErrorCode::Busy,
                    format!("stream {stream}: {applied} of {head} applied so far"),
                ));
            }
            self.wake_apply.notify_one();
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    /// The first of `reached` not applied up to its head yet (a faulted stream never is: it waits).
    fn unapplied(
        &self,
        reached: &[(String, String, u64)],
    ) -> Result<Option<(String, i64, u64)>, StoreError> {
        self.store.reader(|c| {
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
                    r => return Ok(Some((stream.clone(), r.map_or(0, |x| x.0), *head))),
                }
            }
            Ok(None)
        })
    }
}
