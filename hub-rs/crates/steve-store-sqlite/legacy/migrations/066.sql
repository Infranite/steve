-- 66: how each session's turns ended and whether you stopped them (ST-10). The stall watch, Steve's
-- continues and YOLO read it after a restart as before one, never from the session's last system
-- line.
-- `session_stops`, the session's last turn end: `stop_asked`: the attempt your Stop was asked for
-- ('' when none was named), kept until a turn ends; `asked_seq`: the session's last event when your
-- Stop was asked for (a message after it started another turn); `yours`: the last end recorded was
-- the turn you stopped; `seq`: the session's last event then (a message sent after it began another
-- turn).
CREATE TABLE IF NOT EXISTS session_stops (
 session_id TEXT PRIMARY KEY REFERENCES sessions(id) ON DELETE CASCADE,
 attempt TEXT,
 stop_reason TEXT,
 error TEXT,
 yours INTEGER NOT NULL DEFAULT 0 CHECK(yours IN (0,1)),
 stop_asked TEXT,
 asked_seq INTEGER,
 ended INTEGER,
 seq INTEGER,
 updated INTEGER NOT NULL);
-- `session_turns`, each turn of a session by its attempt, so a turn end is judged once also after a
-- restart and a pause's stop survives a quick resume and a restart. `ended`: when that attempt's end
-- came (an end told again, a redelivery, finds it here and is not judged twice); `paused`: the graph
-- stopped this turn (a pause), its cancelled end is no failure of the unit. Only the newest 64 turns
-- of a session are kept.
CREATE TABLE IF NOT EXISTS session_turns (
 session_id TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
 attempt TEXT NOT NULL,
 paused INTEGER NOT NULL DEFAULT 0 CHECK(paused IN (0,1)),
 stop_reason TEXT,
 ended INTEGER,
 updated INTEGER NOT NULL,
 PRIMARY KEY (session_id, attempt));
