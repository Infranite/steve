-- 50: who a run works for and who reads what it makes (path model S1). `items.requested_by` is the
-- member who asked for the item (NULL: Steve, a flow, someone gone). `runtime_sessions.principal` is
-- whose access a Node session runs with, fixed when it is bound: the member who asked for its item,
-- or `svc:<workspace>`, the workspace's service principal, for work nobody (known) asked for;
-- `audience` is the JSON array of who its output goes to (`user:<id>`, `workspace:<id>`);
-- `state_dir` and `view_digest` stay NULL until per-principal state and views exist. `bots.owner` edits the bot (NULL: the instance admin, as for the main bot).
-- `node_grants` lets a Node's owner share it (no quota columns); `node_roots` names a Node's roots
-- by an id of the Hub's; `node_env` is one user's variables on one Node. Recorded only: which
-- Node takes which work does not change here.
ALTER TABLE items ADD COLUMN requested_by TEXT;
ALTER TABLE runtime_sessions ADD COLUMN principal TEXT;
ALTER TABLE runtime_sessions ADD COLUMN audience TEXT;
ALTER TABLE runtime_sessions ADD COLUMN state_dir TEXT;
ALTER TABLE runtime_sessions ADD COLUMN view_digest TEXT;
ALTER TABLE bots ADD COLUMN owner TEXT;
CREATE TABLE node_grants (
 id TEXT PRIMARY KEY,
 node_id TEXT NOT NULL REFERENCES nodes(node_id),
 grantee_kind TEXT NOT NULL CHECK(grantee_kind IN ('user','workspace','team')),
 grantee TEXT NOT NULL,
 roots TEXT NOT NULL DEFAULT '[]',
 bots TEXT NOT NULL DEFAULT '"*"',
 max_concurrent INTEGER CHECK(max_concurrent IS NULL OR max_concurrent > 0),
 expires INTEGER,
 created_by TEXT NOT NULL,
 created INTEGER NOT NULL,
 accepted INTEGER,
 revoked INTEGER,
 UNIQUE(node_id, grantee_kind, grantee));
CREATE TABLE node_roots (
 root_id TEXT PRIMARY KEY,
 node_id TEXT NOT NULL,
 path TEXT NOT NULL,
 mode TEXT NOT NULL CHECK(mode IN ('ro','rw')),
 owner_root INTEGER NOT NULL DEFAULT 0 CHECK(owner_root IN (0,1)),
 created INTEGER NOT NULL,
 UNIQUE(node_id, path));
CREATE TABLE node_env (
 node_id TEXT NOT NULL,
 principal TEXT NOT NULL,
 key TEXT NOT NULL,
 value TEXT,
 vault_ref TEXT,
 note TEXT NOT NULL DEFAULT '',
 updated_by TEXT NOT NULL,
 updated INTEGER NOT NULL,
 CHECK((value IS NULL) <> (vault_ref IS NULL)),
 PRIMARY KEY(node_id, principal, key));

-- backfill: an item's creator is in the audit trail (`item.create` by a member); the bot's owner is
-- its workspace's owner, except the main bot's; a session's principal is its item's creator while a
-- member, else the service principal, and it is read by that one person when the workspace has only
-- them, else by the workspace; a confirmed Node is shared with each workspace its owner owns (the
-- local Node with every workspace), accepted at once, as the owner shares it with themself.
UPDATE items SET requested_by = (
  SELECT a.actor FROM audit a JOIN workspace_members m ON m.workspace_id = items.workspace_id AND m.user_id = a.actor
   WHERE a.action = 'item.create' AND a.target = items.id ORDER BY a.id LIMIT 1);
UPDATE bots SET owner = (
  SELECT m.user_id FROM workspace_members m WHERE m.workspace_id = bots.workspace_id AND m.role = 'owner'
   ORDER BY m.created LIMIT 1) WHERE main = 0;
UPDATE runtime_sessions SET principal = (
  SELECT COALESCE(
    (SELECT i.requested_by FROM items i JOIN workspace_members m ON m.workspace_id = i.workspace_id AND m.user_id = i.requested_by
      WHERE i.id = s.item_id),
    'svc:' || s.workspace_id)
  FROM sessions s WHERE s.id = runtime_sessions.session_id);
UPDATE runtime_sessions SET audience = (
  SELECT CASE WHEN (SELECT COUNT(*) FROM workspace_members m WHERE m.workspace_id = s.workspace_id) = 1
               AND runtime_sessions.principal NOT LIKE 'svc:%'
              THEN json_array('user:' || runtime_sessions.principal)
              ELSE json_array('workspace:' || s.workspace_id) END
  FROM sessions s WHERE s.id = runtime_sessions.session_id);
INSERT OR IGNORE INTO node_grants (id, node_id, grantee_kind, grantee, created_by, created, accepted)
  SELECT 'ng_' || n.node_id || '_' || m.workspace_id, n.node_id, 'workspace', m.workspace_id, n.owner,
         CAST(strftime('%s','now') AS INTEGER) * 1000, CAST(strftime('%s','now') AS INTEGER) * 1000
    FROM nodes n JOIN workspace_members m ON m.user_id = n.owner AND m.role = 'owner'
   WHERE n.state IN ('active','paused') AND n.local = 0;
INSERT OR IGNORE INTO node_grants (id, node_id, grantee_kind, grantee, created_by, created, accepted)
  SELECT 'ng_' || n.node_id || '_' || w.id, n.node_id, 'workspace', w.id, n.owner,
         CAST(strftime('%s','now') AS INTEGER) * 1000, CAST(strftime('%s','now') AS INTEGER) * 1000
    FROM nodes n JOIN workspaces w
   WHERE n.state IN ('active','paused') AND n.local = 1;
