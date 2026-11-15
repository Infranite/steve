-- Realtime output telemetry. Number is provisional until merge; append-only samples are committed
-- with the Node apply watermark, independently of final billing usage and inbox pruning.
CREATE TABLE realtime_output_attempts (
 attempt_id TEXT PRIMARY KEY REFERENCES runtime_attempts(attempt_id) ON DELETE CASCADE,
 session_id TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
 workspace_id TEXT NOT NULL, project_id TEXT, bot_id TEXT,
 node_id TEXT NOT NULL, started INTEGER NOT NULL, ended INTEGER);
CREATE INDEX realtime_output_scope ON realtime_output_attempts(workspace_id, started, ended);
CREATE TABLE realtime_output_samples (
 node_id TEXT NOT NULL, stream TEXT NOT NULL, epoch TEXT NOT NULL, seq INTEGER NOT NULL,
 event_id TEXT NOT NULL, attempt_id TEXT NOT NULL REFERENCES realtime_output_attempts(attempt_id) ON DELETE CASCADE,
 at INTEGER NOT NULL, source_at INTEGER NOT NULL, received_at INTEGER NOT NULL,
 kind TEXT NOT NULL CHECK(kind IN ('activity','text','gap','end')),
 -- text-v1: one quarter-token per ASCII scalar, four per non-ASCII scalar. No per-chunk rounding.
 units INTEGER NOT NULL DEFAULT 0 CHECK(units>=0),
 PRIMARY KEY(node_id,stream,epoch,seq), UNIQUE(node_id,event_id));
CREATE INDEX realtime_output_window ON realtime_output_samples(attempt_id, at);
CREATE TABLE realtime_output_blocks (
 attempt_id TEXT NOT NULL REFERENCES realtime_output_attempts(attempt_id) ON DELETE CASCADE,
 message TEXT NOT NULL, block TEXT NOT NULL, role TEXT NOT NULL,
 utf8_end INTEGER NOT NULL DEFAULT 0 CHECK(utf8_end>=0),
 PRIMARY KEY(attempt_id,message,block));
