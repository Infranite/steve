-- 51: the path model's view of each Node session, in shadow (S2): what the run would be given
-- (`view`: principal, audience, read-write and read-only directories) and how that differs from
-- what the Node lets it reach today: `extra` are today's roots outside the view, `missing` the
-- view's directories no root covers (JSON samples; the counts are all of them; NULL: not known,
-- a remote Node's roots are its own). Recorded when a session is bound, the latest only. Nothing
-- reads it to decide anything yet. An older build ignores it.
CREATE TABLE view_shadow (
 session_id TEXT PRIMARY KEY,
 node_id TEXT NOT NULL,
 kind TEXT NOT NULL,
 at INTEGER NOT NULL,
 digest TEXT NOT NULL,
 view TEXT NOT NULL,
 extra_count INTEGER,
 extra TEXT,
 missing_count INTEGER,
 missing TEXT);
