-- ST-60: durable collaboration graphs. A unit's child item owns its sessions and authorization.
CREATE TABLE workflows (
 item_id TEXT PRIMARY KEY REFERENCES items(id) ON DELETE CASCADE,
 revision INTEGER NOT NULL CHECK(revision > 0),
 cycle INTEGER NOT NULL DEFAULT 1 CHECK(cycle > 0),
 goal_version INTEGER NOT NULL DEFAULT 1 CHECK(goal_version >= 0),
 paused INTEGER NOT NULL DEFAULT 0 CHECK(paused IN (0,1)),
 max_parallel INTEGER NOT NULL CHECK(max_parallel > 0),
 max_attempts INTEGER NOT NULL CHECK(max_attempts > 0),
 created INTEGER NOT NULL, updated INTEGER NOT NULL);
CREATE TABLE workflow_units (
 workflow_id TEXT NOT NULL REFERENCES workflows(item_id) ON DELETE CASCADE,
 id TEXT NOT NULL, position INTEGER NOT NULL, spec TEXT NOT NULL,
 status TEXT NOT NULL CHECK(status IN ('pending','dispatching','running','succeeded','failed','cancelled')),
 item_id TEXT UNIQUE REFERENCES items(id),
 session_id TEXT REFERENCES sessions(id),
 attempt INTEGER NOT NULL DEFAULT 0 CHECK(attempt >= 0),
 summary TEXT NOT NULL DEFAULT '', outputs TEXT NOT NULL DEFAULT '[]',
 checks TEXT NOT NULL DEFAULT '[]', reason TEXT,
 PRIMARY KEY(workflow_id,id));
CREATE TABLE workflow_history (
 workflow_id TEXT NOT NULL REFERENCES workflows(item_id) ON DELETE CASCADE,
 revision INTEGER NOT NULL, action TEXT NOT NULL, reason TEXT NOT NULL,
 unit_id TEXT, detail TEXT NOT NULL, created INTEGER NOT NULL,
 PRIMARY KEY(workflow_id,revision));
CREATE INDEX workflow_units_active ON workflow_units(workflow_id,status);
-- Old child identities remain recognizable after reset/reassignment. They never regain reporting
-- authority, and cannot accidentally enter the ordinary single-item delivery/publishing path.
CREATE TABLE workflow_attempts (
 item_id TEXT PRIMARY KEY REFERENCES items(id),
 workflow_id TEXT NOT NULL REFERENCES workflows(item_id) ON DELETE CASCADE,
 unit_id TEXT NOT NULL, snapshot TEXT NOT NULL, created INTEGER NOT NULL);
