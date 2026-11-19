-- Generated from the frozen legacy opening flow; STEVE_BASELINE_BLESS=1 regenerates.
-- debt:stream/fk/diagnostic_captures.(item_id) -> items.(id)
CREATE TABLE diagnostic_captures (
 item_id TEXT PRIMARY KEY REFERENCES items(id) ON DELETE CASCADE,
 workspace_id TEXT NOT NULL,
 actor TEXT NOT NULL,
 started_at INTEGER NOT NULL,
 expires_at INTEGER NOT NULL,
 stop_reason TEXT,
 stopped_at INTEGER,
 bytes INTEGER NOT NULL DEFAULT 0,
 dropped_at_start INTEGER NOT NULL DEFAULT 0
);

-- debt:stream/fk/diagnostic_events.(item_id) -> items.(id)
CREATE TABLE diagnostic_events (
 seq INTEGER PRIMARY KEY AUTOINCREMENT,
 workspace_id TEXT NOT NULL,
 item_id TEXT NOT NULL REFERENCES items(id) ON DELETE CASCADE,
 at INTEGER NOT NULL,
 session_id TEXT,
 attempt_id TEXT,
 kind TEXT NOT NULL,
 reason_code TEXT NOT NULL,
 severity TEXT NOT NULL,
 detail INTEGER NOT NULL CHECK(detail IN (0,1)),
 data TEXT NOT NULL,
 bytes INTEGER NOT NULL
);

-- debt:stream/fk/diagnostic_item_stats.(item_id) -> items.(id)
CREATE TABLE diagnostic_item_stats (
 item_id TEXT PRIMARY KEY REFERENCES items(id) ON DELETE CASCADE,
 bytes INTEGER NOT NULL DEFAULT 0,
 dropped INTEGER NOT NULL DEFAULT 0,
 removed_through INTEGER NOT NULL DEFAULT 0
);

CREATE TABLE diagnostic_stats (
 id INTEGER PRIMARY KEY CHECK(id=1),
 dropped INTEGER NOT NULL DEFAULT 0
);

CREATE TABLE event_payload_refs (
    event_seq INTEGER NOT NULL REFERENCES events(seq) ON DELETE CASCADE,
    path TEXT NOT NULL,
    session_id TEXT NOT NULL,
    hash TEXT NOT NULL,
    PRIMARY KEY (event_seq, path),
    FOREIGN KEY (session_id, hash) REFERENCES event_payloads(session_id, hash)
);

-- debt:stream/fk/event_payloads.(session_id) -> sessions.(id)
CREATE TABLE event_payloads (
    session_id TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
    hash TEXT NOT NULL,
    data TEXT NOT NULL,
    PRIMARY KEY (session_id, hash)
);

-- debt:stream/fk/events.(session_id) -> sessions.(id)
CREATE TABLE events (seq INTEGER PRIMARY KEY AUTOINCREMENT, session_id TEXT NOT NULL REFERENCES sessions(id), ts INTEGER NOT NULL, kind TEXT NOT NULL, data TEXT NOT NULL, format TEXT NOT NULL DEFAULT 'legacy.acp', source_node TEXT, source_stream TEXT, source_epoch TEXT, source_seq INTEGER CHECK(
 (source_seq IS NULL AND source_node IS NULL AND source_stream IS NULL AND source_epoch IS NULL)
 OR (source_seq IS NOT NULL AND source_seq>0 AND source_node IS NOT NULL AND source_stream IS NOT NULL AND source_epoch IS NOT NULL)));

-- debt:stream/fk/node_event_inbox.(node_id,stream,epoch) -> node_streams.(node_id,stream,epoch)
CREATE TABLE node_event_inbox (
 node_id TEXT NOT NULL, stream TEXT NOT NULL, epoch TEXT NOT NULL, seq INTEGER NOT NULL CHECK(seq>0),
 event_id TEXT NOT NULL, payload TEXT NOT NULL, applied INTEGER NOT NULL DEFAULT 0 CHECK(applied IN (0,1)),
 received INTEGER NOT NULL,
 PRIMARY KEY(node_id,stream,epoch,seq), UNIQUE(node_id,event_id),
 FOREIGN KEY(node_id,stream,epoch) REFERENCES node_streams(node_id,stream,epoch));

-- debt:stream/fk/node_event_quarantine.(node_id,stream,epoch) -> node_streams.(node_id,stream,epoch)
CREATE TABLE node_event_quarantine (
 node_id TEXT NOT NULL, stream TEXT NOT NULL, epoch TEXT NOT NULL, seq INTEGER NOT NULL CHECK(seq>0),
 event_id TEXT NOT NULL, payload TEXT NOT NULL, reason TEXT NOT NULL, received INTEGER NOT NULL,
 PRIMARY KEY(node_id,stream,epoch,seq,event_id),
 FOREIGN KEY(node_id,stream,epoch) REFERENCES node_streams(node_id,stream,epoch));

CREATE TABLE node_vitals (
 node_id TEXT NOT NULL,
 at INTEGER NOT NULL,
 rtt_ms INTEGER,
 data TEXT NOT NULL);

