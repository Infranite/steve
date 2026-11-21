//! T3, receiving: a batch of the Node's events ([`Sink`]) is applied as it comes, in one
//! transaction, and what is applied is acknowledged once it committed.
//!
//! Each stream's events go in order of their position: one at or below what is applied is a
//! resend (skipped), one past the next position waits for the Node to send what comes before it
//! (the stream stops there; the Node sends what was not acknowledged again). Each event is applied
//! in a scope of its own: one that fails is rolled back alone and faults its stream for good (its
//! session is told), while the batch's other streams go on. A faulted stream's events are
//! acknowledged unapplied, so the Node lets them go.

use super::*;

/// The Hub's side of the Node's event stream.
pub struct Sink(pub std::sync::Weak<NodeRuntime>);

#[async_trait]
impl EventSink for Sink {
    async fn commit(&self, batch: EventBatch) -> ApiResult<EventAck> {
        let Some(rt) = self.0.upgrade() else {
            return Err(ApiError::new(ErrorCode::Offline, "the hub is gone"));
        };
        tokio::task::spawn_blocking(move || {
            rt.receive(&batch)
                .map_err(|e| ApiError::new(ErrorCode::Internal, e.to_string()))
        })
        .await
        .map_err(|e| ApiError::new(ErrorCode::Internal, e.to_string()))?
    }
}

impl NodeRuntime {
    // ── T3: receiving ──

    /// Applies a batch from the Node; acknowledges, per stream, what is applied (and what a faulted
    /// stream received). What was applied is told after the commit, in the order it was applied
    /// ([`Self::tell_later`]), without holding up the acknowledgement.
    pub(super) fn receive(&self, batch: &EventBatch) -> Result<EventAck, StoreError> {
        if batch.events.is_empty() {
            // the Node checking the Hub is there: nothing to write
            return Ok(EventAck { durable: vec![] });
        }
        let started = std::time::Instant::now();
        let received = self.clock.now();
        // the streams in the order they first come, each with its events by position
        let mut streams: Vec<(&StreamId, &Epoch, Vec<&NodeEvent>)> = vec![];
        for e in &batch.events {
            let (stream, epoch) = (&e.cursor.stream, &e.cursor.epoch);
            match streams
                .iter_mut()
                .find(|(s, x, _)| *s == stream && *x == epoch)
            {
                Some((_, _, evs)) => evs.push(e),
                None => streams.push((stream, epoch, vec![e])),
            }
        }
        for (_, _, evs) in &mut streams {
            evs.sort_by_key(|e| e.cursor.seq);
        }
        let (ack, applied) = self.tx(|tx| {
            let mut durable = vec![];
            let mut applied = false;
            for (stream, epoch, evs) in &streams {
                let k = StreamKey {
                    node: self.node_id.as_str(),
                    stream: stream.as_str(),
                    epoch: epoch.as_str(),
                };
                let session = evs.iter().find_map(|e| ev_session(e));
                let st = rt::stream_open_in(tx, k, session.as_deref())?;
                let (mut at, mut faulted, mut heard) = (st.applied, st.faulted, 0);
                for e in evs {
                    let seq = e.cursor.seq as i64;
                    heard = heard.max(seq);
                    if faulted || seq <= at {
                        continue;
                    }
                    if seq != at + 1 {
                        break;
                    }
                    let mark = self.staged_mark();
                    match self
                        .store
                        .transaction(|tx| self.apply_one_in(tx, e, received))
                    {
                        Ok(()) => {
                            applied = true;
                            at = seq;
                        }
                        Err(why) => {
                            self.unstage(mark);
                            self.fault_in(tx, k, e, &why, received)?;
                            faulted = true;
                        }
                    }
                }
                if at > st.applied {
                    rt::stream_applied_in(tx, k, at)?;
                }
                let acked = if faulted { heard.max(at) } else { at };
                if acked > 0 {
                    durable.push(Watermark {
                        stream: (*stream).clone(),
                        epoch: (*epoch).clone(),
                        seq: acked as u64,
                    });
                }
            }
            Ok((EventAck { durable }, applied))
        })?;
        metrics::counter!("steve_node_ingest_commits_total").increment(1);
        metrics::histogram!("steve_node_ingest_events").record(batch.events.len() as f64);
        metrics::histogram!("steve_node_ingest_seconds").record(started.elapsed().as_secs_f64());
        if applied {
            self.wake_send.notify_one();
        }
        Ok(ack)
    }

    /// `e` could not be applied (`why`): its stream is faulted with it, and the session it carries,
    /// if one of this Node's, is told once that its Node events stop and its open turn is never
    /// run again ([`Self::is_uncertain`]); its realtime output has a gap from here.
    fn fault_in(
        &self,
        tx: &Tx,
        k: StreamKey<'_>,
        e: &NodeEvent,
        why: &StoreError,
        received: i64,
    ) -> Result<(), StoreError> {
        let session = ev_session(e);
        let reason = why.to_string();
        let fresh = rt::stream_fault_in(
            tx,
            k,
            rt::Fault::Failed,
            rt::FaultNote {
                session: session.as_deref(),
                event: Some(e.id.as_str()),
                reason: Some(&reason),
            },
        )?;
        if !fresh {
            return Ok(());
        }
        tracing::error!(
            "node {}: stream {} epoch {} seq {} ({}) not applied: {reason}; the stream is faulted",
            k.node,
            k.stream,
            k.epoch,
            e.cursor.seq,
            e.id
        );
        self.realtime_gap_in(tx, e, received)?;
        let Some(session) = session else {
            return Ok(());
        };
        let ours: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM runtime_sessions WHERE session_id=? AND node_id=?)",
            rusqlite::params![session, self.node_id.as_str()],
            |r| r.get(0),
        )?;
        if ours {
            // `faulted`: the page offers the way forward (a new session, or closing this one)
            rt::add_event_in(
                tx,
                &session,
                "system",
                json!({
                    "text": steve_i18n::stored(|| steve_i18n::t!("hub.node.faulted")),
                    "faulted": true,
                }),
                V1,
                None,
                now(),
            )?;
        }
        Ok(())
    }
}
