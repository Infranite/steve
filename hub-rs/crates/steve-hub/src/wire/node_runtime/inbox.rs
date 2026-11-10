//! T3, receiving: Node event inbox ingestion ([`Sink`]), quarantine/watermark acknowledgement
//! and bounded inbox pruning.

use super::*;

pub(super) const PRUNE_BATCH: i64 = 500;
/// ?1 node, ?2 received before, ?3 at most. The json_extract terms match migration 036's indexes.
const PRUNE_INBOX_SQL: &str = "DELETE FROM node_event_inbox WHERE rowid IN (
  SELECT e.rowid FROM node_event_inbox e JOIN node_streams s USING(node_id, stream, epoch)
   WHERE e.node_id=?1 AND e.received < ?2 AND e.seq <= s.applied
     AND NOT EXISTS (SELECT 1 FROM runtime_attempts a
           WHERE a.attempt_id = json_extract(e.payload,'$.subject.attempt') AND a.state <> 'finished')
     AND NOT (json_extract(e.payload,'$.body.type')='permission_requested' AND EXISTS (
           SELECT 1 FROM decisions d WHERE d.subject_type='tool_call' AND d.status='open'
              AND d.subject_id = json_extract(e.payload,'$.body.request.id')))
     AND NOT (json_extract(e.payload,'$.body.type')='session_changed'
           AND json_extract(e.payload,'$.body.view.resume') IS NOT NULL
           AND e.seq = (SELECT MAX(x.seq) FROM node_event_inbox x
                 WHERE x.node_id=e.node_id AND x.stream=e.stream AND x.epoch=e.epoch
                   AND json_extract(x.payload,'$.body.type')='session_changed'
                   AND json_extract(x.payload,'$.body.view.id')=json_extract(e.payload,'$.body.view.id')
                   AND json_extract(x.payload,'$.body.view.resume') IS NOT NULL))
     AND NOT (json_extract(e.payload,'$.body.type')='session_changed'
           AND e.seq = (SELECT MAX(x.seq) FROM node_event_inbox x
                 WHERE x.node_id=e.node_id AND x.stream=e.stream AND x.epoch=e.epoch
                   AND json_extract(x.payload,'$.body.type')='session_changed'
                   AND json_extract(x.payload,'$.body.view.id')=json_extract(e.payload,'$.body.view.id')))
     AND NOT (json_extract(e.payload,'$.body.type')='session_changed'
           AND json_extract(e.payload,'$.body.view.revision') = (
                 SELECT json_extract(c.receipt,'$.payload.state.result.revision') FROM node_commands c
                  WHERE c.method='open_session' AND c.state='finished' AND json_extract(c.receipt,'$.success')=1
                    AND json_extract(c.request,'$.body.session')=json_extract(e.payload,'$.body.view.id')
                  ORDER BY c.created DESC, c.rowid DESC LIMIT 1))
     AND NOT (json_extract(e.payload,'$.body.type')='usage'
           AND json_extract(e.payload,'$.body.report.cost') IS NOT NULL
           AND e.seq = (SELECT MAX(x.seq) FROM node_event_inbox x
                 WHERE x.node_id=e.node_id AND x.stream=e.stream AND x.epoch=e.epoch
                   AND json_extract(x.payload,'$.body.type')='usage'
                   AND json_extract(x.payload,'$.body.report.meter_epoch')=json_extract(e.payload,'$.body.report.meter_epoch')
                   AND json_extract(x.payload,'$.body.report.cost') IS NOT NULL))
   LIMIT ?3)";

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

    /// Takes a batch from the Node into the inbox; acknowledges the contiguous durable prefixes
    /// (quarantined events included).
    pub(super) fn receive(&self, batch: &EventBatch) -> Result<EventAck, StoreError> {
        let node = self.node_id.to_string();
        let mut touched: Vec<(String, String)> = vec![];
        self.tx(|tx| {
            for e in &batch.events {
                let k = StreamKey {
                    node: &node,
                    stream: e.cursor.stream.as_str(),
                    epoch: e.cursor.epoch.as_str(),
                };
                let seq = e.cursor.seq as i64;
                let payload = serde_json::to_value(e).map_err(conflict)?;
                // a conflict is set aside on its own stream (D5: acknowledged once it is durably
                // quarantined), so the rest of the batch and every other stream go on; the
                // durable prefix moves once per stream, after the whole batch is in
                if let rt::Inboxed::Conflict(why) =
                    rt::inbox_put_in(tx, k, seq, e.id.as_str(), &payload, now())?
                {
                    let q = rt::quarantine_in(tx, k, seq, e.id.as_str(), &payload, why, now())?;
                    if q.new {
                        tracing::warn!(
                            "node {node}: stream {} epoch {} seq {seq} ({}): {}; quarantined",
                            k.stream,
                            k.epoch,
                            e.id,
                            why.reason()
                        );
                    }
                    if q.faulted {
                        self.faulted_note_in(tx, e)?;
                    }
                }
                let key = (e.cursor.stream.to_string(), e.cursor.epoch.to_string());
                if !touched.contains(&key) {
                    touched.push(key);
                }
            }
            let mut durable = vec![];
            for (stream, epoch) in &touched {
                let k = StreamKey {
                    node: &node,
                    stream,
                    epoch,
                };
                rt::extend_durable_in(tx, k)?;
                let acked = rt::acked_in(tx, k)?;
                if acked > 0 {
                    durable.push(Watermark {
                        stream: idv(stream.clone())?,
                        epoch: idv(epoch.clone())?,
                        seq: acked as u64,
                    });
                }
            }
            Ok(EventAck { durable })
        })
        .inspect(|_| {
            self.dirty.lock().unwrap().extend(touched.iter().cloned());
            self.wake_apply.notify_one();
        })
    }

    /// The one note on a session whose stream just faulted: its Node events stop being applied
    /// until reconciliation, and its open turn is never run again meanwhile ([`Self::is_uncertain`]).
    fn faulted_note_in(&self, tx: &Tx, e: &NodeEvent) -> Result<(), StoreError> {
        let Some(session) = ev_session(e) else {
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
                    "text": steve_i18n::stored(|| steve_i18n::t!("hub.node.quarantined")),
                    "faulted": true,
                }),
                V1,
                None,
                now(),
            )?;
        }
        Ok(())
    }

    /// Drops applied Node events past [`INBOX_KEEP_MS`] that nothing reads again: kept are those
    /// of an attempt still open, a permission request whose card is open, and per stream the last
    /// resume handle of a session and the last cumulative cost of a harness process.
    pub fn prune_inbox(&self, at: i64) {
        loop {
            let r = self.tx(|tx| {
                Ok(tx.execute(
                    PRUNE_INBOX_SQL,
                    rusqlite::params![self.node_id.as_str(), at - INBOX_KEEP_MS, PRUNE_BATCH],
                )?)
            });
            match r {
                Ok(n) if n as i64 == PRUNE_BATCH => continue,
                Ok(_) => return,
                Err(e) => {
                    tracing::error!("prune the node inbox: {e}");
                    return;
                }
            }
        }
    }
}
