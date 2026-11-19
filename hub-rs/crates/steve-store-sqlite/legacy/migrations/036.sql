-- 36: D5. Indexes for the coordinator's lookups in the Node event inbox, which otherwise scan the
-- whole inbox inside write transactions, and for pruning it by age. Indexes only, `IF NOT EXISTS`.
-- Each expression is the very text the queries use (an index on json_extract only serves the same
-- expression), and each partial index carries the literal term its queries carry. Number assigned
-- on merge.

-- a permission request by its id (the card's answer, its expiry)
CREATE INDEX IF NOT EXISTS node_inbox_permission
  ON node_event_inbox(json_extract(payload,'$.body.request.id'))
  WHERE json_extract(payload,'$.body.type')='permission_requested';
-- an attempt's own events (its statistics at its end)
CREATE INDEX IF NOT EXISTS node_inbox_attempt
  ON node_event_inbox(node_id, json_extract(payload,'$.subject.attempt'), stream, seq);
-- a session's last view (its resume handle when it is opened again)
CREATE INDEX IF NOT EXISTS node_inbox_session_view
  ON node_event_inbox(node_id, json_extract(payload,'$.body.type'), json_extract(payload,'$.body.view.id'), received, seq);
-- the cumulative cost a harness process reported last before an event
CREATE INDEX IF NOT EXISTS node_inbox_usage
  ON node_event_inbox(node_id, stream, epoch, json_extract(payload,'$.body.report.meter_epoch'), seq)
  WHERE json_extract(payload,'$.body.type')='usage';
-- pruning applied events by age
CREATE INDEX IF NOT EXISTS node_inbox_received ON node_event_inbox(node_id, received);
