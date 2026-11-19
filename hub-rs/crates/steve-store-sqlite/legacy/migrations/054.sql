-- Diagnostic metadata is separate from business state and execution receipts.
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
CREATE INDEX diagnostic_events_item ON diagnostic_events(workspace_id,item_id,seq);
CREATE INDEX diagnostic_events_expiry ON diagnostic_events(at,seq);
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
INSERT INTO diagnostic_stats(id,dropped) VALUES(1,0);
