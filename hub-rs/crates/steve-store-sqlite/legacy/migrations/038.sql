-- N2: paired Nodes, their one-time pairing codes (only the sha256 is kept) and an audit trail.
-- All new tables: an older build simply ignores them.
CREATE TABLE nodes (
 node_id TEXT PRIMARY KEY,
 owner TEXT NOT NULL,
 name TEXT NOT NULL,
 pubkey BLOB NOT NULL CHECK(length(pubkey)=32),
 fingerprint TEXT NOT NULL UNIQUE,
 state TEXT NOT NULL CHECK(state IN ('pending','active','paused','revoked')),
 generation INTEGER NOT NULL DEFAULT 0,
 incarnation INTEGER,
 local INTEGER NOT NULL DEFAULT 0 CHECK(local IN (0,1)),
 capabilities TEXT,
 created INTEGER NOT NULL,
 last_seen INTEGER,
 revoked_at INTEGER,
 revoked_by TEXT);
CREATE INDEX nodes_owner ON nodes(owner);
CREATE TABLE node_pairings (
 code_hash TEXT PRIMARY KEY,
 created_by TEXT NOT NULL,
 created INTEGER NOT NULL,
 expires INTEGER NOT NULL,
 claimed_at INTEGER,
 node_id TEXT);
CREATE TABLE node_audit (
 at INTEGER NOT NULL,
 node_id TEXT,
 actor TEXT,
 what TEXT NOT NULL,
 safe_detail TEXT);
CREATE INDEX node_audit_node ON node_audit(node_id, at);
