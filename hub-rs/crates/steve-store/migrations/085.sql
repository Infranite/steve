-- Hub-managed Node upgrades: a Node's own policy (NULL: the Hub's default, the setting
-- `node.upgrade_policy`), and every upgrade the Hub ran or asked for, with where it is.
ALTER TABLE nodes ADD COLUMN upgrade_policy TEXT
  CHECK(upgrade_policy IS NULL OR upgrade_policy IN ('auto','notify','off'));
CREATE TABLE node_upgrades (
 operation_id TEXT PRIMARY KEY,
 node_id TEXT NOT NULL,
 version TEXT NOT NULL,
 commit_sha TEXT NOT NULL,
 target TEXT NOT NULL,
 from_version TEXT,
 artifact TEXT NOT NULL,
 file TEXT NOT NULL DEFAULT '',
 sha256 TEXT NOT NULL DEFAULT '',
 size INTEGER NOT NULL DEFAULT 0,
 requested_by TEXT NOT NULL,
 force INTEGER NOT NULL DEFAULT 0 CHECK(force IN (0,1)),
 phase TEXT NOT NULL,
 error TEXT,
 created INTEGER NOT NULL,
 updated INTEGER NOT NULL);
CREATE INDEX node_upgrades_node ON node_upgrades(node_id, created);
