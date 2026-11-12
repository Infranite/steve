-- 66: how each session's last turn ended and whether you stopped it (ST-25). The stall watch, Steve's
-- continues and YOLO read it after a restart as before one, never from the session's last system
-- line. `stop_asked`: the attempt your Stop was asked for ('' when none was named), kept until a turn
-- ends; `yours`: the last end recorded was the turn you stopped; `seq`: the session's last event
-- then (a message sent after it began another turn).
CREATE TABLE IF NOT EXISTS session_stops (
 session_id TEXT PRIMARY KEY REFERENCES sessions(id) ON DELETE CASCADE,
 attempt TEXT,
 stop_reason TEXT,
 error TEXT,
 yours INTEGER NOT NULL DEFAULT 0 CHECK(yours IN (0,1)),
 stop_asked TEXT,
 ended INTEGER,
 seq INTEGER,
 updated INTEGER NOT NULL);