-- debt:stream/fk/realtime_output_attempts.(attempt_id) -> runtime_attempts.(attempt_id)
-- debt:stream/fk/realtime_output_attempts.(session_id) -> sessions.(id)
CREATE TABLE realtime_output_attempts (
 attempt_id TEXT PRIMARY KEY REFERENCES runtime_attempts(attempt_id) ON DELETE CASCADE,
 session_id TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
 workspace_id TEXT NOT NULL, project_id TEXT, bot_id TEXT,
 node_id TEXT NOT NULL, started INTEGER NOT NULL, ended INTEGER, baseline INTEGER NOT NULL DEFAULT 1
 CHECK(baseline IN (0,1)), stop_evidence TEXT, not_started INTEGER NOT NULL DEFAULT 0
 CHECK(not_started IN (0,1)));

CREATE TABLE realtime_output_blocks (
 attempt_id TEXT NOT NULL REFERENCES realtime_output_attempts(attempt_id) ON DELETE CASCADE,
 message TEXT NOT NULL, block TEXT NOT NULL, role TEXT NOT NULL,
 utf8_end INTEGER NOT NULL DEFAULT 0 CHECK(utf8_end>=0),
 PRIMARY KEY(attempt_id,message,block));

CREATE TABLE realtime_output_samples (
 node_id TEXT NOT NULL, stream TEXT NOT NULL, epoch TEXT NOT NULL, seq INTEGER NOT NULL,
 event_id TEXT NOT NULL, attempt_id TEXT NOT NULL REFERENCES realtime_output_attempts(attempt_id) ON DELETE CASCADE,
 at INTEGER NOT NULL, source_at INTEGER NOT NULL, received_at INTEGER NOT NULL,
 kind TEXT NOT NULL CHECK(kind IN ('activity','text','gap','end')),
 -- text-v1: one quarter-token per ASCII scalar, four per non-ASCII scalar. No per-chunk rounding.
 units INTEGER NOT NULL DEFAULT 0 CHECK(units>=0),
 PRIMARY KEY(node_id,stream,epoch,seq), UNIQUE(node_id,event_id));

-- debt:stream/fk/session_turns.(session_id) -> sessions.(id)
CREATE TABLE session_turns (
 session_id TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
 attempt TEXT NOT NULL,
 paused INTEGER NOT NULL DEFAULT 0 CHECK(paused IN (0,1)),
 stop_reason TEXT,
 ended INTEGER,
 updated INTEGER NOT NULL,
 PRIMARY KEY (session_id, attempt));

CREATE TABLE tool_usage (
  bot_id TEXT NOT NULL,
  tool TEXT NOT NULL,
  day INTEGER NOT NULL,
  searched INTEGER NOT NULL DEFAULT 0,
  called INTEGER NOT NULL DEFAULT 0,
  PRIMARY KEY (bot_id, tool, day));

CREATE INDEX diagnostic_events_expiry ON diagnostic_events(at,seq);

CREATE INDEX diagnostic_events_item ON diagnostic_events(workspace_id,item_id,seq);

CREATE INDEX event_payload_refs_payload ON event_payload_refs(session_id, hash);

CREATE INDEX events_message_start ON events (session_id, json_extract(data, '$.message'))
  WHERE kind = 'message' AND format = 'steve.v1';

CREATE UNIQUE INDEX events_node_source ON events(source_node,source_stream,source_epoch,source_seq)
 WHERE source_node IS NOT NULL;

CREATE INDEX events_session ON events(session_id, seq);

CREATE INDEX node_inbox_attempt
  ON node_event_inbox(node_id, json_extract(payload,'$.subject.attempt'), stream, seq);

CREATE INDEX node_inbox_attempt_time
 ON node_event_inbox(node_id,json_extract(payload,'$.subject.attempt'),stream,epoch,received);

CREATE INDEX node_inbox_pending_attempt
 ON node_event_inbox(node_id,json_extract(payload,'$.subject.attempt'),received) WHERE applied=0;

CREATE INDEX node_inbox_permission
  ON node_event_inbox(json_extract(payload,'$.body.request.id'))
  WHERE json_extract(payload,'$.body.type')='permission_requested';

CREATE INDEX node_inbox_received ON node_event_inbox(node_id, received);

CREATE INDEX node_inbox_session_view
  ON node_event_inbox(node_id, json_extract(payload,'$.body.type'), json_extract(payload,'$.body.view.id'), received, seq);

CREATE INDEX node_inbox_usage
  ON node_event_inbox(node_id, stream, epoch, json_extract(payload,'$.body.report.meter_epoch'), seq)
  WHERE json_extract(payload,'$.body.type')='usage';

CREATE INDEX node_vitals_node_at ON node_vitals(node_id, at);

CREATE INDEX realtime_output_gaps ON realtime_output_samples(attempt_id,at) WHERE kind='gap';

CREATE INDEX realtime_output_scope ON realtime_output_attempts(workspace_id, started, ended);

CREATE INDEX realtime_output_window ON realtime_output_samples(attempt_id, at);

CREATE INDEX tool_usage_day ON tool_usage(day);

CREATE TRIGGER event_payload_refs_gc AFTER DELETE ON event_payload_refs
BEGIN
    DELETE FROM event_payloads WHERE session_id = OLD.session_id AND hash = OLD.hash
      AND NOT EXISTS (SELECT 1 FROM event_payload_refs WHERE session_id = OLD.session_id AND hash = OLD.hash);
END;

-- Final fresh-install seeds; historical data backfills are not replayed.
INSERT INTO "diagnostic_stats" VALUES (1,0);
